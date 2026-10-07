//! Die fail-closed-Garantie für Intent-Anker (EA-14) — dieselben Bauformen
//! wie bei Sessions ([`crate::session`]) und Observation-Objekten
//! ([`crate::observations`]):
//!
//! 1. Der Snapshot wird **verbraucht**; im Fehlerfall gibt es keinen Anker.
//! 2. Der Nachweis ist ein Typ: [`RedactedIntent`] hat keinen öffentlichen
//!    Konstruktor, `minds-store` nimmt nur ihn entgegen.
//! 3. Der Inhalts-Hash entsteht **erst nach** der Bereinigung — über genau
//!    die Bytes, die abgelegt werden. Ein Hash über den Klartext wäre ein
//!    Wörterbuch-Orakel für das entfernte Secret und aus dem Store nicht
//!    nachrechenbar.
//!
//! Quelle und Scope werden nicht umgeschrieben, sondern **geprüft**: Der
//! Anker verweist auf genau diese Werte und wird signiert. Ein Platzhalter
//! darin wäre ein anderer Verweis, der Klartext ein Leck — findet die Policy
//! dort etwas, gibt es keinen Anker.

use minds_core::ContentHash;
use minds_core::intent_anchor::{
    IntentAnchor, IntentSource, MAX_SNAPSHOT, content_hash, issue_snapshot,
};

use crate::pipeline::RedactionPipeline;
use crate::session::{Field, RedactionAudit, RedactionError};

/// Ein Snapshot vor dem Bauen: roh, oder schon durch die Redaction
/// gegangen (nur aus [`RedactionPipeline::redacted_issue_snapshot`]s
/// Innerem — privat, kein Weg von außen).
enum Snapshot {
    Raw(String),
    Redacted(String),
}

/// Welche Fassung welches Issues (EA-16): Projektpfad, Nummer und
/// `updated_at` — die Bestandteile der `source=issue:…`-Zeile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    /// Projektpfad (`gruppe/projekt`).
    pub project: String,
    /// Die projektinterne Issue-Nummer.
    pub iid: u64,
    /// `updated_at` der Fassung, RFC 3339, wie GitLab es liefert.
    pub updated_at: String,
}

/// Ein Intent-Anker samt redigiertem Snapshot, der die Redaction
/// **nachweislich** durchlaufen hat. Einziger Weg dorthin:
/// [`RedactionPipeline::redact_intent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedIntent {
    anchor: IntentAnchor,
    text: String,
    id: ContentHash,
    snapshot: String,
    audit: RedactionAudit,
}

impl RedactedIntent {
    /// Der Anker.
    pub fn anchor(&self) -> &IntentAnchor {
        &self.anchor
    }

    /// Die kanonische Textform des Ankers (`minds-intent-v1`).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// `anchor_id = derive_key("minds/intent/v1/anchor", text)`.
    pub fn id(&self) -> &ContentHash {
        &self.id
    }

    /// Der redigierte Snapshot — genau die Bytes hinter `content=`.
    pub fn snapshot(&self) -> &str {
        &self.snapshot
    }

    /// Der Nachweis des Laufs (Zähler und Ortsangaben, keine Werte).
    pub fn audit(&self) -> &RedactionAudit {
        &self.audit
    }
}

impl RedactionPipeline {
    /// Redigiert den Snapshot einer Anforderung und baut daraus den Anker.
    /// Jeder Fehler heißt: **es gibt keinen Anker**.
    ///
    /// `snapshot` sind die Rohbytes der Anforderung: bei einer Datei ihr
    /// Inhalt (nur UTF-8), bei einem Issue die kanonische JSON-Form
    /// `{"description":…,"title":…}` (EA-16), beim Prompt sein Text.
    pub fn redact_intent(
        &self,
        source: IntentSource,
        scope: Vec<String>,
        snapshot: Vec<u8>,
    ) -> Result<RedactedIntent, RedactionError> {
        if self.is_empty() {
            return Err(RedactionError::NoDetectors);
        }
        // Nicht größer, als der Store zurückliest — sonst entstünde ein Anker,
        // der sich ablegen, aber nie wieder lesen ließe.
        if snapshot.len() > MAX_SNAPSHOT {
            return Err(RedactionError::IntentSnapshotTooLarge);
        }
        let snapshot = String::from_utf8(snapshot).map_err(|_| RedactionError::IntentNotUtf8)?;
        self.build_intent(
            source,
            scope,
            Snapshot::Raw(snapshot),
            RedactionAudit::default(),
        )
    }

