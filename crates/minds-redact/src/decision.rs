//! Der Entscheidungs-Record auf dem Weg in den Store: redigiert wie jede
//! Session, nur durch die Pipeline erreichbar (`check → redact → id →
//! store`, siehe `minds_core::decision`).
//!
//! # Zwei Sorten Text
//!
//! - **Freitext** — Thema, die Aussage einer Annahme, ein Ansatz — wird
//!   redigiert wie ein Prompt: Ein Secret darin wird durch den Platzhalter
//!   ersetzt, der Audit zählt es. Das ist der Text, den Menschen und Agenten
//!   später lesen.
//! - **Verweise** — Kennungen, Pfade, Globs, Blob-Ids, Zeitstempel und die
//!   **Muster** der Proben — werden nur **geprüft**: Findet die Policy dort
//!   etwas, gibt es keinen Record. Ein Muster mit Platzhalter prüfte etwas
//!   anderes als gemeint; ein Pfad mit Platzhalter wäre ein anderer Pfad.
//!   Fail-closed statt still verstümmelt — der Aufrufer sieht, welches Feld
//!   ([`RedactionError::DecisionFieldRedacted`]).
//!
//! # Der Nachweis ist ein Typ
//!
//! [`RedactedDecision`] hat keinen öffentlichen Konstruktor; der Store nimmt
//! nur ihn. Die Id entsteht **nach** der Redaction über genau die Bytes, die
//! abgelegt werden — eine Id über den Klartext wäre ein Orakel für das
//! Entfernte und aus dem Store nicht nachrechenbar.

use minds_core::ContentHash;
use minds_core::decision::{Alternative, Assumption, Choice, DecisionRecord, MAX_RECORD, Probe};

use crate::pipeline::RedactionPipeline;
use crate::session::{Field, RedactionAudit, RedactionError};

/// Ein Entscheidungs-Record, der die Redaction **nachweislich** durchlaufen
/// hat. Einziger Weg dorthin: [`RedactionPipeline::redact_decision`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedDecision {
    record: DecisionRecord,
    bytes: Vec<u8>,
    id: ContentHash,
    audit: RedactionAudit,
}

impl RedactedDecision {
    /// Der redigierte Record.
    pub fn record(&self) -> &DecisionRecord {
        &self.record
    }

    /// Die kanonischen Bytes, wie sie abgelegt werden.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// `blake3(bytes)`.
    pub fn id(&self) -> &ContentHash {
        &self.id
    }

    /// Was die Redaction im Freitext ersetzt hat — nur Zähler und Orte.
    pub fn audit(&self) -> &RedactionAudit {
        &self.audit
    }
}

