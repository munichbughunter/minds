//! `minds sign <session> [--key <pfad>]` — signiert die Attribution einer
//! Session; `minds sign --seal <seal-id>` rüstet die Signatur eines
//! Evidence-Seals nach (ADR-0011).
//!
//! Macht aus „Agent X, Modell Y schrieb diese Zeilen" einen **Nachweis**: eine
//! `ssh-sig`-Signatur über den kanonischen Attestation-Payload
//! ([`attestation_payload`](minds_core::attestation_payload)). Die armierte
//! Signatur geht nach stdout; `minds verify` prüft sie wieder.
//!
//! Der Schlüssel kommt aus `--key` oder `git config user.signingkey`. Ein
//! passphrasegeschützter Schlüssel braucht einen laufenden ssh-agent (sonst
//! scheitert das Signieren, statt zu hängen).

use std::path::Path;
use std::process::{Command, ExitCode};

use minds_core::SessionId;

use crate::context::Context;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds sign` aus.
pub fn run(target: Option<&str>, key: Option<&str>, seal: Option<&str>) -> ExitCode {
    let result = match (seal, target) {
        (Some(seal_id), None) => sign_seal(seal_id, key),
        (None, Some(target)) => sign(target, key),
        (Some(_), Some(_)) => Err("either <session-id> or --seal, not both".into()),
        (None, None) => Err("expected <session-id> (b3-…) or --seal <seal-id>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds sign: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Signiert einen abgelegten Seal nach und legt `seal.sig` neben ihn.
///
/// Der Checkpoint signiert best-effort (nur mit konfiguriertem Schlüssel);
/// hier lässt sich das nachholen — etwa nachdem `user.signingkey` gesetzt
/// wurde. Signiert werden exakt die abgelegten Bytes.
fn sign_seal(seal_id: &str, key: Option<&str>) -> Fallible<()> {
    if !minds_attest::ssh_keygen_available() {
        return Err("ssh-keygen not found — required for signatures".into());
    }
    let id: minds_core::ContentHash = seal_id
        .parse()
        .map_err(|err| format!("not a valid seal id {seal_id:?}: {err}"))?;

    let ctx = Context::open()?;
    let text = ctx
        .store
        .seal_text(&id)?
        .ok_or_else(|| format!("seal {id} is not in the store"))?;

    let key = resolve_key(key, &ctx.root)?;
    let signature = minds_attest::ssh_sign(&text, Path::new(&key))?;
    ctx.store.put_seal_signature(&id, &signature)?;
    println!("Seal {id} signed");
    Ok(())
}

fn sign(target: &str, key: Option<&str>) -> Fallible<()> {
    if !minds_attest::ssh_keygen_available() {
        return Err("ssh-keygen not found — required for signed attribution".into());
    }
    let id: SessionId = target
        .parse()
        .map_err(|err| format!("not a valid session id {target:?}: {err}"))?;

    let ctx = Context::open()?;
    let session = ctx
        .store
        .get(id)?
        .ok_or_else(|| format!("session {id} is not in the store"))?;

    let key = resolve_key(key, &ctx.root)?;
    let payload = minds_core::attestation_payload(id, &session)?;
    let signature = minds_attest::ssh_sign(&payload, Path::new(&key))?;
    print!("{signature}");
    Ok(())
}

/// `git config user.signingkey`, falls gesetzt — der Weg, auf dem der
/// Checkpoint entscheidet, ob er best-effort signiert.
pub(crate) fn configured_key(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "user.signingkey"])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !value.is_empty()).then_some(value)
}

/// Der Signaturschlüssel: `--key`, sonst `git config user.signingkey`.
fn resolve_key(key: Option<&str>, root: &Path) -> Fallible<String> {
    if let Some(key) = key {
        return Ok(key.to_string());
    }
    configured_key(root)
        .ok_or_else(|| "no key: pass --key <path> or set `git config user.signingkey`".into())
}
