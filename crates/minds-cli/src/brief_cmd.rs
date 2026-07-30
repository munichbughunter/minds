//! `minds brief [<datei>...] [--hook]` — ein größenbegrenzter Kontext-Block für
//! den Start einer neuen Agent-Session.
//!
//! Vorausschauend statt rückblickend: „Ich fange gleich an *diesen* Dateien an —
//! was muss ein Agent wissen?" Ohne Pfade nimmt es das ganze Repo. Anders als
//! `recall`/`distill` ist die Ausgabe **gedeckelt** (siehe [`CAP`]), damit der
//! Agent-Input klein bleibt und keine Tokens verschwendet.
//!
//! # `--hook`: der Kontext direkt in die Session
//!
//! Mit `--hook` gibt das Kommando statt reinem Markdown das
//! SessionStart-Envelope von Claude Code aus
//! (`hookSpecificOutput.additionalContext`) — genau das, was `minds enable
//! --recall` als SessionStart-Hook registriert, damit jede neue Session den Brief
//! der vorigen automatisch voranstellt (Vision-Problem #3). Der Vertrag ist
//! Claude-spezifisch; andere Agents folgen, sobald ihr Envelope verifiziert ist.

use std::process::ExitCode;

use minds_core::Session;

use crate::context::{self, Context};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Höchstzahl der Einträge je Abschnitt — klein, damit der Block als Agent-Input
/// taugt (Headroom-Rücksicht).
const CAP: usize = 8;

/// Führt `minds brief` aus. `paths` sind die Dateien, um die es geht; leer =
/// ganzes Repo. `hook` verpackt die Ausgabe ins Claude-SessionStart-Envelope.
pub fn run(paths: &[String], hook: bool) -> ExitCode {
    match brief(paths, hook) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds brief: {err}");
            ExitCode::FAILURE
        }
    }
}

fn brief(paths: &[String], hook: bool) -> Fallible<()> {
    let ctx = Context::open()?;
    let all = ctx.all_sessions()?;

    let sessions: Vec<Session> = if paths.is_empty() {
        all
    } else {
        all.into_iter()
            .filter(|session| paths.iter().any(|path| context::touches(session, path)))
            .collect()
    };

    let label = if paths.is_empty() {
        "gesamtes Repo".to_string()
    } else {
        paths.join(", ")
    };
    let markdown = minds_reader::brief::render(
        &format!("Kontext für den Agenten — {label}"),
        &sessions,
        Some(CAP),
    );

    if hook {
        // Das dokumentierte SessionStart-Envelope: additionalContext wird der
        // neuen Session vorangestellt.
        let doc = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": markdown,
            }
        });
        println!("{}", serde_json::to_string(&doc)?);
    } else {
        print!("{markdown}");
    }
    Ok(())
}
