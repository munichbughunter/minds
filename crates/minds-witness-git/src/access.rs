//! `GitObjectAccess` (Spec S2 §7.2 bis §7.5): der `ObjectAccess` des Kerns
//! und die `EvidenceView` des Inspektors über einem
//! [`minds_git::quarantine::ServerRepo`]. Refs werden zuerst aus den
//! Updates des Pushes, dann aus dem memoisierten Vorzustand gelesen;
//! symbolische Refs werden nie verfolgt.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use minds_git::quarantine::{ObjectKind, ReceiveError, RefTarget, ServerRepo};
use minds_witness_core::{
    CommitId, CommitInfo, CommitRange, ObjectAccess, ObjectError, RefUpdate, SealBlob,
};

use crate::binding::CommitBinding;
use crate::budget::Budget;
use crate::candidates::{self, Candidate};
use crate::limits::AdapterLimits;
use crate::trust::TrustConfig;
use crate::view::{EvidenceView, MODE_BLOB, TreeEntry, ViewError};

/// Laufzeit für `rev-list` ohne Deadline (nur Tests).
const DEFAULT_PROCESS_TIME: Duration = Duration::from_secs(30);

/// Objektzugriff eines Hook-Aufrufs.
pub struct GitObjectAccess<'a> {
    repo: &'a ServerRepo,
    updates: &'a [RefUpdate],
    git: &'a Path,
    budget: &'a Budget,
    trust: &'a TrustConfig,
    limits: &'a AdapterLimits,
    evaluated: Cell<u32>,
    changed: RefCell<HashMap<String, BTreeSet<String>>>,
    diagnostics: RefCell<Vec<String>>,
}

fn internal(e: ReceiveError) -> ObjectError {
    ObjectError::Internal(e.to_string())
}

fn view_err(e: ReceiveError) -> ViewError {
    ViewError(match e {
        ReceiveError::NotFound => "object not found",
        ReceiveError::Verify => "object verification failed",
        ReceiveError::MalformedTree => "malformed tree",
        ReceiveError::MalformedCommit => "malformed commit",
        ReceiveError::Limit(_) => "limit exceeded",
        ReceiveError::Precondition(_) | ReceiveError::Read(_) => "repository read failed",
        ReceiveError::RevList(_) => "git rev-list failed",
    })
}

/// Pfadform aus S1 v1.5 V6: gültiges UTF-8 unverändert, sonst
/// `"\0nonutf8:" + hex(bytes)`.
pub fn path_form(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => format!("\0nonutf8:{}", hex::encode(bytes)),
    }
}

impl<'a> GitObjectAccess<'a> {
    /// Ein Zugriff über `repo` mit den Updates des Pushes.
    pub fn new(
        repo: &'a ServerRepo,
        updates: &'a [RefUpdate],
        git: &'a Path,
        budget: &'a Budget,
        trust: &'a TrustConfig,
        limits: &'a AdapterLimits,
    ) -> Self {
        Self {
            repo,
            updates,
            git,
            budget,
            trust,
            limits,
            evaluated: Cell::new(0),
            changed: RefCell::new(HashMap::new()),
            diagnostics: RefCell::new(Vec::new()),
        }
    }

    /// Diagnosen (Ref-Namen und feste Gründe).
    pub fn diagnostics(&self) -> Vec<String> {
        self.diagnostics.borrow().clone()
    }

    fn deadline(&self) -> Result<(), ObjectError> {
        self.budget
            .check_deadline()
            .map_err(|e| ObjectError::Internal(e.to_string()))
    }

    /// Ziel eines Refs: Push vor Vorzustand; Symrefs zählen nicht.
    fn target(&self, name: &str) -> Result<Option<String>, ReceiveError> {
        if let Some(u) = self.updates.iter().find(|u| u.ref_name == name) {
            return Ok((!u.new.is_null()).then(|| u.new.as_str().to_owned()));
        }
        Ok(match self.repo.prestate().read_ref(name)? {
            Some(RefTarget::Direct(id)) => Some(id),
            _ => None,
        })
    }

