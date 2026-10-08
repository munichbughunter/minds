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
pub const NS_DEFAULT: &str = "minds";
/// Witness seals, separate from human reviews and attributions.
pub const NS_WITNESS: &str = "minds-witness";
/// Human intent approvals.
pub const NS_INTENT: &str = "minds-intent";
/// CI anchors and replay records.
pub const NS_ANCHOR: &str = "minds-anchor";
/// Backwards-compatible name for the default namespace.
pub const NAMESPACE: &str = NS_DEFAULT;

/// Fehler beim Signieren oder Verifizieren.
#[derive(Debug, thiserror::Error)]
pub enum AttestError {
    /// Temp-Dateien oder das Starten von `ssh-keygen` schlugen fehl.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// `ssh-keygen` lief, meldete aber einen Fehler.
    #[error("ssh-keygen {operation} failed: {stderr}")]
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
    ssh_keygen_available_at(Path::new("ssh-keygen"))
}

/// Wie [`ssh_keygen_available`], für genau das `ssh-keygen` unter
/// `program` — mit leerer Umgebung, damit die Probe nichts erbt.
pub fn ssh_keygen_available_at(program: &Path) -> bool {
    let mut command = Command::new(program);
    if program.is_absolute() {
        command.env_clear();
    }
    let Ok(output) = command.args(["-Y", "sign"]).stdin(Stdio::null()).output() else {
        return false;
    };
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    !stderr.contains("unknown option") && !stderr.contains("illegal option")
}

/// Signiert `payload` mit dem SSH-Schlüssel unter `key` und gibt die armierte
/// Signatur zurück.
pub fn ssh_sign(payload: &str, key: &Path) -> Result<String, AttestError> {
    ssh_sign_ns(payload, key, NAMESPACE)
}

/// Signiert `payload` im angegebenen SSH-Namespace. Der Namespace trennt
/// Signaturen verschiedener Verwendungszwecke kryptographisch voneinander.
pub fn ssh_sign_ns(payload: &str, key: &Path, namespace: &str) -> Result<String, AttestError> {
    sign_with(Path::new("ssh-keygen"), payload, key, namespace, false)
}

/// Wie [`ssh_sign_ns`], aber mit einem **vorab aufgelösten** `ssh-keygen`
/// (absoluter Pfad). Für Aufrufer, die zwischen Auflösung und Signatur
/// fremden Code ausführen (`minds replay`): Ein über `PATH` gefundenes
/// Programm könnte dieser Code inzwischen untergeschoben haben — und es
/// bekäme den Schlüssel.
pub fn ssh_sign_ns_with(
    program: &Path,
    payload: &str,
    key: &Path,
    namespace: &str,
) -> Result<String, AttestError> {
    sign_with(program, payload, key, namespace, false)
}

/// Wie [`ssh_sign_ns`], aber mit **geerbtem stderr**: Für einen FIDO-Schlüssel
/// (`sk-…`) fordert `ssh-keygen` dort zur Berührung auf („Confirm user
/// presence"). Eingesammelt sähe der Mensch die Aufforderung nie — der
/// Schlüssel blinkte stumm.
/// Daneben erscheinen auch die übrigen Zeilen von `ssh-keygen` („Signing
/// file …", „Write signature to …") — sie nennen nur den privaten
/// Temp-Pfad. stdin bleibt zu: Eine Passphrase-Abfrage
/// scheitert, statt zu hängen.
pub fn ssh_sign_ns_presence(
    payload: &str,
    key: &Path,
    namespace: &str,
) -> Result<String, AttestError> {
    sign_with(Path::new("ssh-keygen"), payload, key, namespace, true)
}