    /// Baut den Anker eines Issues (EA-16) aus Titel und Beschreibung, wie
    /// GitLab sie liefert. Jeder Fehler heißt: **es gibt keinen Anker**.
    ///
    /// Der Snapshot ist genau das Ergebnis von
    /// [`redacted_issue_snapshot`](Self::redacted_issue_snapshot) — derselben
    /// Funktion, mit der `minds verify --online` eine Fassung nachrechnet;
    /// danach wird er nicht noch einmal redigiert. Bauen und Prüfen können
    /// so nie verschieden urteilen, auch nicht mit einem Detektor, der auf
    /// seiner eigenen Ausgabe nicht idempotent wäre.
    pub fn redact_issue_intent(
        &self,
        issue: IssueRef,
        scope: Vec<String>,
        title: String,
        description: String,
    ) -> Result<RedactedIntent, RedactionError> {
        let source = IntentSource::Issue {
            project: issue.project,
            iid: issue.iid,
            updated_at: issue.updated_at,
        };
        let mut audit = RedactionAudit::default();
        let snapshot = self.issue_snapshot(title, description, &mut audit)?;
        self.build_intent(source, scope, Snapshot::Redacted(snapshot), audit)
    }

    /// Der redigierte Snapshot eines Issues: Titel und Beschreibung **je für
    /// sich** redigiert, dann kanonisches JSON (RFC 8785,
    /// [`minds_core::intent_anchor::issue_snapshot`]), dann das JSON als
    /// Ganzes noch einmal.
    ///
    /// Warum zuerst die Felder: Im JSON steht ein Zeilenumbruch als `\n`
    /// mit literalem Backslash. Ein mehrzeiliges Secret (PEM-Block) oder ein
    /// am Zeilenanfang verankertes Muster sähe ein Detektor in der escapten
    /// Form womöglich nicht. Der zweite Lauf über das JSON fängt, was erst
    /// über Feldgrenzen hinweg sichtbar wird.
    ///
    /// Deterministisch (feste Platzhalter, stabile Läufe): Dieselbe Policy
    /// ergibt für denselben Titel und dieselbe Beschreibung dieselben Bytes —
    /// und damit denselben `content=`.
    pub fn redacted_issue_snapshot(
        &self,
        title: String,
        description: String,
    ) -> Result<String, RedactionError> {
        self.issue_snapshot(title, description, &mut RedactionAudit::default())
    }

    fn issue_snapshot(
        &self,
        title: String,
        description: String,
        audit: &mut RedactionAudit,
    ) -> Result<String, RedactionError> {
        if self.is_empty() {
            return Err(RedactionError::NoDetectors);
        }
        if title.len().saturating_add(description.len()) > MAX_SNAPSHOT {
            return Err(RedactionError::IntentSnapshotTooLarge);
        }
        let title = self.redact_field(Field::IntentIssueTitle, title, audit)?;
        let description = self.redact_field(Field::IntentIssueDescription, description, audit)?;
        let json = String::from_utf8(issue_snapshot(&title, &description))
            .map_err(|_| RedactionError::IntentNotUtf8)?;
        self.redact_field(Field::IntentSnapshot, json, audit)
    }

    /// Der gemeinsame Rest: Quelle und Scope prüfen, den Snapshot
    /// redigieren (falls er es nicht schon ist) — erst dann hashen.
    fn build_intent(
        &self,
        source: IntentSource,
        scope: Vec<String>,
        snapshot: Snapshot,
        mut audit: RedactionAudit,
    ) -> Result<RedactedIntent, RedactionError> {
        let (snapshot, already) = match snapshot {
            Snapshot::Raw(text) => (text, false),
            Snapshot::Redacted(text) => (text, true),
        };
        // Auch nach dem Kanonisieren: Escapes verlängern den Text.
        if snapshot.len() > MAX_SNAPSHOT {
            return Err(RedactionError::IntentSnapshotTooLarge);
        }
        // Quelle und Scope zuerst: Findet die Policy dort etwas, wird der
        // Snapshot gar nicht erst angefasst.
        self.check_reference(&source, &scope, &mut audit)?;
        let redacted = if already {
            snapshot.clone()
        } else {
            self.redact_field(Field::IntentSnapshot, snapshot.clone(), &mut audit)?
        };
        // Die Blob-SHA einer Datei ist ein Git-Hash über die **rohen** Bytes.
        // Hat die Redaction im Snapshot etwas entfernt, wäre sie neben dem
        // redigierten Snapshot ein Wörterbuch-Orakel für das Entfernte
        // (Kandidat einsetzen, `git hash-object`, vergleichen). Fail-closed:
        // kein Anker.
        if matches!(source, IntentSource::File { .. }) && redacted != snapshot {
            return Err(RedactionError::IntentSourceWouldOracle);
        }
        let snapshot = redacted;
        let anchor = IntentAnchor {
            source,
            content: content_hash(snapshot.as_bytes()),
            scope,
        };
        let text = self.check_text(&anchor, &mut audit)?;
        let id = IntentAnchor::id_of_text(&text);
        Ok(RedactedIntent {
            anchor,
            text,
            id,
            snapshot,
            audit,
        })
    }

