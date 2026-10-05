//! Das Observation-Objekt des Datei-Beobachters (EA-08): was der Witness im
//! Worktree gesehen hat — repo-relative Pfade und blake3-Hashes, nie Inhalt.
//!
//! # Form
//!
//! Kanonisches JSON nach RFC 8785 ([`crate::canonical`]):
//!
//! ```text
//! {"first_at":…,"last_at":…,"observations":[{"at":…,"content":"b3-…"|null,
//!  "path":…,"reason":null|"…","seq":…}],"schema":1}
//! ```
//!
//! Alle Schlüssel stehen immer da, auch mit `null` — eine Form, ein Hash.
//!
//! # Identität
//!
//! Wie bei Sessions: `blake3(kanonische Bytes)`, ohne eigene Hash-Domain
//! (W4: neue Domains nur, wo eine Spec sie nennt). Die Id steht in der
//! `session=`-Zeile des `witness-fs/v1`-Seals (Outcome
//! `observations_stored`).
//!
//! # Warum kein `forget`
//!
//! Das Objekt trägt nur Pfade (redigiert) und Hashes — für Secret-Dateien,
//! ignorierte Pfade und alles außerhalb des Repos nicht einmal die. Es gibt
//! nichts Tilgbares darin, das nicht schon im Commit selbst stünde.
//!
//! # Wer es baut
//!
//! Der Typ ist öffentlich konstruierbar, gespeichert wird aber nur die
//! redigierte Form (`minds_redact::RedactedObservations`), die ausschließlich
//! die Pipeline erzeugt — dieselbe Typ-Garantie wie bei Sessions.

use serde::{Deserialize, Serialize};

use crate::{CanonError, ContentHash};

/// Schema-Version des Observation-Objekts.
pub const OBSERVATIONS_SCHEMA: u32 = 1;

/// Größte Datei, die der Beobachter liest und hasht (16 MiB); größere
/// erscheinen mit [`ObservationReason::TooLarge`] ohne Hash.
pub const MAX_OBSERVED_BYTES: u64 = 16 * 1024 * 1024;

/// Die Beobachtungen einer versiegelten Epoche des Witness-Streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observations {
    /// [`OBSERVATIONS_SCHEMA`].
    pub schema: u32,
    /// Zeitstempel der ersten Beobachtung (RFC 3339), `None` ohne Beobachtung.
    pub first_at: Option<String>,
    /// Zeitstempel der letzten Beobachtung.
    pub last_at: Option<String>,
    /// Nach `seq` aufsteigend.
    pub observations: Vec<Observation>,
}

/// Eine beobachtete Änderung: `fs.observed { path, content, at }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// Die Sequenznummer des `fs.observed`-Events in seiner Epoche.
    pub seq: u64,
    /// Beobachtungszeit des Witness (RFC 3339).
    pub at: String,
    /// Repo-relativ, `/`-getrennt — nie der absolute Host-Pfad.
    pub path: String,
    /// blake3 des Inhalts; `None` genau dann, wenn [`reason`](Self::reason)
    /// gesetzt ist.
    pub content: Option<ContentHash>,
    /// Warum kein Hash vorliegt.
    pub reason: Option<ObservationReason>,
}

/// Warum eine Beobachtung keinen Inhalts-Hash trägt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationReason {
    /// Die Datei ist größer als [`MAX_OBSERVED_BYTES`].
    TooLarge,
    /// Secret-Datei (`.env`, Schlüssel …): Es wird nie ein Hash gebildet.
    SecretFile,
    /// Der Pfad existiert nicht mehr — beobachtete Abwesenheit.
    Deleted,
    /// Ein Symlink, dessen Ziel außerhalb des Repos liegt, oder eine Datei
    /// mit einem zweiten harten Link (sie kann eine Host-Datei sein).
    OutsideRepo,
    /// Der Inhalt trägt etwas, das die Redaction-Pipeline ersetzen würde —
    /// kein Hash, der ein Wörterbuch-Orakel über ein Secret wäre (dieselbe
    /// Regel wie `Effect.written`).
    RedactedContent,
    /// Kein UTF-8: nicht scanbar, also auch kein Hash (wie `Effect.written`).
    Unscannable,
    /// Ein Grund, den dieses Binary nicht kennt (tolerant gelesen; dieses
    /// Binary schreibt ihn nie).
    #[serde(other)]
    Unknown,
}