    fn changed(&self, id: &str) -> Result<BTreeSet<String>, ReceiveError> {
        if let Some(memo) = self.changed.borrow().get(id) {
            return Ok(memo.clone());
        }
        let c = self.repo.commit_parents(id)?;
        let mut result: Option<BTreeSet<Vec<u8>>> = None;
        if c.parents.is_empty() {
            result = Some(
                self.repo
                    .tree_diff(None, Some(&c.tree))?
                    .into_iter()
                    .map(|d| d.path)
                    .collect(),
            );
        }
        for p in &c.parents {
            let ptree = self.repo.commit_parents(p)?.tree;
            let set: BTreeSet<Vec<u8>> = self
                .repo
                .tree_diff(Some(&ptree), Some(&c.tree))?
                .into_iter()
                .map(|d| d.path)
                .collect();
            result = Some(match result {
                None => set,
                Some(acc) => acc.intersection(&set).cloned().collect(),
            });
        }
        let out: BTreeSet<String> = result
            .unwrap_or_default()
            .iter()
            .map(|p| path_form(p))
            .collect();
        self.changed.borrow_mut().insert(id.to_owned(), out.clone());
        Ok(out)
    }

    /// Ob die Bindung hinter `target` einen anerkannten Schlüssel nennt
    /// (Rust-Vorfilter, ohne Prozess). Unlesbares zählt als nicht anerkannt.
    fn recognized_binding(&self, target: &str) -> Result<bool, ReceiveError> {
        let Ok(c) = self.repo.commit_parents(target) else {
            return Ok(false);
        };
        let Ok(entries) = self.repo.tree_entries(&c.tree) else {
            return Ok(false);
        };
        let Some(e) = entries
            .iter()
            .find(|e| e.name == b"binding" && e.mode == MODE_BLOB)
        else {
            return Ok(false);
        };
        let Some(bytes) = self
            .repo
            .blob_bytes(&e.oid, self.limits.max_binding_bytes)?
        else {
            return Ok(false);
        };
        self.budget
            .take_bytes(bytes.len() as u64)
            .map_err(|_| ReceiveError::Limit("evidence bytes"))?;
        Ok(
            CommitBinding::parse(&bytes, self.limits.max_seals_per_binding)
                .is_ok_and(|b| self.trust.recognizes(b.key())),
        )
    }

    /// Spec S2 §7.5: Kandidaten unter `refs/minds/bindings/<c>/`.
    fn candidates(&self, commit: &str) -> Result<Vec<Candidate>, ReceiveError> {
        let prefix = candidates::prefix(commit);
        let mut all: BTreeMap<String, String> = BTreeMap::new();
        for u in self
            .updates
            .iter()
            .filter(|u| u.ref_name.starts_with(&prefix))
        {
            if !u.new.is_null() {
                all.insert(u.ref_name.clone(), u.new.as_str().to_owned());
            }
        }
        for (name, target) in self.repo.prestate().read_prefix(&prefix)? {
            if !self.updates.iter().any(|u| u.ref_name == name) {
                all.entry(name).or_insert(target);
            }
        }
        let mut list: Vec<Candidate> = all
            .into_iter()
            .take(self.limits.max_binding_enumeration)
            .map(|(ref_name, target)| Candidate { ref_name, target })
            .collect();
        if list.len() > self.limits.max_binding_candidates {
            let mut recognized = Vec::new();
            let mut other = Vec::new();
            for c in list {
                if self.recognized_binding(&c.target)? {
                    recognized.push(c);
                } else {
                    other.push(c);
                }
            }
            recognized.extend(other);
            let rest = recognized.split_off(self.limits.max_binding_candidates);
            for c in rest {
                self.diagnostics
                    .borrow_mut()
                    .push(format!("candidate_over_limit {}", c.ref_name));
            }
            list = recognized;
        }
        Ok(list)
    }
}

