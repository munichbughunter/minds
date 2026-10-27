//! Die Nachrechnung einer `/2`-Quittung (Spec S3 §6.3, Ablauf Schritt 4):
//! Mit dem signierten Trust-Snapshot als Trust-Konfiguration und der
//! beschafften Evidence laufen `GitSealInspector` und die Befund-Regel des
//! Servers ([`minds_witness_core::assess`]) erneut; `seal`, `coverage` und
//! `policy` müssen übereinstimmen.
//!
//! **Später hinzugekommene Bindungen.** Die Quittung nennt die Ids der
//! akzeptierten Bindungen nicht, ihr `seal.hash` deckt sie. Für eine
//! Quittung der Klasse „accepted“ zählen nur Bindungen, die unter dem
//! **historischen** Snapshot akzeptiert sind; unter ihnen wird die
//! Teilmenge `S` mit `H("accepted_bindings", c, S) == seal.hash` gesucht
//! (höchstens 8, also höchstens 255 Teilmengen) und genau mit ihr
//! nachgerechnet. Für die übrigen Klassen hängt die Abdeckung nicht an den
//! Bindungen: Sie wird immer nachgerechnet, nur der Hash notfalls nicht.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use minds_git::quarantine::ServerRepo;
use minds_witness_core::{
    Assessment, CoverageStatus, HashStr, PolicyResult, Receipt, SealError, SealExpectation,
    SealInspector, SealLookup, SealState, TrustSnapshot, assess, judge,
};
use minds_witness_git::access::{RepoView, changed_paths_of};
use minds_witness_git::binding::CommitBinding;
use minds_witness_git::budget::Budget;
use minds_witness_git::candidates::{self, Candidate};
use minds_witness_git::inspector::{
    EVIDENCE_PREFIX, GitSealInspector, InspectorConfig, MAX_SEAL_BYTES, OBSERVATIONS_PREFIX,
    binding_hash,
};
use minds_witness_git::limits::AdapterLimits;
use minds_witness_git::selection;
use minds_witness_git::trust::{TrustConfig, Verifier};
use minds_witness_git::view::{EvidenceView, MODE_BLOB, TreeEntry, ViewError};

/// Höchstzahl akzeptierter Bindungen für die Teilmengen-Suche.
pub(super) const MAX_ACCEPTED: usize = 8;
/// Höchstzahl der Bindungs-Kandidaten eines Commits: `max_binding_candidates`
/// des Servers (nicht konfigurierbar, `AdapterLimits::default()`).
pub(super) fn max_candidates() -> usize {
    AdapterLimits::default().max_binding_candidates
}
/// Frist je `ssh-keygen`-Prüfung.
const SSH_KEYGEN_TIME: Duration = Duration::from_secs(10);

/// Die Evidence-Sicht: ohne Remote die lokalen Refs, mit Remote
/// **ausschließlich** die Tracking-Refs (`refs/minds/<rest>` wird als
/// `refs/minds/remotes/<r>/<rest>` gelesen).
pub(super) struct Tracked<'a> {
    inner: RepoView<'a>,
    prefix: Option<String>,
}

impl<'a> Tracked<'a> {
    pub(super) fn new(repo: &'a ServerRepo, tracking: Option<String>) -> Self {
        Self {
            inner: RepoView::new(repo),
            prefix: tracking,
        }
    }

    /// Der Ref-Name, unter dem `name` hier gelesen wird.
    pub(super) fn local_name(&self, name: &str) -> Option<String> {
        match &self.prefix {
            None => Some(name.to_owned()),
            Some(p) => name
                .strip_prefix("refs/minds/")
                .map(|rest| format!("{p}{rest}")),
        }
    }
}

