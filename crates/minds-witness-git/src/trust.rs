//! Vertrauen in Signaturen unter `minds-witness` (Spec S2 ADR-S2-002,
//! §8.1, §8.2 Schritt 4, §8.3, §9 Trust-Datei-Regeln).
//!
//! Die Signer-Datei wird einmal gelesen und als privater Snapshot für alle
//! Prozesse verwendet. Beim Laden gelten die Trust-Datei-Regeln; danach
//! bestimmt eine Rust-Vorauswahl ohne Prozess, welcher Principal eine
//! Signatur überhaupt tragen kann, und genau ein `ssh-keygen -Y verify -I
//! <principal>` prüft sie. `find-principals` wird nicht verwendet. Das Modul
//! gibt nie Inhalte von Payload oder Signatur in Fehlern aus.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use minds_attest::{NS_WITNESS, principal_lines, public_key_blob, restricted_to};

use crate::budget::{Budget, BudgetError};

/// Präfix getrennter Witness-Principals (ADR-S2-002).
pub const SEPARATED_PREFIX: &str = "minds-witness-user@";
/// Höchstgröße einer armierten Signatur.
pub const MAX_SIGNATURE_BYTES: usize = 8 * 1024;
/// Höchstgröße der Signer-Datei.
pub const MAX_SIGNERS_BYTES: u64 = 1024 * 1024;

/// Warum die Signer-Datei oder die Trust-Konfiguration nicht taugt. Ein
/// Konfigurationsfehler: Das Hook-Binary endet damit in beiden Modi mit
/// Exit 1 (Spec S2 §9).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrustFileError {
    /// Datei nicht lesbar, keine reguläre Datei oder kein UTF-8.
    #[error("signers file unreadable")]
    Unreadable,
    /// Datei größer als [`MAX_SIGNERS_BYTES`].
    #[error("signers file too large")]
    TooLarge,
    /// Snapshot konnte nicht angelegt werden.
    #[error("signers snapshot failed")]
    Snapshot,
    /// `allowed_witness_principals` ist leer.
    #[error("no allowed witness principals")]
    NoPrincipals,
    /// Regel 1: Nicht jede Zeile des Principals bzw. seiner Schlüssel ist
    /// auf genau `namespaces="minds-witness"` beschränkt (oder er fehlt).
    #[error("principal {0} is not restricted to minds-witness")]
    NotRestricted(String),
    /// Regel 2: `valid-after`/`valid-before` in einer Zeile aus Regel 1.
    #[error("validity options are not supported for {0}")]
    ValidityOption(String),
    /// Regel 3: Muster oder nicht genau ein erlaubter Principal in einer
    /// Zeile aus Regel 1.
    #[error("line for {0} must name exactly one allowed principal and no pattern")]
    PrincipalField(String),
    /// Regel 3: derselbe Schlüssel in mehr als einer Zeile aus Regel 1.
    #[error("a witness key appears in more than one line")]
    DuplicateKey,
    /// Regel 4: Zeile aus Regel 1 nicht lesbar (kein Schlüssel).
    #[error("line for {0} is not readable")]
    Unparsable(String),
}

/// Ergebnis einer Signaturprüfung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigVerdict {
    /// Gültig und von einem anerkannten Principal.
    Accepted {
        /// Der Principal der Zeile, die die Signatur trägt.
        principal: String,
    },
    /// Der Signierer ist nicht anerkannt (Vorfilter, ohne Prozess).
    Untrusted,
    /// Signatur ungültig, nicht lesbar, oder ihr Schlüssel passt nicht zum
    /// genannten Schlüssel.
    BadSignature,
}

/// Technischer Fehler einer Prüfung; im Kern `Internal`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// Ein Budget ist erschöpft.
    #[error(transparent)]
    Budget(#[from] BudgetError),
    /// `ssh-keygen` nicht startbar, Timeout oder Abbruch.
    #[error("ssh-keygen failed")]
    Process,
}

