//! Validierte Bezeichner: Commit-Ids, Zeitstempel, Hash-, Schlüssel- und
//! Signatur-Strings. Jeder Typ ist nur über `parse` (bzw. die validierende
//! Deserialisierung) konstruierbar, damit eine ungültige Form nirgends im
//! Kern ankommt. Das Modul prüft ausschließlich die Form, nie die
//! Plausibilität: ein formal gültiger Zeitstempel sagt nichts darüber, ob die
//! Serveruhr richtig ging.

use serde::{Deserialize, Serialize};

/// Fehler bei der Validierung eines Bezeichners.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdError {
    /// Die Länge (in Bytes) passt nicht zur erwarteten Form.
    #[error("ungültige Länge {0}")]
    BadLength(usize),
    /// Der Hex-Teil enthält ein Zeichen außerhalb von `0-9a-f`.
    #[error("kein Kleinbuchstaben-Hex")]
    NotLowerHex,
    /// Das erwartete Präfix (`blake3:` bzw. `ed25519:`) fehlt.
    #[error("falsches Präfix")]
    BadPrefix,
    /// Der String ist kein Zeitstempel der Form `YYYY-MM-DDTHH:MM:SSZ`.
    #[error("kein Zeitstempel der Form YYYY-MM-DDTHH:MM:SSZ")]
    BadTimestamp,
}

