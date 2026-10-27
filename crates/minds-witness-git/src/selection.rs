//! Geteilte, inhaltsgebundene Seal-Auswahl (Spec S3-MVP ADR-S3-005, §6.1;
//! ADR-S2-003). Eine Stelle für die Regel „ein Seal belegt einen
//! geänderten Pfad eines Commits genau dann, wenn eine seiner Observations
//! den Inhalt bzw. die Löschung im Commit belegt“:
//!
//! - Der Server-Inspektor ([`crate::inspector`]) leitet daraus die
//!   abgedeckten Pfade akzeptierter Bindungen ab ([`target`], [`covers`]).
//! - Der Witness wählt damit die Seals einer neuen Bindung
//!   ([`select_seals`]), aus genau denselben Bausteinen; seine Vorschau
//!   „n/m Dateien belegt“ ist die Abdeckung, die der Server berechnet.
//!
//! Die Lese-Regeln für Seal-Text und Observation-Objekt
//! ([`seal_observations`], [`observed_states`]) sind ebenfalls geteilt.
//! Signaturen prüft das Modul nicht: Das tut der Server
//! ([`crate::trust`]); der Witness wählt nur aus eigenen Seals. Die Auswahl
//! ist eine Optimierung, keine Sicherheitsgrenze (Spec S3 §12).

use std::collections::{BTreeMap, BTreeSet};

use minds_core::evidence::{SCOPE_WITNESS_FS_V1, Seal, SealOutcome};
use minds_core::observation::{MAX_OBSERVED_BYTES, ObservationReason, Observations};

use crate::view::{EvidenceView, MODE_BLOB, MODE_EXEC, ViewError};

/// Ein von einer Observation belegter Zustand eines Pfads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// Inhalt mit diesem Hash (`b3-<hex>`).
    Content(String),
    /// Gelöscht.
    Deleted,
}

/// Belegte Zustände je Pfad (Vereinigung über Seals).
pub type States = BTreeMap<String, BTreeSet<State>>;

/// Was der Commit an einem geänderten Pfad tatsächlich enthält, soweit es
/// für die Abdeckung zählt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Der Pfad fehlt im Commit (gelöscht).
    Deleted,
    /// Reguläre Datei mit diesem Inhalts-Hash (`b3-<hex>`), Modusregel
    /// erfüllt.
    Content(String),
    /// Nie abdeckbar: Symlink, Gitlink, Baum, nicht geerbtes `100755`,
    /// Nicht-UTF-8-Pfad, Inhalt über `MAX_OBSERVED_BYTES`, oder es wurde
    /// kein Inhalt verlangt.
    Never,
}

/// Liest einen Seal-Text nach den Regeln des Servers (Spec S2 §8.2
/// Schritt 6, ohne Signatur): Id passt, lesbar, kanonisch. Liefert Scope
/// und, bei `ObservationsStored`, die Observation-Id.
pub fn seal_observations(seal_id: &str, text: &str) -> Option<(String, Option<String>)> {
    if Seal::id_of_text(text).as_str() != seal_id {
        return None;
    }
    let seal = Seal::parse(text).ok()?;
    if seal.to_text().ok().as_deref() != Some(text) {
        return None;
    }
    let observations = match seal.outcome {
        SealOutcome::ObservationsStored { observations } => Some(observations),
        _ => None,
    };
    Some((seal.scope, observations))
}

/// Belegte Zustände eines Observation-Objekts nach den Regeln des Servers
/// (Spec S2 §8.2 Schritt 7): Hash der Bytes = `hex`, lesbar, geprüft,
/// kanonisch, von einem nachweislich getrennten Witness. Gründe außer
/// `deleted` belegen nichts. `None` heißt verworfen.
pub fn observed_states(hex: &str, bytes: &[u8]) -> Option<Vec<(String, State)>> {
    if hex::encode(blake3::hash(bytes).as_bytes()) != hex {
        return None;
    }
    let obs = serde_json::from_slice::<Observations>(bytes).ok()?;
    if obs.check().is_err() || obs.canonical_bytes().ok().as_deref() != Some(bytes) {
        return None;
    }
    let separated = obs
        .witness
        .as_ref()
        .is_some_and(|w| w.profile == "user" && w.isolation.proven());
    if !separated {
        return None;
    }
    Some(
        obs.observations
            .iter()
            .filter_map(|o| match (&o.content, o.reason) {
                (Some(h), None) => Some((o.path.clone(), State::Content(h.as_str().to_owned()))),
                (None, Some(ObservationReason::Deleted)) => Some((o.path.clone(), State::Deleted)),
                _ => None,
            })
            .collect(),
    )
}

