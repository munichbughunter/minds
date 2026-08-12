//! `minds recap [--limit <n>]` — die jüngsten Sessions auf einen Blick.
//!
//! „Was ist zuletzt passiert?" Sortiert die Sessions nach ihrem Startzeitpunkt
//! (best-effort, siehe [`context::time_key`]) und zeigt die neuesten mit
//! Absicht, Akteur und Umfang. Rein lesend.

use std::process::ExitCode;

use crate::context::{self, Context};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Voreinstellung, wie viele Sessions `recap` zeigt.
const DEFAULT_LIMIT: usize = 10;

/// Führt `minds recap` aus. `limit` ist der Wert von `--limit` (roh); `all`
/// zeigt alle Sessions (das frühere `minds log` — die vollständige Liste).
pub fn run(limit: Option<&str>, all: bool) -> ExitCode {
    let limit = if all {
        usize::MAX
    } else {
        match limit {
            Some(raw) => match raw.parse::<usize>() {
                Ok(n) if n > 0 => n,
                _ => {
                    eprintln!("minds recap: --limit erwartet eine Zahl ≥ 1");
                    return ExitCode::FAILURE;
                }
            },
            None => DEFAULT_LIMIT,
        }
    };
    match recap(limit) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds recap: {err}");
            ExitCode::FAILURE
        }
    }
}

fn recap(limit: usize) -> Fallible<()> {
    let ctx = Context::open()?;
    let (mut sessions, skipped) = ctx.all_sessions()?;
    if let Some(note) = skipped.note() {
        eprintln!("minds recap: {note}");
    }

    if sessions.is_empty() {
        println!("Noch keine Sessions erfasst.");
        return Ok(());
    }

    // Jüngste zuerst.
    sessions.sort_by(|a, b| context::time_key(b).cmp(context::time_key(a)));

    let shown = sessions.len().min(limit);
    println!("Die {shown} jüngsten von {} Session(s):\n", sessions.len());
    for session in sessions.iter().take(limit) {
        let when = context::time_key(session);
        let when = if when.is_empty() {
            "—".to_string()
        } else {
            when.replace('T', " ")
        };
        let headline = minds_reader::summary::headline(&session.intent.request, 80);
        println!("{when}  {headline}");
        println!(
            "                       {} · {} · {} Datei(en) · {}/{} Token",
            session.agent.name,
            session.model.id,
            session.produced.files.len(),
            session.usage.input_tokens,
            session.usage.output_tokens,
        );
    }
    Ok(())
}
