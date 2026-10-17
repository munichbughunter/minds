//! Von der Story zur Zeilenliste — UI-frei (C11).
//!
//! Der Reader liefert die Session als Schrittliste
//! ([`minds_reader::story::Story`]): ein Schritt je Tool-Aufruf, in
//! Session-Reihenfolge. Hier entsteht daraus, was die Oberfläche zeichnet —
//! abhängig von der Detailstufe:
//!
//! ```text
//!   1  ▶ ran     cat anforderung.md && ls -la                    +0:00
//!   2  ✎ wrote   src/sort.py                     ◇ reported      +1:28
//!      ◌ you · Und jetzt die Tests.
//!   3  ▶ tests   pytest -q src                   ✓ 8 passed      +2:10
//!      ↳ delegated → 3 steps · ◈ ● ✓  Review the tests
//!   RESULT   2 files · commit 609aca8 · ◦ not reviewed yet
//! ```
//!
//! - *Summary* fasst gleichartige Folgen zusammen (`3 test runs`,
//!   `2 edits of src/a.py`).
//! - *Normal* zeigt jeden Schritt; weitere Prompts des Menschen stehen als
//!   gedimmte Trennzeile dazwischen.
//! - *Verbose* zeigt dazu den Antworttext des Modells vor den Schritten des
//!   Zugs — als Aussage (`◌`), nie als Beleg.
//!
//! Verzweigung nur, wo sie existiert: Sub-Agents hängen als eingerückte
//! Zeile unter dem Schritt, der sie startet; Content-Übergaben stehen vor
//! der RESULT-Zeile. Der Cursor folgt dem **Schritt**, nicht dem Index
//! (B5): [`locate`] findet die Zeile, die einen Schritt zeigt oder enthält.

use minds_core::{Role, SessionId};
use minds_reader::story::{Outcome, Story, Verb, summarize};

/// Die Detailstufe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    /// Gleichartige Folgen zusammengefasst.
    Summary,
    /// Jeder Schritt.
    Normal,
    /// Dazu die Texte der Züge.
    Verbose,
}

impl Zoom {
    /// Die Stufe zu `1`, `2`, `3`; alles andere bleibt unverändert.
    pub fn from_digit(self, digit: u8) -> Self {
        match digit {
            1 => Zoom::Summary,
            2 => Zoom::Normal,
            3 => Zoom::Verbose,
            _ => self,
        }
    }

    /// Die Ziffer für die Anzeige.
    pub fn digit(self) -> u8 {
        match self {
            Zoom::Summary => 1,
            Zoom::Normal => 2,
            Zoom::Verbose => 3,
        }
    }
}

/// Eine Content-Übergabe (Evidence-DAG), wie der Index sie kennt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handover {
    /// Die andere Session.
    pub other: SessionId,
    /// `true`: diese Session las, was `other` schrieb.
    pub incoming: bool,
    /// Der Pfad.
    pub path: String,
}

/// Was eine Zeile zeigt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// Ein Schritt — Index in [`Story::steps`].
    Step(usize),
    /// Ein Lauf gleichartiger Schritte (Summary): erster Schritt, Zahl, das
    /// gemeinsame Verb und Ziel, das Ergebnis des letzten.
    Run {
        /// Index des ersten Schritts.
        first: usize,
        /// Wie viele Schritte (≥ 2; ein einzelner ist ein [`RowKind::Step`]).
        count: usize,
        /// Das gemeinsame Verb.
        verb: Verb,
        /// Das gemeinsame Ziel, bei Datei-Schritten.
        target: Option<String>,
        /// Das Ergebnis des letzten Schritts.
        outcome: Option<Outcome>,
    },
    /// Ein Zug als Trennzeile — Index in [`Story::turns`].
    Turn(usize),
    /// Die Kind-Session eines delegierten Schritts.
    Child {
        /// Der delegierende Schritt.
        step: usize,
        /// Die Kind-Session.
        child: SessionId,
    },
    /// Eine Content-Übergabe.
    Handover(Handover),
    /// Die RESULT-Zeile: Dateien, Commit, Review.
    Result,
}

