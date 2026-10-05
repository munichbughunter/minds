//! Die fail-closed-Garantie für Observation-Objekte des Datei-Beobachters
//! (EA-08) — dieselben drei Bauformen wie bei Sessions ([`crate::session`]):
//!
//! 1. Das Objekt wird **verbraucht**; im Fehlerfall gibt es keins.
//! 2. Der Nachweis ist ein Typ: [`RedactedObservations`] hat keinen
//!    öffentlichen Konstruktor, `minds-store` nimmt nur ihn entgegen.
//! 3. Exhaustives Destructuring: Ein neues Feld bricht den Build hier.
//!
//! Gescannt wird jeder Text: Pfade (ein Dateiname kann ein Token oder einen
//! Kundennamen tragen) und Zeitstempel (Doktrin aus #35 — die Ausnahmeliste
//! bleibt leer). Ungescannt bleibt nur `content`: Der Typ
//! [`ContentHash`](minds_core::ContentHash) erzwingt 64 Hex-Zeichen.
//!
//! Die Schreib-Regeln ([`Observations::check`]) gelten **vor und nach** der
//! Bereinigung: Ein Platzhalter darf aus einem Pfad keinen absoluten oder
//! leeren machen.

use minds_core::observation::{Observation, Observations};

use crate::pipeline::RedactionPipeline;
use crate::session::{Field, RedactionAudit, RedactionError};

/// Ein Observation-Objekt, das die Redaction **nachweislich** durchlaufen
/// hat. Einziger Weg dorthin: [`RedactionPipeline::redact_observations`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedObservations {
    observations: Observations,
    audit: RedactionAudit,
}

impl RedactedObservations {
    /// Das bereinigte Objekt.
    pub fn observations(&self) -> &Observations {
        &self.observations
    }

    /// Der Nachweis des Laufs (Zähler und Ortsangaben, keine Werte).
    pub fn audit(&self) -> &RedactionAudit {
        &self.audit
    }
}

impl RedactionPipeline {
    /// Bereinigt jedes Textfeld eines Observation-Objekts. Jeder Fehler heißt:
    /// **es gibt kein Objekt**.
    pub fn redact_observations(
        &self,
        observations: Observations,
    ) -> Result<RedactedObservations, RedactionError> {
        if self.is_empty() {
            return Err(RedactionError::NoDetectors);
        }
        observations.check()?;
        let mut audit = RedactionAudit::default();
        let Observations {
            schema,
            first_at,
            last_at,
            observations,
        } = observations;
        let first_at = first_at
            .map(|at| self.redact_field(Field::ObservationsFirstAt, at, &mut audit))
            .transpose()?;
        let last_at = last_at
            .map(|at| self.redact_field(Field::ObservationsLastAt, at, &mut audit))
            .transpose()?;
        let mut redacted = Vec::with_capacity(observations.len());
        for (index, observation) in observations.into_iter().enumerate() {
            let Observation {
                seq,
                at,
                path,
                content,
                reason,
            } = observation;
            redacted.push(Observation {
                seq,
                at: self.redact_field(Field::ObservationAt(index), at, &mut audit)?,
                path: self.redact_field(Field::ObservationPath(index), path, &mut audit)?,
                content,
                reason,
            });
        }
        let observations = Observations {
            schema,
            first_at,
            last_at,
            observations: redacted,
        };
        observations.check()?;
        Ok(RedactedObservations {
            observations,
            audit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RedactionConfig;
    use minds_core::ContentHash;
    use minds_core::observation::{ObservationError, ObservationReason};

    fn object(paths: &[&str]) -> Observations {
        Observations::new(
            paths
                .iter()
                .enumerate()
                .map(|(i, path)| Observation {
                    seq: i as u64,
                    at: "2026-10-05T10:00:00Z".into(),
                    path: (*path).into(),
                    content: Some(ContentHash::from_bytes([i as u8; 32])),
                    reason: None,
                })
                .collect(),
        )
    }

    fn pipeline() -> RedactionPipeline {
        RedactionConfig::default().pipeline().unwrap()
    }

    /// Korpus wie bei Sessions: Was in einem Pfad steckt, muss raus; was ein
    /// gewöhnlicher Pfad ist, muss bleiben (sonst erklärt die Reconciliation
    /// nichts mehr).
    const MUST_REDACT: &[&str] = &[
        "exports/glpat-AbCdEfGhIjKlMnOpQrSt.json",
        "notes/alice@example.com.md",
        "dumps/ghp_0123456789abcdefghijABCDEFGHIJ012345.txt",
    ];
    const MUST_SURVIVE: &[&str] = &[
        "src/sort/merge.rs",
        "crates/minds-cli/src/witness_cmd/daemon.rs",
        "Cargo.lock",
        "docs/adr/0012-witnessed-evidence.md",
        "target-dir/x.y",
    ];

    #[test]
    fn paths_are_redacted_fail_closed_and_ordinary_paths_survive() {
        let pipeline = pipeline();
        for path in MUST_REDACT {
            let redacted = pipeline.redact_observations(object(&[path])).unwrap();
            let stored = &redacted.observations().observations[0].path;
            assert_ne!(stored, path, "{path}");
            assert!(!redacted.audit().is_clean());
        }
        for path in MUST_SURVIVE {
            let redacted = pipeline.redact_observations(object(&[path])).unwrap();
            assert_eq!(&redacted.observations().observations[0].path, path);
            assert!(redacted.audit().is_clean(), "{path}");
        }
    }

    #[test]
    fn hashes_and_reasons_stay_and_the_window_is_kept() {
        let mut input = object(&["a.rs", ".env"]);
        input.observations[1].content = None;
        input.observations[1].reason = Some(ObservationReason::SecretFile);
        let redacted = pipeline().redact_observations(input.clone()).unwrap();
        assert_eq!(redacted.observations(), &input);
    }

    #[test]
    fn malformed_objects_are_refused_not_repaired() {
        let empty = RedactionPipeline::new();
        assert_eq!(
            empty.redact_observations(object(&["a"])),
            Err(RedactionError::NoDetectors)
        );
        assert_eq!(
            pipeline().redact_observations(object(&["/home/dev/a.rs"])),
            Err(RedactionError::Observations(ObservationError::Path(0)))
        );
    }
}
