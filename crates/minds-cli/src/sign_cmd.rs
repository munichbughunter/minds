//! `minds sign <session> [--key <pfad>]` — signiert die Attribution einer Session.
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
pub fn run(target: Option<&str>, key: Option<&str>) -> ExitCode {
    let Some(target) = target else {
        eprintln!("minds sign: erwartet <session-id> (b3-…)");
        return ExitCode::FAILURE;
    };
    match sign(target, key) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds sign: {err}");
            ExitCode::FAILURE
        }
    }
}

fn sign(target: &str, key: Option<&str>) -> Fallible<()> {
    if !minds_attest::ssh_keygen_available() {
        return Err("ssh-keygen nicht gefunden — für signierte Attribution nötig".into());
    }
    let id: SessionId = target
        .parse()
        .map_err(|err| format!("keine gültige Session-Id {target:?}: {err}"))?;

    let ctx = Context::open()?;
    let session = ctx
        .store
        .get(id)?
        .ok_or_else(|| format!("Session {id} liegt nicht im Store"))?;

    let key = resolve_key(key, &ctx.root)?;
    let payload = minds_core::attestation_payload(id, &session)?;
    let signature = minds_attest::ssh_sign(&payload, Path::new(&key))?;
    print!("{signature}");
    Ok(())
}

/// Der Signaturschlüssel: `--key`, sonst `git config user.signingkey`.
fn resolve_key(key: Option<&str>, root: &Path) -> Fallible<String> {
    if let Some(key) = key {
        return Ok(key.to_string());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "user.signingkey"])
        .output()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() && !value.is_empty() {
        Ok(value)
    } else {
        Err("kein Schlüssel: --key <pfad> angeben oder `git config user.signingkey` setzen".into())
    }
}
