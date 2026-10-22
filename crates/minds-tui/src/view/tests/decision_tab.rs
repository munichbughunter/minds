//! Der Decisions-Tab, der Entscheidungs-Schritt in der Story und der
//! DECISIONS-Block der Übersicht — gegen eine Quelle, die bewertete
//! Entscheidungen liefert wie `minds decisions`.

use std::cell::Cell;
use std::collections::BTreeMap;

use minds_core::decision::{
    Alternative, Assumption, Choice, DECISION_KIND, DECISION_SCHEMA, DecidedBy, DecisionEvent,
    DecisionRecord, Probe, Reason, Weight,
};
use minds_core::{ContentHash, Effect, EffectKind, Role, ToolCall, Turn};
use minds_reader::decisions::{MemoryTree, assess_all};

use super::*;
use crate::DecisionList;
use crate::app::{Tab, View};

fn text_at(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::view::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

fn id(n: u8) -> ContentHash {
    ContentHash::from_bytes([n; 32])
}

/// flock statt Socket — mit drei Annahmen, eine davon gefallen.
fn lock() -> DecisionRecord {
    DecisionRecord {
        kind: DECISION_KIND.into(),
        schema: DECISION_SCHEMA,
        topic: "lock for the witness socket on macOS".into(),
        decided_by: DecidedBy::Agent,
        at: "2026-07-25T14:12:00Z".into(),
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
            rejected_because: vec![Reason {
                assumption: "fork".into(),
                weight: Weight::LoadBearing,
            }],
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
        ],
        scope: vec!["src/**".into()],
        supersedes: None,
    }
}

/// Eine offene Entscheidung mit zwei Optionen.
fn open() -> DecisionRecord {
    DecisionRecord {
        kind: DECISION_KIND.into(),
        schema: DECISION_SCHEMA,
        topic: "retry policy for the mirror".into(),
        decided_by: DecidedBy::Agent,
        at: "2026-07-25T14:20:00Z".into(),
        chosen: None,
        open: true,
        alternatives: vec![
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
        ],
        assumptions: Vec::new(),
        scope: Vec::new(),
        supersedes: None,
    }
}

/// Die Quelle: der Daemon forkt nicht mehr — Entscheidung 1 ist neu zu
/// bewerten, Entscheidung 2 offen, Entscheidung 3 der abgelöste
/// Vorgänger von 1.
struct Decided {
    asked: Cell<usize>,
    fail: Cell<bool>,
}

impl crate::Source for Decided {
    fn load(&self) -> minds_reader::Result<Inspection> {
        Ok(deciding())
    }
    fn stamp(&self) -> Option<crate::Stamp> {
        None
    }
    fn decisions(&self) -> Result<DecisionList, String> {
        self.asked.set(self.asked.get() + 1);
        if self.fail.get() {
            return Err("the index is not readable".into());
        }
        let tree = MemoryTree::new().with("src/daemon.rs", b"fn main() { spawn() }");
        let mut lock = lock();
        lock.supersedes = Some(id(3));
        let mut old = lock.clone();
        old.supersedes = None;
        old.at = "2026-07-01T00:00:00Z".into();
        old.chosen = Some(Choice {
            approach: "a global mutex".into(),
            rests_on: Vec::new(),
        });
        let entries = assess_all(vec![(id(1), lock), (id(2), open()), (id(3), old)], &tree);
        Ok(DecisionList {
            entries,
            unreadable: 0,
            truncated: false,
        })
    }
}

fn source() -> Decided {
    Decided {
        asked: Cell::new(0),
        fail: Cell::new(false),
    }
}