/// Trust-Konfiguration eines Hook-Aufrufs: Snapshot der Signer-Datei,
/// Programm, Principals und die vorab bestimmte Zuordnung Schlüssel →
/// Principal.
#[derive(Debug)]
pub struct TrustConfig {
    snapshot: tempfile::NamedTempFile,
    program: PathBuf,
    require_separated_witness: bool,
    timeout: Duration,
    keys: Vec<(Vec<u8>, String)>,
}

impl TrustConfig {
    /// Liest die Signer-Datei einmal (reguläre Datei, höchstens
    /// [`MAX_SIGNERS_BYTES`]) und baut die Konfiguration nach
    /// [`TrustConfig::from_text`].
    pub fn load(
        signers: &Path,
        program: &Path,
        allowed_witness_principals: &[String],
        require_separated_witness: bool,
        timeout: Duration,
    ) -> Result<Self, TrustFileError> {
        use std::io::Read;
        let file = std::fs::File::open(signers).map_err(|_| TrustFileError::Unreadable)?;
        if !file.metadata().is_ok_and(|m| m.is_file()) {
            return Err(TrustFileError::Unreadable);
        }
        let mut bytes = Vec::new();
        file.take(MAX_SIGNERS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| TrustFileError::Unreadable)?;
        if bytes.len() as u64 > MAX_SIGNERS_BYTES {
            return Err(TrustFileError::TooLarge);
        }
        let text = String::from_utf8(bytes).map_err(|_| TrustFileError::Unreadable)?;
        Self::from_text(
            &text,
            program,
            allowed_witness_principals,
            require_separated_witness,
            timeout,
        )
    }

    /// Prüft die Trust-Datei-Regeln (Spec S2 §9) auf genau diesem Text und
    /// legt ihn als privaten Snapshot ab, den jede Prüfung verwendet.
    pub fn from_text(
        text: &str,
        program: &Path,
        allowed_witness_principals: &[String],
        require_separated_witness: bool,
        timeout: Duration,
    ) -> Result<Self, TrustFileError> {
        let keys = check_rules(text, allowed_witness_principals)?;
        let mut snapshot = tempfile::Builder::new()
            .prefix("minds-signers-")
            .tempfile()
            .map_err(|_| TrustFileError::Snapshot)?;
        snapshot
            .write_all(text.as_bytes())
            .and_then(|()| snapshot.flush())
            .map_err(|_| TrustFileError::Snapshot)?;
        Ok(Self {
            snapshot,
            program: program.to_owned(),
            require_separated_witness,
            timeout,
            keys,
        })
    }

    /// Der Principal, dessen beschränkte Zeile genau diesen Schlüssel
    /// (SSH-Wire-Format) trägt, sofern er anerkannt werden kann.
    fn principal_for(&self, key: &[u8]) -> Option<&str> {
        self.keys
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, p)| p.as_str())
            .filter(|p| !self.require_separated_witness || p.starts_with(SEPARATED_PREFIX))
    }
}

/// Trust-Datei-Regeln 1 bis 4; liefert die Zuordnung Schlüssel → Principal.
fn check_rules(
    text: &str,
    principals: &[String],
) -> Result<Vec<(Vec<u8>, String)>, TrustFileError> {
    if principals.is_empty() {
        return Err(TrustFileError::NoPrincipals);
    }
    let mut by_line: Vec<(usize, Vec<u8>, String)> = Vec::new();
    for p in principals {
        if !restricted_to(text, p, NS_WITNESS) {
            return Err(TrustFileError::NotRestricted(p.clone()));
        }
        for line in principal_lines(text, p) {
            if line.options.as_deref().is_some_and(|o| {
                o.split(',').any(|o| {
                    let o = o.to_ascii_lowercase();
                    o.starts_with("valid-after") || o.starts_with("valid-before")
                })
            }) {
                return Err(TrustFileError::ValidityOption(p.clone()));
            }
            let blob = line
                .key
                .as_deref()
                .and_then(public_key_blob)
                .ok_or_else(|| TrustFileError::Unparsable(p.clone()))?;
            let named: Vec<&str> = line
                .principals
                .split(',')
                .filter(|name| principals.iter().any(|q| q == name))
                .collect();
            if line.pattern || named.len() != 1 {
                return Err(TrustFileError::PrincipalField(p.clone()));
            }
            if !by_line.iter().any(|(i, _, _)| *i == line.index) {
                by_line.push((line.index, blob, named[0].to_owned()));
            }
        }
    }
    for (i, (index, blob, _)) in by_line.iter().enumerate() {
        if by_line[i + 1..]
            .iter()
            .any(|(j, b, _)| j != index && b == blob)
        {
            return Err(TrustFileError::DuplicateKey);
        }
    }
    Ok(by_line.into_iter().map(|(_, b, p)| (b, p)).collect())
}