impl EvidenceView for Tracked<'_> {
    fn resolve_ref(&self, name: &str) -> Result<Option<String>, ViewError> {
        match self.local_name(name) {
            Some(local) => self.inner.resolve_ref(&local),
            None => Ok(None),
        }
    }

    fn tree(&self, commit: &str) -> Result<Vec<TreeEntry>, ViewError> {
        self.inner.tree(commit)
    }

    fn entry(&self, commit: &str, path: &str) -> Result<Option<TreeEntry>, ViewError> {
        self.inner.entry(commit, path)
    }

    fn parents(&self, commit: &str) -> Result<Vec<String>, ViewError> {
        self.inner.parents(commit)
    }

    fn changed_paths(&self, commit: &str) -> Result<BTreeSet<String>, ViewError> {
        self.inner.changed_paths(commit)
    }

    fn blob(&self, oid: &str, max: u64) -> Result<Option<Vec<u8>>, ViewError> {
        self.inner.blob(oid, max)
    }
}

/// Die Bindungs-Kandidaten von `commit` (kanonische Namen), aus der Sicht.
pub(super) fn candidates_of(
    repo: &ServerRepo,
    view: &Tracked<'_>,
    commit: &str,
) -> Result<Vec<Candidate>, String> {
    let canonical = candidates::prefix(commit);
    let local = view.local_name(&canonical).ok_or("binding prefix")?;
    let refs = repo
        .prestate()
        .read_prefix(&local)
        .map_err(|_| "binding refs unreadable".to_owned())?;
    Ok(refs
        .into_iter()
        .filter_map(|(name, target)| {
            let tail = name.strip_prefix(&local)?;
            Some(Candidate {
                ref_name: format!("{canonical}{tail}"),
                target,
            })
        })
        .collect())
}

/// Die Bindung hinter einem Kandidaten, `None` wenn nicht lesbar.
pub(super) fn binding_of<V: EvidenceView + ?Sized>(
    view: &V,
    cand: &Candidate,
) -> Result<Option<CommitBinding>, String> {
    let tree = view.tree(&cand.target).map_err(|e| e.to_string())?;
    let Some(entry) = tree
        .iter()
        .find(|e| e.name == "binding" && e.mode == MODE_BLOB)
    else {
        return Ok(None);
    };
    let Some(bytes) = view
        .blob(&entry.oid, AdapterLimits::default().max_binding_bytes)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    Ok(CommitBinding::parse(&bytes, AdapterLimits::default().max_seals_per_binding).ok())
}

/// Die Observation-Id, die ein Seal nennt (`None`: keine oder Seal nicht
/// lesbar). Liest über die Sicht.
pub(super) fn observations_of<V: EvidenceView + ?Sized>(
    view: &V,
    seal_id: &str,
) -> Result<Option<String>, String> {
    let Some(hex) = seal_id.strip_prefix("b3-") else {
        return Ok(None);
    };
    let Some(target) = view
        .resolve_ref(&format!("{EVIDENCE_PREFIX}{hex}"))
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let tree = view.tree(&target).map_err(|e| e.to_string())?;
    let Some(entry) = tree
        .iter()
        .find(|e| e.name == "seal" && e.mode == MODE_BLOB)
    else {
        return Ok(None);
    };
    let Some(bytes) = view
        .blob(&entry.oid, MAX_SEAL_BYTES)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(None);
    };
    Ok(selection::seal_observations(seal_id, text).and_then(|(_, obs)| obs))
}

/// Die Grenzen des Inspektors wie im Hook (`hook/run.rs`): aus
/// `AdapterLimits`, die der Server nicht konfigurierbar macht.
fn inspector_config(project: &str) -> InspectorConfig {
    let limits = AdapterLimits::default();
    let mut config = InspectorConfig::new(project);
    config.max_binding_bytes = limits.max_binding_bytes;
    config.max_seals_per_binding = limits.max_seals_per_binding;
    config.max_observations_bytes = limits.max_observations_bytes;
    config
}

/// Was die Nachrechnung verwendet hat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Counts {
    pub(super) bindings: usize,
    pub(super) seals: usize,
    pub(super) observations: usize,
}

