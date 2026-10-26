//! Quittung des Witness: Kanonisierung, BLAKE3-Hash, Ed25519-Signatur und
//! Verifikation. Die kanonische Form ist `serde_json::to_vec` über Structs mit
//! alphabetisch deklarierten Feldern; für dieses Schema ist das byteidentisch
//! mit RFC 8785. Das Modul erzeugt keine Schlüssel und kennt keine
//! Schlüsselverwaltung: der Signierschlüssel kommt als fester Seed von außen.

use std::fmt;

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::RECEIPT_SCHEMA;
use crate::finding::{Coverage, PolicyResult, PushInfo, SealStatus};
use crate::ids::{CommitId, HashStr, KeyStr, SigStr, Timestamp};
use crate::push::{Pusher, ServerInfo};

/// Identität des Witness, die jede Quittung trägt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitnessIdentity {
    /// Hash des Witness-Binaries.
    pub binary: HashStr,
    /// Public Key des Signierschlüssels.
    pub key: KeyStr,
}

/// Unsignierter Inhalt einer Quittung; seine kanonische Form ist die
/// Grundlage von Receipt-Id und Signatur.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptBody {
    /// Geprüfter Commit.
    pub commit: CommitId,
    /// Abdeckung.
    pub coverage: Coverage,
    /// Policy-Ergebnis.
    pub policy: PolicyResult,
    /// Ref-Update.
    pub push: PushInfo,
    /// Pusher.
    pub pusher: Pusher,
    /// Repository-Kennung.
    pub repo: String,
    /// Immer `RECEIPT_SCHEMA`.
    pub schema: String,
    /// Seal-Angaben.
    pub seal: SealStatus,
    /// Serverangaben.
    pub server: ServerInfo,
    /// Witness-Identität.
    pub witness: WitnessIdentity,
}

/// Signierte Quittung: alle Felder des `ReceiptBody` plus `signature`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// Geprüfter Commit.
    pub commit: CommitId,
    /// Abdeckung.
    pub coverage: Coverage,
    /// Policy-Ergebnis.
    pub policy: PolicyResult,
    /// Ref-Update.
    pub push: PushInfo,
    /// Pusher.
    pub pusher: Pusher,
    /// Repository-Kennung.
    pub repo: String,
    /// Schema-Kennung.
    pub schema: String,
    /// Seal-Angaben.
    pub seal: SealStatus,
    /// Serverangaben.
    pub server: ServerInfo,
    /// Ed25519-Signatur über den BLAKE3-Digest der kanonischen Body-Bytes.
    pub signature: SigStr,
    /// Witness-Identität.
    pub witness: WitnessIdentity,
}

/// Signierschlüssel des Witness. Einzige Konstruktion ist `from_seed`; der
/// Seed verlässt den Typ nie.
pub struct Signer {
    key: SigningKey,
    valid_from: Timestamp,
}

impl Signer {
    /// `valid_from` ist der Beginn der Gültigkeit dieses Schlüssels; der Adapter liest ihn aus der Schlüsseldatei (Schritt 3), Tests setzen ihn fest.
    pub fn from_seed(seed: [u8; 32], valid_from: Timestamp) -> Self {
        Self {
            key: SigningKey::from_bytes(&seed),
            valid_from,
        }
    }

    /// Public Key als Schlüssel-String.
    pub fn public_key(&self) -> KeyStr {
        KeyStr::from_key_bytes(&self.key.verifying_key().to_bytes())
    }

    /// Beginn der Gültigkeit dieses Schlüssels.
    pub fn valid_from(&self) -> &Timestamp {
        &self.valid_from
    }
}

impl fmt::Debug for Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Ausschließlich der Public Key; der Seed erscheint nie.
        f.debug_struct("Signer")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}

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

/// Kanonische Bytes eines Werts: `serde_json::to_vec` ohne Whitespace, Felder
/// in Deklarationsreihenfolge.
pub fn canonical_bytes<T: Serialize>(value: &T) -> Vec<u8> {
    // Kein Typ im Crate ist nicht serialisierbar (keine Maps mit
    // Nicht-String-Schlüsseln, keine Floats); ein Fehler wäre ein
    // Programmierfehler.
    serde_json::to_vec(value).expect("alle Quittungstypen sind serialisierbar")
}

/// Receipt-Id: `HashStr::of_bytes(canonical_bytes(body))`, die Content-Adresse
/// des Befunds.
pub fn receipt_id(body: &ReceiptBody) -> HashStr {
    HashStr::of_bytes(&canonical_bytes(body))
}

impl ReceiptBody {
    /// Signiert den BLAKE3-Digest der kanonischen Bytes (genau 32 Byte).
    pub fn sign(self, signer: &Signer) -> Receipt {
        let digest = blake3::hash(&canonical_bytes(&self));
        let sig = signer.key.sign(digest.as_bytes());
        let signature = SigStr::from_sig_bytes(&sig.to_bytes());
        Receipt {
            commit: self.commit,
            coverage: self.coverage,
            policy: self.policy,
            push: self.push,
            pusher: self.pusher,
            repo: self.repo,
            schema: self.schema,
            seal: self.seal,
            server: self.server,
            signature,
            witness: self.witness,
        }
    }
}

