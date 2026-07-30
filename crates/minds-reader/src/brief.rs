//! Der Kontext-Brief: eine Session-Menge, verdichtet zu lesbarem Markdown.
//!
//! Geteilt von `minds recall`, `distill` und `brief` (Track R). Reine Funktion,
//! kein I/O: `&[Session]` rein, Markdown-String raus. Damit ist die Ausgabe
//! **deterministisch** und in Golden-Tests festnagelbar — gleiche Sessions ⇒
//! gleicher Brief, Byte für Byte.
//!
//! # Was hier zusammenkommt
//!
//! - die **Absichten** direkt aus den Sessions ([`Intent`](minds_core::Intent)),
//! - die **beobachteten Fakten** aus [`Extract`](minds_core::Extract) (Befehle,
//!   Hot-Files, Co-Changes),
//! - die **heuristischen** Signale (Rework, Korrekturen), klar als solche
//!   beschriftet.
//!
//! Leere Abschnitte fallen weg — der Brief behauptet nie einen Fakt, den es
//! nicht gibt. Ist gar nichts da, sagt er genau das.

use std::fmt::Write as _;

use minds_core::{Extract, ReworkKind, Session};

use crate::summary::headline;

/// Wie lang die Überschrift einer Absicht höchstens wird.
const REQUEST_MAX: usize = 100;

/// Rendert einen Kontext-Brief als Markdown.
///
/// `cap` deckelt jeden Abschnitt auf höchstens so viele Einträge (für
/// `minds brief`, das klein bleiben soll, damit der Agent-Input nicht ausufert).
/// `None` heißt vollständig (für `recall`/`distill`).
pub fn render(title: &str, sessions: &[Session], cap: Option<usize>) -> String {
    let extract = Extract::from_sessions(sessions);
    let take = |n: usize| cap.map_or(n, |c| c.min(n));

    let mut out = String::new();
    let _ = writeln!(out, "# {title}\n");
    let _ = writeln!(
        out,
        "_{} Session(s), deterministisch aus dem erfassten Kontext — 0 Tokens._\n",
        sessions.len()
    );

    // --- Absicht ---------------------------------------------------------
    let requests = dedup(sessions.iter().filter_map(|s| {
        let line = headline(&s.intent.request, REQUEST_MAX);
        (line != "(kein Prompt erfasst)").then_some(line)
    }));
    section(&mut out, "Absicht", &requests, take, |r| format!("- {r}"));

    // --- Constraints & deklarierte Sackgassen ----------------------------
    let constraints = dedup(sessions.iter().flat_map(|s| s.intent.constraints.clone()));
    section(&mut out, "Constraints", &constraints, take, |c| {
        format!("- {c}")
    });

    let discarded = dedup(sessions.iter().flat_map(|s| s.intent.discarded.clone()));
    section(
        &mut out,
        "Verworfene Ansätze (deklariert)",
        &discarded,
        take,
        |d| format!("- {d}"),
    );

    // --- Beobachtete Fakten ----------------------------------------------
    section(
        &mut out,
        "Funktionierende Befehle",
        &extract.commands,
        take,
        |c| format!("- `{}`{}", c.command, times(c.count)),
    );

    section(
        &mut out,
        "Häufig geänderte Dateien",
        &extract.hot_files,
        take,
        |f| {
            format!(
                "- `{}` — {} {} in {} {}",
                f.path,
                f.changes,
                plural(f.changes, "Änderung", "Änderungen"),
                f.sessions,
                plural(f.sessions, "Session", "Sessions"),
            )
        },
    );

    section(
        &mut out,
        "Zusammen geändert",
        &extract.co_changes,
        take,
        |c| format!("- `{}` + `{}`{}", c.a, c.b, times(c.count)),
    );

    // --- Heuristisch ------------------------------------------------------
    section(
        &mut out,
        "Sackgassen (heuristisch)",
        &extract.reworks,
        take,
        |r| match &r.kind {
            ReworkKind::WrittenThenDeleted => {
                format!("- `{}`: angelegt und wieder gelöscht", r.path)
            }
            ReworkKind::Churned { edits } => {
                format!("- `{}`: {edits}× umgeschrieben", r.path)
            }
        },
    );

    section(
        &mut out,
        "Korrekturen (heuristisch)",
        &extract.corrections,
        take,
        |c| format!("- „{}“", c.text),
    );

    // Nichts Verwertbares? Ehrlich sagen statt leerer Überschriften.
    if requests.is_empty() && constraints.is_empty() && discarded.is_empty() && extract.is_empty() {
        let _ = writeln!(out, "_Kein verwertbarer Kontext gefunden._");
    }

    out
}

