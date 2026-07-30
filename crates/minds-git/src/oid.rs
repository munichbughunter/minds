//! Die Identitäten von Git-Objekten: [`CommitId`], [`TreeId`], [`BlobId`].
//!
//! Dünne Newtypes über `gix::ObjectId`. Drei Gründe, warum es sie gibt und
//! wir nicht einfach gix' Typ durchreichen:
//!
//! 1. **Die Fassade bleibt dicht.** Wie beim Fehlertyp (siehe
//!    `error.rs`) soll kein gix-Typ in der Signatur von `minds-store`
//!    oder `minds-cli` auftauchen.
//! 2. **Eine kanonische Textform.** Kleingeschriebenes Hex, volle Länge —
//!    genau das, was später in `index.json` landet. Beim *Lesen* tolerant
//!    (Großbuchstaben erlaubt), beim *Schreiben* kanonisch: dieselbe Regel wie
//!    bei `minds_core::SessionId` und beim Trailer.
//! 3. **Serde ohne Umwege.** Der Reader-Index bildet `SessionId → {commit, …}`
//!    ab; die Commit-Id muss also als JSON-String round-trippen.
//!
//! # Warum drei Typen und nicht einer
//!
//! Auf Git-Ebene sind Commit, Tree und Blob derselbe 20-Byte-Hash — nichts am
//! Hash verrät, worauf er zeigt. Genau das ist das Problem: Ein Blob-Hash, der
//! versehentlich als Baum an einen Commit gehängt wird, ist ein Fehler, den Git
//! erst beim Lesen bemerkt (und `minds-store` womöglich nie). Drei Typen machen
//! daraus einen Compile-Fehler. Die Kosten sind ein paar Zeilen Boilerplate,
//! der Nutzen ist eine ganze Fehlerklasse.
//!
//! Nur [`CommitId`] trägt Serde und `FromStr`: Sie steht in `index.json` und
//! auf der Kommandozeile (`minds show <sha>`). [`TreeId`] und [`BlobId`] sind
//! reine Zwischenwährung zwischen `minds-git` und `minds-store` — sie werden
//! nie persistiert, brauchen also auch keine parsbare Textform. Sobald doch,
//! ist es ein bewusster Schritt und kein Versehen.
//!
//! # Verwechslungsgefahr, absichtlich ausgeschlossen
//!
//! `CommitId` und `minds_core::SessionId` sind beides Hashes und beides
//! 32-Byte-nahe Hex-Strings — aber sie zeigen auf grundverschiedene Dinge und
//! sind unterschiedlich stabil: Die `SessionId` überlebt Rebase und Squash
//! (sie hängt am Inhalt), die `CommitId` **nicht** (sie hängt am Commit-Objekt,
//! das jeder History-Rewrite ersetzt). Genau deshalb steht im Trailer die
//! `SessionId` und nicht die `CommitId`. Zwei getrennte Typen machen die
//! Verwechslung zu einem Compile-Fehler statt zu einem stillen Bug.
//!
//! Der Hash-Algorithmus bleibt gix überlassen: SHA-1 heute, SHA-256 in
//! SHA-256-Repos. [`CommitId`] speichert, was das Repo liefert, und formatiert
//! es in voller Länge — Kürzen ist Präsentation, nie Identität.

use std::fmt;
use std::str::FromStr;

use gix::ObjectId;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Anzahl Hex-Zeichen der Kurzform. Gits Default für `--abbrev`; rein für
/// Menschen.
const SHORT_LEN: usize = 7;

/// Die Identität eines Commits — sein Git-Objekt-Hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommitId(ObjectId);

impl CommitId {
    /// Die abgekürzte Textform (7 Hex-Zeichen), wie `git log --oneline` sie
    /// zeigt.
    ///
    /// **Nur für Ausgabe an Menschen.** Nie als Schlüssel, nie in eine Datei,
    /// nie in einen Vergleich: Kurzformen sind repo-abhängig mehrdeutig, und
    /// eine mehrdeutige Id in einem Audit-Record wäre ein Widerspruch in sich.
    pub fn short(&self) -> String {
        self.0.to_hex_with_len(SHORT_LEN).to_string()
    }

    /// Der rohe gix-Hash. Crate-intern, damit der Typ die Fassade nicht
    /// durchlöchert.
    pub(crate) fn to_gix(self) -> ObjectId {
        self.0
    }

    /// Übernimmt einen gix-Hash. Crate-intern; von außen entsteht eine
    /// `CommitId` nur aus dem Repository oder durch Parsen ihrer Textform.
    pub(crate) fn from_gix(id: ObjectId) -> Self {
        Self(id)
    }
}

/// Volle Länge, Kleinschreibung — die kanonische Textform.
impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.to_hex())
    }
}

/// Zeigt die lesbare Textform statt eines Byte-Arrays — hält Testausgaben und
/// Logs verständlich (wie bei `minds_core::SessionId`).
impl fmt::Debug for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CommitId({self})")
    }
}