impl ObjectAccess for GitObjectAccess<'_> {
    fn commit(&self, id: &CommitId) -> Result<CommitInfo, ObjectError> {
        self.deadline()?;
        let c = self.repo.commit_parents(id.as_str()).map_err(|e| match e {
            ReceiveError::NotFound => ObjectError::NotFound(id.as_str().to_owned()),
            e => internal(e),
        })?;
        let parents = c
            .parents
            .iter()
            .map(|p| CommitId::parse(p).map_err(|_| ObjectError::Internal("commit id".to_owned())))
            .collect::<Result<_, _>>()?;
        Ok(CommitInfo {
            id: id.clone(),
            parents,
        })
    }

    fn new_commits(
        &self,
        old: Option<&CommitId>,
        new: &CommitId,
        limit: u32,
    ) -> Result<CommitRange, ObjectError> {
        self.deadline()?;
        // Spec S2 §7.0 Regel 8: Ein bewerteter Ref muss auf einen Commit
        // zeigen; Tags werden nicht aufgelöst.
        match self.repo.kind(new.as_str()).map_err(internal)? {
            Some(ObjectKind::Commit) => {}
            Some(_) => return Err(ObjectError::Internal("target is not a commit".to_owned())),
            None => return Err(ObjectError::NotFound(new.as_str().to_owned())),
        }
        let nots: Vec<String> = old.map(|o| o.as_str().to_owned()).into_iter().collect();
        let time = self.budget.remaining().unwrap_or(DEFAULT_PROCESS_TIME);
        let rows = self
            .repo
            .rev_list(self.git, new.as_str(), &nots, limit as usize + 1, time)
            .map_err(internal)?;
        let truncated = rows.len() > limit as usize;
        let set: BTreeSet<&str> = rows.iter().map(|(c, _)| c.as_str()).collect();
        // Jede gelieferte Id als Commit verifizieren.
        for (c, _) in &rows {
            self.repo.commit_parents(c).map_err(internal)?;
        }
        // Kahn, eingeschränkt auf M, kleinste Id zuerst.
        let mut indegree: BTreeMap<&str, usize> = rows
            .iter()
            .map(|(c, ps)| {
                (
                    c.as_str(),
                    ps.iter().filter(|p| set.contains(p.as_str())).count(),
                )
            })
            .collect();
        let mut order = Vec::with_capacity(rows.len());
        while let Some(next) = indegree.iter().find(|(_, n)| **n == 0).map(|(c, _)| *c) {
            indegree.remove(next);
            for (c, ps) in &rows {
                if ps.iter().any(|p| p == next)
                    && let Some(n) = indegree.get_mut(c.as_str())
                {
                    *n -= 1;
                }
            }
            order.push(next);
        }
        if order.len() != rows.len() {
            return Err(ObjectError::Internal("commit graph has a cycle".to_owned()));
        }
        order.truncate(limit as usize);
        let counted = if truncated { 1 } else { order.len() as u32 };
        let total = self.evaluated.get().saturating_add(counted);
        if total > self.limits.max_total_commits_per_push {
            return Err(ObjectError::Internal("commit budget exhausted".to_owned()));
        }
        self.evaluated.set(total);
        let commits = order
            .into_iter()
            .map(|c| CommitId::parse(c).map_err(|_| ObjectError::Internal("commit id".to_owned())))
            .collect::<Result<_, _>>()?;
        Ok(CommitRange { commits, truncated })
    }

    fn changed_paths(&self, id: &CommitId) -> Result<BTreeSet<String>, ObjectError> {
        self.deadline()?;
        self.changed(id.as_str()).map_err(internal)
    }

    fn seal_for_commit(&self, id: &CommitId) -> Result<Option<SealBlob>, ObjectError> {
        self.deadline()?;
        let list = self.candidates(id.as_str()).map_err(internal)?;
        Ok((!list.is_empty()).then(|| candidates::to_blob(id.as_str(), list)))
    }
}

impl EvidenceView for GitObjectAccess<'_> {
    fn resolve_ref(&self, name: &str) -> Result<Option<String>, ViewError> {
        self.target(name).map_err(view_err)
    }

    fn tree(&self, commit: &str) -> Result<Vec<TreeEntry>, ViewError> {
        let c = self.repo.commit_parents(commit).map_err(view_err)?;
        let entries = self.repo.tree_entries(&c.tree).map_err(view_err)?;
        entries
            .into_iter()
            .map(|e| {
                let size = self.repo.size(&e.oid).map_err(view_err)?.unwrap_or(0);
                Ok(TreeEntry {
                    name: String::from_utf8_lossy(&e.name).into_owned(),
                    mode: e.mode,
                    oid: e.oid,
                    size,
                })
            })
            .collect()
    }

    fn entry(&self, commit: &str, path: &str) -> Result<Option<TreeEntry>, ViewError> {
        let c = self.repo.commit_parents(commit).map_err(view_err)?;
        let parts: Vec<&[u8]> = path.split('/').map(str::as_bytes).collect();
        let Some(e) = self.repo.lookup(&c.tree, &parts).map_err(view_err)? else {
            return Ok(None);
        };
        let size = self.repo.size(&e.oid).map_err(view_err)?.unwrap_or(0);
        Ok(Some(TreeEntry {
            name: path.to_owned(),
            mode: e.mode,
            oid: e.oid,
            size,
        }))
    }

    fn parents(&self, commit: &str) -> Result<Vec<String>, ViewError> {
        Ok(self.repo.commit_parents(commit).map_err(view_err)?.parents)
    }

    fn changed_paths(&self, commit: &str) -> Result<BTreeSet<String>, ViewError> {
        self.changed(commit).map_err(view_err)
    }

    fn blob(&self, oid: &str, max: u64) -> Result<Option<Vec<u8>>, ViewError> {
        self.repo.blob_bytes(oid, max).map_err(view_err)
    }
}
