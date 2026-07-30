//! Content-adressierte Identität einer Session.
//!
//! Die [`SessionId`] ist `blake3(canonical_json(session))` — der Hash *ist* die
//! ID (Architektur-Prinzip 1 im Plan). Daraus fallen mehrere Eigenschaften
//! gratis ab:
//!
//! - **Dedup:** gleicher Inhalt ⇒ gleiche ID ⇒ genau ein Objekt im Store.
//! - **Verifizierbarkeit:** jeder Dritte (Python, Go, ein Auditor) kann die ID
//!   aus dem JSON reproduzieren, solange er dieselbe Kanonisierung (RFC 8785,
//!   siehe [`crate::canonical`]) und blake3 verwendet. Deshalb ist die
//!   Kanonisierung ein Standard und keine Haus-Erfindung.
//! - **Stabilität über Rebase/Squash/Cherry-Pick:** die ID hängt am Inhalt der
//!   Session, nicht am Commit-Hash. Der Trailer `Minds-Session-Id: <id>` trägt
//!   sie in die Commit-Message und überlebt damit History-Rewrites.
//!
//! # Textform
//!
//! Die kanonische Textform ist `b3-<64 Hex>` in Kleinschreibung. Das Präfix
//! `b3-` benennt den Hash-Algorithmus explizit: Käme je ein zweiter Algorithmus
//! hinzu, blieben bestehende IDs eindeutig lesbar. Genau diese Textform steht
//! im Commit-Trailer und in `index.json`.
//!
//! Dieses Modul hat **kein I/O**: es kanonisiert (in-memory) und hasht. Serde
//! bildet die `SessionId` auf ihre Textform ab, damit sie in JSON round-trippt
//! und mit der Trailer-/Index-Schreibweise bit-identisch ist.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::canonical::{CanonError, to_canonical_json};
use crate::session::Session;

/// Präfix der Textform. Benennt den Hash-Algorithmus (blake3) und macht spätere
/// Algorithmus-Wechsel eindeutig unterscheidbar.
pub const SESSION_ID_PREFIX: &str = "b3-";

/// Länge der Hex-Kodierung eines 32-Byte-blake3-Digests.
const HEX_LEN: usize = 64;

/// Content-adressierte Identität einer [`Session`]: `blake3(canonical_json(..))`.
///
/// Intern die rohen 32 Bytes des Digests. `Ord`/`Hash` sind damit ableitbar,
/// was die Verwendung als Map-Schlüssel im Reader-Index (`index.json`) erlaubt.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId([u8; 32]);

impl SessionId {
    /// Berechnet die ID als blake3-Hash der **kanonischen** JSON-Bytes von
    /// `value`. Das ist der einzige Weg, eine ID aus Inhalt zu erzeugen — er
    /// bindet sie unauflöslich an die kanonische Repräsentation.
    pub fn of<T: Serialize>(value: &T) -> Result<Self, CanonError> {
        Ok(Self::from_canonical_bytes(&to_canonical_json(value)?))
    }

    /// Hasht bereits kanonisierte Bytes. Nützlich, wenn die kanonische Form
    /// ohnehin vorliegt (Store/Reader), und macht den Vertrag explizit: die ID
    /// ist der Hash *genau dieser* Bytes.
    ///
    /// Der Aufrufer verbürgt sich dafür, dass `bytes` tatsächlich kanonisch sind
    /// — sonst entsteht eine ID, die niemand reproduzieren kann.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Die rohen 32 Bytes des Digests.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Session {
    /// Die content-adressierte [`SessionId`] dieser Session.
    ///
    /// Bequemer Aufruf für `SessionId::of(self)`. Steht bewusst hier und nicht
    /// in `session.rs`, damit das Datenmodell-Modul frei von Hashing bleibt.
    pub fn id(&self) -> Result<SessionId, CanonError> {
        SessionId::of(self)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(SESSION_ID_PREFIX)?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Debug zeigt die lesbare Textform statt eines 32-Byte-Arrays — das hält
/// Test-Ausgaben und Logs verständlich.
impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionId({self})")
    }
}

/// Fehler beim Parsen einer [`SessionId`] aus ihrer Textform.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionIdParseError {
    /// Der `b3-`-Präfix fehlt.
    #[error("SessionId muss mit \"b3-\" beginnen")]
    MissingPrefix,

    /// Nach dem Präfix stehen nicht genau 64 Hex-Zeichen.
    #[error("SessionId braucht 64 Hex-Zeichen, gefunden: {0}")]
    WrongLength(usize),

    /// Ein Zeichen im Hex-Teil ist keine gültige Hex-Ziffer.
    #[error("ungültiges Hex-Zeichen in SessionId: {0:?}")]
    InvalidHexDigit(char),
}

impl FromStr for SessionId {
    type Err = SessionIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s
            .strip_prefix(SESSION_ID_PREFIX)
            .ok_or(SessionIdParseError::MissingPrefix)?;
        if hex.len() != HEX_LEN {
            return Err(SessionIdParseError::WrongLength(hex.len()));
        }

