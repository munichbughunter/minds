//! Die fail-closed-Garantie für Observation-Objekte des Datei-Beobachters
//! (EA-08) — dieselben drei Bauformen wie bei Sessions ([`crate::session`]):
//!
//! 1. Das Objekt wird **verbraucht**; im Fehlerfall gibt es keins.
//! 2. Der Nachweis ist ein Typ: [`RedactedObservations`] hat keinen
//!    öffentlichen Konstruktor, `minds-store` nimmt nur ihn entgegen.
//! 3. Exhaustives Destructuring: Ein neues Feld bricht den Build hier.
//!
//! Gescannt wird jeder Text: Pfade (ein Dateiname kann ein Token oder einen
//! Kundennamen tragen), Zeitstempel (Doktrin aus #35 — die Ausnahmeliste
//! bleibt leer) und das Profilwort der Witness-Fakten. Ungescannt bleiben
//! nur Hashes (`content`, Soll-Id und Umschläge: Der Typ
//! [`ContentHash`](minds_core::ContentHash) erzwingt 64 Hex-Zeichen),
//! Kennungen und Wahrheitswerte.
//!
//! Die Schreib-Regeln ([`Observations::check`]) gelten **vor und nach** der
//! Bereinigung: Ein Platzhalter darf aus einem Pfad keinen absoluten oder
//! leeren machen.

