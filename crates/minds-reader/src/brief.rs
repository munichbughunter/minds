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

use crate::decisions::{Assessment, Standing};
use crate::summary::headline;

/// Wie lang die Überschrift einer Absicht höchstens wird.
const REQUEST_MAX: usize = 100;

/// Rendert einen Kontext-Brief als Markdown.
///
/// `cap` deckelt jeden Abschnitt auf höchstens so viele Einträge (für
/// `minds brief`, das klein bleiben soll, damit der Agent-Input nicht ausufert).
/// `None` heißt alle Einträge (für `distill`); Absichten bleiben Überschriften.
/// Für den vollständigen Wortlaut gibt es [`render_full_intents`].
pub fn render(title: &str, sessions: &[Session], cap: Option<usize>) -> String {
    render_inner(title, sessions, cap, false)
}

/// Wie [`render`], aber mit den vollständigen, mehrzeiligen Absichten für `recall`.
pub fn render_full_intents(title: &str, sessions: &[Session]) -> String {
    render_inner(title, sessions, None, true)
}

/// Die Entscheidungen im Brief: zuerst, was der nächste Agent **neu
/// bewerten** muss, dann, was **in Kraft** ist — mit den Annahmen, unter
/// denen es entschieden wurde, und ihrem Stand heute. Leere Abschnitte
/// fallen weg, wie überall im Brief.
///
/// `assessments` sind die Spitzen ihrer Ketten (abgelöste Records werden
/// übergangen); `cap` deckelt den In-Kraft-Abschnitt (die jüngsten zuerst
/// — so liefert [`crate::decisions::assess_all`]), nie den
/// Re-Check-Abschnitt: Was zurück auf den Tisch gehört, wird nicht
/// gekappt. Jeder Text stammt aus dem entschärften [`Assessment`] oder ist
/// aus dem Record **geparst** (das Datum, die Gewichte); die Ids sind
/// gekürzt.
pub fn render_decisions(assessments: &[Assessment], cap: Option<usize>) -> String {
    let heads: Vec<&Assessment> = assessments.iter().filter(|a| a.is_head()).collect();
    let attention: Vec<&&Assessment> = heads
        .iter()
        .filter(|a| matches!(a.standing, Standing::Review | Standing::ReCheck))
        .collect();
    let rest: Vec<&&Assessment> = heads
        .iter()
        .filter(|a| !matches!(a.standing, Standing::Review | Standing::ReCheck))
        .collect();
    let mut out = String::new();
    if !attention.is_empty() {
        let _ = writeln!(
            out,
            "## Decisions to re-check\n\n_Probes evaluated against the index now — 0 tokens. A load-bearing assumption behind these no longer holds or changed._\n"
        );
        for a in &attention {
            decision_entry(&mut out, a, true);
        }
        out.push('\n');
    }
    if !rest.is_empty() {
        let take = cap.map_or(rest.len(), |c| c.min(rest.len()));
        let _ = writeln!(
            out,
            "## Decisions on record\n\n_In force, open or unchecked — recorded with the assumptions they rest on; each assumption with its standing today._\n"
        );
        for a in rest.iter().take(take) {
            decision_entry(&mut out, a, false);
        }
        if take < rest.len() {
            let _ = writeln!(out, "- … (+{} more — minds decisions)", rest.len() - take);
        }
        out.push('\n');
    }
    out
}