        let mut bytes = [0u8; 32];
        let raw = hex.as_bytes();
        for (i, byte) in bytes.iter_mut().enumerate() {
            let hi = hex_digit(raw[2 * i])?;
            let lo = hex_digit(raw[2 * i + 1])?;
            *byte = (hi << 4) | lo;
        }
        Ok(Self(bytes))
    }
}

/// Akzeptiert Groß- und Kleinschreibung beim **Lesen** — ein hand-editierter
/// oder von einer Fremdimplementierung geschriebener Trailer soll auflösbar
/// bleiben. **Geschrieben** wird ausschließlich Kleinschreibung (siehe
/// [`Display`](fmt::Display)), damit die Textform kanonisch ist.
fn hex_digit(byte: u8) -> Result<u8, SessionIdParseError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        other => Err(SessionIdParseError::InvalidHexDigit(other as char)),
    }
}

impl Serialize for SessionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{SESSION_ID_PREFIX, SessionId, SessionIdParseError};
    use crate::canonical::{to_canonical_json, to_canonical_string};
    use crate::session::{
        Agent, Intent, Model, Produced, Redaction, RedactionCounts, Role, Session, Turn, Usage,
    };

    fn sample() -> Session {
        let mut s = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.0.0".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent {
                request: "Flaky Retry-Test reparieren".into(),
                constraints: vec!["keine neuen Dependencies".into()],
                discarded: vec!["Timeout einfach hochsetzen".into()],
            },
        );
        s.turns.push(Turn {
            role: Role::User,
            text: "Der Retry-Test flackert, bitte fixen.".into(),
            tool_calls: Vec::new(),
            parent: None,
            at: None,
        });
        s.usage = Usage {
            input_tokens: 1234,
            output_tokens: 567,
        };
        s.produced = Produced {
            commit_hint: None,
            files: vec!["src/retry.rs".into()],
        };
        s.redaction = Redaction {
            applied: true,
            counts: RedactionCounts { secrets: 0, pii: 1 },
        };
        s
    }

    // --- Golden-Tests: eingefrorener Known-Answer-Vektor ----------------------
    //
    // Die übrigen Tests hier sind *relativ*: Roundtrip, Determinismus,
    // Selbstkonsistenz (`id == blake3(canonical(..))`). Sie blieben grün, wenn
    // sich die Kanonisierung *konsistent* änderte. Für einen content-adressierten
    // Record ist genau das der Super-GAU: die ID ist ein Versprechen an Dritte
    // (Python, Go, ein Auditor), aus demselben JSON denselben Hash zu bilden.
    // Diese Werte frieren das Versprechen ein. Ändert sich einer, ist das ein
    // bewusster Schema-/Kanonisierungs-Bruch (Versions-Bump) — kein Refactor.
    // Neu erzeugen mit:
    //   cargo test -p minds-core -- --ignored --nocapture reference_vector

    /// Kanonische Form von [`sample()`] nach RFC 8785 (497 Bytes, reines ASCII).
    /// Raw-String, damit der Wert 1:1 gegen die Ausgabe von `to_canonical_string`
    /// diffbar bleibt (der JSON-Text enthält `"`, aber nie die Sequenz `"#`).
    const GOLDEN_CANONICAL: &str = r#"{"agent":{"name":"claude-code","version":"1.0.0"},"intent":{"constraints":["keine neuen Dependencies"],"discarded":["Timeout einfach hochsetzen"],"request":"Flaky Retry-Test reparieren"},"model":{"id":"claude-opus-4","provider":"anthropic"},"produced":{"files":["src/retry.rs"]},"redaction":{"applied":true,"counts":{"pii":1,"secrets":0}},"schema_version":1,"turns":[{"role":"user","text":"Der Retry-Test flackert, bitte fixen.","tool_calls":[]}],"usage":{"input_tokens":1234,"output_tokens":567}}"#;

    /// `blake3(GOLDEN_CANONICAL)` in Textform — der sprachunabhängige KAV.
    const GOLDEN_SESSION_ID: &str =
        "b3-a20e4a60acb3c7973efd344b3f27e91bf3b21211dbb64fc965bc32b4a8140bbd";

    #[test]
    fn golden_canonical_form_is_frozen() {
        // Pinnt die exakten kanonischen Bytes. Bricht bei jeder Änderung an
        // Schlüssel-Sortierung, Escaping, Whitespace oder Zahl-Formatierung —
        // und sobald ein Feld aus der Serialisierung fällt oder hinzukommt.
        assert_eq!(to_canonical_string(&sample()).unwrap(), GOLDEN_CANONICAL);
    }

    #[test]
    fn golden_session_id_is_frozen() {
        // Pinnt die ID über den vollen Weg: Struct → canonical → blake3 → Textform.
        assert_eq!(sample().id().unwrap().to_string(), GOLDEN_SESSION_ID);
    }

    #[test]
    fn golden_id_reproducible_from_canonical_text() {
        // Der Audit-Fall: ein Dritter hat *nur* den JSON-Text und blake3, keine
        // Rust-Structs. Aus genau diesen Bytes muss die eingefrorene ID fallen.
        // Bindet zugleich die zwei Konstanten aneinander — passt eine nicht mehr
        // zur anderen, war die Änderung inkonsistent.
        let id = SessionId::from_canonical_bytes(GOLDEN_CANONICAL.as_bytes());
        assert_eq!(id.to_string(), GOLDEN_SESSION_ID);
    }

    #[test]
    fn same_content_yields_same_id() {
        // Determinismus: dieselbe Session unabhängig zweimal gebaut ⇒ gleiche ID.
        assert_eq!(sample().id().unwrap(), sample().id().unwrap());
    }

    #[test]
    fn different_content_yields_different_id() {
        let a = sample();
        let mut b = sample();
        b.intent.request = "Etwas ganz anderes".into();
        assert_ne!(a.id().unwrap(), b.id().unwrap());
    }

    #[test]
    fn id_is_blake3_of_canonical_bytes() {
        // Der Vertrag: id == blake3(canonical_json(session)). Beide Wege müssen
        // dasselbe liefern.
        let s = sample();
        let bytes = to_canonical_json(&s).unwrap();
        assert_eq!(s.id().unwrap(), SessionId::from_canonical_bytes(&bytes));
    }

    #[test]
    fn display_is_prefixed_lowercase_hex() {
        let text = sample().id().unwrap().to_string();
        assert!(text.starts_with(SESSION_ID_PREFIX));
        assert_eq!(text.len(), SESSION_ID_PREFIX.len() + 64);
        let hex = &text[SESSION_ID_PREFIX.len()..];
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn from_str_roundtrips_display() {
        let id = sample().id().unwrap();
        let parsed: SessionId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn parse_accepts_uppercase_hex_digits() {
        // Lenient beim Lesen: Großbuchstaben im Hex-Teil sind erlaubt.
        let id = sample().id().unwrap();
        let hex_upper = id.to_string()[SESSION_ID_PREFIX.len()..].to_uppercase();
        let text = format!("{SESSION_ID_PREFIX}{hex_upper}");
        assert_eq!(text.parse::<SessionId>().unwrap(), id);
    }

    #[test]
    fn parse_rejects_uppercased_prefix() {
        // Das Präfix bleibt case-sensitiv; "B3-" ist keine gültige SessionId.
        let text = sample().id().unwrap().to_string().to_uppercase();
        assert_eq!(
            text.parse::<SessionId>(),
            Err(SessionIdParseError::MissingPrefix)
        );
    }

    #[test]
    fn parse_rejects_missing_prefix() {
        let bare = "0".repeat(64);
        assert_eq!(
            bare.parse::<SessionId>(),
            Err(SessionIdParseError::MissingPrefix)
        );
    }

    #[test]
    fn parse_rejects_wrong_length() {
        assert_eq!(
            "b3-abc".parse::<SessionId>(),
            Err(SessionIdParseError::WrongLength(3))
        );
    }

    #[test]
    fn parse_rejects_invalid_digit() {
        let bad = format!("b3-{}", "g".repeat(64));
        assert_eq!(
            bad.parse::<SessionId>(),
            Err(SessionIdParseError::InvalidHexDigit('g'))
        );
    }

    #[test]
    fn serializes_as_json_string() {
        let id = sample().id().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
    }

    #[test]
    fn serde_roundtrips_through_json() {
        let id = sample().id().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn works_as_json_map_key() {
        // index.json-Fall: die ID muss als String-Schlüssel round-trippen.
        use std::collections::BTreeMap;
        let id = sample().id().unwrap();
        let mut map = BTreeMap::new();
        map.insert(id, 1u32);
        let json = serde_json::to_string(&map).unwrap();
        assert_eq!(json, format!("{{\"{id}\":1}}"));
        let back: BTreeMap<SessionId, u32> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.get(&id), Some(&1));
    }

    #[test]
    fn debug_shows_textform() {
        let id = sample().id().unwrap();
        assert_eq!(format!("{id:?}"), format!("SessionId({id})"));
    }

    // Regenerator für die eingefrorenen Golden-Werte oben (`GOLDEN_CANONICAL`,
    // `GOLDEN_SESSION_ID`). Läuft nicht im normalen Testlauf (Grün-bleiben-
    // Prinzip), sondern druckt bei
    //   cargo test -p minds-core -- --ignored --nocapture reference_vector
    // die aktuelle kanonische Form und SessionId. Bei einem bewussten Schema-
    // oder Kanonisierungs-Bruch (Versions-Bump) hiermit neu erzeugen und die
    // GOLDEN_*-Konstanten ersetzen. Sprachunabhängig: ein Dritter (Python, Go)
    // muss exakt denselben Hash reproduzieren.
    #[test]
    #[ignore = "Regenerator; Werte eingefroren in GOLDEN_CANONICAL / GOLDEN_SESSION_ID"]
    fn reference_vector() {
        let s = sample();
        println!("canonical  = {}", to_canonical_string(&s).unwrap());
        println!("session_id = {}", s.id().unwrap());
    }
}