/// Ergebnis der Nachrechnung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Stimmt; `note` ist die Hinweiszeile für einen nicht nachrechenbaren
    /// Hash ohne Einfluss auf den Exit-Code.
    Matches {
        counts: Counts,
        note: Option<&'static str>,
    },
    /// Weicht ab (Exit 1): was abweicht.
    Differs(&'static str),
    /// Evidence fehlt (Exit 3): je Objekt eine Angabe.
    Missing(Vec<String>),
    /// Nicht nachrechenbar (Exit 3).
    NotRecomputable(&'static str),
    /// Technischer Fehler (Exit 4).
    Failed(String),
}

fn internal(e: SealError) -> Outcome {
    Outcome::Failed(e.to_string())
}

/// Vergleicht eine Nachrechnung mit der Quittung.
fn compare(receipt: &Receipt, got: &Assessment, policy: &PolicyResult) -> Option<&'static str> {
    if got.seal != receipt.seal {
        Some("seal.hash")
    } else if got.coverage != receipt.coverage {
        Some("Abdeckung")
    } else if *policy != receipt.policy {
        Some("Policy")
    } else {
        None
    }
}

/// Alle nicht leeren Teilmengen von `items` (höchstens [`MAX_ACCEPTED`]
/// Elemente), kleinste zuerst.
fn subsets<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    let n = items.len().min(MAX_ACCEPTED);
    let mut all: Vec<Vec<T>> = (1u32..(1 << n))
        .map(|mask| {
            (0..n)
                .filter(|i| mask & (1 << i) != 0)
                .map(|i| items[i].clone())
                .collect()
        })
        .collect();
    all.sort_by_key(Vec::len);
    all
}

/// Für eine Quittung außerhalb der Klasse „accepted“, deren Hash heute
/// nicht nachrechenbar ist: Erklärt eine echte Teilmenge der heutigen
/// Kandidaten den signierten Hash (dieselbe Hash-Regel wie der Inspektor)?
/// `Err`: zu viele Kandidaten für die Suche.
fn earlier_set_explains(
    view: &Tracked<'_>,
    receipt: &Receipt,
    all: &[Candidate],
) -> Result<bool, &'static str> {
    let c = receipt.commit.as_str();
    let Some(hash) = receipt.seal.hash.as_ref() else {
        // Beim Push gab es keinen Kandidaten; heute gibt es welche.
        return Ok(receipt.seal.status == SealState::Absent && !all.is_empty());
    };
    let id_of = |cand: &Candidate| {
        cand.ref_name
            .rsplit('/')
            .next()
            .map(|hex| format!("b3-{hex}"))
    };
    match (&receipt.seal.status, &receipt.coverage.status) {
        // Eine echte Teilmenge: heute mehr als der eine Kandidat.
        (SealState::Mismatched, _) => Ok(all.len() > 1
            && all
                .iter()
                .filter_map(id_of)
                .any(|id| binding_hash("mismatched_binding", c, vec![id]) == *hash)),
        (SealState::Invalid | SealState::Unsigned, CoverageStatus::Computed) => {
            let readable: Vec<String> = all
                .iter()
                .filter(|cand| matches!(binding_of(view, cand), Ok(Some(_))))
                .filter_map(id_of)
                .collect();
            if readable.len() > MAX_ACCEPTED {
                return Err("mehr als 8 lesbare Bindungen");
            }
            Ok(subsets(&readable)
                .into_iter()
                .filter(|s| s.len() < readable.len())
                .any(|s| binding_hash("rejected_bindings", c, s) == *hash))
        }
        (SealState::Invalid, CoverageStatus::NoSeal) => {
            if all.len() > MAX_ACCEPTED {
                return Err("mehr als 8 Bindungen");
            }
            Ok(subsets(all)
                .into_iter()
                .filter(|s| s.len() < all.len())
                .any(|s| HashStr::of_bytes(&candidates::to_blob(c, s).bytes) == *hash))
        }
        _ => Ok(false),
    }
}

