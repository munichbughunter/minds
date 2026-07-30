//! `minds distill [--path <dir>] [--out <datei>]` — die Historie des Repos als
//! AGENTS.md-Entwurf.
//!
//! Repo-weite Kontext-Rückführung: alle Sessions (oder die zu einem Pfad)
//! verdichtet zu beobachteten Fakten — funktionierende Befehle, Hot-Files,
//! Sackgassen, Korrekturen. Deterministisch, 0 Tokens.
//!
//! **Entwurf, kein Merge.** `--out` schreibt eine Datei; liegt dort schon eine
//! AGENTS.md, wird sie überschrieben. Das Zusammenführen mit einer bestehenden
//! Datei ist bewusst dem Menschen überlassen (siehe Plan-v0.2, offene
//! Entscheidung 3).

use std::process::ExitCode;

use crate::context::{self, Context};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds distill` aus.
pub fn run(path: Option<&str>, out: Option<&str>) -> ExitCode {
    match distill(path, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds distill: {err}");
            ExitCode::FAILURE
        }
    }
}

fn distill(path: Option<&str>, out: Option<&str>) -> Fallible<()> {
    let ctx = Context::open()?;
    let mut sessions = ctx.all_sessions()?;
    if let Some(path) = path {
        sessions.retain(|session| context::touches(session, path));
    }

    let title = match path {
        Some(path) => format!("AGENTS.md-Entwurf — {path}"),
        None => "AGENTS.md-Entwurf".to_string(),
    };
    let markdown = minds_reader::brief::render(&title, &sessions, None);

    match out {
        Some(file) => {
            std::fs::write(file, &markdown)?;
            eprintln!("geschrieben: {file} ({} Sessions)", sessions.len());
        }
        None => print!("{markdown}"),
    }
    Ok(())
}
