//! `GitSealInspector` (Spec S2 §8): prüft die Kandidatenmenge der
//! Bindungen eines Commits kryptografisch, liest die Seals akzeptierter
//! Bindungen und ihre Observations und leitet die abgedeckten Pfade
//! **inhaltsgebunden** ab (ADR-S2-003). Ungültige Kandidaten werden
//! verworfen, abgedeckt ist die Vereinigung über alle akzeptierten
//! Bindungen (ADR-S2-004). Das Modul schreibt keine Log-Ereignisse; es
//! sammelt Verwerfungsgründe in einem Diagnose-Puffer, nie mit
//! Objektinhalten oder Signaturen.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use minds_core::evidence::{SCOPE_WITNESS_FS_V1, Seal, SealOutcome};
use minds_core::observation::{MAX_OBSERVED_BYTES, ObservationReason, Observations};
use minds_witness_core::{
    CommitId, HashStr, SealBlob, SealError, SealExpectation, SealInspector, SealSignature,
    SealSummary,
};
use serde::Serialize;

use crate::binding::{CommitBinding, DEFAULT_MAX_SEALS};
use crate::budget::Budget;
use crate::candidates::{self, Candidate};
use crate::trust::{MAX_SIGNATURE_BYTES, SigVerdict, Verifier, VerifyError};
use crate::view::{EvidenceView, MODE_BLOB, MODE_EXEC, ViewError};

/// Höchstgröße eines Seal-Textes.
pub const MAX_SEAL_BYTES: u64 = 64 * 1024;
/// Präfix der Seal-Refs.
pub const EVIDENCE_PREFIX: &str = "refs/minds/evidence/";
/// Präfix der Observation-Refs.
pub const OBSERVATIONS_PREFIX: &str = "refs/minds/observations/";

/// Konfiguration des Inspektors (Spec S2 §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorConfig {
    /// Projekt-Identität dieses Repositorys (Spec S2 §4.2).
    pub project: String,
    /// Höchstgröße einer Bindung.
    pub max_binding_bytes: u64,
    /// Höchstzahl der Seals je Bindung.
    pub max_seals_per_binding: usize,
    /// Höchstgröße eines Observation-Objekts.
    pub max_observations_bytes: u64,
}

impl InspectorConfig {
    /// Die Voreinstellungen aus Spec S2 §9 für `project`.
    pub fn new(project: &str) -> Self {
        Self {
            project: project.to_owned(),
            max_binding_bytes: 64 * 1024,
            max_seals_per_binding: DEFAULT_MAX_SEALS,
            max_observations_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Warum ein Kandidat verworfen wurde (Spec S2 §8.2 Schritt 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Unlesbar (Baumform, Größe, Format, Ref-Id).
    Malformed,
    /// Ohne Signatur.
    Unsigned,
    /// Signierer nicht anerkannt.
    Untrusted,
    /// Signatur ungültig oder Schlüssel passt nicht.
    BadSignature,
    /// Gültig signiert und anerkannt, aber für anderen Commit oder anderes
    /// Projekt.
    Mismatch,
}

impl Reason {
    /// Das feste Wort für die Diagnose.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Unsigned => "unsigned",
            Self::Untrusted => "untrusted",
            Self::BadSignature => "bad_signature",
            Self::Mismatch => "mismatch",
        }
    }
}

/// Ein Eintrag des Diagnose-Puffers: nur Ref-Namen, Ids, feste Gründe und
/// Principals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Der betroffene Ref.
    pub ref_name: String,
    /// Id des Objekts, falls lesbar (`b3-…`).
    pub id: Option<String>,
    /// Fester Grund (`malformed`, `seal_discarded`, …).
    pub reason: &'static str,
    /// Principal, falls eine Signatur anerkannt war.
    pub principal: Option<String>,
}

/// Ein belegter Zustand eines Pfads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum State {
    Content(String),
    Deleted,
}

/// Belegte Zustände eines Observation-Objekts, `None` wenn verworfen.
type Observed = Option<Vec<(String, State)>>;

/// Ergebnis eines geprüften Seals: Scope und Observation-Id.
#[derive(Debug, Clone)]
struct SealOk {
    scope: String,
    observations: Option<String>,
}

/// Ergebnis eines Kandidaten.
struct Checked {
    binding: Option<CommitBinding>,
    reason: Option<Reason>,
    principal: Option<String>,
}