    /// Prüft einen fertigen Anker so, wie [`redact_intent`](Self::redact_intent)
    /// ihn baut: Quelle und Scope Feld für Feld, dann die ganze Textform.
    /// Gibt die Textform zurück. Der Witness prüft eine Aktivierung genau
    /// hiermit — Bauen und Annehmen urteilen nie verschieden.
    pub fn check_intent_anchor(&self, anchor: &IntentAnchor) -> Result<String, RedactionError> {
        if self.is_empty() {
            return Err(RedactionError::NoDetectors);
        }
        let mut audit = RedactionAudit::default();
        self.check_reference(&anchor.source, &anchor.scope, &mut audit)?;
        self.check_text(anchor, &mut audit)
    }

    /// Quelle und Scope: Die Policy darf darin nichts finden.
    fn check_reference(
        &self,
        source: &IntentSource,
        scope: &[String],
        audit: &mut RedactionAudit,
    ) -> Result<(), RedactionError> {
        // Exhaustiv: Bekommt die Quelle eine neue Form, bricht hier der Build.
        match source {
            IntentSource::File { path, blob } => {
                // Die Secretfile-Mauer gilt auch hier (EA-15): Was in `.env`
                // steht, finden die Detektoren nicht immer.
                if crate::is_secret_file(&format!("/{path}")) {
                    return Err(RedactionError::IntentSecretFile);
                }
                self.unchanged(Field::IntentSource, path, audit)?;
                self.unchanged(Field::IntentSource, blob, audit)?;
            }
            IntentSource::Issue {
                project,
                iid: _,
                updated_at,
            } => {
                self.unchanged(Field::IntentSource, project, audit)?;
                self.unchanged(Field::IntentSource, updated_at, audit)?;
            }
            IntentSource::Prompt => {}
        }
        for (index, glob) in scope.iter().enumerate() {
            self.unchanged(Field::IntentScope(index), glob, audit)?;
        }
        Ok(())
    }

    /// Die ganze Textform: Ein Fund über Feldgrenzen hinweg (mit Komma
    /// verbundene Globs, `schlüssel=wert`-Muster) bleibt so nicht unentdeckt.
    fn check_text(
        &self,
        anchor: &IntentAnchor,
        audit: &mut RedactionAudit,
    ) -> Result<String, RedactionError> {
        let text = anchor.to_text()?;
        self.unchanged(Field::IntentAnchor, &text, audit)?;
        Ok(text)
    }