/// Session a hat Entscheidung 1 aufgenommen: ein `minds decide`-Aufruf,
/// dessen Antwort die Id nannte.
fn deciding() -> Inspection {
    let mut a = session("Fix retry handling", "2026-07-25T14:10:00Z");
    a.turns.push(Turn {
        role: Role::Assistant,
        text: "I record the lock decision.".into(),
        tool_calls: vec![
            ToolCall {
                at: Some("2026-07-25T14:11:00Z".into()),
                ended_at: None,
                outcome: None,
                capture: None,
                name: "Bash".into(),
                arguments: r#"{"command":"cargo test"}"#.into(),
                effect: Some(Effect {
                    kind: EffectKind::Exec,
                    path: None,
                    content: None,
                    written: None,
                    written_unavailable: None,
                    written_at: None,
                }),
            },
            ToolCall {
                at: Some("2026-07-25T14:12:00Z".into()),
                ended_at: Some("2026-07-25T14:12:05Z".into()),
                outcome: None,
                capture: None,
                name: "Bash".into(),
                arguments: r#"{"command":"minds decide <<'EOF'\n{}\nEOF"}"#.into(),
                effect: Some(Effect {
                    kind: EffectKind::Exec,
                    path: None,
                    content: None,
                    written: None,
                    written_unavailable: None,
                    written_at: None,
                }),
            },
        ],
        parent: None,
        at: Some("2026-07-25T14:11:00Z".into()),
    });
    // Die Nennung zeigt auf den zweiten Aufruf des letzten Zugs; der Record
    // (14:12:00) liegt in dessen Fenster (14:12:00–14:12:05).
    a.decision_events.push(DecisionEvent {
        seq: 7,
        decision_id: id(1),
        turn: Some(a.turns.len() - 1),
        call: Some(1),
    });
    // Session b legt die offene Entscheidung dem Menschen vor.
    let mut b = session("Add exponential backoff", "2026-07-25T13:41:00Z");
    b.turns.push(Turn {
        role: Role::Assistant,
        text: "I leave the retry policy to you.".into(),
        tool_calls: vec![ToolCall {
            at: Some("2026-07-25T14:19:58Z".into()),
            ended_at: Some("2026-07-25T14:20:03Z".into()),
            outcome: None,
            capture: None,
            name: "Bash".into(),
            arguments: r#"{"command":"minds decide --human < open.json"}"#.into(),
            effect: Some(Effect {
                kind: EffectKind::Exec,
                path: None,
                content: None,
                written: None,
                written_unavailable: None,
                written_at: None,
            }),
        }],
        parent: None,
        at: Some("2026-07-25T14:19:58Z".into()),
    });
    b.decision_events.push(DecisionEvent {
        seq: 3,
        decision_id: id(2),
        turn: Some(b.turns.len() - 1),
        call: Some(0),
    });
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), a);
    sessions.insert(sid('b'), b);
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "t")
}

fn opened<'r>(repo: &'r Repo, source: &Decided) -> App<'r> {
    let mut app = App::new(deciding(), repo, None);
    app.reduce(Action::TabTo(4));
    assert!(app.fill_decisions(source));
    app
}

/// Die Liste: Spitzen nach Dringlichkeit — offen zuerst, dann neu zu
/// bewerten; der abgelöste Record fehlt. Das Detail: Thema, Stand als
/// Satz, der gewählte Weg mit seiner Annahme, die Alternative mit
/// `✗ reconsider`, jede Annahme mit Ergebnis, die Historie, die Session.
#[test]
fn the_tab_lists_heads_by_urgency_and_shows_the_decision_in_full() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    assert_eq!(app.tab, Tab::Decisions);
    assert_eq!(source.asked.get(), 1);
    let out = render(&mut app);
    assert!(out.contains("F5 Decisions"), "{out}");
    assert!(
        out.contains("Decisions · 2 · 1 open · 1 to re-evaluate"),
        "{out}"
    );
    let open_at = out.find("◦ 02020202").expect("open listed");
    let review_at = out.find("✗ 01010101").expect("re-evaluate listed");
    assert!(open_at < review_at, "offen vor neu zu bewerten:\n{out}");
    assert!(
        !out.contains("03030303"),
        "der abgelöste Record ist keine Spitze:\n{out}"
    );
    // Der Cursor steht auf der ersten (offenen) Entscheidung.
    assert!(
        out.contains("OPTIONS (2) — the choice is with a human"),
        "{out}"
    );
    assert!(out.contains("exponential backoff"), "{out}");

    app.reduce(Action::Down);
    // Hoch genug, dass das ganze Detail steht.
    let out = text_at(&mut app, 200, 60);
    assert!(
        out.contains("DECISION  ⚖ 01010101 · agent · 2026-07-25"),
        "{out}"
    );
    assert!(
        out.contains("◌ lock for the witness socket on macOS"),
        "{out}"
    );
    assert!(
        out.contains("✗ re-evaluate: fork no longer holds → reconsider Unix socket as the lock"),
        "{out}"
    );
    assert!(out.contains("CHOSEN"), "{out}");
    assert!(out.contains("◌ flock on a lock file"), "{out}");
    assert!(out.contains("rests on  local-fs"), "{out}");
    assert!(out.contains("· not checkable"), "{out}");
    assert!(out.contains("REJECTED"), "{out}");
    assert!(
        out.contains("Unix socket as the lock  ✗ reconsider"),
        "{out}"
    );
    assert!(out.contains("(load-bearing) ✗ no longer holds"), "{out}");
    assert!(out.contains("ASSUMPTIONS (2)"), "{out}");
    assert!(out.contains("HISTORY"), "{out}");
    assert!(
        out.contains("supersedes 03030303 · 2026-07-01 · agent — a global mutex"),
        "{out}"
    );
    assert!(out.contains("SESSIONS (1)"), "{out}");
    assert!(out.contains("b3-aaaaaaaa"), "{out}");
    // Die Fußzeile nennt die Entscheidung mit ihrem Stand.
    assert!(out.contains("⚖ 01010101 · re-evaluate:"), "{out}");
    // Technik erst auf Space.
    assert!(
        !out.contains("re-evaluate with: minds decide --supersedes"),
        "{out}"
    );
    app.reduce(Action::Space);
    let out = text_at(&mut app, 200, 60);
    // Die Zeile bricht um (Wrap): das Kommando, dann die Id.
    assert!(
        out.contains("re-evaluate with: minds decide --supersedes"),
        "{out}"
    );
    assert!(out.contains("· id b3-01010101"), "{out}");
    assert!(
        out.contains("fork: pattern in src/daemon.rs must match"),
        "{out}"
    );
}