fn sign_with(
    program: &Path,
    payload: &str,
    key: &Path,
    namespace: &str,
    inherit_stderr: bool,
) -> Result<String, AttestError> {
    let dir = private_tempdir()?;
    let data = dir.path().join("payload");
    write_private(&data, payload.as_bytes())?;
    // ssh-keygen hängt ".sig" an den Payload-Pfad an — die Signatur entsteht
    // im selben privaten Verzeichnis, das mit dem TempDir-Drop verschwindet.
    let sig = dir.path().join("payload.sig");
    let mut command = Command::new(program);
    command
        .args(["-Y", "sign", "-n", namespace, "-f"])
        .arg(key)
        .arg(&data)
        .stdin(Stdio::null()) // ein passphrasegeschützter Schlüssel scheitert, statt zu hängen
        .stdout(Stdio::null());
    let (status, stderr) = if inherit_stderr {
        let status = command.stderr(Stdio::inherit()).status()?;
        (status, "see the ssh-keygen output above".to_string())
    } else {
        let output = command.stderr(Stdio::piped()).output()?;
        (
            output.status,
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )
    };
    if !status.success() {
        return Err(AttestError::Keygen {
            operation: "sign",
            stderr,
        });
    }
    std::fs::read_to_string(&sig).map_err(|_| AttestError::Keygen {
        operation: "sign",
        stderr: "exit 0, but no signature file written".to_string(),
    })
}

/// Die Schlüsseltypen, deren Signatur eine Berührung verlangt (FIDO,
/// „user presence") — samt ihren Zertifikatsformen.
const SECURITY_KEY_TYPES: &[&str] = &[
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
    "sk-ssh-ed25519-cert-v01@openssh.com",
    "sk-ecdsa-sha2-nistp256-cert-v01@openssh.com",
];

/// Ob `key_type` ein FIDO-Schlüssel ist (`sk-ssh-ed25519@openssh.com`,
/// `sk-ecdsa-sha2-nistp256@openssh.com`).
pub fn is_security_key_type(key_type: &str) -> bool {
    SECURITY_KEY_TYPES.contains(&key_type)
}

/// Der Schlüsseltyp einer `.pub`-Zeile (`<typ> <base64> [<kommentar>]`) —
/// das erste Wort, sofern ihm ein Schlüssel folgt.
pub fn public_key_type(line: &str) -> Option<&str> {
    let mut words = line.split_whitespace();
    let kind = words.next()?;
    words.next()?;
    Some(kind)
}

/// Der Kommentar einer `.pub`-Zeile (alles nach Typ und Schlüssel), falls
/// vorhanden.
pub fn public_key_comment(line: &str) -> Option<&str> {
    let line = line.trim();
    let (_, rest) = line.split_once(char::is_whitespace)?;
    let (_, comment) = rest.trim_start().split_once(char::is_whitespace)?;
    let comment = comment.trim();
    (!comment.is_empty()).then_some(comment)
}

/// Was eine armierte `ssh-sig`-Signatur über ihren Schlüssel sagt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureKey {
    /// Der Schlüsseltyp (`ssh-ed25519`, `sk-ssh-ed25519@openssh.com`, …).
    pub key_type: String,
    /// Nur bei FIDO-Schlüsseln: ob das User-Presence-Flag der Signatur
    /// gesetzt ist (eine Berührung fand statt). `ssh-keygen -Y verify`
    /// erzwingt das Flag nicht — ein mit `-O no-touch-required` erzeugter
    /// Schlüssel signiert ohne Berührung.
    pub user_presence: bool,
}