fn is_lower_hex(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Prüft `prefix` + genau `hex_len` Zeichen Kleinbuchstaben-Hex.
// SPEC-GAP: Reihenfolge Präfix, Länge, Zeichenvorrat; `BadLength` meldet die
// Länge des Hex-Teils nach dem Präfix, weil nur dieser eine feste Länge hat.
fn check_prefixed_hex(s: &str, prefix: &str, hex_len: usize) -> Result<(), IdError> {
    let rest = s.strip_prefix(prefix).ok_or(IdError::BadPrefix)?;
    if rest.len() != hex_len {
        return Err(IdError::BadLength(rest.len()));
    }
    if !is_lower_hex(rest) {
        return Err(IdError::NotLowerHex);
    }
    Ok(())
}

/// Objekt-Id eines Commits: Kleinbuchstaben-Hex der Länge 40 (SHA-1) oder 64
/// (SHA-256).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommitId(String);

impl CommitId {
    /// Akzeptiert nur Kleinbuchstaben-Hex der Länge 40 oder 64.
    pub fn parse(s: &str) -> Result<Self, IdError> {
        // SPEC-GAP: Prüfreihenfolge nicht festgelegt; zuerst Länge, dann
        // Zeichenvorrat, analog zur Reihenfolge der IdError-Varianten.
        if s.len() != 40 && s.len() != 64 {
            return Err(IdError::BadLength(s.len()));
        }
        if !is_lower_hex(s) {
            return Err(IdError::NotLowerHex);
        }
        Ok(Self(s.to_owned()))
    }

    /// Die Id als String-Slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `true`, wenn alle Zeichen `'0'` sind (Ref-Anlage bzw. -Löschung).
    pub fn is_null(&self) -> bool {
        self.0.bytes().all(|b| b == b'0')
    }

    /// Null-Id der Länge `len`; `len` muss 40 oder 64 sein, sonst
    /// `IdError::BadLength`.
    pub fn null_of_len(len: usize) -> Result<Self, IdError> {
        if len != 40 && len != 64 {
            return Err(IdError::BadLength(len));
        }
        Ok(Self("0".repeat(len)))
    }
}

impl TryFrom<String> for CommitId {
    type Error = IdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<CommitId> for String {
    fn from(v: CommitId) -> Self {
        v.0
    }
}

/// Hash-String `"blake3:"` + 64 Zeichen Kleinbuchstaben-Hex (BLAKE3-256).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HashStr(String);

impl HashStr {
    /// BLAKE3-256 über `data`, als Hash-String.
    pub fn of_bytes(data: &[u8]) -> Self {
        Self(format!(
            "blake3:{}",
            hex::encode(blake3::hash(data).as_bytes())
        ))
    }

    /// Validiert die Form `"blake3:"` + 64 Hex.
    pub fn parse(s: &str) -> Result<Self, IdError> {
        check_prefixed_hex(s, "blake3:", 64)?;
        Ok(Self(s.to_owned()))
    }

    /// Der Hash-String als String-Slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HashStr {
    type Error = IdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<HashStr> for String {
    fn from(v: HashStr) -> Self {
        v.0
    }
}

/// Schlüssel-String `"ed25519:"` + 64 Hex (32 Byte Ed25519-Public-Key).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KeyStr(String);

impl KeyStr {
    /// Validiert die Form `"ed25519:"` + 64 Hex.
    pub fn parse(s: &str) -> Result<Self, IdError> {
        check_prefixed_hex(s, "ed25519:", 64)?;
        Ok(Self(s.to_owned()))
    }

    /// Der Schlüssel-String als String-Slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Die 32 Rohbytes des Public Keys.
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        hex::decode_to_slice(&self.0["ed25519:".len()..], &mut out)
            .expect("validiert bei Konstruktion");
        out
    }

    pub(crate) fn from_key_bytes(bytes: &[u8; 32]) -> Self {
        Self(format!("ed25519:{}", hex::encode(bytes)))
    }
}

impl TryFrom<String> for KeyStr {
    type Error = IdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<KeyStr> for String {
    fn from(v: KeyStr) -> Self {
        v.0
    }
}

/// Signatur-String `"ed25519:"` + 128 Hex (64 Byte Ed25519-Signatur).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SigStr(String);

impl SigStr {
    /// Validiert die Form `"ed25519:"` + 128 Hex.
    pub fn parse(s: &str) -> Result<Self, IdError> {
        check_prefixed_hex(s, "ed25519:", 128)?;
        Ok(Self(s.to_owned()))
    }

    /// Der Signatur-String als String-Slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Die 64 Rohbytes der Signatur.
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        hex::decode_to_slice(&self.0["ed25519:".len()..], &mut out)
            .expect("validiert bei Konstruktion");
        out
    }

    pub(crate) fn from_sig_bytes(bytes: &[u8; 64]) -> Self {
        Self(format!("ed25519:{}", hex::encode(bytes)))
    }
}

impl TryFrom<String> for SigStr {
    type Error = IdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<SigStr> for String {
    fn from(v: SigStr) -> Self {
        v.0
    }
}

/// Zeitstempel der Form `YYYY-MM-DDTHH:MM:SSZ` (RFC 3339, UTC, Sekunden,
/// genau 20 Zeichen). Wegen der festen Form ist die lexikografische Ordnung
/// die chronologische.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Timestamp(String);

impl Timestamp {
    /// Validiert die Form; prüft Ziffernpositionen und die Bereiche von
    /// Monat (01–12), Tag (01–31), Stunde (00–23), Minute und Sekunde
    /// (00–59). Kalendergültigkeit (etwa 31. Februar) prüft sie bewusst nicht.
    pub fn parse(s: &str) -> Result<Self, IdError> {
        let b = s.as_bytes();
        if b.len() != 20 {
            return Err(IdError::BadTimestamp);
        }
        for (i, &c) in b.iter().enumerate() {
            let ok = match i {
                4 | 7 => c == b'-',
                10 => c == b'T',
                13 | 16 => c == b':',
                19 => c == b'Z',
                _ => c.is_ascii_digit(),
            };
            if !ok {
                return Err(IdError::BadTimestamp);
            }
        }
        let num = |i: usize| u32::from(b[i] - b'0') * 10 + u32::from(b[i + 1] - b'0');
        let (month, day, hour, minute, second) = (num(5), num(8), num(11), num(14), num(17));
        if !(1..=12).contains(&month)
            || !(1..=31).contains(&day)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return Err(IdError::BadTimestamp);
        }
        Ok(Self(s.to_owned()))
    }

    /// Der Zeitstempel als String-Slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Timestamp {
    type Error = IdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<Timestamp> for String {
    fn from(v: Timestamp) -> Self {
        v.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_form() {
        assert!(Timestamp::parse("2026-10-25T10:00:00Z").is_ok());
        assert!(Timestamp::parse("2026-13-25T10:00:00Z").is_err());
        assert!(Timestamp::parse("2026-00-25T10:00:00Z").is_err());
        assert!(Timestamp::parse("2026-10-32T10:00:00Z").is_err());
        assert!(Timestamp::parse("2026-10-25T24:00:00Z").is_err());
        assert!(Timestamp::parse("2026-10-25T10:60:00Z").is_err());
        assert!(Timestamp::parse("2026-10-25T10:00:60Z").is_err());
        assert!(Timestamp::parse("2026-10-25 10:00:00Z").is_err());
        assert!(Timestamp::parse("2026-10-25T10:00:00+").is_err());
        assert!(Timestamp::parse("2026-10-25T10:00:00.0Z").is_err());
        assert!(Timestamp::parse("2026-1a-25T10:00:00Z").is_err());
        assert!(Timestamp::parse("2026-10-25T10:00:0é").is_err());
    }

    #[test]
    fn commit_id_null() {
        assert!(CommitId::null_of_len(40).unwrap().is_null());
        assert!(CommitId::null_of_len(64).unwrap().is_null());
        assert_eq!(CommitId::null_of_len(41), Err(IdError::BadLength(41)));
        assert!(!CommitId::parse(&"1".repeat(40)).unwrap().is_null());
    }

    #[test]
    fn key_and_sig_roundtrip_bytes() {
        let k = KeyStr::from_key_bytes(&[0xab; 32]);
        assert_eq!(k.to_bytes(), [0xab; 32]);
        let s = SigStr::from_sig_bytes(&[0x01; 64]);
        assert_eq!(s.to_bytes(), [0x01; 64]);
    }
}