/// Enter öffnet die Story der Session, die die Entscheidung aufgenommen
/// hat; dort steht der Schritt als `⚖ decided` mit Thema und Stand, die
/// DECISIONS-Zeile im Kopf, und `D` führt zurück in den Tab.
#[test]
fn enter_opens_the_story_with_the_decided_step_and_d_jumps_back() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    app.reduce(Action::Down);
    app.reduce(Action::Enter);
    assert_eq!(app.tab, Tab::Sessions);
    assert!(matches!(app.top(), Some(View::Story { id, .. }) if *id == sid('a')));
    let out = render(&mut app);
    assert!(out.contains("DECISIONS"), "{out}");
    assert!(out.contains("⚖ decided"), "{out}");
    assert!(
        out.contains("lock for the witness socket on macOS"),
        "{out}"
    );
    assert!(out.contains("✗ re-evaluate"), "{out}");
    // Der Testlauf davor bleibt ein gewöhnlicher Schritt.
    assert!(out.contains("▶ tests"), "{out}");
    // Auf dem Schritt: die Detailbox nennt Entscheidung und Stand. Der
    // Cursor läuft bis zur Zeile des Schritts — deterministisch, nicht
    // „n Mal runter und hoffen".
    let rows = match app.top() {
        Some(View::Story { rows, .. }) => rows.clone(),
        _ => panic!("story"),
    };
    let step = rows
        .iter()
        .position(|r| matches!(r.kind, crate::layout::RowKind::Step(4)))
        .expect("the decide step (after read, edit and two test runs)");
    for _ in 0..step {
        app.reduce(Action::Down);
    }
    let out = text_at(&mut app, 200, 60);
    assert!(out.contains("STEP 5 · decided"), "{out}");
    assert!(
        out.contains("Decision   ◌ lock for the witness socket on macOS"),
        "{out}"
    );
    assert!(out.contains("D: open in Decisions"), "{out}");
    // `D` springt in den Tab — und Esc führt zurück in die Story (B2).
    app.reduce(Action::Decisions);
    assert_eq!(app.tab, Tab::Decisions);
    let out = render(&mut app);
    assert!(out.contains("DECISION  ⚖ 01010101"), "{out}");
    app.reduce(Action::Back);
    assert_eq!(app.tab, Tab::Sessions);
    assert!(matches!(app.top(), Some(View::Story { id, .. }) if *id == sid('a')));

    // Eine offene Entscheidung heißt im Schritt `asked you`.
    app.reduce(Action::TabTo(4));
    app.reduce(Action::Home);
    app.reduce(Action::Enter);
    assert!(matches!(app.top(), Some(View::Story { id, .. }) if *id == sid('b')));
    let out = render(&mut app);
    assert!(out.contains("⚖ asked you"), "{out}");
    assert!(out.contains("retry policy for the mirror"), "{out}");
    assert!(out.contains("◦ open"), "{out}");
}