/// Eine gezeichnete Zeile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Was sie zeigt.
    pub kind: RowKind,
    /// Der Schritt, den die Zeile zeigt oder zu dem sie gehört — für den
    /// Cursor über Zoomstufen hinweg (B5). `None` bei Übergaben und RESULT.
    pub step: Option<usize>,
    /// Die Einrückung (0 oder 1).
    pub depth: usize,
}

/// Die Zeilen einer Story in der Stufe `zoom`.
pub fn rows(story: &Story, zoom: Zoom, handovers: &[Handover]) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let child_rows = |out: &mut Vec<Row>, step: usize| {
        if let Some(child) = story.steps.get(step).and_then(|s| s.child) {
            out.push(Row {
                kind: RowKind::Child { step, child },
                step: Some(step),
                depth: 1,
            });
        }
    };
    match zoom {
        Zoom::Summary => {
            for run in summarize(&story.steps) {
                if run.count == 1 {
                    out.push(Row {
                        kind: RowKind::Step(run.first),
                        step: Some(run.first),
                        depth: 0,
                    });
                } else {
                    out.push(Row {
                        kind: RowKind::Run {
                            first: run.first,
                            count: run.count,
                            verb: run.verb,
                            target: run.target.clone(),
                            outcome: run.outcome,
                        },
                        step: Some(run.first),
                        depth: 0,
                    });
                }
                for step in run.first..run.first + run.count {
                    child_rows(&mut out, step);
                }
            }
        }
        Zoom::Normal | Zoom::Verbose => {
            // Der erste Prompt steht im PROMPT-Abschnitt — nicht noch einmal.
            let first_user = story
                .turns
                .iter()
                .position(|t| t.role == Role::User && !t.text.is_empty());
            let mut next = 0;
            for (i, turn) in story.turns.iter().enumerate() {
                let shown = !turn.text.is_empty()
                    && Some(i) != first_user
                    && match zoom {
                        Zoom::Verbose => true,
                        _ => turn.role == Role::User,
                    };
                if shown {
                    out.push(Row {
                        kind: RowKind::Turn(i),
                        step: (next < story.steps.len()).then_some(next),
                        depth: 0,
                    });
                }
                while next < story.steps.len() && story.steps[next].turn == i {
                    out.push(Row {
                        kind: RowKind::Step(next),
                        step: Some(next),
                        depth: 0,
                    });
                    child_rows(&mut out, next);
                    next += 1;
                }
            }
            // Schritte ohne Zug gibt es nicht — fail-soft trotzdem anhängen.
            while next < story.steps.len() {
                out.push(Row {
                    kind: RowKind::Step(next),
                    step: Some(next),
                    depth: 0,
                });
                child_rows(&mut out, next);
                next += 1;
            }
        }
    }
    for handover in handovers {
        out.push(Row {
            kind: RowKind::Handover(handover.clone()),
            step: None,
            depth: 0,
        });
    }
    out.push(Row {
        kind: RowKind::Result,
        step: None,
        depth: 0,
    });
    out
}