/// Liest Schlüsseltyp und — bei FIDO-Schlüsseln — das User-Presence-Flag
/// aus einer armierten `ssh-sig`-Signatur.
///
/// Das Format (OpenSSH `PROTOCOL.sshsig`): `"SSHSIG"`, `uint32` Version,
/// `string` öffentlicher Schlüssel (dessen erstes Feld der Typ ist),
/// `string` Namespace, `string` reserviert, `string` Hash-Algorithmus,
/// `string` Signatur. Die Signatur besteht aus `string` Typ, `string`
/// Signaturbytes und bei `sk-…` zusätzlich `byte` Flags und `uint32` Zähler
/// (`PROTOCOL.u2f`); Bit `0x01` ist User Presence.
///
/// Das prüft die Signatur **nicht** — ob sie gilt, entscheidet
/// [`ssh_verify_ns`]. Es liest nur, was die (danach geprüfte) Signatur über
/// ihren Schlüssel sagt. `None` für alles, was nicht so aussieht.
pub fn signature_key(armored: &str) -> Option<SignatureKey> {
    // Genau ein Block: BEGIN, Base64-Zeilen, END — nichts davor oder danach.
    let lines: Vec<&str> = armored.trim_end_matches('\n').split('\n').collect();
    let [first, body @ .., last] = lines.as_slice() else {
        return None;
    };
    if *first != "-----BEGIN SSH SIGNATURE-----"
        || *last != "-----END SSH SIGNATURE-----"
        || body.is_empty()
        || body
            .iter()
            .any(|line| line.is_empty() || line.starts_with('-'))
    {
        return None;
    }
    let blob = base64_decode(&body.concat())?;
    let rest = blob.strip_prefix(b"SSHSIG")?;
    if rest.get(..4)? != 1u32.to_be_bytes() {
        return None; // Version
    }
    let rest = &rest[4..];
    let (public_key, rest) = ssh_string(rest)?;
    let (kind, _) = ssh_string(public_key)?;
    let kind = std::str::from_utf8(kind).ok()?;
    if kind.is_empty()
        || !kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'@' | b'.'))
    {
        return None;
    }
    let (_, rest) = ssh_string(rest)?; // Namespace
    let (_, rest) = ssh_string(rest)?; // reserviert
    let (_, rest) = ssh_string(rest)?; // Hash-Algorithmus
    let (signature, rest) = ssh_string(rest)?;
    if !rest.is_empty() {
        return None;
    }
    let (_, tail) = ssh_string(signature)?; // Typ
    let (_, tail) = ssh_string(tail)?; // Signaturbytes
    let user_presence = if is_security_key_type(kind) {
        // Flags (1 Byte) und Zähler (4 Bytes), nichts danach.
        let [flags, _, _, _, _] = tail else {
            return None;
        };
        flags & 0x01 != 0
    } else {
        if !tail.is_empty() {
            return None;
        }
        false
    };
    Some(SignatureKey {
        key_type: kind.to_owned(),
        user_presence,
    })
}

/// Der Schlüsseltyp einer armierten `ssh-sig`-Signatur — siehe
/// [`signature_key`].
pub fn signature_key_type(armored: &str) -> Option<String> {
    signature_key(armored).map(|key| key.key_type)
}

/// Ein SSH-`string`: `uint32` Länge (big endian), dann die Bytes.
fn ssh_string(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let len = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    let value = bytes.get(4..4usize.checked_add(len)?)?;
    Some((value, &bytes[4 + len..]))
}

/// Base64 (Standard-Alphabet, mit Padding) — nur so viel, wie eine
/// `ssh-sig`-Armierung braucht; keine eigene Abhängigkeit dafür.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let chunks = bytes.len() / 4;
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let padding = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if padding > 2 || (padding > 0 && index + 1 != chunks) {
            return None;
        }
        let mut word = 0u32;
        for &c in &chunk[..4 - padding] {
            word = (word << 6) | value(c)?;
        }
        word <<= 6 * padding as u32;
        let decoded = [(word >> 16) as u8, (word >> 8) as u8, word as u8];
        out.extend_from_slice(&decoded[..3 - padding]);
    }
    Some(out)
}

/// Verifiziert `signature` über `payload` gegen die `allowed_signers`-Datei für
/// `identity`. `Ok(false)` heißt „Signatur ungültig" — kein Fehler, ein Ergebnis.
pub fn ssh_verify(
    payload: &str,
    signature: &str,
    signers: &Path,
    identity: &str,
) -> Result<bool, AttestError> {
    ssh_verify_ns(payload, signature, signers, identity, NS_DEFAULT)
}