/// Ohne Entscheidung unter dem Fokus sagt `D`, dass es keine gibt — und
/// eine Quelle, die nicht lesen kann, wird so gezeigt, nie als „in Kraft".
#[test]
fn d_without_a_decision_explains_and_an_unreadable_source_is_named() {
    let (_dir, repo) = repo();
    let source = source();
    let mut inspection = deciding();
    // Session b ohne ihre Entscheidung.
    let mut sessions = BTreeMap::new();
    for (id, s) in inspection.index().sessions() {
        let mut s = s.clone();
        if *id == sid('b') {
            s.decision_events.clear();
            s.turns.clear();
        }
        sessions.insert(*id, s);
    }
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    inspection = Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "t");
    let mut app = App::new(inspection, &repo, None);
    app.reduce(Action::Down); // Session b
    app.reduce(Action::Decisions);
    assert_eq!(app.tab, Tab::Sessions);
    assert!(
        app.closed
            .as_deref()
            .is_some_and(|c| c.contains("no decision recorded"))
    );

    source.fail.set(true);
    let mut app = opened(&repo, &source);
    let out = render(&mut app);
    assert!(
        out.contains("decisions not readable (the index is not readable)"),
        "{out}"
    );
    assert!(!out.contains("in force"), "{out}");
}

/// Nach einem Neuladen gilt kein Stand mehr — auch nicht in der Story —,
/// bis neu gerechnet ist; der Tab sagt „rechecking…".
#[test]
fn after_a_reload_no_standing_is_shown_until_recomputed() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    app.reduce(Action::Down);
    assert!(render(&mut app).contains("re-evaluate:"));
    app.decisions.as_mut().unwrap().invalidate(false);
    let out = render(&mut app);
    assert!(out.contains("rechecking…"), "{out}");
    assert!(!out.contains("re-evaluate: fork"), "{out}");
    // Die Story behält Thema und Verb, nur der Stand wartet.
    app.reduce(Action::Enter);
    let story = render(&mut app);
    assert!(story.contains("⚖ decided"), "{story}");
    assert!(
        story.contains("lock for the witness socket on macOS"),
        "{story}"
    );
    assert!(story.contains("? rechecking"), "{story}");
    assert!(!story.contains("✗ re-evaluate"), "{story}");
    app.reduce(Action::Back);
    // Auch die Liste und die Zählung im Titel zeigen keinen alten Stand.
    assert!(!out.contains("✗ 01010101"), "{out}");
    assert!(!out.contains("1 to re-evaluate"), "{out}");
    assert!(out.contains("? 01010101"), "{out}");
    assert!(app.decision(&id(1)).is_none());
    assert!(app.fill_decisions(&source));
    assert!(render(&mut app).contains("re-evaluate: fork"));
}

/// Ein Filter, der nichts trifft, sagt das — und End bleibt in der
/// gefilterten Liste.
#[test]
fn the_filter_says_when_nothing_matches() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    app.reduce(Action::SearchStart);
    for c in "retry".chars() {
        app.reduce(Action::SearchInput(c));
    }
    app.reduce(Action::SearchCommit);
    app.reduce(Action::End);
    let out = render(&mut app);
    assert!(out.contains("retry policy for the mirror"), "{out}");
    assert!(!out.contains("DECISION  ⚖ 01010101"), "{out}");
    app.reduce(Action::SearchStart);
    for c in "zzz".chars() {
        app.reduce(Action::SearchInput(c));
    }
    app.reduce(Action::SearchCommit);
    let out = render(&mut app);
    assert!(out.contains("no decision matches the filter"), "{out}");
}

