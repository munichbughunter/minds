//! Signieren und Verifizieren über `ssh-keygen -Y sign/verify` (SSH-Signaturen).
//!
//! Warum `ssh-sig` und nicht sigstore/gitsign: Es ist überall da, wo `ssh` ist —
//! kein Netz, kein OIDC, air-gap-tauglich (siehe Plan-v0.2, offene Entscheidung).
//! Dasselbe Verfahren, mit dem Git SSH-Commits signiert.
//!
//! Reines Shell-Werkzeug hinter einer schmalen Naht: `minds-core` liefert den
//! kanonischen Payload ([`attestation_payload`](minds_core::attestation_payload)),
//! hier wird er signiert und geprüft.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Der ssh-sig-Namespace — trennt Minds-Signaturen von anderen ssh-sig-Domänen.
pub const NAMESPACE: &str = "minds";

/// Ob `ssh-keygen` im PATH ist.
pub fn ssh_keygen_available() -> bool {
    Command::new("ssh-keygen")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok()
}

/// Signiert `payload` mit dem SSH-Schlüssel unter `key` und gibt die armierte
/// Signatur zurück.
pub fn ssh_sign(payload: &str, key: &Path) -> Fallible<String> {
    let data = TempFile::with_contents("sign", payload.as_bytes())?;
    // ssh-keygen schreibt die Signatur nach <data>.sig.
    let output = Command::new("ssh-keygen")
        .args(["-Y", "sign", "-n", NAMESPACE, "-f"])
        .arg(key)
        .arg(&data.path)
        .stdin(Stdio::null()) // ein passphrasegeschützter Schlüssel scheitert, statt zu hängen
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "ssh-keygen sign fehlgeschlagen: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let sig_path = PathBuf::from(format!("{}.sig", data.path.display()));
    let signature = std::fs::read_to_string(&sig_path)?;
    let _ = std::fs::remove_file(&sig_path);
    Ok(signature)
}

/// Verifiziert `signature` über `payload` gegen die `allowed_signers`-Datei für
/// `identity`. `Ok(false)` heißt „Signatur ungültig" — kein Fehler, ein Ergebnis.
pub fn ssh_verify(
    payload: &str,
    signature: &str,
    signers: &Path,
    identity: &str,
) -> Fallible<bool> {
    let sig = TempFile::with_contents("verify-sig", signature.as_bytes())?;
    let mut child = Command::new("ssh-keygen")
        .args(["-Y", "verify", "-n", NAMESPACE, "-I", identity, "-f"])
        .arg(signers)
        .arg("-s")
        .arg(&sig.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    {
        // Die signierten Daten kommen über stdin; das Schließen (Drop) signalisiert
        // ssh-keygen das Ende.
        let mut stdin = child.stdin.take().ok_or("ssh-keygen: kein stdin")?;
        stdin.write_all(payload.as_bytes())?;
    }
    Ok(child.wait()?.success())
}

/// Eine Temp-Datei, die sich beim Fallenlassen selbst entfernt.
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn with_contents(tag: &str, bytes: &[u8]) -> Fallible<Self> {
        let path = unique_temp(tag);
        std::fs::write(&path, bytes)?;
        Ok(Self { path })
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn unique_temp(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("minds-{tag}-{}-{nanos}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip_and_detect_tampering() {
        // Braucht ssh-keygen; ohne wird der Test übersprungen (nicht falsch-rot).
        if !ssh_keygen_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id");
        let generated = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-C", "test@minds", "-q", "-f"])
            .arg(&key)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !generated {
            return;
        }

        let pubkey = std::fs::read_to_string(dir.path().join("id.pub")).unwrap();
        let signers = dir.path().join("allowed_signers");
        std::fs::write(&signers, format!("test@minds {}", pubkey.trim())).unwrap();

        let sig = ssh_sign("hallo welt", &key).unwrap();

        // Gültig.
        assert!(
            ssh_verify("hallo welt", &sig, &signers, "test@minds").unwrap(),
            "eine echte Signatur muss verifizieren"
        );
        // Manipulierter Payload → ungültig (das eigentliche Sicherheitsziel).
        assert!(!ssh_verify("hallo WELT", &sig, &signers, "test@minds").unwrap());
        // Manipulierte Signatur → ungültig, kein Absturz.
        let broken = sig.replace('A', "B");
        assert!(!ssh_verify("hallo welt", &broken, &signers, "test@minds").unwrap_or(false));
    }
}
