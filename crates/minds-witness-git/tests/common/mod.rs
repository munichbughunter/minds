//! Gemeinsame Testhelfer: Schlüssel zur Laufzeit, Signieren, Pfade.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const PROJECT: &str = "git.test/project-42";
pub const PRINCIPAL: &str = "minds-witness-user@test";

/// Das `ssh-keygen` der Testumgebung.
pub fn ssh_keygen() -> PathBuf {
    for candidate in [
        "/usr/bin/ssh-keygen",
        "/bin/ssh-keygen",
        "/opt/homebrew/bin/ssh-keygen",
    ] {
        if Path::new(candidate).exists() {
            return PathBuf::from(candidate);
        }
    }
    PathBuf::from("ssh-keygen")
}

pub fn timeout() -> Duration {
    Duration::from_secs(10)
}

/// Ein frischer Ed25519-Schlüssel in `dir`; liefert Pfad und `<typ> <base64>`.
pub fn keypair(dir: &Path, name: &str) -> (PathBuf, String) {
    let path = dir.join(name);
    let status = Command::new(ssh_keygen())
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let public = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    let mut words = public.split_whitespace();
    let line = format!("{} {}", words.next().unwrap(), words.next().unwrap());
    (path, line)
}

/// Armierte Signatur über `payload` unter `namespace`.
pub fn sign(payload: &[u8], key: &Path, namespace: &str) -> Vec<u8> {
    minds_attest::ssh_sign_ns(std::str::from_utf8(payload).unwrap(), key, namespace)
        .unwrap()
        .into_bytes()
}

/// Eine beschränkte Witness-Zeile.
pub fn witness_line(principal: &str, key: &str) -> String {
    format!("{principal} namespaces=\"minds-witness\" {key}\n")
}

pub fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