/// Eine Entscheidung, die eine Session nennt, der Store aber nicht hat,
/// heißt in der Story „not in this store" — nie dauerhaft „… reading"; und
/// eine Quelle, die nicht lesen kann, sagt das dort auch.
#[test]
fn a_named_decision_the_store_lacks_says_so_in_the_story() {
    let (_dir, repo) = repo();
    let source = source();
    let mut inspection = deciding();
    let mut sessions = BTreeMap::new();
    for (sid_, s) in inspection.index().sessions() {
        let mut s = s.clone();
        if *sid_ == sid('a') {
            s.decision_events[0].decision_id = id(9);
        }
        sessions.insert(*sid_, s);
    }
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    inspection = Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "t");
    let mut app = App::new(inspection, &repo, None);
    app.reduce(Action::Enter);
    // Vor dem Lesen: genannt, „… reading".
    let before = render(&mut app);
    assert!(before.contains("09090909"), "{before}");
    assert!(before.contains("… reading"), "{before}");
    assert!(app.fill_decisions(&source));
    let out = render(&mut app);
    // Nach dem vollständigen Lesen ist eine Id ohne Record keine
    // Entscheidung: kein Schritt, keine Kopfzeile, kein Sprung.
    assert!(!out.contains("⚖ decided"), "{out}");
    assert!(!out.contains("09090909"), "{out}");
    assert_eq!(app.decision_absent(&id(9)), "not in this store");
    assert!(app.linked_decisions(sid('a')).is_empty());
    let asked = source.asked.get();
    app.reduce(Action::Decisions);
    assert_eq!(app.tab, Tab::Sessions);
    assert!(
        app.closed
            .as_deref()
            .is_some_and(|c| c.contains("no decision recorded")),
        "{:?}",
        app.closed
    );
    assert!(!app.fill_decisions(&source), "kein neues Holen");
    assert_eq!(source.asked.get(), asked);
    source.fail.set(true);
    app.decisions.as_mut().unwrap().invalidate(true);
    assert!(app.fill_decisions(&source));
    assert_eq!(app.decision_absent(&id(9)), "decisions not readable");
}

/// Eine Nennung, deren Record nicht im Zeitfenster ihres Aufrufs liegt —
/// eine fremde Id, die ein Kommando neben `minds decide` druckte —, wird
/// nirgends als Entscheidung dieser Session gezeigt: nicht in der Story,
/// nicht im Tab unter SESSIONS, nicht beim Sprung mit `D`. Solange die
/// Records nicht geholt sind, gilt sie als noch nicht prüfbar.
#[test]
fn a_named_id_outside_the_calls_window_is_not_this_sessions_decision() {
    let (_dir, repo) = repo();
    let source = source();
    let mut inspection = deciding();
    let mut sessions = BTreeMap::new();
    for (sid_, s) in inspection.index().sessions() {
        let mut s = s.clone();
        if *sid_ == sid('a') {
            // Dieselbe Stelle wie die echte Nennung, aber der offene Record
            // (14:20:00) liegt außerhalb von 14:12:00–14:12:05.
            let genuine = s.decision_events[0].clone();
            s.decision_events.push(DecisionEvent {
                seq: 7,
                decision_id: id(2),
                turn: genuine.turn,
                call: genuine.call,
            });
        }
        sessions.insert(*sid_, s);
    }
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    inspection = Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "t");
    let mut app = App::new(inspection, &repo, None);
    // Vor dem Holen: beide genannt, keine prüfbar — die Story zeigt die
    // Kurz-Ids mit „… reading".
    assert_eq!(app.linked_decisions(sid('a')), vec![id(1), id(2)]);
    app.reduce(Action::Enter);
    let before = render(&mut app);
    assert!(before.contains("… reading"), "{before}");
    assert!(app.fill_decisions(&source));
    assert_eq!(app.linked_decisions(sid('a')), vec![id(1)]);
    assert_eq!(app.sessions_naming(&id(2)), vec![sid('b')]);
    assert_eq!(app.sessions_naming(&id(1)), vec![sid('a')]);
    let out = render(&mut app);
    assert!(
        out.contains("lock for the witness socket on macOS"),
        "{out}"
    );
    assert!(!out.contains("retry policy for the mirror"), "{out}");
    // Die Story-Zeile des Aufrufs trägt nur die passende Entscheidung.
    assert!(out.contains("⚖ decided"), "{out}");
    assert!(!out.contains("⚖ asked you"), "{out}");
}

