//! `minds recall <ziel>` — der verdichtete Kontext-Brief hinter einer Datei,
//! einer Zeile oder einem Commit. Die Agent-Schwester von `minds why`.
//!
//! `why` ist der menschliche Deep-Dive (der volle Verlauf hinter einer Zeile);
//! `recall` ist der handlungsorientierte Brief — verdichtet, deterministisch, für
//! den Menschen wie für den nächsten Agenten. Es aggregiert über **alle**
//! Sessions hinter dem Ziel, nicht nur eine.

use std::process::ExitCode;

use minds_core::Session;

use crate::context::{Context, Skipped};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds recall` aus. `target` ist eine Datei, `datei:zeile` oder ein
/// Commit.
pub fn run(target: Option<&str>) -> ExitCode {
    let Some(target) = target else {
        eprintln!("minds recall: erwartet <ziel> (datei, datei:zeile oder commit)");
        return ExitCode::FAILURE;
    };
    match recall(target) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds recall: {err}");
            ExitCode::FAILURE
        }
    }
}

fn recall(target: &str) -> Fallible<()> {
    let ctx = Context::open()?;
    let mut skipped = Skipped::default();
    let resolved = resolve_target(&ctx, target, &mut skipped);
    // Der Hinweis kommt vor dem Ergebnis — auch wenn das Ziel nichts (mehr)
    // liefert, denn gerade dann erklärt er, warum (#83).
    if let Some(note) = skipped.note() {
        eprintln!("minds recall: {note}");
    }
    let (label, sessions) = resolved?;
    let markdown =
        minds_reader::brief::render(&format!("Kontext-Brief — {label}"), &sessions, None);
    print!("{markdown}");
    Ok(())
}

/// Löst `target` zu einer Menge Sessions auf und liefert dazu eine Beschriftung.
/// Übersprungenes sammelt sich in `skipped`.
///
/// Reihenfolge der Deutung: `datei:zeile` (Blame) → Git-Revision → Dateipfad.
/// Die erste, die greift, gewinnt.
fn resolve_target(
    ctx: &Context,
    target: &str,
    skipped: &mut Skipped,
) -> Fallible<(String, Vec<Session>)> {
    if let Some((path, line)) = split_file_line(target) {
        if let Some(commit) = ctx.blame_commit(path, line)? {
            let (sessions, s) = ctx.sessions_of_commit(commit)?;
            skipped.merge(s);
            return Ok((format!("{path}:{line} → commit {commit}"), sessions));
        }
    }

    if let Some(commit) = ctx.resolve_rev(target) {
        let (sessions, s) = ctx.sessions_of_commit(commit)?;
        skipped.merge(s);
        return Ok((format!("commit {commit}"), sessions));
    }

    let (touching, s) = ctx.sessions_touching(target)?;
    skipped.merge(s);
    if !touching.is_empty() {
        return Ok((format!("Datei {target}"), touching));
    }

    Err(format!(
        "kein Kontext für {target:?} — weder Zeile, Commit noch Datei mit erfasstem Kontext"
    )
    .into())
}

/// Zerlegt `<datei>:<zeile>` — am **letzten** Doppelpunkt, wie `why`. Kein
/// Treffer heißt „ist kein datei:zeile", nicht Fehler: der Aufrufer probiert
/// dann die nächste Deutung.
fn split_file_line(target: &str) -> Option<(&str, u32)> {
    let (path, line) = target.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    (!path.is_empty() && line > 0).then_some((path, line))
}

#[cfg(test)]
mod tests {
    use super::split_file_line;

    #[test]
    fn recognises_file_and_line() {
        assert_eq!(
            split_file_line("src/retry.rs:42"),
            Some(("src/retry.rs", 42))
        );
    }

    #[test]
    fn a_bare_path_is_not_a_file_line() {
        assert_eq!(split_file_line("src/retry.rs"), None);
        assert_eq!(split_file_line("HEAD"), None);
        assert_eq!(split_file_line("src/retry.rs:0"), None);
    }
}