impl ObservationReason {
    /// Das Wire-Wort.
    pub const fn word(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::SecretFile => "secret_file",
            Self::Deleted => "deleted",
            Self::OutsideRepo => "outside_repo",
            Self::RedactedContent => "redacted_content",
            Self::Unscannable => "unscannable",
            Self::Unknown => "unknown",
        }
    }
}

/// Warum ein Observation-Objekt nicht geschrieben werden darf. Nennt das
/// Problem, zitiert nie einen Pfad.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ObservationError {
    #[error("unsupported observation schema")]
    Schema,
    #[error("observation {0}: path is not a plain repo-relative path")]
    Path(usize),
    #[error("observation {0}: content and reason must be exclusive")]
    ContentReason(usize),
    #[error("observation {0}: sequence numbers must increase")]
    Order(usize),
    #[error("observation window does not match the observations")]
    Window,
}

impl Observations {
    /// Baut das Objekt aus Beobachtungen in `seq`-Reihenfolge; das Fenster
    /// ergibt sich aus der ersten und letzten.
    pub fn new(observations: Vec<Observation>) -> Self {
        Self {
            schema: OBSERVATIONS_SCHEMA,
            first_at: observations.first().map(|o| o.at.clone()),
            last_at: observations.last().map(|o| o.at.clone()),
            observations,
        }
    }

    /// Die Schreib-Regeln: Schema bekannt, Pfade schlicht repo-relativ,
    /// `content` xor `reason`, `seq` streng steigend, Fenster passend.
    /// Der Leser ist tolerant; geschrieben wird nur, was hier besteht.
    pub fn check(&self) -> Result<(), ObservationError> {
        if self.schema != OBSERVATIONS_SCHEMA {
            return Err(ObservationError::Schema);
        }
        let mut last = None;
        for (index, o) in self.observations.iter().enumerate() {
            if !plain_relative(&o.path) {
                return Err(ObservationError::Path(index));
            }
            let unknown = o.reason == Some(ObservationReason::Unknown);
            if o.content.is_some() == o.reason.is_some() || unknown {
                return Err(ObservationError::ContentReason(index));
            }
            if last.is_some_and(|seq| o.seq <= seq) {
                return Err(ObservationError::Order(index));
            }
            last = Some(o.seq);
        }
        if self.first_at.as_ref() != self.observations.first().map(|o| &o.at)
            || self.last_at.as_ref() != self.observations.last().map(|o| &o.at)
        {
            return Err(ObservationError::Window);
        }
        Ok(())
    }

    /// Die kanonischen Bytes (RFC 8785).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonError> {
        crate::to_canonical_json(self)
    }

    /// Die Identität: `blake3(kanonische Bytes)`.
    pub fn id(&self) -> Result<ContentHash, CanonError> {
        Ok(Self::id_of_bytes(&self.canonical_bytes()?))
    }

    /// Die Identität gespeicherter Bytes — der Leser prüft gegen sie, nicht
    /// gegen eine Neu-Serialisierung (Vorwärts-Toleranz wie bei Sessions).
    pub fn id_of_bytes(bytes: &[u8]) -> ContentHash {
        ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
    }
}

