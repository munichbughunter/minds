//! `minds forget <session> [--reason <text>]` — die DSGVO-Löschung einer Session.
//!
//! Ersetzt die Nutzlast einer Session im Store durch einen Tombstone: die
//! Referenz (der Trailer im Commit) bleibt auflösbar, der Inhalt verschwindet.
//! `minds why`/`show`/der Reader zeigen die Session danach als „vergessen", nicht
//! als Fehler — graceful degradation, kein Bruch.
//!
//! # Grenzen, ehrlich benannt
//!
//! - Gelöscht wird der **maßgebliche Store-Record** (`refs/minds/context`). Ein
//!   bereits in die Forge gepushter **Session-Branch** (`minds/session/<hash>`,
//!   Child-Backend) trägt den Inhalt weiter, bis er dort separat entfernt wird.
//! - Der alte Blob überlebt in der **Historie** des Kontext-Refs, bis ein
//!   History-Rewrite ihn tilgt. Der aktuelle Stand ist sofort inhaltsfrei.

use std::process::ExitCode;

use minds_core::SessionId;
use minds_store::{Forget, tombstone};

use crate::context::Context;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds forget` aus. `target` ist die volle Session-Id (`b3-…`).
pub fn run(target: Option<&str>, reason: Option<&str>) -> ExitCode {
    let Some(target) = target else {
        eprintln!("minds forget: erwartet <session-id> (b3-…, etwa aus `minds show`)");
        return ExitCode::FAILURE;
    };
    match forget(target, reason.unwrap_or(tombstone::DEFAULT_REASON)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds forget: {err}");
            ExitCode::FAILURE
        }
    }
}

fn forget(target: &str, reason: &str) -> Fallible<()> {
    let id: SessionId = target
        .parse()
        .map_err(|err| format!("keine gültige Session-Id {target:?}: {err}"))?;

    let ctx = Context::open()?;
    match ctx.store.forget(id, reason)? {
        Forget::Forgotten(id) => {
            println!("vergessen: {id}");
            println!("  Grund: {reason}");
            println!(
                "  Die Referenz bleibt auflösbar; der Inhalt ist aus dem aktuellen Stand des Stores entfernt."
            );
        }
        Forget::Absent(id) => {
            println!("nichts zu vergessen: {id}");
            println!("  liegt nicht im Store oder wurde bereits vergessen.");
        }
    }
    Ok(())
}