/// Verifies both the signature namespace and the allowed signer's restrictions.
pub fn ssh_verify_ns(
    payload: &str,
    signature: &str,
    signers: &Path,
    identity: &str,
    namespace: &str,
) -> Result<bool, AttestError> {
    // A missing/unreadable trust file is an operational error, not tampering.
    std::fs::File::open(signers)?;
    let dir = private_tempdir()?;
    let sig = dir.path().join("attest.sig");
    write_private(&sig, signature.as_bytes())?;
    let mut child = Command::new("ssh-keygen")
        .args(["-Y", "verify", "-n", namespace, "-I", identity, "-f"])
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
            .ok_or_else(|| std::io::Error::other("ssh-keygen: no stdin"))?;
        if let Err(err) = stdin.write_all(payload.as_bytes()) {
            if err.kind() != std::io::ErrorKind::BrokenPipe {
                return Err(err.into());
            }
        }
    }
    Ok(child.wait()?.success())
}

/// Finds candidate principals for a signature's key. This does not verify the
/// payload or namespace: each candidate must still pass [`ssh_verify_ns`].
/// An invalid signature or an untrusted key yields an empty list.
pub fn ssh_find_principals(signature: &str, signers: &Path) -> Result<Vec<String>, AttestError> {
    let allowed = std::fs::read_to_string(signers)?;
    let dir = private_tempdir()?;
    let sig = dir.path().join("attest.sig");
    write_private(&sig, signature.as_bytes())?;
    let mut principals = Vec::new();
    // OpenSSH stops at the first matching allowed_signers record, regardless
    // of its namespace restriction. Discover per record so a human-only entry
    // cannot hide a later witness entry for the same key. OpenSSH still does
    // all parsing and key matching; verification uses the original trust file.
    for (index, line) in allowed.lines().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let record = dir.path().join(format!("signers-{index}"));
        write_private(&record, format!("{line}\n").as_bytes())?;
        let output = Command::new("ssh-keygen")
            .args(["-Y", "find-principals", "-f"])
            .arg(record)
            .arg("-s")
            .arg(&sig)
            .stdin(Stdio::null())
            .output()?;
        if output.status.success() {
            for principal in String::from_utf8_lossy(&output.stdout).lines() {
                if !principal.is_empty() && !principals.iter().any(|p| p == principal) {
                    principals.push(principal.to_owned());
                }
            }
        }
    }
    Ok(principals)
}