/// Die Identität eines Baums — ein Verzeichnis in Gits Objektmodell.
///
/// Ein Baum listet Namen mit Modus und Ziel-Hash; unter `refs/minds/context`
/// ist er das Verzeichnis, in dem `sessions/b3/<hash>.json` und `index.json`
/// liegen.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TreeId(ObjectId);

impl TreeId {
    pub(crate) fn to_gix(self) -> ObjectId {
        self.0
    }

    pub(crate) fn from_gix(id: ObjectId) -> Self {
        Self(id)
    }
}

impl fmt::Display for TreeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.to_hex())
    }
}

impl fmt::Debug for TreeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TreeId({self})")
    }
}

/// Die Identität eines Blobs — der reine Inhalt einer Datei, ohne Namen.
///
/// Dass der Name nicht Teil des Blobs ist, ist genau der Grund, warum Gits
/// Dedup gratis funktioniert: Dieselbe Session unter zwei Pfaden ist **ein**
/// Objekt. Das ist die Hälfte des „idempotenten put" aus M4, ohne dass der
/// Store dafür etwas tun müsste.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobId(ObjectId);

impl BlobId {
    pub(crate) fn to_gix(self) -> ObjectId {
        self.0
    }

    pub(crate) fn from_gix(id: ObjectId) -> Self {
        Self(id)
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.to_hex())
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobId({self})")
    }
}

/// Der Text ließ sich nicht als Git-Objekt-Hash lesen.
///
/// Bewusst ein eigener Typ statt gix' Decode-Fehler: Er ist Teil der
/// öffentlichen API (jemand tippt `minds show <sha>` auf der Kommandozeile) und
/// soll sich nicht mit gix bewegen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("kein gültiger Git-Objekt-Hash: {input:?}")]
pub struct CommitIdParseError {
    /// Der abgewiesene Text. Ein Commit-Hash ist öffentlich — hier steht nie
    /// etwas Sensibles.
    pub input: String,
}

impl CommitIdParseError {
    fn new(input: &str) -> Self {
        Self {
            input: input.to_owned(),
        }
    }
}

impl FromStr for CommitId {
    type Err = CommitIdParseError;

    /// Tolerant beim Lesen: Groß- und Kleinschreibung sind erlaubt, die Länge
    /// muss aber vollständig sein (40 bzw. 64 Zeichen). Kurzformen werden
    /// **nicht** akzeptiert — deren Auflösung braucht ein Repository und ist
    /// mehrdeutig; das gehört an die Stelle, die das Repo kennt, nicht in einen
    /// `FromStr`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lowered = s.to_ascii_lowercase();
        ObjectId::from_hex(lowered.as_bytes())
            .map(Self)
            .map_err(|_| CommitIdParseError::new(s))
    }
}

impl Serialize for CommitId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CommitId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein gültiger SHA-1-Hash in Textform.
    const HEX: &str = "1e4f0b6a8c2d3e5f7a9b0c1d2e3f4a5b6c7d8e9f";

    #[test]
    fn display_is_full_lowercase_hex() {
        let id: CommitId = HEX.parse().unwrap();
        assert_eq!(id.to_string(), HEX);
    }

    #[test]
    fn from_str_roundtrips_display() {
        let id: CommitId = HEX.parse().unwrap();
        assert_eq!(id.to_string().parse::<CommitId>().unwrap(), id);
    }

    #[test]
    fn parse_accepts_uppercase_and_normalises_it() {
        // Lesen tolerant, Schreiben kanonisch.
        let id: CommitId = HEX.to_uppercase().parse().unwrap();
        assert_eq!(id.to_string(), HEX);
    }

    #[test]
    fn parse_rejects_abbreviated_hash() {
        // Kurzformen sind mehrdeutig und brauchen ein Repository — hier nicht.
        assert!("1e4f0b6".parse::<CommitId>().is_err());
    }

    #[test]
    fn parse_rejects_non_hex() {
        let bad = "g".repeat(40);
        let err = bad.parse::<CommitId>().unwrap_err();
        assert_eq!(err.input, bad);
    }

    #[test]
    fn short_is_seven_characters_of_the_full_form() {
        let id: CommitId = HEX.parse().unwrap();
        assert_eq!(id.short(), &HEX[..SHORT_LEN]);
    }

    #[test]
    fn serializes_as_json_string() {
        let id: CommitId = HEX.parse().unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{HEX}\""));
    }

    #[test]
    fn serde_roundtrips_through_json() {
        let id: CommitId = HEX.parse().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<CommitId>(&json).unwrap(), id);
    }

    #[test]
    fn debug_shows_textform() {
        let id: CommitId = HEX.parse().unwrap();
        assert_eq!(format!("{id:?}"), format!("CommitId({HEX})"));
    }
}