/// Der Zielzustand von `path` in `c`. Mit `want_content = false` wird kein
/// Blob gelesen; eine Datei ist dann [`Target::Never`] (kein belegter
/// Inhalt könnte passen).
pub fn target<V: EvidenceView + ?Sized>(
    view: &V,
    c: &str,
    path: &str,
    want_content: bool,
) -> Result<Target, ViewError> {
    // S1 v1.5 V6: Nicht-UTF-8-Pfade sind nie abgedeckt.
    if path.starts_with('\0') {
        return Ok(Target::Never);
    }
    match view.entry(c, path)? {
        None => Ok(Target::Deleted),
        Some(e) if e.mode == MODE_BLOB || e.mode == MODE_EXEC => {
            let mode_ok = e.mode == MODE_BLOB || exec_inherited(view, c, path)?;
            if !mode_ok || !want_content {
                return Ok(Target::Never);
            }
            Ok(match view.blob(&e.oid, MAX_OBSERVED_BYTES)? {
                Some(bytes) => Target::Content(format!(
                    "b3-{}",
                    hex::encode(blake3::hash(&bytes).as_bytes())
                )),
                None => Target::Never,
            })
        }
        // Symlink, Gitlink, Baum: nie abdeckbar.
        Some(_) => Ok(Target::Never),
    }
}

/// Ob die belegten Zustände `seen` das Ziel abdecken.
pub fn covers(target: &Target, seen: &BTreeSet<State>) -> bool {
    match target {
        Target::Deleted => seen.contains(&State::Deleted),
        Target::Content(h) => seen.contains(&State::Content(h.clone())),
        Target::Never => false,
    }
}

/// Ob unter `seen` ein Inhalt belegt ist (dann muss [`target`] den Blob
/// lesen).
pub fn wants_content(seen: &BTreeSet<State>) -> bool {
    seen.iter().any(|s| matches!(s, State::Content(_)))
}

/// Modusregel für `100755` (ADR-S2-003 Nr. 2): Es gibt mindestens einen
/// Elternteil mit `path`, und in jedem Elternteil, der `path` hat, ist es
/// `100755`; ohne Eltern nie.
pub fn exec_inherited<V: EvidenceView + ?Sized>(
    view: &V,
    c: &str,
    path: &str,
) -> Result<bool, ViewError> {
    let mut found = false;
    for p in &view.parents(c)? {
        match view.entry(p, path)? {
            Some(e) if e.mode == MODE_EXEC => found = true,
            Some(_) => return Ok(false),
            None => {}
        }
    }
    Ok(found)
}

/// Ein Auswahl-Kandidat: ein eigener Seal mit seinen belegten Zuständen.
/// Nur über [`SealEvidence::from_parts`] konstruierbar, also nur mit den
/// Lese-Regeln des Servers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealEvidence {
    id: String,
    states: Vec<(String, State)>,
}

impl SealEvidence {
    /// Baut einen Kandidaten aus Seal-Text und Observation-Bytes mit genau
    /// den Lese-Regeln des Servers, einschließlich seiner Größengrenzen
    /// (`MAX_SEAL_BYTES`, `max_observations_bytes` der Server-Grenzen).
    /// `None`, wenn der Server diesen Seal nicht anrechnen würde (zu groß,
    /// falsche Id, nicht kanonisch, anderer Scope, keine gespeicherten
    /// Observations, Observation-Id passt nicht, Witness nicht getrennt).
    pub fn from_parts(
        seal_id: &str,
        seal_text: &str,
        observations: &[u8],
        max_observations_bytes: u64,
    ) -> Option<Self> {
        if seal_text.len() as u64 > crate::inspector::MAX_SEAL_BYTES
            || observations.len() as u64 > max_observations_bytes
        {
            return None;
        }
        let (scope, obs_id) = seal_observations(seal_id, seal_text)?;
        if scope != SCOPE_WITNESS_FS_V1 {
            return None;
        }
        let hex = obs_id?.strip_prefix("b3-")?.to_owned();
        let states = observed_states(&hex, observations)?;
        Some(Self {
            id: seal_id.to_owned(),
            states,
        })
    }

