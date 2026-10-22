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
        eprintln!("minds recall: expected <target> (file, file:line, or commit)");
        return ExitCode::FAILURE;
    };
    match recall(target) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds recall: {}", crate::text::sanitize(&err.to_string()));
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
    // Die Entscheidungen dazu: die, die eine der Sessions aufgenommen hat,
    // und die, deren Scope den Pfad betrifft — vollständig, ohne Deckel.
    // Eine Entscheidung ist Kontext auch dort, wo noch keine Session die
    // Datei berührt hat: Dann steht der Brief nur aus ihr.
    let path = split_file_line(target)
        .map(|(path, _)| path)
        .or_else(|| (ctx.resolve_rev(target).is_none()).then_some(target));
    let paths: Vec<String> = path.map(str::to_owned).into_iter().collect();
    let decisions = |sessions: &[Session]| -> String {
        match crate::decide_cmd::Assessed::load(&ctx) {
            Ok(assessed) => {
                let heads: Vec<minds_reader::decisions::Assessment> = assessed
                    .for_sessions(sessions, &paths)
                    .into_iter()
                    .cloned()
                    .collect();
                minds_reader::brief::render_decisions(&heads, None)
            }
            Err(err) => {
                eprintln!(
                    "minds recall: decisions left out: {}",
                    crate::text::sanitize(&err.to_string())
                );
                String::new()
            }
        }
    };
    let (label, sessions) = match resolved? {
        Some(resolved) => resolved,
        // Kein Kontext — kein Fehler: Dann steht der Brief aus den
        // Entscheidungen, oder es gibt ihn nicht.
        None => {
            let section = decisions(&[]);
            if section.is_empty() {
                return Err(format!(
                    "no context for {target:?} — no line, commit, file or decision with captured context"
                )
                .into());
            }
            let markdown = format!(
                "# Context brief — {}\n\n_No session has touched this target yet; the decisions concerning it:_\n\n{section}",
                crate::text::sanitize(target)
            );
            print!("{}", terminal_markdown(&markdown, terminal_width()));
            return Ok(());
        }
    };
    let mut markdown =
        minds_reader::brief::render_full_intents(&format!("Context brief — {label}"), &sessions);
    markdown.push_str(&decisions(&sessions));
    print!("{}", terminal_markdown(&markdown, terminal_width()));
    Ok(())
}

/// Die Breite des Terminals: gemessen, sonst `COLUMNS`, sonst 80.
fn terminal_width() -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(width), _)| usize::from(width))
        .or_else(|| std::env::var("COLUMNS").ok()?.parse().ok())
        .filter(|width| *width > 0)
        .unwrap_or(80)
}

/// Originale Zeilen erhalten, fremden Text vor dem Umbruch entschärfen und
/// Fortsetzungen unter dem Listeneintrag einrücken.
fn terminal_markdown(markdown: &str, width: usize) -> String {
    let mut out = String::new();
    for line in markdown.lines() {
        let safe = crate::text::sanitize(line);
        let indent = if safe.starts_with("- ") || safe.starts_with("  ") {
            "  "
        } else {
            ""
        };
        let options = textwrap::Options::new(width).subsequent_indent(indent);
        out.push_str(&textwrap::fill(&safe, options));
        out.push('\n');
    }
    out
}

/// Löst `target` zu einer Menge Sessions auf und liefert dazu eine Beschriftung.
/// Übersprungenes sammelt sich in `skipped`.
///
/// Reihenfolge der Deutung: `datei:zeile` (Blame) → Git-Revision → Dateipfad.
/// Die erste, die greift, gewinnt.
///
/// Ein Dateipfad wird aufgelöst wie bei `minds blame`: **repo-relativ**, nicht
/// relativ zum Arbeitsverzeichnis, und über das Blame der ganzen Datei — jede
/// Zeile zu ihrem Commit, jeder Commit zu seinen Sessions. Dazu kommen die
/// Sessions, die die Datei laut eigenem Record berührt haben (für Arbeit, die
/// noch in keinem Commit steht). Allein auf den Record zu bauen, trug nicht:
/// Ein Agent nennt den Pfad, wie *er* ihn sieht (absolut, aus einem anderen
/// Namensraum), und der Vergleich mit dem Aufruf ging ins Leere, während
/// `blame` dieselbe Datei längst vollständig zuordnete.
fn resolve_target(
    ctx: &Context,
    target: &str,
    skipped: &mut Skipped,
) -> Fallible<Option<(String, Vec<Session>)>> {
    if let Some((path, line)) = split_file_line(target) {
        if let Some(commit) = ctx.blame_commit(path, line)? {
            let (sessions, s) = ctx.sessions_of_commit(commit)?;
            skipped.merge(s);
            return Ok(Some((format!("{path}:{line} → commit {commit}"), sessions)));
        }
    }

    if let Some(commit) = ctx.resolve_rev(target) {
        let (sessions, s) = ctx.sessions_of_commit(commit)?;
        skipped.merge(s);
        return Ok(Some((format!("commit {commit}"), sessions)));
    }

    let (mut behind, s) = ctx.sessions_of_file(target)?;
    skipped.merge(s);
    let (touching, s) = ctx.sessions_touching(target)?;
    skipped.merge(s);
    for session in touching {
        if !behind.contains(&session) {
            behind.push(session);
        }
    }
    if !behind.is_empty() {
        return Ok(Some((format!("file {target}"), behind)));
    }
    // Kein Kontext ist kein Betriebsfehler: Der wäre oben mit `?`
    // durchgereicht worden.
    Ok(None)
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
    use super::{split_file_line, terminal_markdown};

    #[test]
    fn multiline_intents_are_sanitized_and_wrapped() {
        let out = terminal_markdown("- one two three four\n  next\n  \u{1b}[31mred\n", 13);
        assert_eq!(
            out,
            "- one two\n  three four\n  next\n  \\u{1b}\n  [31mred\n"
        );
        assert!(!out.contains('\u{1b}'));
    }

    #[test]
    fn wrapping_counts_terminal_columns_for_unicode() {
        assert_eq!(terminal_markdown("- 中文 中文\n", 6), "- 中文\n  中文\n");
    }

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