/// Repo-relativ, `/`-getrennt, ohne leere, `.`- oder `..`-Komponenten, ohne
/// Backslash und Steuerzeichen.
pub fn plain_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Observations {
        Observations::new(vec![
            Observation {
                seq: 1,
                at: "2026-10-05T10:00:00.250Z".into(),
                path: "src/sort/merge.rs".into(),
                content: Some(ContentHash::from_bytes(
                    *blake3::hash(b"fn merge() {}\n").as_bytes(),
                )),
                reason: None,
            },
            Observation {
                seq: 2,
                at: "2026-10-05T10:00:01Z".into(),
                path: ".env".into(),
                content: None,
                reason: Some(ObservationReason::SecretFile),
            },
            Observation {
                seq: 4,
                at: "2026-10-05T10:00:02Z".into(),
                path: "old.txt".into(),
                content: None,
                reason: Some(ObservationReason::Deleted),
            },
        ])
    }

    #[test]
    fn observation_object_canonical_golden() {
        let object = sample();
        object.check().unwrap();
        let text = String::from_utf8(object.canonical_bytes().unwrap()).unwrap();
        assert_eq!(
            text,
            concat!(
                r#"{"first_at":"2026-10-05T10:00:00.250Z","last_at":"2026-10-05T10:00:02Z","#,
                r#""observations":[{"at":"2026-10-05T10:00:00.250Z","#,
                r#""content":"b3-77d93191302c0026c78d4347b6b5b6ecff6225060cb3128e487a849f474f0857","path":"src/sort/merge.rs","reason":null,"seq":1},"#,
                r#"{"at":"2026-10-05T10:00:01Z","content":null,"path":".env","reason":"secret_file","seq":2},"#,
                r#"{"at":"2026-10-05T10:00:02Z","content":null,"path":"old.txt","reason":"deleted","seq":4}],"#,
                r#""schema":1}"#
            )
        );
        assert_eq!(
            object.id().unwrap().as_str(),
            "b3-f5817f8e867da041c4993e0efb23b7c1ce4bc318f18312c08d2f323b3aca81f0"
        );
    }

    #[test]
    fn observation_object_roundtrips_and_reads_tolerantly() {
        let object = sample();
        let bytes = object.canonical_bytes().unwrap();
        let back: Observations = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, object);
        assert_eq!(Observations::id_of_bytes(&bytes), object.id().unwrap());
        // Ein künftiger Grund und ein unbekanntes Feld brechen den Leser nicht.
        let future = String::from_utf8(bytes)
            .unwrap()
            .replace("\"deleted\"", "\"renamed\"")
            .replace("\"schema\":1", "\"schema\":1,\"extra\":true");
        let read: Observations = serde_json::from_str(&future).unwrap();
        assert_eq!(
            read.observations[2].reason,
            Some(ObservationReason::Unknown)
        );
        // … geschrieben wird so etwas aber nie.
        assert_eq!(read.check(), Err(ObservationError::ContentReason(2)));
    }

    #[test]
    fn check_refuses_what_must_not_be_written() {
        let mut absolute = sample();
        absolute.observations[0].path = "/home/dev/repo/a.rs".into();
        assert_eq!(absolute.check(), Err(ObservationError::Path(0)));
        for path in ["", "a/../b", "./a", "a//b", "a\\b", "a\nb"] {
            let mut bad = sample();
            bad.observations[0].path = path.into();
            assert_eq!(bad.check(), Err(ObservationError::Path(0)), "{path:?}");
        }
        let mut both = sample();
        both.observations[1].content = both.observations[0].content.clone();
        assert_eq!(both.check(), Err(ObservationError::ContentReason(1)));
        let mut neither = sample();
        neither.observations[0].content = None;
        assert_eq!(neither.check(), Err(ObservationError::ContentReason(0)));
        let mut order = sample();
        order.observations[2].seq = 1;
        assert_eq!(order.check(), Err(ObservationError::Order(2)));
        let mut window = sample();
        window.last_at = None;
        assert_eq!(window.check(), Err(ObservationError::Window));
        let mut schema = sample();
        schema.schema = 2;
        assert_eq!(schema.check(), Err(ObservationError::Schema));
        assert!(Observations::new(Vec::new()).check().is_ok());
    }
}