/// `D` springt auf die Entscheidung mit Record, auch wenn eine ungeprüfte
/// Nennung (ein `cat notes.md` in derselben Kette) davor steht; und eine
/// Nennung ohne Zug und Aufruf (älterer Record) zählt nie.
#[test]
fn d_prefers_a_decision_with_a_record_and_legacy_names_never_count() {
    let (_dir, repo) = repo();
    let source = source();
    let mut inspection = deciding();
    let mut sessions = BTreeMap::new();
    for (sid_, s) in inspection.index().sessions() {
        let mut s = s.clone();
        if *sid_ == sid('a') {
            let genuine = s.decision_events[0].clone();
            s.decision_events.insert(
                0,
                DecisionEvent {
                    seq: 7,
                    decision_id: id(9),
                    turn: genuine.turn,
                    call: genuine.call,
                },
            );
            s.decision_events.push(DecisionEvent {
                seq: 8,
                decision_id: id(2),
                turn: None,
                call: None,
            });
        }
        sessions.insert(*sid_, s);
    }
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    inspection = Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "t");
    let mut app = App::new(inspection, &repo, None);
    assert_eq!(app.linked_decisions(sid('a')), vec![id(9), id(1)]);
    app.reduce(Action::Enter);
    assert!(app.fill_decisions(&source));
    assert_eq!(app.linked_decisions(sid('a')), vec![id(1)]);
    app.reduce(Action::Decisions);
    assert_eq!(app.tab, Tab::Decisions);
    assert!(render(&mut app).contains("DECISION  ⚖ 01010101"));
}

/// Das Fenster eines Schritts ist das seines Aufrufs, nicht das des ganzen
/// Zugs: Ein Record aus einem früheren Aufruf desselben Zugs gehört nicht
/// zum späteren `minds decide`-Schritt.
#[test]
fn the_steps_window_is_the_calls_not_the_turns() {
    let (_dir, repo) = repo();
    let source = source();
    let mut inspection = deciding();
    let mut sessions = BTreeMap::new();
    for (sid_, s) in inspection.index().sessions() {
        let mut s = s.clone();
        if *sid_ == sid('a') {
            // Zug ab 14:11:30: Aufruf 1 (`cargo test`) 14:11:30–14:14:00,
            // Aufruf 2 (`minds decide`) 14:19:58–14:20:03. Der Record lock()
            // (14:12:00) liegt im Zug, aber nicht im zweiten Aufruf.
            let turn = s.turns.last_mut().unwrap();
            turn.at = Some("2026-07-25T14:11:30Z".into());
            turn.tool_calls[0].at = Some("2026-07-25T14:11:30Z".into());
            turn.tool_calls[0].ended_at = Some("2026-07-25T14:14:00Z".into());
            turn.tool_calls[1].at = Some("2026-07-25T14:19:58Z".into());
            turn.tool_calls[1].ended_at = Some("2026-07-25T14:20:03Z".into());
        }
        sessions.insert(*sid_, s);
    }
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    inspection = Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "t");
    let mut app = App::new(inspection, &repo, None);
    app.reduce(Action::Enter);
    assert!(app.fill_decisions(&source));
    assert!(app.linked_decisions(sid('a')).is_empty());
    let out = text_at(&mut app, 200, 60);
    assert!(!out.contains("⚖ decided"), "{out}");
    assert!(
        !out.contains("lock for the witness socket on macOS"),
        "{out}"
    );
}

/// Die Übersicht: der Commit mit Session a zeigt DECISIONS (1) mit Thema
/// und Stand, sobald die Entscheidungen geholt sind.
#[test]
fn the_overview_detail_lists_the_decisions_of_the_commit() {
    let (dir, _) = repo();
    let git = |args: &[&str]| {
        // Eine Identität nur für diesen Aufruf: Ohne globale Konfiguration
        // (CI) hätte der Commit sonst keinen Autor.
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["commit", "-q", "--allow-empty", "-m", "feat: lock"]);
    let head: CommitId = git(&["rev-parse", "HEAD"]).parse().unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    let base = deciding();
    let mut sessions = BTreeMap::new();
    for (id, s) in base.index().sessions() {
        sessions.insert(*id, s.clone());
    }
    let mut commits = BTreeMap::new();
    commits.insert(head, vec![sid('a')]);
    let inspection = Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "t");
    let source = source();
    let mut app = App::new(inspection, &repo, None);
    app.open_overview();
    assert_eq!(app.tab, Tab::Overview);
    assert!(app.decisions_wanted(), "der Commit trägt eine Entscheidung");
    assert!(app.fill_decisions(&source));
    let out = text_at(&mut app, 220, 48);
    assert!(out.contains("DECISIONS (1)"), "{out}");
    assert!(out.contains("⚖ 01010101"), "{out}");
    assert!(
        out.contains("lock for the witness socket on macOS"),
        "{out}"
    );
    assert!(out.contains("✗ re-evaluate"), "{out}");
}