pub fn seal_id(byte: u8) -> String {
    format!("b3-{}", hex(&[byte; 32]))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Welt für Inspektor-Tests: Objektsicht, Witness-Schlüssel, Evidence-Bau.
// ---------------------------------------------------------------------------

use std::collections::{BTreeMap, BTreeSet};

use minds_core::ContentHash;
use minds_core::evidence::{SCOPE_WITNESS_FS_V1, Seal, SealOutcome};
use minds_core::observation::{
    Isolation, Observation, ObservationReason, Observations, WitnessFacts,
};
use minds_witness_core::{
    CommitId, CommitInfo, CommitRange, ObjectAccess, ObjectError, SealBlob, SealExpectation,
};
use minds_witness_git::binding::{CommitBinding, DEFAULT_MAX_SEALS};
use minds_witness_git::budget::Budget;
use minds_witness_git::candidates::{self, Candidate};
use minds_witness_git::inspector::{GitSealInspector, InspectorConfig};
use minds_witness_git::trust::{TrustConfig, Verifier};
use minds_witness_git::view::{EvidenceView, MODE_BLOB, MemoryView};

/// Was eine Beobachtung über einen Pfad sagt.
pub enum Obs<'a> {
    Content(&'a [u8]),
    Deleted,
    Reason(ObservationReason),
}

pub fn b3(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

pub fn separated() -> WitnessFacts {
    WitnessFacts {
        profile: "user".to_owned(),
        isolation: Isolation {
            witness_uid: 1001,
            agent_uids: vec![1000],
            home_private: true,
            binary_protected: true,
        },
        spec: None,
    }
}

pub fn observations(entries: &[(&str, Obs<'_>)], witness: Option<WitnessFacts>) -> Observations {
    let list = entries
        .iter()
        .enumerate()
        .map(|(i, (path, obs))| {
            let (content, reason) = match obs {
                Obs::Content(bytes) => (Some(b3(bytes)), None),
                Obs::Deleted => (None, Some(ObservationReason::Deleted)),
                Obs::Reason(r) => (None, Some(*r)),
            };
            Observation {
                seq: i as u64 + 1,
                at: format!("2026-10-26T10:00:{:02}Z", i + 1),
                path: (*path).to_owned(),
                content,
                reason,
            }
        })
        .collect();
    let o = Observations::new("2026-10-26T10:00:00Z", list);
    match witness {
        Some(w) => o.with_witness(w),
        None => o,
    }
}

pub fn seal_text(scope: &str, outcome: SealOutcome, agent: &str) -> String {
    Seal {
        root: ContentHash::from_bytes([7; 32]),
        agent: agent.to_owned(),
        scope: scope.to_owned(),
        first_seq: 1,
        last_seq: 3,
        events: 3,
        gaps: 0,
        pre_chain: 0,
        outcome,
        previous: None,
        last_event_at: "2026-10-26T10:00:03Z".to_owned(),
    }
    .to_text()
    .unwrap()
}

pub struct World {
    pub dir: tempfile::TempDir,
    pub key: PathBuf,
    pub public: String,
    pub view: MemoryView,
    pub trust_text: String,
    salt: u32,
}

impl World {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, public) = keypair(dir.path(), "witness");
        let trust_text = witness_line(PRINCIPAL, &public);
        Self {
            dir,
            key,
            public,
            view: MemoryView::new(),
            trust_text,
            salt: 0,
        }
    }

    fn salt(&mut self) -> String {
        self.salt += 1;
        format!("s{}", self.salt)
    }

    pub fn commit(&mut self, parents: &[String], files: &[(&str, u32, &[u8])]) -> String {
        let salt = self.salt();
        self.view.add_commit(parents, files, &salt)
    }

    /// Legt ein Observation-Objekt unter seinem Ref ab; liefert die Id.
    pub fn put_observations(&mut self, obs: &Observations) -> String {
        let bytes = obs.canonical_bytes().unwrap();
        self.put_observation_bytes(&bytes)
    }

    pub fn put_observation_bytes(&mut self, bytes: &[u8]) -> String {
        let id = format!("b3-{}", hex(blake3::hash(bytes).as_bytes()));
        self.put_observation_bytes_at(bytes, &id[3..]);
        id
    }

    pub fn put_observation_bytes_at(&mut self, bytes: &[u8], hex_id: &str) {
        let tip = self.commit(&[], &[("observations.json", MODE_BLOB, bytes)]);
        self.view
            .set_ref(&format!("refs/minds/observations/{hex_id}"), &tip);
    }

    /// Legt ein Seal als Wurzel-Commit `{seal}` mit Kind-Commit
    /// `{seal, seal.sig}` ab (wie `put_seal` + `put_seal_signature`).
    pub fn put_seal(&mut self, text: &str, namespace: &str) -> String {
        let id = Seal::id_of_text(text).as_str().to_owned();
        self.put_seal_at(text, namespace, &id[3..]);
        id
    }

    pub fn put_seal_at(&mut self, text: &str, namespace: &str, hex_id: &str) {
        let sig = sign(text.as_bytes(), &self.key, namespace);
        let root = self.commit(&[], &[("seal", MODE_BLOB, text.as_bytes())]);
        let tip = self.commit(
            &[root],
            &[
                ("seal", MODE_BLOB, text.as_bytes()),
                ("seal.sig", MODE_BLOB, &sig),
            ],
        );
        self.view
            .set_ref(&format!("refs/minds/evidence/{hex_id}"), &tip);
    }

    /// Observation-Objekt plus `witness-fs/v1`-Seal, signiert unter
    /// `minds-witness`; liefert die Seal-Id.
    pub fn evidence(&mut self, obs: &Observations) -> String {
        let obs_id = self.put_observations(obs);
        let text = seal_text(
            SCOPE_WITNESS_FS_V1,
            SealOutcome::ObservationsStored {
                observations: obs_id,
            },
            "witness",
        );
        self.put_seal(&text, "minds-witness")
    }

    pub fn binding(&self, commit: &str, project: &str, seals: &[String]) -> CommitBinding {
        let mut seals = seals.to_vec();
        seals.sort();
        seals.dedup();
        CommitBinding::new(commit, &self.public, project, seals, DEFAULT_MAX_SEALS).unwrap()
    }

    /// Legt eine Bindung unter `refs/minds/bindings/<under>/<id>` ab, mit
    /// Signatur von `key` (oder ohne).
    pub fn put_binding_as(&mut self, b: &CommitBinding, under: &str, key: Option<&Path>) -> String {
        let bytes = b.to_bytes();
        let mut files: Vec<(&str, u32, &[u8])> = vec![("binding", MODE_BLOB, &bytes)];
        let sig;
        if let Some(key) = key {
            sig = sign(&bytes, key, "minds-witness");
            files.push(("binding.sig", MODE_BLOB, &sig));
        }
        let tip = self.commit(&[], &files);
        let name = format!("refs/minds/bindings/{under}/{}", &b.id()[3..]);
        self.view.set_ref(&name, &tip);
        name
    }

    pub fn put_binding(&mut self, b: &CommitBinding) -> String {
        let key = self.key.clone();
        self.put_binding_as(b, b.commit(), Some(&key))
    }

    /// Ein beliebiger Kandidat unter dem Präfix von `commit`.
    pub fn put_raw_candidate(
        &mut self,
        commit: &str,
        name: &str,
        files: &[(&str, u32, &[u8])],
    ) -> String {
        let tip = self.commit(&[], files);
        let r = format!("refs/minds/bindings/{commit}/{name}");
        self.view.set_ref(&r, &tip);
        r
    }

    pub fn blob_for(&self, commit: &str) -> Option<SealBlob> {
        let list: Vec<Candidate> = self
            .view
            .refs_with_prefix(&candidates::prefix(commit))
            .into_iter()
            .map(|(ref_name, target)| Candidate { ref_name, target })
            .collect();
        (!list.is_empty()).then(|| candidates::to_blob(commit, list))
    }

    pub fn trust(&self) -> TrustConfig {
        TrustConfig::from_text(
            &self.trust_text,
            &ssh_keygen(),
            &[PRINCIPAL.to_owned()],
            true,
            timeout(),
        )
        .unwrap()
    }

    /// Prüft `commit` mit frischem Budget und Inspektor; liefert Ergebnis
    /// und Diagnosen.
    pub fn inspect(
        &self,
        commit: &str,
    ) -> (
        Result<minds_witness_core::SealSummary, minds_witness_core::SealError>,
        Vec<minds_witness_git::inspector::Diagnostic>,
    ) {
        let trust = self.trust();
        let budget = Budget::unlimited();
        let verifier = Verifier::new(&trust, &budget);
        let inspector = GitSealInspector::new(
            &self.view,
            &verifier,
            &budget,
            InspectorConfig::new(PROJECT),
        );
        let blob = self.blob_for(commit).expect("Kandidaten vorhanden");
        let result = minds_witness_core::SealInspector::inspect(
            &inspector,
            &blob,
            &SealExpectation {
                repo: PROJECT.to_owned(),
                commit: CommitId::parse(commit).unwrap(),
            },
        );
        (result, inspector.diagnostics())
    }

    pub fn touched(&self, commit: &str) -> BTreeSet<String> {
        self.inspect(commit).0.unwrap().touched_paths
    }
}

/// `ObjectAccess` über einer `MemoryView`, für Vertragstests mit dem Kern.
pub struct ViewAccess<'a>(pub &'a World);

impl ViewAccess<'_> {
    fn reach(&self, from: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![from.to_owned()];
        while let Some(c) = stack.pop() {
            if seen.insert(c.clone())
                && let Ok(ps) = self.0.view.parents(&c)
            {
                stack.extend(ps);
            }
        }
        seen
    }
}