/// Die Nachrechnung einer Quittung, deren Signatur und Snapshot geprüft
/// sind.
pub(super) fn recompute(
    repo: &ServerRepo,
    view: &Tracked<'_>,
    program: &Path,
    receipt: &Receipt,
    snapshot: &TrustSnapshot,
) -> Outcome {
    if receipt.repo != snapshot.project {
        return Outcome::Differs("Projekt");
    }
    let c = receipt.commit.as_str();
    let parents = match repo.commit_parents(c) {
        Ok(data) => data.parents.len(),
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let paths = match changed_paths_of(repo, c) {
        Ok(p) => p,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let policy = snapshot.policy.evaluation_policy();
    let policy_result = |result| PolicyResult {
        mode: policy.mode.clone(),
        result,
        version: policy.version,
    };
    let trust = match TrustConfig::from_text(
        &snapshot.signers,
        program,
        &snapshot.allowed_witness_principals,
        snapshot.require_separated_witness,
        SSH_KEYGEN_TIME,
    ) {
        Ok(t) => t,
        Err(_) => return Outcome::NotRecomputable("Signer-Snapshot nicht auswertbar"),
    };
    let budget = Budget::unlimited();
    let verifier = Verifier::new(&trust, &budget);
    let inspector = GitSealInspector::new(
        view,
        &verifier,
        &budget,
        inspector_config(&snapshot.project),
    );
    let all = match candidates_of(repo, view, c) {
        Ok(list) => list,
        Err(e) => return Outcome::Failed(e),
    };
    if all.len() > max_candidates() {
        return Outcome::NotRecomputable("mehr als 64 Bindungen");
    }
    let commit = receipt.commit.clone();
    let expected = SealExpectation {
        repo: receipt.repo.clone(),
        commit: commit.clone(),
    };
    let inspect = |list: Vec<Candidate>| -> Result<SealLookup, SealError> {
        if list.is_empty() {
            return Ok(SealLookup::Absent);
        }
        let blob = candidates::to_blob(c, list);
        match inspector.inspect(&blob, &expected) {
            Ok(summary) => Ok(SealLookup::Inspected(blob, summary)),
            Err(SealError::Malformed(_)) => Ok(SealLookup::Malformed(blob)),
            Err(e) => Err(e),
        }
    };

    let accepted_class = receipt.seal.status == SealState::Valid
        && receipt.coverage.status == CoverageStatus::Computed;
    if !accepted_class {
        // Die Abdeckung hängt hier nicht an den Bindungen; der Hash nur,
        // wenn die Kandidatenmenge seither gleich blieb.
        let got = match assess(
            &commit,
            &receipt.repo,
            parents,
            &paths,
            || inspect(all.clone()),
            &policy,
        ) {
            Ok(a) => a,
            Err(e) => return internal(e),
        };
        let counts = Counts {
            bindings: all.len(),
            ..Counts::default()
        };
        if compare(receipt, &got, &policy_result(got.result.clone())).is_none() {
            return Outcome::Matches { counts, note: None };
        }
        let again = judge(
            receipt.seal.clone(),
            receipt.coverage.status.clone(),
            &paths,
            Some(&BTreeSet::new()),
            &policy,
        );
        if let Some(what) = compare(receipt, &again, &policy_result(again.result.clone())) {
            return Outcome::Differs(what);
        }
        // Der Hash darf nur abweichen, weil seither Kandidaten hinzukamen:
        // Eine Teilmenge der heutigen erklärt ihn, sonst weicht er ab.
        return match earlier_set_explains(view, receipt, &all) {
            Ok(true) => Outcome::Matches {
                counts,
                note: Some("seal.hash nicht nachrechenbar (Kandidatenmenge seither erweitert)"),
            },
            Ok(false) => Outcome::Differs("seal.hash"),
            Err(why) => Outcome::NotRecomputable(why),
        };
    }

    // Klasse „accepted“: nur unter dem historischen Snapshot akzeptierte
    // Bindungen, je einzeln geprüft.
    if all.is_empty() {
        return Outcome::Missing(vec![format!("Bindung für {}", &c[..12])]);
    }
    let mut accepted: Vec<(String, Candidate)> = Vec::new();
    for cand in &all {
        let Some(hex) = cand.ref_name.rsplit('/').next() else {
            continue;
        };
        let id = format!("b3-{hex}");
        let single = binding_hash("accepted_bindings", c, vec![id.clone()]);
        match inspect(vec![cand.clone()]) {
            Ok(SealLookup::Inspected(_, summary))
                if summary.hash == single && summary.bound_commit == commit =>
            {
                accepted.push((id, cand.clone()));
            }
            Ok(_) => {}
            Err(e) => return internal(e),
        }
    }
    if accepted.len() > MAX_ACCEPTED {
        return Outcome::NotRecomputable("mehr als 8 akzeptierte Bindungen");
    }
    let Some(hash) = receipt.seal.hash.as_ref() else {
        return Outcome::Differs("seal.hash");
    };
    let Some(chosen) = subsets(&accepted).into_iter().find(|s| {
        binding_hash(
            "accepted_bindings",
            c,
            s.iter().map(|(id, _)| id.clone()).collect(),
        ) == *hash
    }) else {
        return Outcome::Differs("seal.hash (keine passende Teilmenge der Bindungen)");
    };

    // Fehlt Evidence der gewählten Bindungen, wird nicht still gerechnet.
    let mut missing = Vec::new();
    let mut counts = Counts {
        bindings: chosen.len(),
        ..Counts::default()
    };
    let mut seal_ids: BTreeSet<String> = BTreeSet::new();
    for (_, cand) in &chosen {
        match binding_of(view, cand) {
            Ok(Some(b)) => seal_ids.extend(b.seals().iter().cloned()),
            Ok(None) => return Outcome::Differs("Bindung nicht lesbar"),
            Err(e) => return Outcome::Failed(e),
        }
    }
    for seal_id in &seal_ids {
        let present = view
            .resolve_ref(&format!("{EVIDENCE_PREFIX}{}", &seal_id[3..]))
            .map(|t| t.is_some());
        match present {
            Ok(true) => counts.seals += 1,
            Ok(false) => {
                missing.push(format!("Seal {seal_id}"));
                continue;
            }
            Err(e) => return Outcome::Failed(e.to_string()),
        }
        let obs = match observations_of(view, seal_id) {
            Ok(obs) => obs,
            Err(e) => return Outcome::Failed(e),
        };
        if let Some(obs) = obs {
            let present = view
                .resolve_ref(&format!("{OBSERVATIONS_PREFIX}{}", &obs[3..]))
                .map(|t| t.is_some());
            match present {
                Ok(true) => counts.observations += 1,
                Ok(false) => missing.push(format!("Observations {obs}")),
                Err(e) => return Outcome::Failed(e.to_string()),
            }
        }
    }
    if !missing.is_empty() {
        return Outcome::Missing(missing);
    }
    let list: Vec<Candidate> = chosen.into_iter().map(|(_, cand)| cand).collect();
    let got = match assess(
        &commit,
        &receipt.repo,
        parents,
        &paths,
        || inspect(list),
        &policy,
    ) {
        Ok(a) => a,
        Err(e) => return internal(e),
    };
    match compare(receipt, &got, &policy_result(got.result.clone())) {
        None => Outcome::Matches { counts, note: None },
        Some(what) => Outcome::Differs(what),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsets_are_complete_and_smallest_first() {
        let s = subsets(&[1, 2, 3]);
        assert_eq!(s.len(), 7);
        assert_eq!(s[0].len(), 1);
        assert_eq!(s.last().unwrap(), &vec![1, 2, 3]);
        assert_eq!(subsets(&(0..8).collect::<Vec<_>>()).len(), 255);
        assert!(subsets::<u8>(&[]).is_empty());
    }
}