impl RedactionPipeline {
    /// Prüft den Record ([`DecisionRecord::check`]), redigiert seinen
    /// Freitext, prüft seine Verweise und bildet die Id. Jeder Fehler heißt:
    /// **es gibt keinen Record.**
    pub fn redact_decision(
        &self,
        record: DecisionRecord,
    ) -> Result<RedactedDecision, RedactionError> {
        if self.is_empty() {
            return Err(RedactionError::NoDetectors);
        }
        record.check()?;
        let mut audit = RedactionAudit::default();

        // Exhaustiv: Ein neues Feld kompiliert erst, wenn entschieden ist,
        // ob es redigiert oder geprüft wird.
        let DecisionRecord {
            kind,
            schema,
            topic,
            decided_by,
            at,
            chosen,
            open,
            alternatives,
            assumptions,
            scope,
            supersedes,
        } = record;

        self.unchanged(Field::DecisionAt, &at, &mut audit)?;
        let topic = self.redact_field(Field::DecisionTopic, topic, &mut audit)?;

        let chosen = match chosen {
            Some(Choice { approach, rests_on }) => Some(Choice {
                approach: self.redact_field(Field::DecisionChosen, approach, &mut audit)?,
                rests_on,
            }),
            None => None,
        };

        let mut redacted_alternatives = Vec::with_capacity(alternatives.len());
        for (i, alternative) in alternatives.into_iter().enumerate() {
            let Alternative {
                approach,
                rejected_because,
                rests_on,
            } = alternative;
            redacted_alternatives.push(Alternative {
                approach: self.redact_field(Field::DecisionAlternative(i), approach, &mut audit)?,
                rejected_because,
                rests_on,
            });
        }

        let mut redacted_assumptions = Vec::with_capacity(assumptions.len());
        for (i, assumption) in assumptions.into_iter().enumerate() {
            let Assumption {
                id,
                statement,
                probe,
            } = assumption;
            self.unchanged(Field::DecisionAssumptionId(i), &id, &mut audit)?;
            if let Some(probe) = &probe {
                self.check_probe(i, probe, &mut audit)?;
            }
            redacted_assumptions.push(Assumption {
                id,
                statement: self.redact_field(
                    Field::DecisionAssumption(i),
                    statement,
                    &mut audit,
                )?,
                probe,
            });
        }

        for (i, glob) in scope.iter().enumerate() {
            self.unchanged(Field::DecisionScope(i), glob, &mut audit)?;
        }

        let record = DecisionRecord {
            kind,
            schema,
            topic,
            decided_by,
            at,
            chosen,
            open,
            alternatives: redacted_alternatives,
            assumptions: redacted_assumptions,
            scope,
            supersedes,
        };
        // Noch einmal: Ein Platzhalter kann einen Text nicht leeren — aber
        // verlängern. Was danach über `MAX_TEXT` liegt, bekommt seinen
        // eigenen Grund; alles andere wäre ein Fehler der Pipeline.
        record.check().map_err(|err| match err {
            minds_core::decision::DecisionError::Text(_) => RedactionError::DecisionRedactedTooLong,
            other => RedactionError::Decision(other),
        })?;
        let bytes = record
            .canonical_bytes()
            .map_err(|_| RedactionError::DecisionNotCanonical)?;
        if bytes.len() > MAX_RECORD {
            return Err(RedactionError::DecisionTooLarge);
        }
        let id = DecisionRecord::id_of_bytes(&bytes);
        Ok(RedactedDecision {
            record,
            bytes,
            id,
            audit,
        })
    }