    /// Die Seal-Id `b3-<hex>`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Die belegten Zustände aus dem Observation-Objekt.
    pub fn states(&self) -> &[(String, State)] {
        &self.states
    }
}

/// Ergebnis von [`select_seals`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Gewählte Seal-Ids, aufsteigend (Form der Bindung, Spec S2 §4.1).
    pub seals: Vec<String>,
    /// Von den gewählten Seals belegte geänderte Pfade.
    pub covered: BTreeSet<String>,
    /// Zahl der geänderten Pfade des Commits (S1-Definition).
    pub total: usize,
}

/// Wählt aus `candidates` die Seals einer Bindung für `c` (Spec S3 §6.1
/// Schritt 3): gierig jeweils den Seal mit den meisten noch nicht belegten
/// Pfaden, bei Gleichstand die kleinste Id; Schluss, wenn keiner mehr etwas
/// beiträgt oder `max` erreicht ist. Deterministisch und unabhängig von der
/// Reihenfolge der Kandidaten; doppelte Ids werden zusammengeführt.
///
/// Der Aufrufer übergibt als `max` die Grenze der Bindung
/// (`max_seals_per_binding`) und legt bei leerer Auswahl keine Bindung an;
/// beides lehnt [`crate::binding::CommitBinding::new`] sonst ab.
///
/// Ohne Grenze belegt die Auswahl genau die Pfade, die alle Kandidaten
/// zusammen belegen. Die gemeldete Abdeckung ist die, die der Server für
/// eine Bindung mit genau diesen Seals berechnet (sofern er die Seals
/// anerkennt).
pub fn select_seals<V: EvidenceView + ?Sized>(
    view: &V,
    c: &str,
    candidates: &[SealEvidence],
    max: usize,
) -> Result<Selection, ViewError> {
    let changed = view.changed_paths(c)?;
    // Zustände je Seal, nur für geänderte Pfade.
    let mut by_seal: BTreeMap<&str, States> = BTreeMap::new();
    for cand in candidates {
        let entry = by_seal.entry(cand.id.as_str()).or_default();
        for (path, state) in &cand.states {
            if changed.contains(path) {
                entry.entry(path.clone()).or_default().insert(state.clone());
            }
        }
    }
    // Ziel je Pfad einmal bestimmen; Inhalt nur lesen, wenn ihn ein Seal
    // belegt.
    let mut union: States = BTreeMap::new();
    for states in by_seal.values() {
        for (path, seen) in states {
            union
                .entry(path.clone())
                .or_default()
                .extend(seen.iter().cloned());
        }
    }
    let mut targets: BTreeMap<&str, Target> = BTreeMap::new();
    for (path, seen) in &union {
        targets.insert(path, target(view, c, path, wants_content(seen))?);
    }
    // Belegte Pfade je Seal.
    let mut cover: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for (id, states) in &by_seal {
        let set: BTreeSet<String> = states
            .iter()
            .filter(|(path, seen)| covers(&targets[path.as_str()], seen))
            .map(|(path, _)| path.clone())
            .collect();
        if !set.is_empty() {
            cover.insert(id, set);
        }
    }
    let mut covered = BTreeSet::new();
    let mut seals = Vec::new();
    while seals.len() < max {
        // `cover` ist nach Id sortiert: `max_by_key` nähme bei Gleichstand
        // den letzten, daher strikt größer.
        let mut best: Option<(&str, usize)> = None;
        for (id, set) in &cover {
            let gain = set.difference(&covered).count();
            if gain > 0 && best.is_none_or(|(_, g)| gain > g) {
                best = Some((id, gain));
            }
        }
        let Some((id, _)) = best else { break };
        covered.extend(cover[id].iter().cloned());
        seals.push(id.to_owned());
        cover.remove(id);
    }
    seals.sort();
    Ok(Selection {
        seals,
        covered,
        total: changed.len(),
    })
}