/// Ein Temp-Verzeichnis mit zufälligem Namen, nur für den Eigentümer lesbar.
/// Der Modus 0700 wird beim Anlegen gesetzt (nicht per chmod nachgereicht) —
/// es gibt kein Fenster, in dem das Verzeichnis offener stünde.
fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
    }
    #[cfg(not(unix))]
    tempfile::Builder::new().tempdir()
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
    fn namespace_restriction_blocks_cross_role_use() {
        assert!(
            ssh_keygen_available(),
            "SSH signature tests require ssh-keygen"
        );
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id");
        assert!(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        let signers = dir.path().join("allowed_signers");
        std::fs::write(
            &signers,
            format!(
                "minds-witness@host namespaces=\"minds-witness\" {}",
                public.trim()
            ),
        )
        .unwrap();
        let payload = "minds-review-v1\ndecision=approve\n";
        let witness = ssh_sign_ns(payload, &key, NS_WITNESS).unwrap();
        assert_eq!(
            ssh_find_principals(&witness, &signers).unwrap(),
            vec!["minds-witness@host"]
        );
        assert!(
            ssh_verify_ns(
                payload,
                &witness,
                &signers,
                "minds-witness@host",
                NS_WITNESS
            )
            .unwrap()
        );
        assert!(
            !ssh_verify_ns(
                "altered",
                &witness,
                &signers,
                "minds-witness@host",
                NS_WITNESS
            )
            .unwrap()
        );
        for namespace in [NS_DEFAULT, NS_INTENT, NS_ANCHOR] {
            let signature = ssh_sign_ns(payload, &key, namespace).unwrap();
            assert!(
                !ssh_verify_ns(
                    payload,
                    &signature,
                    &signers,
                    "minds-witness@host",
                    namespace
                )
                .unwrap()
            );
            assert!(
                !ssh_verify_ns(
                    payload,
                    &signature,
                    &signers,
                    "minds-witness@host",
                    NS_WITNESS
                )
                .unwrap()
            );
        }
        // Remove just the restriction: the same review signature now verifies.
        let review = ssh_sign(payload, &key).unwrap();
        std::fs::write(&signers, format!("minds-witness@host {}", public.trim())).unwrap();
        assert!(ssh_verify(payload, &review, &signers, "minds-witness@host").unwrap());
        assert!(
            ssh_find_principals("malformed signature", &signers)
                .unwrap()
                .is_empty()
        );
        std::fs::write(&signers, "").unwrap();
        assert!(ssh_find_principals(&witness, &signers).unwrap().is_empty());
        assert!(ssh_find_principals(&witness, &dir.path().join("missing")).is_err());
    }

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

    /// Base64 für die Tests — das Gegenstück zu [`base64_decode`].
    fn base64_encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let mut word = 0u32;
            for (i, &b) in chunk.iter().enumerate() {
                word |= u32::from(b) << (16 - 8 * i);
            }
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[((word >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// Eine armierte `ssh-sig`-Hülle mit einem Schlüssel vom Typ `kind` —
    /// so, wie ein FIDO-Schlüssel sie erzeugt (Signaturbytes beliebig).
    fn armored_with_key_type(kind: &str) -> String {
        armored_with(kind, Some(0x01))
    }

    /// Wie [`armored_with_key_type`]; `flags` hängt bei `Some` Flags und
    /// Zähler an die Signatur (FIDO-Form).
    fn armored_with(kind: &str, flags: Option<u8>) -> String {
        fn string(out: &mut Vec<u8>, bytes: &[u8]) {
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(bytes);
        }
        let mut key = Vec::new();
        string(&mut key, kind.as_bytes());
        string(&mut key, &[7; 32]);
        let mut blob = b"SSHSIG".to_vec();
        blob.extend_from_slice(&1u32.to_be_bytes());
        string(&mut blob, &key);
        string(&mut blob, b"minds-intent");
        string(&mut blob, b"");
        string(&mut blob, b"sha512");
        let mut signature = Vec::new();
        string(&mut signature, kind.as_bytes());
        string(&mut signature, &[9; 64]);
        if let Some(flags) = flags {
            signature.push(flags);
            signature.extend_from_slice(&7u32.to_be_bytes());
        }
        string(&mut blob, &signature);
        let body = base64_encode(&blob);
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(70)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        format!(
            "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
            lines.join("\n")
        )
    }

    #[test]
    fn base64_roundtrips_and_rejects_garbage() {
        for input in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            if input.is_empty() {
                assert_eq!(base64_decode(""), None);
                continue;
            }
            assert_eq!(base64_decode(&base64_encode(input)).unwrap(), input);
        }
        for bad in ["Zm9", "Zm=v", "Zg==Zg==", "Z===", "Zm9v!A==", "Zm 9v"] {
            assert_eq!(base64_decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn security_key_types_are_recognized() {
        for kind in [
            "sk-ssh-ed25519@openssh.com",
            "sk-ecdsa-sha2-nistp256@openssh.com",
        ] {
            assert!(is_security_key_type(kind));
            assert_eq!(
                signature_key_type(&armored_with_key_type(kind)).as_deref(),
                Some(kind)
            );
        }
        // User Presence aus den Flags der Signatur — nicht aus dem Typ.
        let touched = signature_key(&armored_with("sk-ssh-ed25519@openssh.com", Some(0x05)));
        assert!(touched.unwrap().user_presence);
        let untouched = signature_key(&armored_with("sk-ssh-ed25519@openssh.com", Some(0x04)));
        assert_eq!(
            untouched,
            Some(SignatureKey {
                key_type: "sk-ssh-ed25519@openssh.com".into(),
                user_presence: false,
            })
        );
        // Eine sk-Signatur ohne Flags ist keine.
        assert_eq!(
            signature_key(&armored_with("sk-ssh-ed25519@openssh.com", None)),
            None
        );
        assert!(
            !signature_key(&armored_with("ssh-ed25519", None))
                .unwrap()
                .user_presence
        );
        // Rahmung strikt: CRLF, zwei Blöcke, fremde Version, riesige Länge,
        // Bytes nach dem Zähler.
        let one = armored_with_key_type("sk-ssh-ed25519@openssh.com");
        assert_eq!(signature_key(&one.replace('\n', "\r\n")), None);
        assert_eq!(signature_key(&format!("{one}{one}")), None);
        let wrap = |blob: &[u8]| {
            format!(
                "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
                base64_encode(blob)
            )
        };
        let mut v2 = b"SSHSIG".to_vec();
        v2.extend_from_slice(&2u32.to_be_bytes());
        assert_eq!(signature_key(&wrap(&v2)), None);
        let mut huge = b"SSHSIG".to_vec();
        huge.extend_from_slice(&1u32.to_be_bytes());
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        huge.extend_from_slice(b"ssh-ed25519");
        assert_eq!(signature_key(&wrap(&huge)), None);

        for kind in ["ssh-ed25519", "ecdsa-sha2-nistp256", "ssh-rsa", "sk-", ""] {
            assert!(!is_security_key_type(kind), "{kind}");
        }
        assert_eq!(
            public_key_type("sk-ssh-ed25519@openssh.com AAAAGnNr patrick@doering-it\n"),
            Some("sk-ssh-ed25519@openssh.com")
        );
        assert_eq!(public_key_type("ssh-ed25519"), None);
        assert_eq!(
            public_key_comment("ssh-ed25519 AAAAC3 patrick@doering-it\n"),
            Some("patrick@doering-it")
        );
        assert_eq!(public_key_comment("ssh-ed25519 AAAAC3\n"), None);
        // Was keine ssh-sig-Hülle ist, hat keinen Typ.
        for garbage in [
            "",
            "-----BEGIN SSH SIGNATURE-----\n-----END SSH SIGNATURE-----\n",
            "-----BEGIN SSH SIGNATURE-----\nU1NIU0lH\n-----END SSH SIGNATURE-----\n",
            &armored_with_key_type("ssh-ed25519\n\u{1b}[31m"),
        ] {
            assert_eq!(signature_key_type(garbage), None);
        }
    }

    #[test]
    fn a_real_signature_names_its_key_type() {
        if !ssh_keygen_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id");
        assert!(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        let signature = ssh_sign_ns("payload", &key, NS_INTENT).unwrap();
        assert_eq!(
            signature_key_type(&signature).as_deref(),
            Some("ssh-ed25519")
        );
        // Geerbtes stderr ändert die Signatur nicht (Ed25519 ist
        // deterministisch).
        assert_eq!(
            ssh_sign_ns_presence("payload", &key, NS_INTENT).unwrap(),
            signature
        );
    }

    #[test]
    fn availability_check_terminates_without_tty() {
        // cargo test läuft ohne TTY an stdin; ein Check, der interaktiv würde
        // (der frühere argumentlose Aufruf startet den Keygen-Dialog), bliebe
        // hier hängen (lokal sichtbar, im CI als Timeout). Terminieren ist der
        // Beweis.
        let _ = ssh_keygen_available();
    }

    #[test]
    fn explicit_namespace_is_bound_into_the_signature() {
        if !ssh_keygen_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id");
        assert!(
            Command::new("ssh-keygen")
                .args(["-t", "ed25519", "-N", "", "-q", "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        let payload = "fixed namespace test payload";
        // Ed25519 is deterministic: the wrapper must sign the same bytes.
        assert_eq!(
            ssh_sign(payload, &key).unwrap(),
            ssh_sign_ns(payload, &key, NAMESPACE).unwrap()
        );
        let signature = ssh_sign_ns(payload, &key, "test-checkpoint").unwrap();
        let sig = dir.path().join("payload.sig");
        std::fs::write(&sig, &signature).unwrap();
        for (namespace, valid) in [("test-checkpoint", true), (NAMESPACE, false)] {
            let mut child = Command::new("ssh-keygen")
                .args(["-Y", "check-novalidate", "-n", namespace, "-s"])
                .arg(&sig)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(payload.as_bytes())
                .unwrap();
            assert_eq!(child.wait().unwrap().success(), valid);
        }
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
