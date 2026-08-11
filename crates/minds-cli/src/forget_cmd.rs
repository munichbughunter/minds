//! `minds forget <session> [--reason <text>]` — die DSGVO-Löschung einer Session.
//!
//! Ersetzt die Nutzlast einer Session im Store durch einen Tombstone: die
//! Referenz (der Trailer im Commit) bleibt auflösbar, der Inhalt verschwindet.
//! `minds why`/`show`/der Reader zeigen die Session danach als „vergessen", nicht
//! als Fehler — graceful degradation, kein Bruch.
//!
//! # Grenzen, ehrlich benannt
//!
//! - Getilgt werden **alle lokalen Orte**: der maßgebliche Store-Ref, der
//!   browsbare **Session-Branch** (`minds/session/<hash>`, `session.json` *und*
//!   `session.md`) und der Kontext-Baum eines Bestandsrepos. `forget` benennt in
//!   seiner Ausgabe, welche es waren.
//! - Der Tombstone wird als **elternloser** Wurzel-Commit gesetzt (#14): Der alte
//!   Blob ist danach über **keinen** Ref mehr erreichbar — auch nicht über
//!   `<ref>~1` — und nach `git gc` endgültig fort. Auch die eigene Push-
//!   Buchhaltung (`refs/minds/remotes/*`) wird vom Klartext gelöst, sonst hielte
//!   sie ihn gc-immun. Die Löschung ist damit kein „aktueller Stand leer,
//!   Historie voll" mehr, sondern lokal vollständig. (Ein Restanker bleibt nur bei
//!   `core.logAllRefUpdates=always` — dann führt Git auch für `refs/minds/*` ein
//!   Reflog, das den verwaisten Commit bis zum Reflog-Expiry hält; die
//!   Standard-Konfiguration reflogged diese Refs nicht.)
//! - Weil das die Ref-Kette neu schreibt, ist der Tombstone **kein**
//!   Fast-Forward: Ein bereits auf die Forge **gepushter** Ref braucht einen
//!   Force-Push, damit die Löschung dort ankommt. `minds sync` (das bewusst nie
//!   mit `--force` pusht) überträgt einen solchen Ref deshalb noch nicht — der
//!   gezielte Force-Push für Tombstones ist einem eigenen Schritt vorbehalten
//!   (#102). Bis dahin trägt die Forge den alten Stand, und der Push ist von Hand
//!   mit `--force` nachzuziehen.

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
        forget @ Forget::Forgotten(id, _) => {
            println!("vergessen: {id}");
            println!("  Grund: {reason}");
            println!("  Getilgt an:");
            for place in forget.places() {
                println!("    - {}", place.label());
            }
            println!(
                "  Die Referenzen bleiben auflösbar; der Klartext ist als elternloser Tombstone \
                 gelöscht — auch aus der Historie, nicht nur aus dem aktuellen Stand."
            );
            println!(
                "  Ein bereits auf die Forge gepushter Ref braucht dafür einen Force-Push \
                 (`minds sync` überträgt ihn noch nicht)."
            );
        }
        Forget::Absent(id) => {
            println!("nichts zu vergessen: {id}");
            println!("  liegt nicht im Store oder wurde bereits vergessen.");
        }
    }
    Ok(())
}