fn internal(e: ViewError) -> SealError {
    SealError::Internal(e.to_string())
}

fn verify_error(e: VerifyError) -> SealError {
    SealError::Internal(e.to_string())
}

/// Der Inspektor über einer [`EvidenceView`].
pub struct GitSealInspector<'a, V: EvidenceView + ?Sized> {
    view: &'a V,
    verifier: &'a Verifier<'a>,
    budget: &'a Budget,
    config: InspectorConfig,
    diagnostics: RefCell<Vec<Diagnostic>>,
    /// Memo je (Seal-Id, Ref-Ziel): Die Id-Prüfung gehört zum Ergebnis.
    seals: RefCell<HashMap<(String, String), Option<SealOk>>>,
    /// Memo je (Observation-Id, Ref-Ziel).
    observations: RefCell<HashMap<(String, String), Observed>>,
    /// Blob-Oids, deren Bytes schon im Budget gezählt sind.
    counted: RefCell<std::collections::HashSet<String>>,
}

impl<'a, V: EvidenceView + ?Sized> GitSealInspector<'a, V> {
    /// Ein Inspektor über `view`, der Signaturen mit `verifier` prüft und
    /// gelesene Evidence-Bytes aus `budget` zählt.
    pub fn new(
        view: &'a V,
        verifier: &'a Verifier<'a>,
        budget: &'a Budget,
        config: InspectorConfig,
    ) -> Self {
        Self {
            view,
            verifier,
            budget,
            config,
            diagnostics: RefCell::new(Vec::new()),
            seals: RefCell::new(HashMap::new()),
            observations: RefCell::new(HashMap::new()),
            counted: RefCell::new(std::collections::HashSet::new()),
        }
    }