    /// Jede Angabe einer Probe muss die Redaction unverändert passieren.
    fn check_probe(
        &self,
        index: usize,
        probe: &Probe,
        audit: &mut RedactionAudit,
    ) -> Result<(), RedactionError> {
        // Exhaustiv: Eine neue Probenart kompiliert erst mit einer
        // Entscheidung, was an ihr zu prüfen ist.
        let values: Vec<&str> = match probe {
            Probe::Pattern {
                path,
                glob,
                must_match,
                must_not_match,
            } => [path, glob, must_match, must_not_match]
                .into_iter()
                .filter_map(|v| v.as_deref())
                .collect(),
            Probe::Path { path, exists: _ } => vec![path.as_str()],
            Probe::Blob { path, blob } => vec![path.as_str(), blob.as_str()],
        };
        for value in values {
            self.unchanged(Field::DecisionProbe(index), value, audit)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RedactionConfig;
    use minds_core::decision::{DECISION_KIND, DECISION_SCHEMA, DecidedBy, Reason, Weight};

    fn pipeline() -> RedactionPipeline {
        RedactionConfig::default().pipeline().unwrap()
    }

    /// Ein Token, das der Default-Detektor sicher erkennt.
    const TOKEN: &str = "ghp_u8jzPde0IgxLd6GncfBAepfJBd0Kh8oOOL8d";

    fn record() -> DecisionRecord {
        DecisionRecord {
            kind: DECISION_KIND.into(),
            schema: DECISION_SCHEMA,
            topic: "lock for the witness socket on macOS".into(),
            decided_by: DecidedBy::Agent,
            at: "2026-10-21T13:09:00Z".into(),
            chosen: Some(Choice {
                approach: "flock on a lock file".into(),
                rests_on: vec![Reason {
                    assumption: "a1".into(),
                    weight: Weight::LoadBearing,
                }],
            }),
            open: false,
            alternatives: vec![Alternative {
                approach: "Unix socket as the lock".into(),
                rejected_because: vec![Reason {
                    assumption: "a2".into(),
                    weight: Weight::LoadBearing,
                }],
                rests_on: Vec::new(),
            }],
            assumptions: vec![
                Assumption {
                    id: "a1".into(),
                    statement: "the lock file lives on a local file system".into(),
                    probe: None,
                },
                Assumption {
                    id: "a2".into(),
                    statement: "daemon.rs forks".into(),
                    probe: Some(Probe::Pattern {
                        path: Some("crates/minds-cli/src/witness_cmd/daemon.rs".into()),
                        glob: None,
                        must_match: Some(r"\bfork\(".into()),
                        must_not_match: None,
                    }),
                },
            ],
            scope: vec!["crates/minds-cli/src/witness_cmd/**".into()],
            supersedes: None,
        }
    }

    #[test]
    fn a_clean_record_passes_unchanged_and_the_id_is_over_the_stored_bytes() {
        let out = pipeline().redact_decision(record()).unwrap();
        assert_eq!(out.record(), &record());
        assert_eq!(out.audit().counts().secrets, 0);
        assert_eq!(out.audit().counts().pii, 0);
        assert_eq!(&DecisionRecord::id_of_bytes(out.bytes()), out.id());
        assert_eq!(out.bytes(), record().canonical_bytes().unwrap());
    }

    #[test]
    fn free_text_is_redacted_and_the_id_follows_the_redacted_bytes() {
        let mut r = record();
        r.topic = format!("rotate {TOKEN} before release");
        r.assumptions[0].statement = format!("CI still uses {TOKEN}");
        r.alternatives[0].approach = format!("mail {TOKEN} around");
        let out = pipeline().redact_decision(r.clone()).unwrap();
        assert!(!out.record().topic.contains(TOKEN));
        assert!(!out.record().assumptions[0].statement.contains(TOKEN));
        assert!(!out.record().alternatives[0].approach.contains(TOKEN));
        assert_eq!(out.audit().counts().secrets, 3);
        // Die Id ist die der redigierten Bytes — nie die des Klartexts.
        assert_ne!(out.id(), &r.id().unwrap());
        assert_eq!(out.id(), &out.record().id().unwrap());
        assert!(!String::from_utf8_lossy(out.bytes()).contains(TOKEN));
    }

    #[test]
    fn a_secret_in_a_probe_pattern_refuses_the_record() {
        let mut r = record();
        r.assumptions[1].probe = Some(Probe::Pattern {
            path: Some("src/cfg.rs".into()),
            glob: None,
            must_match: Some(TOKEN.into()),
            must_not_match: None,
        });
        assert!(matches!(
            pipeline().redact_decision(r),
            Err(RedactionError::DecisionFieldRedacted {
                field: Field::DecisionProbe(1)
            })
        ));
    }

    #[test]
    fn ordinary_patterns_and_ids_survive() {
        // Muster, die wie Secret-Präfixe aussehen, aber Muster sind; Hex-Ids
        // unter der Entropieschwelle; Pfade mit Punktdateien.
        let mut r = record();
        r.assumptions[1].probe = Some(Probe::Pattern {
            path: None,
            glob: Some("**/.env.example".into()),
            must_match: Some("sk-[A-Za-z0-9]{20,}".into()),
            must_not_match: Some("[A-Za-z0-9+/]{40}=".into()),
        });
        r.assumptions.push(Assumption {
            id: "blob".into(),
            statement: "Cargo.toml unchanged".into(),
            probe: Some(Probe::Blob {
                path: "Cargo.toml".into(),
                blob: "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b".into(),
            }),
        });
        r.supersedes = Some(ContentHash::from_bytes([7; 32]));
        let out = pipeline().redact_decision(r.clone()).unwrap();
        assert_eq!(out.record(), &r);

        // MUST_SURVIVE: Verweise, die wie Secrets aussehen, aber Muster,
        // Pfade oder Ids sind — eine falsche Ablehnung wäre ein Record, den
        // niemand aufnehmen kann.
        for (path, glob, must_match) in [
            (
                Some("config/secrets.yml.enc"),
                None,
                Some("ghp_[A-Za-z0-9]{36}"),
            ),
            (None, Some("**/.env.example"), Some("AKIA[0-9A-Z]{16}")),
            (Some("src/slack.rs"), None, Some("xox[bp]-[0-9A-Za-z-]+")),
        ] {
            let mut r = record();
            r.assumptions[1].probe = Some(Probe::Pattern {
                path: path.map(str::to_owned),
                glob: glob.map(str::to_owned),
                must_match: must_match.map(str::to_owned),
                must_not_match: None,
            });
            let out = pipeline().redact_decision(r.clone()).unwrap();
            assert_eq!(out.record(), &r, "{path:?} {glob:?} {must_match:?}");
        }
        // Eine SHA-256-Blob-Id (64 Hex) ist ein Verweis, kein Secret.
        let mut r = record();
        r.assumptions[1].probe = Some(Probe::Blob {
            path: "Cargo.toml".into(),
            blob: "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7".into(),
        });
        let out = pipeline().redact_decision(r.clone()).unwrap();
        assert_eq!(out.record(), &r);
    }

    /// MUST_REDACT: Freitext mit Secret oder PII wird ersetzt — und der
    /// Record bleibt aufnehmbar.
    #[test]
    fn secrets_and_pii_in_free_text_are_replaced() {
        let aws = "AKIAIOSFODNN7EXAMPLEX";
        let mut r = record();
        r.topic = format!("rotate the key {aws} in staging");
        r.assumptions[0].statement =
            "CI pushes with https://deploy:s3cr3t-pass-word@gitlab.example.com/x.git".into();
        r.alternatives[0].approach = "mail the key to ops@example.com".into();
        let out = pipeline().redact_decision(r).unwrap();
        let text = String::from_utf8_lossy(out.bytes()).into_owned();
        assert!(!text.contains(aws), "{text}");
        assert!(!text.contains("s3cr3t-pass-word"), "{text}");
        assert!(!text.contains("ops@example.com"), "{text}");
        assert!(out.audit().counts().secrets + out.audit().counts().pii >= 3);
    }

    #[test]
    fn an_invalid_record_is_refused_before_anything_is_touched() {
        let mut r = record();
        r.kind = "note".into();
        assert!(matches!(
            pipeline().redact_decision(r),
            Err(RedactionError::Decision(_))
        ));
        let mut r = record();
        r.alternatives[0].rejected_because[0].assumption = "nope".into();
        assert!(matches!(
            pipeline().redact_decision(r),
            Err(RedactionError::Decision(_))
        ));
    }

    #[test]
    fn a_placeholder_that_lengthens_a_text_past_the_limit_is_named_as_such() {
        // Ein kurzes Secret, dessen Platzhalter länger ist als es selbst:
        // Der Text ist genau `MAX_TEXT` lang und wächst beim Ersetzen.
        let secret = "DB_PASSWORD=hunter2";
        let mut r = record();
        r.assumptions[0].statement = format!(
            "{} {secret}",
            "x".repeat(minds_core::decision::MAX_TEXT - secret.len() - 1)
        );
        assert!(matches!(
            pipeline().redact_decision(r),
            Err(RedactionError::DecisionRedactedTooLong)
        ));
    }

    #[test]
    fn a_secret_as_an_assumption_id_refuses_the_record() {
        // 40 Zeichen aus `[A-Za-z0-9_-]` — eine Kennung, die ein Token ist.
        let mut r = record();
        r.assumptions[0].id = TOKEN.into();
        r.chosen.as_mut().unwrap().rests_on[0].assumption = TOKEN.into();
        assert!(matches!(
            pipeline().redact_decision(r),
            Err(RedactionError::DecisionFieldRedacted {
                field: Field::DecisionAssumptionId(0)
            })
        ));
    }

    #[test]
    fn a_secret_in_a_scope_glob_refuses_the_record() {
        let mut r = record();
        r.scope = vec![format!("{TOKEN}/**")];
        assert!(matches!(
            pipeline().redact_decision(r),
            Err(RedactionError::DecisionFieldRedacted {
                field: Field::DecisionScope(0)
            })
        ));
    }
}