/// Die Zeile, die den Schritt `step` zeigt — bei einer Zusammenfassung die
/// zusammenfassende Zeile (B5). `None`, wenn keine passt.
pub fn locate(rows: &[Row], step: usize) -> Option<usize> {
    rows.iter()
        .position(|row| match &row.kind {
            RowKind::Step(s) => *s == step,
            RowKind::Run { first, count, .. } => (*first..*first + *count).contains(&step),
            _ => false,
        })
        .or_else(|| rows.iter().position(|row| row.step == Some(step)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{Agent, Effect, EffectKind, Intent, Model, Session, ToolCall, Turn};
    use minds_reader::Index;
    use std::collections::BTreeMap;

    fn sid() -> SessionId {
        format!("b3-{}", "a".repeat(64)).parse().unwrap()
    }

    fn call(name: &str, kind: EffectKind, path: &str) -> ToolCall {
        ToolCall {
            outcome: None,
            capture: None,
            name: name.into(),
            arguments: format!("{{\"command\":\"{path}\"}}"),
            effect: Some(Effect {
                kind,
                path: (kind != EffectKind::Exec).then(|| path.into()),
                content: None,
                written: None,
                written_unavailable: None,
            }),
        }
    }

    fn turn(role: Role, text: &str, calls: Vec<ToolCall>) -> Turn {
        Turn {
            role,
            text: text.into(),
            tool_calls: calls,
            parent: None,
            at: Some("2026-07-25T09:00:00Z".into()),
        }
    }

    fn session() -> Session {
        let mut s = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "opus".into(),
            },
            Intent {
                request: "Fix".into(),
                ..Intent::default()
            },
        );
        s.turns.push(turn(Role::User, "Fix", vec![]));
        s.turns.push(turn(
            Role::Assistant,
            "lese",
            vec![
                call("Read", EffectKind::Read, "a.rs"),
                call("Edit", EffectKind::Write, "a.rs"),
                call("Edit", EffectKind::Write, "a.rs"),
            ],
        ));
        s.turns.push(turn(Role::User, "und testen", vec![]));
        s.turns.push(turn(
            Role::Assistant,
            "teste",
            vec![
                call("Bash", EffectKind::Exec, "cargo test"),
                call("Bash", EffectKind::Exec, "cargo test"),
            ],
        ));
        s
    }

    fn story() -> Story {
        let index = Index::from_parts(BTreeMap::new(), BTreeMap::new());
        Story::of(sid(), &session(), &index)
    }

    fn kinds(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match &r.kind {
                RowKind::Step(s) => format!("step {s}"),
                RowKind::Run { first, count, .. } => format!("run {first}+{count}"),
                RowKind::Turn(t) => format!("turn {t}"),
                RowKind::Child { .. } => "child".into(),
                RowKind::Handover(_) => "handover".into(),
                RowKind::Result => "result".into(),
            })
            .collect()
    }

    #[test]
    fn normal_zoom_shows_every_step_and_later_prompts_as_separators() {
        let rows = rows(&story(), Zoom::Normal, &[]);
        assert_eq!(
            kinds(&rows),
            vec![
                "step 0", "step 1", "step 2", "turn 2", "step 3", "step 4", "result"
            ]
        );
        // Der erste Prompt steht im PROMPT-Abschnitt, nicht als Trenner.
        assert!(!kinds(&rows).contains(&"turn 0".to_string()));
        assert_eq!(
            rows[3].step,
            Some(3),
            "a separator belongs to the next step"
        );
    }

    #[test]
    fn verbose_zoom_adds_the_agent_text_before_its_steps() {
        let rows = rows(&story(), Zoom::Verbose, &[]);
        assert_eq!(
            kinds(&rows),
            vec![
                "turn 1", "step 0", "step 1", "step 2", "turn 2", "turn 3", "step 3", "step 4",
                "result"
            ]
        );
    }

    #[test]
    fn summary_zoom_folds_consecutive_steps_of_the_same_kind() {
        let rows = rows(&story(), Zoom::Summary, &[]);
        assert_eq!(kinds(&rows), vec!["step 0", "run 1+2", "run 3+2", "result"]);
        assert_eq!(rows[1].step, Some(1));
    }

    #[test]
    fn handovers_stand_before_the_result() {
        let handover = Handover {
            other: sid(),
            incoming: true,
            path: "a.rs".into(),
        };
        let rows = rows(&story(), Zoom::Summary, &[handover]);
        assert_eq!(kinds(&rows)[3..], ["handover", "result"]);
        assert_eq!(rows[3].step, None);
    }

    #[test]
    fn locate_follows_the_step_into_a_run_and_back() {
        let story = story();
        let normal = rows(&story, Zoom::Normal, &[]);
        let summary = rows(&story, Zoom::Summary, &[]);
        // Schritt 2 (die zweite Edit) steckt in der Zusammenfassung `1+2`.
        assert_eq!(locate(&normal, 2), Some(2));
        assert_eq!(locate(&summary, 2), Some(1));
        assert_eq!(locate(&summary, 4), Some(2));
        assert_eq!(locate(&normal, 9), None);
    }

    #[test]
    fn zoom_digits_map_and_unknown_digits_keep_the_level() {
        assert_eq!(Zoom::Normal.from_digit(1), Zoom::Summary);
        assert_eq!(Zoom::Normal.from_digit(3), Zoom::Verbose);
        assert_eq!(Zoom::Verbose.from_digit(9), Zoom::Verbose);
        assert_eq!(Zoom::Summary.digit(), 1);
    }
}
