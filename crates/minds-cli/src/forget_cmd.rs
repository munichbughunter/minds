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
//!   Force-Push, damit die Löschung dort ankommt. Den erledigt `minds sync` beim
//!   nächsten Push (oder von Hand aufgerufen) **gezielt** für die getilgten
//!   Session-Refs — es prüft, dass der neue Stand ein Tombstone ist; jeder
//!   andere Ref bleibt strikt fast-forward (#102). Nur der geteilte Kontext-Ref
//!   eines Bestandsrepos bleibt außen vor: Er trägt auch die übrigen Sessions
//!   und ist von Hand nachzuziehen, wenn seine Remote-Historie weichen soll.

use std::process::ExitCode;
use std::time::Duration;

use minds_core::SessionId;
use minds_store::{Forget, ForgottenPlace, tombstone};

use crate::context::Context;
use crate::sync;

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
    // Dasselbe Lock wie `minds sync` (#102): Tilgte `forget` mitten in einem
    // laufenden Sync, könnte dessen `record` den eben gelöschten Tracking-Ref
    // am Klartext-Commit neu erschaffen — die Forge trüge den Klartext dann
    // bis zum übernächsten Sync weiter, obwohl hier Erfolg gemeldet wurde.
    // Kurz warten ist billig; kommt das Lock nicht frei, lieber ehrlich
    // abbrechen als mit offenem Fenster tilgen.
    let git_dir = ctx.repo.git_dir().to_path_buf();
    let mut lock = None;
    for _ in 0..50 {
        match sync::Lock::acquire(&git_dir)? {
            Some(acquired) => {
                lock = Some(acquired);
                break;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let Some(_lock) = lock else {
        return Err("ein `minds sync` läuft gerade — bitte gleich erneut versuchen".into());
    };
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
            // Die Remote-Zusage gilt nur für die session-exklusiven Orte, die
            // `sync` per Force-Push nachziehen darf. Lag die Session (auch) im
            // geteilten Kontext-Baum, wäre der Satz dort eine falsche
            // Datenschutz-Zusage — der Kontext-Ref wird nie force-gepusht.
            let places = forget.places();
            if places
                .iter()
                .any(|p| matches!(p, ForgottenPlace::StoreRef | ForgottenPlace::SessionBranch))
            {
                println!(
                    "  Ein bereits auf die Forge gepushter Session-Ref wird beim nächsten Push \
                     (oder `minds sync`) gezielt per Force-Push nachgezogen."
                );
            }
            if places
                .iter()
                .any(|p| matches!(p, ForgottenPlace::ContextTree))
            {
                println!(
                    "  Der geteilte Kontext-Ref (Bestandsformat) wird nie force-gepusht; war er \
                     schon auf der Forge, ist seine Remote-Historie von Hand nachzuziehen."
                );
            }
        }
        Forget::Absent(id) => {
            println!("nichts zu vergessen: {id}");
            println!("  liegt nicht im Store oder wurde bereits vergessen.");
        }
    }
    Ok(())
}
