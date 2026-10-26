//! Quittung des Witness: Kanonisierung, BLAKE3-Hash, Ed25519-Signatur und
//! Verifikation. Die kanonische Form ist `serde_json::to_vec` über Structs mit
//! alphabetisch deklarierten Feldern; für dieses Schema ist das byteidentisch
//! mit RFC 8785. Das Modul erzeugt keine Schlüssel und kennt keine
//! Schlüsselverwaltung: der Signierschlüssel kommt als fester Seed von außen.

use serde::{Deserialize, Serialize};

use crate::ids::{CommitId, HashStr, KeyStr, Timestamp};

/// Ablageschlüssel einer Quittung.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptKey {
    /// Commit der Quittung.
    pub commit: CommitId,
    /// Receipt-Id (Content-Adresse des `ReceiptBody`).
    pub id: HashStr,
}

// SPEC-GAP: `ReceiptKey` soll `PartialOrd`/`Ord` implementieren, `HashStr`
// hat laut Signatur aber kein `Ord`. Statt die Derive-Liste von `HashStr` zu
// erweitern, ordnet `ReceiptKey` manuell nach (commit, id als String). Das ist
// dieselbe Ordnung, die ein Derive ergäbe.
impl PartialOrd for ReceiptKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ReceiptKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.commit
            .cmp(&other.commit)
            .then_with(|| self.id.as_str().cmp(other.id.as_str()))
    }
}

/// Eintrag der Schlüsselliste.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKeyEntry {
    /// Public Key.
    pub key: KeyStr,
    /// Beginn der Gültigkeit (einschließlich).
    pub valid_from: Timestamp,
    /// Ende der Gültigkeit (ausschließlich), sofern zurückgezogen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<Timestamp>,
}