/// Ein Eintrag: Thema, Stand, dann die Annahmen und die Alternativen mit
/// ihren Gründen — alles, was der Agent braucht, um die Entscheidung unter
/// den **damaligen** Umständen zu verstehen und den heutigen Stand zu
/// sehen.
fn decision_entry(out: &mut String, a: &Assessment, attention: bool) {
    let r = &a.record;
    let glyph = match a.standing {
        Standing::Open => "◦",
        Standing::Review => "✗",
        Standing::ReCheck => "~",
        Standing::Unchecked => "?",
        Standing::InForce => "✓",
    };
    // Das Datum nur aus einem lesbaren Zeitstempel — der Record ist roh,
    // und ein Ref im Store kann jeder schreiben.
    let day = crate::decisions::day_of(&r.at);
    let _ = writeln!(
        out,
        "- {glyph} {} (b3-{}, {day}, {})",
        a.topic,
        &a.id.hex()[..8],
        crate::decisions::by_word(r.decided_by)
    );
    let _ = writeln!(out, "  {}", a.headline());
    match &a.chosen {
        Some(chosen) => {
            let _ = writeln!(out, "  chosen: {chosen}");
        }
        None => {
            let _ = writeln!(out, "  options: {}", a.alternatives.join(" · "));
        }
    }
    if !a.assumptions.is_empty() {
        let list: Vec<String> = a
            .assumptions
            .iter()
            .map(|s| format!("{} {} [{}]", s.id, s.statement, s.result.as_str()))
            .collect();
        let _ = writeln!(out, "  assumptions: {}", list.join(" · "));
    }
    if r.chosen.is_some() && !r.alternatives.is_empty() {
        let list: Vec<String> = r
            .alternatives
            .iter()
            .zip(&a.alternatives)
            .map(|(alt, approach)| {
                let because: Vec<String> = alt
                    .rejected_because
                    .iter()
                    .map(|reason| {
                        format!(
                            "{} {}",
                            crate::text::sanitize(&reason.assumption),
                            reason.weight.as_str()
                        )
                    })
                    .collect();
                if because.is_empty() {
                    approach.clone()
                } else {
                    format!("{approach} (because {})", because.join(", "))
                }
            })
            .collect();
        let _ = writeln!(out, "  rejected: {}", list.join(" · "));
    }
    if attention {
        for reconsider in &a.reconsider {
            let _ = writeln!(
                out,
                "  → reconsider: {} — {} no longer {}",
                reconsider.alternative,
                reconsider.fallen.join(", "),
                if reconsider.fallen.len() == 1 {
                    "holds"
                } else {
                    "hold"
                }
            );
        }
    }
    for note in &a.notes {
        let _ = writeln!(out, "  note: {note}");
    }
}