    /// Prüft, dass die Policy in `value` nichts findet.
    fn unchanged(
        &self,
        field: Field,
        value: &str,
        audit: &mut RedactionAudit,
    ) -> Result<(), RedactionError> {
        if self.redact_field(field, value.to_owned(), audit)? != value {
            return Err(RedactionError::IntentFieldRedacted { field });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RedactionConfig;

    fn pipeline() -> RedactionPipeline {
        RedactionConfig::default().pipeline().unwrap()
    }

    /// Ein Token, das der Default-Detektor sicher erkennt.
    const TOKEN: &str = "ghp_R4nd0mT0k3nV4lu3F0rT3st1ngPurp0s3s00";

    fn file() -> IntentSource {
        IntentSource::File {
            path: "docs/spec.md".into(),
            blob: "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b".into(),
        }
    }

    /// EA-15: Eine Zugangsdaten-Datei bekommt keinen Anker — auch nicht mit
    /// Inhalt, den kein Detektor erkennt; eine Anforderung, die nur über
    /// Passwörter spricht, schon.
    #[test]
    fn credential_files_get_no_anchor() {
        let pipeline = pipeline();
        let text = b"DB_PASS=Winter2024orders\n".to_vec();
        for path in [
            ".env",
            "config/.pgpass",
            ".netrc",
            "deploy/credentials.json",
        ] {
            let source = IntentSource::File {
                path: path.into(),
                blob: "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b".into(),
            };
            assert!(
                matches!(
                    pipeline.redact_intent(source.clone(), Vec::new(), text.clone()),
                    Err(RedactionError::IntentSecretFile)
                ),
                "{path}"
            );
            let anchor = IntentAnchor {
                source,
                content: content_hash(&text),
                scope: Vec::new(),
            };
            assert!(matches!(
                pipeline.check_intent_anchor(&anchor),
                Err(RedactionError::IntentSecretFile)
            ));
        }
        for path in [
            "docs/env-variables.md",
            "docs/anforderung.md",
            ".env.example",
        ] {
            let source = IntentSource::File {
                path: path.into(),
                blob: "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b".into(),
            };
            pipeline
                .redact_intent(
                    source,
                    Vec::new(),
                    "Passwörter werden mit Argon2 gehasht.\n"
                        .as_bytes()
                        .to_vec(),
                )
                .unwrap();
        }
    }

    #[test]
    fn intent_snapshot_is_redacted_before_hash() {
        let raw = format!("Deploy with token {TOKEN} to staging.\n");
        let redacted = pipeline()
            .redact_intent(
                IntentSource::Prompt,
                vec!["src/**".into()],
                raw.clone().into_bytes(),
            )
            .unwrap();
        assert!(!redacted.snapshot().contains(TOKEN));
        assert!(!redacted.text().contains(TOKEN));
        assert!(redacted.audit().fields_changed() > 0);
        // Der Hash gehört zu den abgelegten Bytes, nie zum Klartext.
        assert_eq!(
            redacted.anchor().content,
            content_hash(redacted.snapshot().as_bytes())
        );
        assert_ne!(redacted.anchor().content, content_hash(raw.as_bytes()));
        assert_eq!(redacted.id(), &IntentAnchor::id_of_text(redacted.text()));
        assert_eq!(
            &IntentAnchor::parse(redacted.text()).unwrap(),
            redacted.anchor()
        );
    }

    #[test]
    fn a_clean_snapshot_survives_unchanged() {
        let raw = "Die Retry-Logik soll exponentiell zurückfallen.\n";
        let redacted = pipeline()
            .redact_intent(IntentSource::Prompt, Vec::new(), raw.as_bytes().to_vec())
            .unwrap();
        assert_eq!(redacted.snapshot(), raw);
        assert_eq!(redacted.anchor().content, content_hash(raw.as_bytes()));
        assert!(redacted.audit().is_clean());
    }

    #[test]
    fn a_secret_in_source_or_scope_refuses_the_anchor() {
        let source = IntentSource::File {
            path: format!("docs/{TOKEN}.md"),
            blob: "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b".into(),
        };
        assert_eq!(
            pipeline().redact_intent(source, Vec::new(), b"x".to_vec()),
            Err(RedactionError::IntentFieldRedacted {
                field: Field::IntentSource
            })
        );
        assert_eq!(
            pipeline().redact_intent(
                IntentSource::Prompt,
                vec!["src/**".into(), format!("{TOKEN}/**")],
                b"x".to_vec()
            ),
            Err(RedactionError::IntentFieldRedacted {
                field: Field::IntentScope(1)
            })
        );
        let issue = IntentSource::Issue {
            project: format!("team/{TOKEN}"),
            iid: 7,
            updated_at: "2026-10-01T08:15:00Z".into(),
        };
        assert!(matches!(
            pipeline().redact_intent(issue, Vec::new(), b"x".to_vec()),
            Err(RedactionError::IntentFieldRedacted { .. })
        ));
    }

    /// Aus dem Security-Review: das Issue-JSON mit escaptem Token (EA-16
    /// liefert genau diese Form) und ein Token im Projektpfad.
    #[test]
    fn issue_forms_with_tokens_never_reach_the_anchor() {
        let pat = concat!("glpat", "-AbCdEfGhIjKlMnOpQrSt");
        let json = format!(r#"{{"description":"export TOKEN=\"{pat}\"","title":"x"}}"#);
        let redacted = pipeline()
            .redact_intent(IntentSource::Prompt, Vec::new(), json.into_bytes())
            .unwrap();
        assert!(
            !redacted.snapshot().contains(pat),
            "{}",
            redacted.snapshot()
        );

        let issue = IntentSource::Issue {
            project: format!("{pat}/minds"),
            iid: 7,
            updated_at: "2026-10-01T08:15:00Z".into(),
        };
        assert_eq!(
            pipeline().redact_intent(issue, Vec::new(), b"x".to_vec()),
            Err(RedactionError::IntentFieldRedacted {
                field: Field::IntentSource
            })
        );
    }

    /// Korpus-Fälle aus dem Security-Review (Iteration 2).
    #[test]
    fn reference_secrets_are_refused_and_plain_text_survives() {
        let blob = "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b";
        // MUST_REDACT — als Verweis abgelehnt.
        for (source, scope) in [
            (
                IntentSource::Prompt,
                vec!["https://deploy:S3cr3tPassw0rd@git.example.com/**".to_owned()],
            ),
            (
                IntentSource::File {
                    path: "docs/AKIAIOSFODNN7EXAMPLE.md".into(),
                    blob: blob.into(),
                },
                Vec::new(),
            ),
            (
                IntentSource::Issue {
                    project: format!("team/{}/x", concat!("glpat", "-AbCdEfGhIjKlMnOpQrSt")),
                    iid: 1,
                    updated_at: "2026-10-01T08:15:00Z".into(),
                },
                Vec::new(),
            ),
            // Über Feldgrenzen: erst die ganze Textform zeigt das Muster.
            (
                IntentSource::Prompt,
                vec!["password=hunter2".to_owned(), "x".to_owned()],
            ),
        ] {
            assert!(
                matches!(
                    pipeline().redact_intent(source.clone(), scope.clone(), b"x".to_vec()),
                    Err(RedactionError::IntentFieldRedacted { .. })
                ),
                "{source:?} {scope:?}"
            );
        }
        // MUST_REDACT im Snapshot: maskiertes Anführungszeichen im Secret.
        let json = r#"{"description":"DB_PASSWORD=\"hunt\\\"er2-Zq9xK7pL\"","title":"x"}"#;
        let redacted = pipeline()
            .redact_intent(IntentSource::Prompt, Vec::new(), json.as_bytes().to_vec())
            .unwrap();
        assert!(
            !redacted.snapshot().contains("er2-Zq9xK7pL"),
            "{}",
            redacted.snapshot()
        );

        // MUST_SURVIVE: ein echter Anker samt sichtbarer Interpunktion.
        let text = "Retry soll exponentiell zurückfallen (max. 5×, 200 ms Basis).\n";
        let redacted = pipeline()
            .redact_intent(
                IntentSource::File {
                    path: "docs/@team/spec.md".into(),
                    blob: "a".repeat(64),
                },
                vec!["src/retry/**".into(), "tests/retry_*.rs".into()],
                text.as_bytes().to_vec(),
            )
            .unwrap();
        assert_eq!(redacted.snapshot(), text);
        assert_eq!(
            pipeline().check_intent_anchor(redacted.anchor()).unwrap(),
            redacted.text()
        );
    }

    /// Security-Review Iteration 3: Die Blob-SHA einer Datei hasht die rohen
    /// Bytes — mit Secret darin gibt es keinen Anker. Eine saubere Datei
    /// bleibt verankerbar.
    #[test]
    fn a_file_whose_snapshot_needs_redaction_gets_no_anchor() {
        let raw = format!("DB_PASSWORD=hunter2\nToken {TOKEN}\n");
        assert_eq!(
            pipeline().redact_intent(file(), Vec::new(), raw.into_bytes()),
            Err(RedactionError::IntentSourceWouldOracle)
        );
        assert!(
            pipeline()
                .redact_intent(file(), Vec::new(), b"Retry exponentiell.\n".to_vec())
                .is_ok()
        );
    }

    /// Ein über Kommas verteiltes Token fängt erst die Prüfung der ganzen
    /// Textform.
    #[test]
    fn a_token_split_across_scope_globs_is_refused() {
        for scope in [
            vec![format!("token={TOKEN}")],
            vec![
                "Authorization: Bearer".to_owned(),
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJlLXZhbHVl".to_owned(),
            ],
        ] {
            assert!(
                pipeline()
                    .redact_intent(IntentSource::Prompt, scope.clone(), b"x".to_vec())
                    .is_err(),
                "{scope:?}"
            );
        }
    }

    #[test]
    fn a_snapshot_larger_than_the_store_reads_gets_no_anchor() {
        let max = minds_core::intent_anchor::MAX_SNAPSHOT;
        assert_eq!(
            pipeline().redact_intent(IntentSource::Prompt, Vec::new(), vec![b'a'; max + 1]),
            Err(RedactionError::IntentSnapshotTooLarge)
        );
    }

    #[test]
    fn invalid_input_yields_no_anchor() {
        assert_eq!(
            pipeline().redact_intent(IntentSource::Prompt, Vec::new(), vec![0xff, 0xfe]),
            Err(RedactionError::IntentNotUtf8)
        );
        assert!(matches!(
            pipeline().redact_intent(IntentSource::Prompt, vec![String::new()], b"x".to_vec()),
            Err(RedactionError::IntentAnchor(_))
        ));
        let empty = RedactionPipeline::default();
        assert_eq!(
            empty.redact_intent(IntentSource::Prompt, Vec::new(), b"x".to_vec()),
            Err(RedactionError::NoDetectors)
        );
    }

    fn issue() -> IssueRef {
        IssueRef {
            project: "team/minds".into(),
            iid: 42,
            updated_at: "2026-10-01T08:15:00Z".into(),
        }
    }

    /// EA-16: Titel und Beschreibung werden je Feld redigiert, bevor sie in
    /// das JSON gehen — ein PEM-Block mit echten Zeilenumbrüchen fällt, und
    /// das Nachrechnen ergibt denselben Snapshot.
    #[test]
    fn issue_fields_are_redacted_before_canonicalizing() {
        let pipeline = pipeline();
        let description = "Deploy-Key:\n-----BEGIN RSA PRIVATE KEY-----\n\
                           MIIEowIBAAKCAQEAx7Vn9pQmKtLb\n-----END RSA PRIVATE KEY-----\n";
        let title = format!("Token {TOKEN} rotieren");
        let intent = pipeline
            .redact_issue_intent(issue(), Vec::new(), title.clone(), description.into())
            .unwrap();
        for secret in [TOKEN, "MIIEowIBAAKCAQEAx7Vn9pQmKtLb"] {
            assert!(!intent.snapshot().contains(secret), "{}", intent.snapshot());
            assert!(!intent.text().contains(secret));
        }
        assert!(
            intent
                .snapshot()
                .starts_with("{\"description\":\"Deploy-Key:\\n")
        );
        assert!(!intent.audit().is_clean());
        assert_eq!(
            intent.anchor().content,
            content_hash(intent.snapshot().as_bytes())
        );
        assert_eq!(
            pipeline
                .redacted_issue_snapshot(title, description.into())
                .unwrap(),
            intent.snapshot()
        );
        assert!(matches!(
            &intent.anchor().source,
            IntentSource::Issue { project, iid: 42, .. } if project == "team/minds"
        ));
    }

    /// Ein sauberes Issue: Der Snapshot ist genau die kanonische Rohform.
    #[test]
    fn a_clean_issue_snapshot_is_the_canonical_form() {
        let intent = pipeline()
            .redact_issue_intent(
                issue(),
                vec!["src/retry/**".into()],
                "Retry mit Backoff".into(),
                "Exponentiell, max. 5×.".into(),
            )
            .unwrap();
        assert_eq!(
            intent.snapshot().as_bytes(),
            issue_snapshot("Retry mit Backoff", "Exponentiell, max. 5×.")
        );
        assert!(intent.audit().is_clean());
    }

    /// Ein Issue, das größer ist, als der Store zurückliest, bekommt keinen
    /// Anker — weder roh noch erst nach dem Kanonisieren.
    #[test]
    fn an_oversized_issue_gets_no_anchor() {
        let max = MAX_SNAPSHOT;
        assert_eq!(
            pipeline().redacted_issue_snapshot("t".into(), "a".repeat(max)),
            Err(RedactionError::IntentSnapshotTooLarge)
        );
        // Escapes verlängern den Text: unter der Grenze roh, darüber als JSON.
        assert_eq!(
            pipeline().redact_issue_intent(
                issue(),
                Vec::new(),
                "t".into(),
                "\"".repeat(max / 2 + 1)
            ),
            Err(RedactionError::IntentSnapshotTooLarge)
        );
    }
}