/// Prüft Signaturen unter `minds-witness` mit gemeinsamem Budget und
/// Cache eines Hook-Aufrufs.
#[derive(Debug)]
pub struct Verifier<'a> {
    trust: &'a TrustConfig,
    budget: &'a Budget,
    cache: RefCell<HashMap<[u8; 32], bool>>,
}

impl<'a> Verifier<'a> {
    /// Ein Prüfer über `trust`, der Prozesse aus `budget` verbraucht.
    pub fn new(trust: &'a TrustConfig, budget: &'a Budget) -> Self {
        Self {
            trust,
            budget,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Prüft `signature` über `payload` unter `minds-witness`
    /// (Spec S2 §8.2 Schritt 4). Ist `claimed_key` gesetzt (das Feld `key`
    /// einer Bindung), muss die Signatur genau diesen Schlüssel tragen.
    /// Startet höchstens einen Prozess; eine gleiche Prüfung im selben
    /// Aufruf startet keinen weiteren.
    pub fn verify(
        &self,
        payload: &[u8],
        signature: &[u8],
        claimed_key: Option<&str>,
    ) -> Result<SigVerdict, VerifyError> {
        let (Ok(payload_text), Ok(armored)) =
            (std::str::from_utf8(payload), std::str::from_utf8(signature))
        else {
            return Ok(SigVerdict::BadSignature);
        };
        if signature.len() > MAX_SIGNATURE_BYTES {
            return Ok(SigVerdict::BadSignature);
        }
        let Some(signer) = minds_attest::signature_public_key(armored) else {
            return Ok(SigVerdict::BadSignature);
        };
        if let Some(claimed) = claimed_key
            && public_key_blob(claimed).as_deref() != Some(signer.as_slice())
        {
            return Ok(SigVerdict::BadSignature);
        }
        let Some(principal) = self.trust.principal_for(&signer) else {
            return Ok(SigVerdict::Untrusted);
        };
        let key = cache_key(payload, signature);
        if let Some(valid) = self.cache.borrow().get(&key) {
            return Ok(verdict(*valid, principal));
        }
        let timeout = self.budget.take_process(self.trust.timeout)?;
        let valid = minds_attest::ssh_verify_ns_within(
            &self.trust.program,
            payload_text,
            armored,
            self.trust.snapshot.path(),
            principal,
            NS_WITNESS,
            timeout,
        )
        .map_err(|_| VerifyError::Process)?;
        self.cache.borrow_mut().insert(key, valid);
        Ok(verdict(valid, principal))
    }
}

fn verdict(valid: bool, principal: &str) -> SigVerdict {
    if valid {
        SigVerdict::Accepted {
            principal: principal.to_owned(),
        }
    } else {
        SigVerdict::BadSignature
    }
}

/// Spec S2 §8.3: blake3 über Typ, Namespace, Bytes und Signatur mit
/// u64-LE-Längen.
fn cache_key(payload: &[u8], signature: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for part in [
        b"ssh-verify".as_slice(),
        NS_WITNESS.as_bytes(),
        payload,
        signature,
    ] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part);
    }
    *h.finalize().as_bytes()
}