    /// Die gesammelten Diagnosen in Entstehungsreihenfolge.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics.borrow().clone()
    }

    fn note(
        &self,
        ref_name: &str,
        id: Option<String>,
        reason: &'static str,
        principal: Option<String>,
    ) {
        self.diagnostics.borrow_mut().push(Diagnostic {
            ref_name: ref_name.to_owned(),
            id,
            reason,
            principal,
        });
    }

    /// Liest einen Blob höchstens `max` groß und zählt ihn im Budget;
    /// `Ok(None)` heißt zu groß.
    fn read(&self, oid: &str, max: u64) -> Result<Option<Vec<u8>>, SealError> {
        let Some(bytes) = self.view.blob(oid, max).map_err(internal)? else {
            return Ok(None);
        };
        if self.counted.borrow_mut().insert(oid.to_owned()) {
            self.budget
                .take_bytes(bytes.len() as u64)
                .map_err(|e| SealError::Internal(e.to_string()))?;
        }
        Ok(Some(bytes))
    }

    /// Spec S2 §8.2 Schritte 3 bis 5 für einen Kandidaten.
    fn check_candidate(&self, c: &str, cand: &Candidate) -> Result<Checked, SealError> {
        let malformed = || Checked {
            binding: None,
            reason: Some(Reason::Malformed),
            principal: None,
        };
        let tree = self.view.tree(&cand.target).map_err(internal)?;
        let mut binding_entry = None;
        let mut sig_entry = None;
        for e in &tree {
            match (e.name.as_str(), e.mode) {
                ("binding", MODE_BLOB) if e.size <= self.config.max_binding_bytes => {
                    binding_entry = Some(e)
                }
                ("binding.sig", MODE_BLOB) if e.size <= MAX_SIGNATURE_BYTES as u64 => {
                    sig_entry = Some(e)
                }
                _ => return Ok(malformed()),
            }
        }
        let Some(binding_entry) = binding_entry else {
            return Ok(malformed());
        };
        let Some(bytes) = self.read(&binding_entry.oid, self.config.max_binding_bytes)? else {
            return Ok(malformed());
        };
        let Ok(binding) = CommitBinding::parse(&bytes, self.config.max_seals_per_binding) else {
            return Ok(malformed());
        };
        if cand.ref_name != format!("{}{}", candidates::prefix(c), &binding.id()[3..]) {
            return Ok(malformed());
        }
        let mismatch = binding.commit() != c || binding.project() != self.config.project;
        let Some(sig_entry) = sig_entry else {
            return Ok(Checked {
                binding: Some(binding),
                reason: Some(Reason::Unsigned),
                principal: None,
            });
        };
        let Some(sig) = self.read(&sig_entry.oid, MAX_SIGNATURE_BYTES as u64)? else {
            return Ok(malformed());
        };
        let (reason, principal) = match self
            .verifier
            .verify(&bytes, &sig, Some(binding.key()))
            .map_err(verify_error)?
        {
            SigVerdict::Untrusted => (Some(Reason::Untrusted), None),
            SigVerdict::BadSignature => (Some(Reason::BadSignature), None),
            SigVerdict::Accepted { principal } if mismatch => {
                (Some(Reason::Mismatch), Some(principal))
            }
            SigVerdict::Accepted { principal } => (None, Some(principal)),
        };
        Ok(Checked {
            binding: Some(binding),
            reason,
            principal,
        })
    }

    /// Spec S2 §8.2 Schritt 6: ein Seal einer akzeptierten Bindung.
    fn seal(&self, seal_id: &str) -> Result<Option<SealOk>, SealError> {
        let ref_name = format!("{EVIDENCE_PREFIX}{}", &seal_id[3..]);
        let Some(target) = self.view.resolve_ref(&ref_name).map_err(internal)? else {
            self.note(&ref_name, Some(seal_id.to_owned()), "seal_missing", None);
            return Ok(None);
        };
        let key = (seal_id.to_owned(), target);
        if let Some(memo) = self.seals.borrow().get(&key) {
            return Ok(memo.clone());
        }
        let result = self.read_seal(seal_id, &key.1)?;
        if result.is_none() {
            self.note(&ref_name, Some(seal_id.to_owned()), "seal_discarded", None);
        }
        self.seals.borrow_mut().insert(key, result.clone());
        Ok(result)
    }

    fn read_seal(&self, seal_id: &str, target: &str) -> Result<Option<SealOk>, SealError> {
        let tree = self.view.tree(target).map_err(internal)?;
        let (mut text_entry, mut sig_entry) = (None, None);
        for e in &tree {
            match (e.name.as_str(), e.mode) {
                ("seal", MODE_BLOB) if e.size <= MAX_SEAL_BYTES => text_entry = Some(e),
                ("seal.sig", MODE_BLOB) if e.size <= MAX_SIGNATURE_BYTES as u64 => {
                    sig_entry = Some(e)
                }
                _ => return Ok(None),
            }
        }
        let (Some(text_entry), Some(sig_entry)) = (text_entry, sig_entry) else {
            return Ok(None);
        };
        let Some(bytes) = self.read(&text_entry.oid, MAX_SEAL_BYTES)? else {
            return Ok(None);
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return Ok(None);
        };
        if Seal::id_of_text(text).as_str() != seal_id {
            return Ok(None);
        }
        let Ok(seal) = Seal::parse(text) else {
            return Ok(None);
        };
        if seal.to_text().ok().as_deref() != Some(text) {
            return Ok(None);
        }
        let Some(sig) = self.read(&sig_entry.oid, MAX_SIGNATURE_BYTES as u64)? else {
            return Ok(None);
        };
        match self
            .verifier
            .verify(&bytes, &sig, None)
            .map_err(verify_error)?
        {
            SigVerdict::Accepted { .. } => {}
            _ => return Ok(None),
        }
        let observations = match seal.outcome {
            SealOutcome::ObservationsStored { observations } => Some(observations),
            _ => None,
        };
        Ok(Some(SealOk {
            scope: seal.scope,
            observations,
        }))
    }

    /// Spec S2 §8.2 Schritt 7: belegte Zustände eines Observation-Objekts.
    fn observed(&self, id: &str) -> Result<Observed, SealError> {
        let Some(hex) = id.strip_prefix("b3-") else {
            return Ok(None);
        };
        let ref_name = format!("{OBSERVATIONS_PREFIX}{hex}");
        let Some(target) = self.view.resolve_ref(&ref_name).map_err(internal)? else {
            self.note(&ref_name, Some(id.to_owned()), "observations_missing", None);
            return Ok(None);
        };
        let key = (id.to_owned(), target);
        if let Some(memo) = self.observations.borrow().get(&key) {
            return Ok(memo.clone());
        }
        let result = self.read_observations(hex, &key.1)?;
        if result.is_none() {
            self.note(
                &ref_name,
                Some(id.to_owned()),
                "observations_discarded",
                None,
            );
        }
        self.observations.borrow_mut().insert(key, result.clone());
        Ok(result)
    }

    fn read_observations(&self, hex: &str, target: &str) -> Result<Observed, SealError> {
        let tree = self.view.tree(target).map_err(internal)?;
        let [entry] = tree.as_slice() else {
            return Ok(None);
        };
        if entry.name != "observations.json"
            || entry.mode != MODE_BLOB
            || entry.size > self.config.max_observations_bytes
        {
            return Ok(None);
        }
        let Some(bytes) = self.read(&entry.oid, self.config.max_observations_bytes)? else {
            return Ok(None);
        };
        if hex::encode(blake3::hash(&bytes).as_bytes()) != hex {
            return Ok(None);
        }
        let Ok(obs) = serde_json::from_slice::<Observations>(&bytes) else {
            return Ok(None);
        };
        if obs.check().is_err() || obs.canonical_bytes().ok().as_deref() != Some(bytes.as_slice()) {
            return Ok(None);
        }
        let separated = obs
            .witness
            .as_ref()
            .is_some_and(|w| w.profile == "user" && w.isolation.proven());
        if !separated {
            return Ok(None);
        }
        Ok(Some(
            obs.observations
                .iter()
                .filter_map(|o| match (&o.content, o.reason) {
                    (Some(h), None) => {
                        Some((o.path.clone(), State::Content(h.as_str().to_owned())))
                    }
                    (None, Some(ObservationReason::Deleted)) => {
                        Some((o.path.clone(), State::Deleted))
                    }
                    _ => None,
                })
                .collect(),
        ))
    }

    /// ADR-S2-003: die abgedeckten Pfade von `c` aus den belegten Zuständen.
    fn covered(
        &self,
        c: &str,
        states: &BTreeMap<String, BTreeSet<State>>,
    ) -> Result<BTreeSet<String>, SealError> {
        let changed = self.view.changed_paths(c).map_err(internal)?;
        let mut out = BTreeSet::new();
        for path in changed.iter().filter(|p| states.contains_key(*p)) {
            // Spec S2 §9: Die Deadline ist eine harte Obergrenze, auch hier.
            self.budget
                .check_deadline()
                .map_err(|e| SealError::Internal(e.to_string()))?;
            // S1 v1.5 V6: Nicht-UTF-8-Pfade sind nie abgedeckt.
            if path.starts_with('\0') {
                continue;
            }
            let seen = &states[path];
            let covered = match self.view.entry(c, path).map_err(internal)? {
                None => seen.contains(&State::Deleted),
                Some(e) if e.mode == MODE_BLOB || e.mode == MODE_EXEC => {
                    let mode_ok = e.mode == MODE_BLOB || self.exec_inherited(c, path)?;
                    let wanted: Vec<&String> = seen
                        .iter()
                        .filter_map(|s| match s {
                            State::Content(h) => Some(h),
                            State::Deleted => None,
                        })
                        .collect();
                    mode_ok
                        && !wanted.is_empty()
                        && match self
                            .view
                            .blob(&e.oid, MAX_OBSERVED_BYTES)
                            .map_err(internal)?
                        {
                            Some(bytes) => {
                                let h =
                                    format!("b3-{}", hex::encode(blake3::hash(&bytes).as_bytes()));
                                wanted.iter().any(|w| **w == h)
                            }
                            None => false,
                        }
                }
                // Symlink, Gitlink, Baum: nie abdeckbar.
                Some(_) => false,
            };
            if covered {
                out.insert(path.clone());
            }
        }
        Ok(out)
    }

    /// Modusregel für `100755` (ADR-S2-003 Nr. 2): Es gibt mindestens einen
    /// Elternteil mit `p`, und in jedem Elternteil, der `p` hat, ist es
    /// `100755`; ohne Eltern nie.
    fn exec_inherited(&self, c: &str, path: &str) -> Result<bool, SealError> {
        let parents = self.view.parents(c).map_err(internal)?;
        let mut found = false;
        for p in &parents {
            match self.view.entry(p, path).map_err(internal)? {
                Some(e) if e.mode == MODE_EXEC => found = true,
                Some(_) => return Ok(false),
                None => {}
            }
        }
        Ok(found)
    }

    fn summary(&self, blob: &SealBlob, c: &str) -> Result<SealSummary, SealError> {
        let cands = candidates::parse(&blob.bytes, c)
            .ok_or_else(|| SealError::Malformed("candidate list unreadable".to_owned()))?;
        let commit_id =
            |s: &str| CommitId::parse(s).map_err(|_| SealError::Internal("commit id".to_owned()));
        let mut accepted: Vec<CommitBinding> = Vec::new();
        let mut first_mismatch: Option<CommitBinding> = None;
        let (mut rejected, mut unsigned) = (false, false);
        let mut readable: Vec<String> = Vec::new();
        for cand in &cands {
            let checked = self.check_candidate(c, cand)?;
            let id = checked.binding.as_ref().map(CommitBinding::id);
            if let Some(id) = &id {
                readable.push(id.clone());
            }
            match checked.reason {
                None => {
                    if let Some(b) = checked.binding {
                        accepted.push(b);
                    }
                }
                Some(reason) => {
                    self.note(&cand.ref_name, id, reason.word(), checked.principal.clone());
                    match reason {
                        Reason::Mismatch if first_mismatch.is_none() => {
                            first_mismatch = checked.binding
                        }
                        Reason::Mismatch | Reason::Malformed => {}
                        Reason::Untrusted | Reason::BadSignature => rejected = true,
                        Reason::Unsigned => unsigned = true,
                    }
                }
            }
        }
        let project = Some(self.config.project.clone());
        if !accepted.is_empty() {
            let mut states: BTreeMap<String, BTreeSet<State>> = BTreeMap::new();
            let mut seal_ids: BTreeSet<&str> = BTreeSet::new();
            for b in &accepted {
                seal_ids.extend(b.seals().iter().map(String::as_str));
            }
            for seal_id in seal_ids {
                let Some(seal) = self.seal(seal_id)? else {
                    continue;
                };
                let (Some(obs_id), true) = (seal.observations, seal.scope == SCOPE_WITNESS_FS_V1)
                else {
                    continue;
                };
                for (path, state) in self.observed(&obs_id)?.unwrap_or_default() {
                    states.entry(path).or_default().insert(state);
                }
            }
            let ids = accepted.iter().map(CommitBinding::id).collect();
            return Ok(SealSummary {
                hash: binding_hash("accepted_bindings", c, ids),
                signature: SealSignature::Valid,
                bound_commit: commit_id(c)?,
                bound_repo: project,
                touched_paths: self.covered(c, &states)?,
            });
        }
        if let Some(b) = first_mismatch {
            return Ok(SealSummary {
                hash: binding_hash("mismatched_binding", c, vec![b.id()]),
                signature: SealSignature::Valid,
                bound_commit: commit_id(b.commit())?,
                bound_repo: Some(b.project().to_owned()),
                touched_paths: BTreeSet::new(),
            });
        }
        let signature = if rejected {
            SealSignature::Invalid
        } else if unsigned {
            SealSignature::Unsigned
        } else {
            return Err(SealError::Malformed("no readable candidate".to_owned()));
        };
        Ok(SealSummary {
            hash: binding_hash("rejected_bindings", c, readable),
            signature,
            bound_commit: commit_id(c)?,
            bound_repo: project,
            touched_paths: BTreeSet::new(),
        })
    }
}