/// Schreibt einen Abschnitt, wenn er Einträge hat — sonst nichts.
fn section<T>(
    out: &mut String,
    title: &str,
    items: &[T],
    take: impl Fn(usize) -> usize,
    line: impl Fn(&T) -> String,
) {
    if items.is_empty() {
        return;
    }
    let n = take(items.len());
    let _ = writeln!(out, "## {title}\n");
    for item in items.iter().take(n) {
        let _ = writeln!(out, "{}", line(item));
    }
    if n < items.len() {
        let _ = writeln!(out, "- … (+{} weitere)", items.len() - n);
    }
    out.push('\n');
}

/// Dedupliziert unter Erhalt der ersten Reihenfolge.
fn dedup(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

/// `" (3×)"` für Zähler > 1, sonst leer — ein einmaliges Vorkommen braucht keine
/// Zahl.
fn times(count: u32) -> String {
    if count > 1 {
        format!(" ({count}×)")
    } else {
        String::new()
    }
}

fn plural(n: u32, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{Agent, Effect, EffectKind, Intent, Model, Role, ToolCall, Turn};

    fn session(request: &str) -> Session {
        Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent {
                request: request.into(),
                ..Default::default()
            },
        )
    }

    fn exec(cmd: &str) -> ToolCall {
        ToolCall {
            name: "Bash".into(),
            arguments: format!(r#"{{"command":"{cmd}"}}"#),
            effect: Some(Effect {
                kind: EffectKind::Exec,
                path: None,
                content: None,
            }),
        }
    }

    fn write(path: &str) -> ToolCall {
        ToolCall {
            name: "Edit".into(),
            arguments: "{}".into(),
            effect: Some(Effect {
                kind: EffectKind::Write,
                path: Some(path.into()),
                content: None,
            }),
        }
    }

    fn assistant(calls: Vec<ToolCall>) -> Turn {
        Turn {
            role: Role::Assistant,
            text: String::new(),
            tool_calls: calls,
            parent: None,
            at: None,
        }
    }

    fn user(text: &str) -> Turn {
        Turn {
            role: Role::User,
            text: text.into(),
            tool_calls: Vec::new(),
            parent: None,
            at: None,
        }
    }

    #[test]
    fn empty_sessions_say_so() {
        let out = render("Kontext-Brief", &[], None);
        assert!(out.contains("Kein verwertbarer Kontext"));
        assert!(!out.contains("## Absicht"));
    }

    #[test]
    fn a_brief_lists_intent_and_facts() {
        let mut s = session("Retry-Test reparieren");
        s.turns
            .push(assistant(vec![exec("cargo test"), write("src/retry.rs")]));

        let out = render("Kontext-Brief", &[s], None);
        assert!(out.contains("## Absicht"));
        assert!(out.contains("- Retry-Test reparieren"));
        assert!(out.contains("## Funktionierende Befehle"));
        assert!(out.contains("`cargo test`"));
        assert!(out.contains("## Häufig geänderte Dateien"));
        assert!(out.contains("`src/retry.rs`"));
    }

    #[test]
    fn the_same_sessions_render_identically() {
        // Die Zusage dieses Moduls.
        let mut s = session("x");
        s.turns.push(assistant(vec![exec("make")]));
        assert_eq!(
            render("T", std::slice::from_ref(&s), None),
            render("T", &[s], None)
        );
    }

    #[test]
    fn cap_limits_each_section_and_notes_the_rest() {
        let mut s = session("many commands");
        s.turns
            .push(assistant(vec![exec("cmd-a"), exec("cmd-b"), exec("cmd-c")]));
        let out = render("T", &[s], Some(1));
        // Genau ein Befehl plus ein „weitere"-Hinweis.
        assert!(out.contains("(+2 weitere)"), "{out}");
    }

    #[test]
    fn empty_prompt_is_not_listed_as_an_intent() {
        let s = session("   ");
        let out = render("T", &[s], None);
        assert!(!out.contains("## Absicht"), "{out}");
    }

    /// R.7 — der Brief ist Byte für Byte stabil. Ändert sich das Format
    /// versehentlich, schlägt genau dieser Test an; ändert es sich absichtlich,
    /// aktualisiert man den Golden-String hier bewusst.
    #[test]
    fn golden_brief_is_byte_stable() {
        let mut s = session("Fix the retry test.");
        s.turns.push(user("Fix the retry test."));
        s.turns
            .push(assistant(vec![exec("cargo test"), write("src/retry.rs")]));
        s.turns.push(user("Nein, das ist falsch."));

        let expected = "\
# Kontext-Brief — Test

_1 Session(s), deterministisch aus dem erfassten Kontext — 0 Tokens._

## Absicht

- Fix the retry test.

## Funktionierende Befehle

- `cargo test`

## Häufig geänderte Dateien

- `src/retry.rs` — 1 Änderung in 1 Session

## Korrekturen (heuristisch)

- „Nein, das ist falsch.“

";
        assert_eq!(render("Kontext-Brief — Test", &[s], None), expected);
    }
}
