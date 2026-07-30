//! Die **Change-Id**: eine stabile Identität für *diese Änderung*, unabhängig von
//! *dieser Version dieser Änderung*.
//!
//! Der Commit-Hash ist keine Identität für eine logische Änderung: `rebase`,
//! `squash`, `amend` und `cherry-pick` erzeugen einen neuen Hash für dieselbe
//! Absicht. Die Change-Id (wie bei Gerrit und Jujutsu) trennt beides — sie wird
//! **einmal** erzeugt und über all diese Operationen hinweg **mitgeführt**, weil
//! sie als Trailer in der Commit-Message steht (siehe [`crate::trailer`]) und die
//! Message diese Operationen überlebt.
//!
//! # Format: Gerrit-kompatibel
//!
//! `I` gefolgt von 40 Hex-Zeichen (`I<40 hex>`) — dieselbe Form, die Gerrits
//! `commit-msg`-Hook erzeugt. So greifen vorhandene Erwartungen und Regexe
//! (`I[0-9a-f]{40}`) ohne Anpassung.
//!
//! **Lesen tolerant, Schreiben kanonisch** (wie [`SessionId`](crate::SessionId)
//! und [`ContentHash`](crate::ContentHash)): [`FromStr`] akzeptiert Groß-/Klein-
//! schreibung und ein fehlendes `I`-Präfix; [`fmt::Display`] gibt ausschließlich
//! `I` + 40 Kleinbuchstaben-Hex aus.

use std::fmt;
use std::str::FromStr;

/// Präfix der kanonischen Textform — ein `I` wie bei Gerrit.
pub const CHANGE_ID_PREFIX: &str = "I";

/// Länge des Hex-Anteils: 20 Byte, wie ein SHA-1.
const HEX_LEN: usize = 40;

/// Eine stabile Änderungs-Identität, `I<40 hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChangeId(String);

impl ChangeId {
    /// Aus 20 Rohbytes (die übliche SHA-1-Länge) — für die Generierung aus
    /// Entropie in der CLI.
    pub fn from_bytes(digest: [u8; 20]) -> Self {
        let mut s = String::with_capacity(CHANGE_ID_PREFIX.len() + HEX_LEN);
        s.push_str(CHANGE_ID_PREFIX);
        for byte in digest {
            s.push(char::from_digit((byte >> 4) as u32, 16).expect("nibble"));
            s.push(char::from_digit((byte & 0x0f) as u32, 16).expect("nibble"));
        }
        Self(s)
    }

    /// Die kanonische Textform mit Präfix.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Der Hex-Anteil ohne `I`.
    pub fn hex(&self) -> &str {
        &self.0[CHANGE_ID_PREFIX.len()..]
    }
}

impl fmt::Display for ChangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ChangeId {
    type Err = ChangeIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        // `I`/`i`-Präfix tolerant abstreifen; bare Hex bleibt unberührt (Hex
        // beginnt nie mit einem alleinstehenden `i`, das kein Hex-Zeichen ist…
        // doch `i` ist keins — genau deshalb ist die Unterscheidung sicher).
        let hex = match s.strip_prefix(['I', 'i']) {
            Some(rest) => rest,
            None => s,
        };
        if hex.len() != HEX_LEN {
            return Err(ChangeIdParseError::Length(hex.len()));
        }
        if let Some(c) = hex.chars().find(|c| !c.is_ascii_hexdigit()) {
            return Err(ChangeIdParseError::NotHex(c));
        }
        Ok(Self(format!(
            "{CHANGE_ID_PREFIX}{}",
            hex.to_ascii_lowercase()
        )))
    }
}

/// Warum eine Zeichenkette keine [`ChangeId`] ist.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChangeIdParseError {
    #[error("Change-Id braucht {HEX_LEN} Hex-Zeichen, hat aber {0}")]
    Length(usize),

    #[error("Change-Id enthält ein Nicht-Hex-Zeichen: {0:?}")]
    NotHex(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bytes_writes_canonical_form() {
        let id = ChangeId::from_bytes([0xab; 20]);
        assert_eq!(id.to_string(), format!("I{}", "ab".repeat(20)));
        assert_eq!(id.hex().len(), HEX_LEN);
    }

    #[test]
    fn reads_tolerantly_writes_canonically() {
        let upper = format!("I{}", "AB".repeat(20));
        let lower_i = format!("i{}", "ab".repeat(20));
        let bare = "ab".repeat(20);

        let a: ChangeId = upper.parse().unwrap();
        let b: ChangeId = lower_i.parse().unwrap();
        let c: ChangeId = bare.parse().unwrap();

        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(a.to_string(), format!("I{}", "ab".repeat(20)));
    }

    #[test]
    fn rejects_wrong_shape() {
        assert!(matches!(
            "Iabc".parse::<ChangeId>(),
            Err(ChangeIdParseError::Length(3))
        ));
        assert!(matches!(
            format!("I{}", "zz".repeat(20)).parse::<ChangeId>(),
            Err(ChangeIdParseError::NotHex('z'))
        ));
    }

    #[test]
    fn roundtrips_through_parse() {
        let id = ChangeId::from_bytes([0x12; 20]);
        assert_eq!(id.to_string().parse::<ChangeId>().unwrap(), id);
    }
}