impl ObjectAccess for ViewAccess<'_> {
    fn commit(&self, id: &CommitId) -> Result<CommitInfo, ObjectError> {
        let parents = self
            .0
            .view
            .parents(id.as_str())
            .map_err(|_| ObjectError::NotFound(id.as_str().to_owned()))?;
        Ok(CommitInfo {
            id: id.clone(),
            parents: parents
                .iter()
                .map(|p| CommitId::parse(p).unwrap())
                .collect(),
        })
    }
    fn new_commits(
        &self,
        old: Option<&CommitId>,
        new: &CommitId,
        limit: u32,
    ) -> Result<CommitRange, ObjectError> {
        let mut set = self.reach(new.as_str());
        if let Some(old) = old {
            for c in self.reach(old.as_str()) {
                set.remove(&c);
            }
        }
        // Kahn, kleinste Id zuerst.
        let mut order = Vec::new();
        let mut left: BTreeMap<String, usize> = set
            .iter()
            .map(|c| {
                let n = self
                    .0
                    .view
                    .parents(c)
                    .unwrap()
                    .iter()
                    .filter(|p| set.contains(*p))
                    .count();
                (c.clone(), n)
            })
            .collect();
        while let Some(next) = left.iter().find(|(_, n)| **n == 0).map(|(c, _)| c.clone()) {
            left.remove(&next);
            for (c, n) in left.iter_mut() {
                if self.0.view.parents(c).unwrap().contains(&next) {
                    *n -= 1;
                }
            }
            order.push(CommitId::parse(&next).unwrap());
        }
        let truncated = order.len() > limit as usize;
        order.truncate(limit as usize);
        Ok(CommitRange {
            commits: order,
            truncated,
        })
    }
    fn changed_paths(&self, id: &CommitId) -> Result<BTreeSet<String>, ObjectError> {
        self.0
            .view
            .changed_paths(id.as_str())
            .map_err(|_| ObjectError::Internal("view".to_owned()))
    }
    fn seal_for_commit(&self, id: &CommitId) -> Result<Option<SealBlob>, ObjectError> {
        Ok(self.0.blob_for(id.as_str()))
    }
}

impl World {
    /// Prüft mehrere Commits nacheinander mit **einem** Inspektor (gemeinsame
    /// Memos wie in einem Hook-Aufruf).
    pub fn inspect_many(&self, commits: &[&str]) -> Vec<minds_witness_core::SealSummary> {
        let trust = self.trust();
        let budget = Budget::unlimited();
        let verifier = Verifier::new(&trust, &budget);
        let inspector = GitSealInspector::new(
            &self.view,
            &verifier,
            &budget,
            InspectorConfig::new(PROJECT),
        );
        commits
            .iter()
            .map(|c| {
                let blob = self.blob_for(c).expect("Kandidaten vorhanden");
                minds_witness_core::SealInspector::inspect(
                    &inspector,
                    &blob,
                    &SealExpectation {
                        repo: PROJECT.to_owned(),
                        commit: CommitId::parse(c).unwrap(),
                    },
                )
                .unwrap()
            })
            .collect()
    }
}