fn render_inner(
    title: &str,
    sessions: &[Session],
    cap: Option<usize>,
    full_intents: bool,
) -> String {
    let extract = Extract::from_sessions(sessions);
    let take = |n: usize| cap.map_or(n, |c| c.min(n));

    let mut out = String::new();
    let _ = writeln!(out, "# {title}\n");
    let _ = writeln!(
        out,
        "_{} session(s), derived deterministically from captured context — 0 tokens._\n",
        sessions.len()
    );

    // --- Absicht ---------------------------------------------------------
    let requests = dedup(sessions.iter().filter_map(|s| {
        if full_intents {
            let request = minds_core::without_pasted_wrapper(&s.intent.request);
            return (!request.trim().is_empty()).then(|| request.into_owned());
        }
        let line = headline(&s.intent.request, REQUEST_MAX);
        (line != "(no prompt captured)").then_some(line)
    }));
    section(&mut out, "Intent", &requests, take, |r| {
        format!("- {}", r.replace('\n', "\n  "))
    });

    // --- Constraints & deklarierte Sackgassen ----------------------------
    let constraints = dedup(sessions.iter().flat_map(|s| s.intent.constraints.clone()));
    section(&mut out, "Constraints", &constraints, take, |c| {
        format!("- {c}")
    });

    let discarded = dedup(sessions.iter().flat_map(|s| s.intent.discarded.clone()));
    section(
        &mut out,
        "Discarded approaches (declared)",
        &discarded,
        take,
        |d| format!("- {d}"),
    );

    // --- Beobachtete Fakten ----------------------------------------------
    section(
        &mut out,
        "Commands that worked",
        &extract.commands,
        take,
        |c| format!("- `{}`{}", c.command, times(c.count)),
    );

    section(
        &mut out,
        "Frequently changed files",
        &extract.hot_files,
        take,
        |f| {
            format!(
                "- `{}` — {} {} in {} {}",
                f.path,
                f.changes,
                plural(f.changes, "change", "changes"),
                f.sessions,
                plural(f.sessions, "session", "sessions"),
            )
        },
    );

    section(
        &mut out,
        "Changed together",
        &extract.co_changes,
        take,
        |c| format!("- `{}` + `{}`{}", c.a, c.b, times(c.count)),
    );

    // --- Heuristisch ------------------------------------------------------
    section(
        &mut out,
        "Dead ends (heuristic)",
        &extract.reworks,
        take,
        |r| match &r.kind {
            ReworkKind::WrittenThenDeleted => {
                format!("- `{}`: created and deleted again", r.path)
            }
            ReworkKind::Churned { edits } => {
                format!("- `{}`: rewritten {edits}×", r.path)
            }
        },
    );

    section(
        &mut out,
        "Corrections (heuristic)",
        &extract.corrections,
        take,
        |c| format!("- “{}”", c.text),
    );

    // Nichts Verwertbares? Ehrlich sagen statt leerer Überschriften.
    if requests.is_empty() && constraints.is_empty() && discarded.is_empty() && extract.is_empty() {
        let _ = writeln!(out, "_No usable context found._");
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
        let _ = writeln!(out, "- … (+{} more)", items.len() - n);
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
            at: None,
            ended_at: None,
            outcome: None,
            capture: None,
            name: "Bash".into(),
            arguments: format!(r#"{{"command":"{cmd}"}}"#),
            effect: Some(Effect {
                kind: EffectKind::Exec,
                path: None,
                content: None,
                written: None,
                written_unavailable: None,
                written_at: None,
            }),
        }
    }

    fn write(path: &str) -> ToolCall {
        ToolCall {
            at: None,
            ended_at: None,
            outcome: None,
            capture: None,
            name: "Edit".into(),
            arguments: "{}".into(),
            effect: Some(Effect {
                kind: EffectKind::Write,
                path: Some(path.into()),
                content: None,
                written: None,
                written_unavailable: None,
                written_at: None,
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
        assert!(out.contains("No usable context"));
        assert!(!out.contains("## Intent"));
    }

    #[test]
    fn a_brief_lists_intent_and_facts() {
        let mut s = session("Retry-Test reparieren");
        s.turns
            .push(assistant(vec![exec("cargo test"), write("src/retry.rs")]));

        let out = render("Kontext-Brief", &[s], None);
        assert!(out.contains("## Intent"));
        assert!(out.contains("- Retry-Test reparieren"));
        assert!(out.contains("## Commands that worked"));
        assert!(out.contains("`cargo test`"));
        assert!(out.contains("## Frequently changed files"));
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
        assert!(out.contains("(+2 more)"), "{out}");
    }

    #[test]
    fn empty_prompt_is_not_listed_as_an_intent() {
        let s = session("   ");
        let out = render("T", &[s], None);
        assert!(!out.contains("## Intent"), "{out}");
    }

    #[test]
    fn full_intents_keep_shared_headlines_and_all_lines() {
        let first = format!(
            "{}\nFirst details.\n\nLast details.",
            "long request ".repeat(12)
        );
        let second = format!("{}\nOther details.", "long request ".repeat(12));
        let sessions = [session(&first), session(&second), session(&first)];
        let out = render_full_intents("T", &sessions);
        assert!(out.contains(&format!("- {}", first.replace('\n', "\n  "))));
        assert!(out.contains(&format!("- {}", second.replace('\n', "\n  "))));
        assert_eq!(out.matches("First details.").count(), 1);
        assert!(!render("T", &sessions, Some(1)).contains("First details."));
        assert!(!render_full_intents("T", &[session(" \n ")]).contains("## Intent"));
    }

    /// R.7 — der Brief ist Byte für Byte stabil. Ändert sich das Format
    /// versehentlich, schlägt genau dieser Test an; ändert es sich absichtlich,
    /// aktualisiert man den Golden-String hier bewusst.
    /// Die Entscheidungen im Brief, eingefroren: ein Record, der neu zu
    /// bewerten ist (mit der Alternative, die zurückkommt), einer in Kraft,
    /// ein offener — und ein abgelöster, der nicht erscheint.
    #[test]
    fn golden_decisions_sections_are_byte_stable() {
        use crate::decisions::{MemoryTree, assess_all};
        use minds_core::ContentHash;
        use minds_core::decision::{
            Alternative, Assumption, Choice, DECISION_KIND, DECISION_SCHEMA, DecidedBy,
            DecisionRecord, Probe, Reason, Weight,
        };
        let id = |n: u8| ContentHash::from_bytes([n; 32]);
        let lock = DecisionRecord {
            kind: DECISION_KIND.into(),
            schema: DECISION_SCHEMA,
            topic: "lock for the witness socket on macOS".into(),
            decided_by: DecidedBy::Agent,
            at: "2026-10-21T13:09:00Z".into(),
            chosen: Some(Choice {
                approach: "flock on a lock file".into(),
                rests_on: vec![Reason {
                    assumption: "local-fs".into(),
                    weight: Weight::LoadBearing,
                }],
            }),
            open: false,
            alternatives: vec![Alternative {
                approach: "Unix socket as the lock".into(),
                rejected_because: vec![
                    Reason {
                        assumption: "fork".into(),
                        weight: Weight::LoadBearing,
                    },
                    Reason {
                        assumption: "no-windows".into(),
                        weight: Weight::Marginal,
                    },
                ],
                rests_on: Vec::new(),
            }],
            assumptions: vec![
                Assumption {
                    id: "local-fs".into(),
                    statement: "the lock file lives on a local file system".into(),
                    probe: None,
                },
                Assumption {
                    id: "fork".into(),
                    statement: "daemon.rs forks".into(),
                    probe: Some(Probe::Pattern {
                        path: Some("src/daemon.rs".into()),
                        glob: None,
                        must_match: Some(r"\bfork\(".into()),
                        must_not_match: None,
                    }),
                },
                Assumption {
                    id: "no-windows".into(),
                    statement: "no Windows support is needed".into(),
                    probe: Some(Probe::Pattern {
                        path: None,
                        glob: Some("crates/*/Cargo.toml".into()),
                        must_match: None,
                        must_not_match: Some("windows-sys".into()),
                    }),
                },
            ],
            scope: vec!["src/**".into()],
            supersedes: None,
        };
        let mut refs = lock.clone();
        refs.topic = "one ref per session".into();
        refs.at = "2026-07-29T10:00:00Z".into();
        refs.chosen = Some(Choice {
            approach: "one ref per session".into(),
            rests_on: Vec::new(),
        });
        refs.alternatives.clear();
        refs.assumptions.clear();
        refs.scope.clear();
        let mut old = refs.clone();
        old.topic = "one ref for everything".into();
        old.at = "2026-07-01T10:00:00Z".into();
        refs.supersedes = Some(id(3));
        let mut open = lock.clone();
        open.topic = "retry policy for the mirror".into();
        open.at = "2026-10-20T09:00:00Z".into();
        open.chosen = None;
        open.open = true;
        open.alternatives = vec![
            Alternative {
                approach: "exponential backoff".into(),
                rejected_because: Vec::new(),
                rests_on: Vec::new(),
            },
            Alternative {
                approach: "fail fast".into(),
                rejected_because: Vec::new(),
                rests_on: Vec::new(),
            },
        ];
        open.assumptions.clear();
        open.scope.clear();

        // Ein Record mit rohem Zeitstempel — für die Probe unten, dass nur
        // ein geparstes Datum den Brief erreicht.
        let mut raw = lock.clone();
        raw.at = "yesterday\u{1b}[2J".into();
        // Der Daemon forkt nicht mehr.
        let tree = MemoryTree::new()
            .with("src/daemon.rs", b"fn main() { spawn() }")
            .with("crates/a/Cargo.toml", b"[dependencies]\n");
        let all = assess_all(
            vec![(id(1), lock), (id(2), refs), (id(3), old), (id(4), open)],
            &tree,
        );
        let out = render_decisions(&all, Some(8));
        let expected = [
            "## Decisions to re-check",
            "",
            "_Probes evaluated against the index now — 0 tokens. A load-bearing assumption behind these no longer holds or changed._",
            "",
            "- ✗ lock for the witness socket on macOS (b3-01010101, 2026-10-21, agent)",
            "  re-evaluate: fork no longer holds → reconsider Unix socket as the lock",
            "  chosen: flock on a lock file",
            "  assumptions: local-fs the lock file lives on a local file system [not checkable] · fork daemon.rs forks [no longer holds] · no-windows no Windows support is needed [holds]",
            "  rejected: Unix socket as the lock (because fork load-bearing, no-windows marginal)",
            "  → reconsider: Unix socket as the lock — fork no longer holds",
            "",
            "## Decisions on record",
            "",
            "_In force, open or unchecked — recorded with the assumptions they rest on; each assumption with its standing today._",
            "",
            "- ◦ retry policy for the mirror (b3-04040404, 2026-10-20, agent)",
            "  open — waiting for a human · 2 options",
            "  options: exponential backoff · fail fast",
            "- ✓ one ref per session (b3-02020202, 2026-07-29, agent)",
            "  in force",
            "  chosen: one ref per session",
            "",
            "",
        ]
        .join("\n");
        assert_eq!(out, expected);
        // Der Deckel gilt nur dem In-Kraft-Abschnitt.
        let capped = render_decisions(&all, Some(1));
        assert!(capped.contains("- … (+1 more — minds decisions)"));
        assert!(capped.contains("→ reconsider"));
        // Nichts → nichts.
        assert_eq!(render_decisions(&[], None), "");
        // Ein roher Zeitstempel erreicht den Brief nie — nur ein geparster.
        let all = assess_all(vec![(id(9), raw)], &tree);
        let out = render_decisions(&all, None);
        assert!(out.contains("unknown date"), "{out}");
        assert!(!out.contains('\u{1b}'), "{out}");
    }

    #[test]
    fn golden_brief_is_byte_stable() {
        let mut s = session("Fix the retry test.");
        s.turns.push(user("Fix the retry test."));
        s.turns
            .push(assistant(vec![exec("cargo test"), write("src/retry.rs")]));
        s.turns.push(user("Nein, das ist falsch."));

        let expected = "\
# Kontext-Brief — Test

_1 session(s), derived deterministically from captured context — 0 tokens._

## Intent

- Fix the retry test.

## Commands that worked

- `cargo test`

## Frequently changed files

- `src/retry.rs` — 1 change in 1 session

## Corrections (heuristic)

- “Nein, das ist falsch.”

";
        assert_eq!(render("Kontext-Brief — Test", &[s], None), expected);
    }
}