impl<V: EvidenceView + ?Sized> SealInspector for GitSealInspector<'_, V> {
    fn inspect(
        &self,
        blob: &SealBlob,
        expected: &SealExpectation,
    ) -> Result<SealSummary, SealError> {
        self.budget
            .check_deadline()
            .map_err(|e| SealError::Internal(e.to_string()))?;
        if expected.repo != self.config.project {
            return Err(SealError::Internal(
                "project mismatch in configuration".to_owned(),
            ));
        }
        self.summary(blob, expected.commit.as_str())
    }
}

#[derive(Serialize)]
struct HashDoc {
    bindings: Vec<String>,
    commit: String,
    kind: String,
}

/// `H(kind, c, ids)` aus Spec S2 §8.4: `HashStr::of_bytes` über das
/// kanonische JSON `{"bindings":[ids aufsteigend],"commit":c,"kind":kind}`.
pub fn binding_hash(kind: &str, commit: &str, mut ids: Vec<String>) -> HashStr {
    ids.sort();
    ids.dedup();
    let doc = HashDoc {
        bindings: ids,
        commit: commit.to_owned(),
        kind: kind.to_owned(),
    };
    // Nur Strings: Die Serialisierung kann nicht scheitern.
    HashStr::of_bytes(
        &minds_core::to_canonical_json(&doc).expect("Hash-Dokument ist serialisierbar"),
    )
}
