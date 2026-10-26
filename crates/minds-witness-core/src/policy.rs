//! Policy des Server-Witness als reiner Datentyp mit Default, Validierung und
//! Muster-Matching. Das Modul entscheidet nichts über einen Push; es legt nur
//! fest, welche Refs bewertet und welche erzwungen werden und ob eine Policy
//! in sich stimmig ist. Das Parsen aus TOML gehört zum Adapter.

use serde::{Deserialize, Serialize};

/// Betriebsmodus des Witness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Beobachten: jede Entscheidung außer dem Namensraum-Schutz ist `Accept`.
    Observe,
    /// Erzwingen: erzwungene Refs werden nur bei vollständiger, bestandener
    /// Prüfung angenommen.
    Enforce,
}

/// Wie der Pusher im Befund erscheint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PusherMode {
    /// Klartext.
    Plain,
    /// Gesalzener BLAKE3-Hash von `id` und `username`.
    Hashed,
}

/// Konfiguration einer Prüfung.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Betriebsmodus.
    pub mode: Mode,
    /// Muster für bewertete Refs.
    pub evaluate_refs: Vec<String>,
    /// Muster für erzwungene Refs; jedes muss von `evaluate_refs` abgedeckt sein.
    pub enforce_refs: Vec<String>,
    /// Obergrenze neuer Commits je Ref, die vollständig bewertet werden.
    pub max_commits_per_push: u32,
    /// Maximale Länge von `Coverage::uncovered`.
    pub uncovered_cap: u32,
    /// Untergrenze der Abdeckung in Prozent für `PolicyVerdict::Pass`.
    pub coverage_threshold_percent: u8,
    /// Darstellung des Pushers im Befund.
    pub pusher_mode: PusherMode,
    /// Salz für `PusherMode::Hashed`.
    pub pusher_salt: String,
    /// Version der Policy, unverändert in `PolicyResult::version`.
    pub version: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: Mode::Observe,
            evaluate_refs: vec!["refs/heads/*".to_owned()],
            enforce_refs: Vec::new(),
            max_commits_per_push: 200,
            uncovered_cap: 100,
            coverage_threshold_percent: 100,
            pusher_mode: PusherMode::Plain,
            pusher_salt: String::new(),
            version: 1,
        }
    }
}

/// Fehler einer ungültigen Policy.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    /// Ein Muster verletzt die Musterregeln.
    #[error("Muster ungültig: {0}")]
    BadPattern(String),
    /// `max_commits_per_push` ist 0.
    #[error("max_commits_per_push muss ≥ 1 sein")]
    MaxCommitsZero,
    /// `coverage_threshold_percent` ist größer als 100.
    #[error("coverage_threshold_percent muss ≤ 100 sein")]
    ThresholdOver100,
    /// `PusherMode::Hashed` ohne Salz.
    #[error("pusher_salt darf bei PusherMode::Hashed nicht leer sein")]
    SaltMissing,
    /// Ein Enforce-Muster ist von keinem Evaluate-Muster abgedeckt.
    #[error("enforce_refs-Muster {0} wird von keinem evaluate_refs-Muster abgedeckt")]
    EnforceNotEvaluated(String),
}

fn pattern_is_valid(p: &str) -> bool {
    if p.is_empty() || !p.starts_with("refs/") {
        return false;
    }
    match p.matches('*').count() {
        0 => true,
        1 => p.ends_with("/*"),
        _ => false,
    }
}

impl Policy {
    /// Prüft die Policy in der Reihenfolge aus Abschnitt 6 und gibt den
    /// ersten Fehler zurück.
    pub fn validate(&self) -> Result<(), PolicyError> {
        for p in self.evaluate_refs.iter().chain(self.enforce_refs.iter()) {
            if !pattern_is_valid(p) {
                return Err(PolicyError::BadPattern(p.clone()));
            }
        }
        if self.max_commits_per_push < 1 {
            return Err(PolicyError::MaxCommitsZero);
        }
        if self.coverage_threshold_percent > 100 {
            return Err(PolicyError::ThresholdOver100);
        }
        if self.pusher_mode == PusherMode::Hashed && self.pusher_salt.is_empty() {
            return Err(PolicyError::SaltMissing);
        }
        for e in &self.enforce_refs {
            if !self.evaluate_refs.iter().any(|v| covers(v, e)) {
                return Err(PolicyError::EnforceNotEvaluated(e.clone()));
            }
        }
        Ok(())
    }

    pub(crate) fn is_evaluated(&self, ref_name: &str) -> bool {
        self.evaluate_refs
            .iter()
            .any(|p| matches_pattern(p, ref_name))
    }

    pub(crate) fn is_enforced(&self, ref_name: &str) -> bool {
        self.enforce_refs
            .iter()
            .any(|p| matches_pattern(p, ref_name))
    }
}

/// `true`, wenn `pattern` den `ref_name` trifft: Gleichheit, oder `pattern`
/// endet auf `/*`, `ref_name` beginnt mit dem Präfix vor dem `*` und hat
/// danach mindestens ein Zeichen.
pub fn matches_pattern(pattern: &str, ref_name: &str) -> bool {
    if pattern == ref_name {
        return true;
    }
    match pattern.strip_suffix('*') {
        Some(prefix) if pattern.ends_with("/*") => {
            ref_name.starts_with(prefix) && ref_name.len() > prefix.len()
        }
        _ => false,
    }
}

/// `true`, wenn jede Ref-Bezeichnung, die `enforce` trifft, auch `evaluate`
/// trifft. Bewusst konservativ: bejaht nur, was aus den Mustern selbst folgt.
pub fn covers(evaluate: &str, enforce: &str) -> bool {
    if evaluate == enforce {
        return true;
    }
    match evaluate.strip_suffix('*') {
        Some(prefix) if evaluate.ends_with("/*") => {
            enforce.starts_with(prefix) && enforce.len() > prefix.len()
        }
        _ => false,
    }
}
