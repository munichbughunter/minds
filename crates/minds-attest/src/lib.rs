//! Signieren und Verifizieren über `ssh-keygen -Y sign/verify` (SSH-Signaturen).
//!
//! Warum `ssh-sig` und nicht sigstore/gitsign: Es ist überall da, wo `ssh` ist —
//! kein Netz, kein OIDC, air-gap-tauglich (siehe Plan-v0.2, offene Entscheidung).
//! Dasselbe Verfahren, mit dem Git SSH-Commits signiert.
//!
//! Als eigene Crate, damit jeder Prüfer dasselbe Vertrauensmodell teilt (#26):
//! die CLI, `minds-gitlab` (Webhook → Review → Signatur) und ein künftiger
//! CI-Verifier. `minds-core` liefert den kanonischen Payload
//! (`attestation_payload`), hier wird er signiert und geprüft — die Crate kennt
//! nur Strings und Pfade, keine Minds-Typen.
//!
//! Attestation-Payloads können Intent-Text (Prompts) enthalten — also genau die
//! Daten, die das Redaction-System sonst schützt. Deshalb entsteht alles, was
//! ssh-keygen als Datei braucht, in einem privaten Temp-Verzeichnis (0700,
//! zufälliger Name) mit Dateien im Modus 0600 und `create_new`-Semantik: nicht
//! welt-lesbar, kein Symlink-Race über vorhersagbare Namen.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Der ssh-sig-Namespace — trennt Minds-Signaturen von anderen ssh-sig-Domänen.
pub const NAMESPACE: &str = "minds";

/// Fehler beim Signieren oder Verifizieren.
#[derive(Debug, thiserror::Error)]
pub enum AttestError {
    /// Temp-Dateien oder das Starten von `ssh-keygen` schlugen fehl.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// `ssh-keygen` lief, meldete aber einen Fehler.
    #[error("ssh-keygen {operation} fehlgeschlagen: {stderr}")]
    Keygen {
        /// Die Unteroperation (`sign` oder `verify`).
        operation: &'static str,
        /// Die (getrimmte) stderr-Ausgabe von ssh-keygen.
        stderr: String,
    },
}

/// Ob `ssh-keygen` verfügbar ist und `-Y sign` beherrscht.
///
/// Der Probe-Aufruf `ssh-keygen -Y sign` ohne weitere Argumente terminiert
/// sofort mit einem Argument-Fehler — ohne TTY-Interaktion (stdin ist zu,
/// stdout/stderr werden eingesammelt). Ein ssh-keygen ohne `-Y`-Unterstützung
/// (OpenSSH < 8.0) meldet stattdessen eine unbekannte Option und gilt als
/// nicht verfügbar.
pub fn ssh_keygen_available() -> bool {
    let Ok(output) = Command::new("ssh-keygen")
        .args(["-Y", "sign"])
        .stdin(Stdio::null())
        .output()
    else {
        return false;
    };
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    !stderr.contains("unknown option") && !stderr.contains("illegal option")
}

/// Signiert `payload` mit dem SSH-Schlüssel unter `key` und gibt die armierte
/// Signatur zurück.
pub fn ssh_sign(payload: &str, key: &Path) -> Result<String, AttestError> {
    let dir = private_tempdir()?;
    let data = dir.path().join("payload");
    write_private(&data, payload.as_bytes())?;
    // ssh-keygen hängt ".sig" an den Payload-Pfad an — die Signatur entsteht
    // im selben privaten Verzeichnis, das mit dem TempDir-Drop verschwindet.
    let sig = dir.path().join("payload.sig");
    let output = Command::new("ssh-keygen")
        .args(["-Y", "sign", "-n", NAMESPACE, "-f"])
        .arg(key)
        .arg(&data)
        .stdin(Stdio::null()) // ein passphrasegeschützter Schlüssel scheitert, statt zu hängen
        .output()?;
    if !output.status.success() {
        return Err(AttestError::Keygen {
            operation: "sign",
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    std::fs::read_to_string(&sig).map_err(|_| AttestError::Keygen {
        operation: "sign",
        stderr: "Exit 0, aber keine Signaturdatei geschrieben".to_string(),
    })
}

/// Verifiziert `signature` über `payload` gegen die `allowed_signers`-Datei für
/// `identity`. `Ok(false)` heißt „Signatur ungültig" — kein Fehler, ein Ergebnis.
pub fn ssh_verify(
    payload: &str,
    signature: &str,
    signers: &Path,
    identity: &str,
) -> Result<bool, AttestError> {
    let dir = private_tempdir()?;
    let sig = dir.path().join("attest.sig");
    write_private(&sig, signature.as_bytes())?;
    let mut child = Command::new("ssh-keygen")
        .args(["-Y", "verify", "-n", NAMESPACE, "-I", identity, "-f"])
        .arg(signers)
        .arg("-s")
        .arg(&sig)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    {
        // Die signierten Daten kommen über stdin; das Schließen (Drop) signalisiert
        // ssh-keygen das Ende. Stirbt ssh-keygen früh (kaputte Signaturdatei),
        // ist der Write ein EPIPE — kein Fehler: Das Urteil fällt allein der
        // Exit-Status, sonst wäre „ungültig" mal Ok(false), mal Err (Race).
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("ssh-keygen: kein stdin"))?;
        if let Err(err) = stdin.write_all(payload.as_bytes()) {
            if err.kind() != std::io::ErrorKind::BrokenPipe {
                return Err(err.into());
            }
        }
    }
    Ok(child.wait()?.success())
}

/// Ein Temp-Verzeichnis mit zufälligem Namen, nur für den Eigentümer lesbar.
/// Der Modus 0700 wird beim Anlegen gesetzt (nicht per chmod nachgereicht) —
/// es gibt kein Fenster, in dem das Verzeichnis offener stünde.
fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir()
}

/// Legt `path` neu an (`create_new`: existiert er schon — auch als Symlink —
/// scheitert der Aufruf) und schreibt `bytes`; auf Unix mit Modus 0600.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)
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
        // Manipulierte Signatur → ungültig, kein Absturz — auch wenn ssh-keygen
        // stirbt, bevor es den Payload von stdin liest (EPIPE ist kein Fehler).
        let broken = sig.replace('A', "B");
        assert!(!ssh_verify("hallo welt", &broken, &signers, "test@minds").unwrap());
    }

    #[test]
    fn availability_check_terminates_without_tty() {
        // cargo test läuft ohne TTY an stdin; ein Check, der interaktiv würde
        // (der frühere argumentlose Aufruf startet den Keygen-Dialog), bliebe
        // hier hängen (lokal sichtbar, im CI als Timeout). Terminieren ist der
        // Beweis.
        let _ = ssh_keygen_available();
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_owner_only_and_create_new() {
        use std::os::unix::fs::PermissionsExt;

        let dir = private_tempdir().unwrap();
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let path = dir.path().join("payload");
        write_private(&path, b"geheim").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // create_new: ein zweites Anlegen desselben Pfads scheitert, statt zu
        // überschreiben oder einem Symlink zu folgen.
        assert!(write_private(&path, b"nochmal").is_err());
    }
}
