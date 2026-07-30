//! `minds search <query>` — Prompts und Sessions durchsuchen.
//!
//! Die schlichte, aber bis hierher fehlende Frage „wo habe ich das schon mal
//! gemacht?". Sucht case-insensitiv in Absicht, Verlauf und berührten Dateien.
//! Rein lesend, deterministisch.

use std::process::ExitCode;

use minds_core::Session;

use crate::context::Context;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds search` aus.
pub fn run(query: Option<&str>) -> ExitCode {
    let Some(query) = query else {
        eprintln!("minds search: erwartet <query>");
        return ExitCode::FAILURE;
    };
    match search(query) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds search: {err}");
            ExitCode::FAILURE
        }
    }
}

fn search(query: &str) -> Fallible<()> {
    let ctx = Context::open()?;
    let needle = query.to_lowercase();

    let hits: Vec<Session> = ctx
        .all_sessions()?
        .into_iter()
        .filter(|session| matches(session, &needle))
        .collect();

    if hits.is_empty() {
        println!("Keine Treffer für {query:?}.");
        return Ok(());
    }

    println!("{} Treffer für {query:?}:\n", hits.len());
    for session in &hits {
        let headline = minds_reader::summary::headline(&session.intent.request, 90);
        println!("▸ {headline}");
        println!(
            "  {} · {} · {} Datei(en)",
            session.agent.name,
            session.model.id,
            session.produced.files.len()
        );
    }
    Ok(())
}

/// `true`, wenn `needle` (bereits klein) in Absicht, einem Turn-Text oder einem
/// berührten Pfad vorkommt.
fn matches(session: &Session, needle: &str) -> bool {
    session.intent.request.to_lowercase().contains(needle)
        || session
            .turns
            .iter()
            .any(|turn| turn.text.to_lowercase().contains(needle))
        || session
            .produced
            .files
            .iter()
            .any(|file| file.to_lowercase().contains(needle))
}

#[cfg(test)]
mod tests {
    use super::matches;
    use minds_core::{Agent, Intent, Model, Role, Session, Turn};

    fn session(request: &str, turn: &str, file: &str) -> Session {
        let mut s = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "m".into(),
            },
            Intent {
                request: request.into(),
                ..Default::default()
            },
        );
        s.turns.push(Turn {
            role: Role::User,
            text: turn.into(),
            tool_calls: Vec::new(),
            parent: None,
            at: None,
        });
        s.produced.files.push(file.into());
        s
    }

    #[test]
    fn matches_request_turn_and_file_case_insensitively() {
        let s = session("Retry-Test reparieren", "die Backoff-Logik", "src/retry.rs");
        assert!(matches(&s, "retry-test"));
        assert!(matches(&s, "backoff"));
        assert!(matches(&s, "src/retry.rs"));
        assert!(!matches(&s, "kubernetes"));
    }
}