impl Receipt {
    /// Receipt-Id dieser Quittung (`receipt_id(&self.body())`).
    pub fn id(&self) -> HashStr {
        receipt_id(&self.body())
    }

    /// Der unsignierte Inhalt.
    pub fn body(&self) -> ReceiptBody {
        ReceiptBody {
            commit: self.commit.clone(),
            coverage: self.coverage.clone(),
            policy: self.policy.clone(),
            push: self.push.clone(),
            pusher: self.pusher.clone(),
            repo: self.repo.clone(),
            schema: self.schema.clone(),
            seal: self.seal.clone(),
            server: self.server.clone(),
            witness: self.witness.clone(),
        }
    }

    /// Kanonische Bytes der Quittung.
    pub fn to_bytes(&self) -> Vec<u8> {
        canonical_bytes(self)
    }

    /// Liest eine Quittung; jede Abweichung von Schema-Form oder Feldern ist
    /// `VerifyError::Malformed`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Receipt, VerifyError> {
        serde_json::from_slice(bytes).map_err(|e| VerifyError::Malformed(e.to_string()))
    }
}

/// Ergebnis einer erfolgreichen Verifikation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedReceipt {
    /// Die geprüfte Quittung.
    pub receipt: Receipt,
    /// Der Schlüsseleintrag, gegen den geprüft wurde.
    pub key: PublicKeyEntry,
}

/// Grund, aus dem eine Quittung nicht verifiziert.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    /// Bytes sind keine gültige Quittung.
    #[error("Quittung nicht lesbar: {0}")]
    Malformed(String),
    /// Schema ist nicht `RECEIPT_SCHEMA`.
    #[error("unbekanntes Schema: {0}")]
    UnknownSchema(String),
    /// Schlüssel der Quittung fehlt in der Liste.
    #[error("Schlüssel nicht in der Liste: {0}")]
    UnknownKey(String),
    /// Kein Listeneintrag des Schlüssels ist zum Serverzeitpunkt gültig.
    #[error("Schlüssel zum Zeitpunkt {time} nicht gültig")]
    KeyNotValidAt {
        /// Serverzeit der Quittung.
        time: String,
    },
    /// Signatur passt nicht.
    #[error("Signatur ungültig")]
    BadSignature,
    /// Bytes sind nicht die kanonische Form der Quittung.
    #[error("Quittung nicht kanonisch")]
    NotCanonical,
}

/// Verifiziert Quittungsbytes gegen eine Schlüsselliste (Abschnitt 9,
/// Schritte 1 bis 7; erster Fehler gewinnt).
pub fn verify_receipt(
    bytes: &[u8],
    keys: &[PublicKeyEntry],
) -> Result<VerifiedReceipt, VerifyError> {
    let receipt = Receipt::from_bytes(bytes)?;
    if receipt.schema != RECEIPT_SCHEMA {
        return Err(VerifyError::UnknownSchema(receipt.schema));
    }
    if receipt.to_bytes() != bytes {
        return Err(VerifyError::NotCanonical);
    }
    if !keys.iter().any(|k| k.key == receipt.witness.key) {
        return Err(VerifyError::UnknownKey(
            receipt.witness.key.as_str().to_owned(),
        ));
    }
    let time = &receipt.server.time;
    let entry = keys
        .iter()
        .filter(|k| k.key == receipt.witness.key)
        .find(|k| k.valid_from <= *time && k.valid_until.as_ref().is_none_or(|u| time < u))
        .ok_or_else(|| VerifyError::KeyNotValidAt {
            time: time.as_str().to_owned(),
        })?
        .clone();
    // SPEC-GAP: Ein formal gültiger Schlüssel-String, der kein Punkt auf der
    // Kurve ist, kann keine Signatur tragen; er ergibt `BadSignature`.
    let verifying =
        VerifyingKey::from_bytes(&entry.key.to_bytes()).map_err(|_| VerifyError::BadSignature)?;
    let sig = Signature::from_bytes(&receipt.signature.to_bytes());
    let digest = blake3::hash(&canonical_bytes(&receipt.body()));
    verifying
        .verify_strict(digest.as_bytes(), &sig)
        .map_err(|_| VerifyError::BadSignature)?;
    Ok(VerifiedReceipt {
        receipt,
        key: entry,
    })
}

#[cfg(test)]
mod tests {
    use super::canonical_bytes;

    /// RFC 8785 übernimmt die Kurzformen aus ECMAScript `JSON.stringify`:
    /// `\b \t \n \f \r` statt `\u00XX`; alle übrigen Steuerzeichen als `\u00xx`
    /// in Kleinbuchstaben. serde_json schreibt genau das.
    #[test]
    fn short_escapes_match_jcs() {
        let s = String::from_utf8(canonical_bytes(&"\u{8}\t\n\u{c}\r\u{1}\u{1b}")).unwrap();
        assert_eq!(s, r#""\b\t\n\f\r\u0001\u001b""#);
    }
}