use minds_core::observation::{
    Isolation, Observation, ObservationGap, Observations, SpecSeen, WitnessFacts,
};

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
            started_at,
            first_at,
            last_at,
            observations,
            witness,
            gaps,
        } = observations;
        let started_at = started_at
            .map(|at| self.redact_field(Field::ObservationsStartedAt, at, &mut audit))
            .transpose()?;
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
        let witness = witness
            .map(|facts| {
                let WitnessFacts {
                    profile,
                    isolation,
                    spec,
                } = facts;
                let Isolation {
                    witness_uid,
                    agent_uids,
                    home_private,
                    binary_protected,
                } = isolation;
                // Exhaustiv, ohne Text: Soll-Id und Umschläge sind Hashes. Ein
                // neues Feld muss hier entschieden werden.
                if let Some(SpecSeen {
                    id: _,
                    envelopes: _,
                }) = &spec
                {}
                Ok::<_, RedactionError>(WitnessFacts {
                    profile: self.redact_field(
                        Field::ObservationsWitnessProfile,
                        profile,
                        &mut audit,
                    )?,
                    isolation: Isolation {
                        witness_uid,
                        agent_uids,
                        home_private,
                        binary_protected,
                    },
                    spec,
                })
            })
            .transpose()?;
        // Lücken: der Zeitstempel ist Text und wird gescannt; Grund (ein
        // geschlossenes Enum), Nummer und Zähler tragen keinen Freitext.
        let mut redacted_gaps = Vec::with_capacity(gaps.len());
        for (index, gap) in gaps.into_iter().enumerate() {
            let ObservationGap {
                seq,
                at,
                reason,
                dropped,
            } = gap;
            redacted_gaps.push(ObservationGap {
                seq,
                at: self.redact_field(Field::ObservationGapAt(index), at, &mut audit)?,
                reason,
                dropped,
            });
        }
        let observations = Observations {
            schema,
            started_at,
            first_at,
            last_at,
            observations: redacted,
            witness,
            gaps: redacted_gaps,
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

    /// Die Fakten des Witness kommen unverändert durch; ihr Profilwort wird
    /// gescannt, Hashes und Kennungen bleiben.
    #[test]
    fn witness_facts_survive_redaction() {
        use minds_core::observation::{Isolation, SpecSeen, WitnessFacts};
        let facts = WitnessFacts {
            profile: "user".into(),
            isolation: Isolation {
                witness_uid: 498,
                agent_uids: vec![20, 501],
                home_private: true,
                binary_protected: false,
            },
            spec: Some(SpecSeen {
                id: ContentHash::from_bytes([7; 32]),
                envelopes: vec![ContentHash::from_bytes([1; 32])],
            }),
        };
        let pipeline = RedactionConfig::default().pipeline().unwrap();
        let redacted = pipeline
            .redact_observations(object(&["src/a.rs"]).with_witness(facts.clone()))
            .unwrap();
        assert_eq!(redacted.observations().witness, Some(facts));
    }

    /// Würde die Pipeline das Profilwort ersetzen, gibt es kein Objekt —
    /// nie ein still verändertes Profil.
    #[test]
    fn a_redacted_profile_word_refuses_the_object() {
        use minds_core::observation::{Isolation, WitnessFacts};
        let facts = WitnessFacts {
            profile: "user".into(),
            isolation: Isolation {
                witness_uid: 498,
                agent_uids: vec![501],
                home_private: true,
                binary_protected: true,
            },
            spec: None,
        };
        let config: RedactionConfig = serde_json::from_str(r#"{"deny_pii":["user"]}"#).unwrap();
        let pipeline = config.pipeline().unwrap();
        assert!(
            pipeline
                .redact_observations(object(&["src/a.rs"]).with_witness(facts))
                .is_err()
        );
    }

    fn object(paths: &[&str]) -> Observations {
        Observations::new(
            "2026-10-05T09:59:00Z",
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

    /// Die Lücken des Beobachters kommen durch; ihr Zeitstempel wird gescannt
    /// wie jeder andere, Grund und Zähler bleiben.
    #[test]
    fn observer_gaps_survive_redaction() {
        use minds_core::observation::GapReason;
        let object = object(&["src/a.rs"]).with_gaps(vec![ObservationGap {
            seq: 2,
            at: "2026-10-05T10:00:01Z".into(),
            reason: GapReason::Overflow,
            dropped: Some(7),
        }]);
        let redacted = pipeline().redact_observations(object.clone()).unwrap();
        assert_eq!(redacted.observations(), &object);
        assert!(redacted.audit().is_clean());
        // Ein Zeitstempel, der wie ein Secret aussieht, wird ersetzt — und
        // das Objekt bleibt schreibbar.
        let mut suspicious = object;
        suspicious.gaps[0].at = "ghp_0123456789abcdefghijABCDEFGHIJ012345".into();
        let redacted = pipeline().redact_observations(suspicious).unwrap();
        assert!(!redacted.audit().is_clean());
        assert!(!redacted.observations().gaps[0].at.contains("ghp_"));
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

    /// Jede Epoche trägt jetzt mindestens einen Zeitstempel: Trifft ein
    /// Detektor der Standard-Policy einen, würde jede Epoche zum Block-Seal.
    const TIMESTAMPS_MUST_SURVIVE: &[&str] = &[
        "2026-10-05T09:59:58.125Z",
        "1970-01-01T00:00:00.000Z",
        "2554-07-21T23:34:33.709Z",
        "2026-10-05T10:00:00Z",
    ];

    #[test]
    fn witness_timestamps_survive_the_default_policy() {
        let pipeline = pipeline();
        for at in TIMESTAMPS_MUST_SURVIVE {
            let mut input = object(&["a.rs"]);
            input.started_at = Some((*at).into());
            input.observations[0].at = (*at).into();
            input.first_at = Some((*at).into());
            input.last_at = Some((*at).into());
            let redacted = pipeline.redact_observations(input.clone()).unwrap();
            assert_eq!(redacted.observations(), &input, "{at}");
            assert!(redacted.audit().is_clean(), "{at}");
        }
    }

    #[test]
    fn the_epoch_start_is_scanned_like_every_text_field() {
        let pipeline = pipeline();
        let clean = pipeline.redact_observations(object(&["a.rs"])).unwrap();
        assert_eq!(
            clean.observations().started_at.as_deref(),
            Some("2026-10-05T09:59:00Z")
        );
        assert!(clean.audit().is_clean());
        // Was kein lesbarer Zeitpunkt ist (etwa mit einem Token darin), wird
        // schon vor der Bereinigung abgelehnt — es gibt kein Objekt. Ein
        // Platzhalter danach machte ihn ebenso unlesbar (Prüfung danach).
        let mut input = object(&["a.rs"]);
        input.started_at = Some("2026-10-05T09:59:00Z glpat-AbCdEfGhIjKlMnOpQrSt".into());
        assert_eq!(
            pipeline.redact_observations(input),
            Err(RedactionError::Observations(ObservationError::Start))
        );
        // Fehlt er, gibt es ebenfalls keins.
        let mut input = object(&["a.rs"]);
        input.started_at = None;
        assert_eq!(
            pipeline.redact_observations(input),
            Err(RedactionError::Observations(ObservationError::Start))
        );
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
