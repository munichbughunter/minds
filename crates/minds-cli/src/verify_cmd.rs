//! `minds verify <session> --sig <datei> [--signers <datei>] [--identity <id>]`
//! — prüft eine signierte Attribution.
//!
//! Rekonstruiert den kanonischen Payload aus der (hash-geprüften) Session im
//! Store und verifiziert die Signatur dagegen. Ändert sich der Session-Inhalt,
//! ändert sich die `SessionId` und damit der Payload — die Signatur passt dann
//! nicht mehr. Manipulation fliegt auf.

use std::path::Path;
use std::process::{Command, ExitCode};

use minds_core::SessionId;

use crate::context::Context;
use crate::signing;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds verify` aus.
pub fn run(
    target: Option<&str>,
    sig: Option<&str>,
    signers: Option<&str>,
    identity: Option<&str>,
) -> ExitCode {
    let Some(target) = target else {
        eprintln!("minds verify: erwartet <session-id>");
        return ExitCode::FAILURE;
    };
    let Some(sig) = sig else {
        eprintln!("minds verify: --sig <datei> erforderlich");
        return ExitCode::FAILURE;
    };
    match verify(target, sig, signers, identity) {
        Ok(true) => {
            println!("gültig");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("UNGÜLTIG");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("minds verify: {err}");
            ExitCode::FAILURE
        }
    }
}

fn verify(
    target: &str,
    sig_file: &str,
    signers: Option<&str>,
    identity: Option<&str>,
) -> Fallible<bool> {
    if !signing::ssh_keygen_available() {
        return Err("ssh-keygen nicht gefunden".into());
    }
    let id: SessionId = target
        .parse()
        .map_err(|err| format!("keine gültige Session-Id {target:?}: {err}"))?;

    let ctx = Context::open()?;
    let session = ctx
        .store
        .get(id)?
        .ok_or_else(|| format!("Session {id} liegt nicht im Store"))?;

    let payload = minds_core::attestation_payload(id, &session)?;
    let signature = std::fs::read_to_string(sig_file)
        .map_err(|err| format!("Signaturdatei {sig_file:?} nicht lesbar: {err}"))?;
    let signers = resolve_signers(signers, &ctx.root)?;
    let identity = resolve_identity(identity, &ctx.root)?;

    signing::ssh_verify(&payload, &signature, Path::new(&signers), &identity)
}

/// Die allowed_signers-Datei: `--signers`, sonst `git config
/// gpg.ssh.allowedSignersFile`, sonst `~/.ssh/allowed_signers`.
fn resolve_signers(signers: Option<&str>, root: &Path) -> Fallible<String> {
    if let Some(signers) = signers {
        return Ok(signers.to_string());
    }
    if let Some(configured) = git_config(root, "gpg.ssh.allowedSignersFile") {
        return Ok(configured);
    }
    if let Ok(home) = std::env::var("HOME") {
        let default = format!("{home}/.ssh/allowed_signers");
        if Path::new(&default).exists() {
            return Ok(default);
        }
    }
    Err(
        "keine allowed_signers-Datei: --signers <datei> angeben oder \
         `git config gpg.ssh.allowedSignersFile` setzen"
            .into(),
    )
}

/// Die Identität (Principal in allowed_signers): `--identity`, sonst
/// `git config user.email`.
fn resolve_identity(identity: Option<&str>, root: &Path) -> Fallible<String> {
    if let Some(identity) = identity {
        return Ok(identity.to_string());
    }
    git_config(root, "user.email").ok_or_else(|| {
        "keine Identität: --identity <id> angeben oder `git config user.email` setzen".into()
    })
}

fn git_config(root: &Path, key: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", key])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !value.is_empty()).then_some(value)
}
