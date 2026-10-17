//! C11 · Session Story: die Schrittliste ersetzt den Graph-Baum — die
//! Akzeptanzproben der Spec mit einer Demo-nahen Fixture (Prompt,
//! Kommando, Schreibung mit absolutem Pfad, Testlauf mit Ergebnis,
//! Delegation an einen Sub-Agent, Modell nicht erfasst, Review offen).

use super::*;
use crate::app::Tab;
use crate::layout::RowKind;
use minds_core::{
    Capture, CaptureStatus, Edge, EdgeKind, Endpoint, ExecClass, ExecOutcome, TestCounts,
};

const CWD: &str = "/Users/pd/dev/minds-demo";

fn call(name: &str, args: &str, effect: Option<Effect>) -> ToolCall {
    ToolCall {
        name: name.into(),
        arguments: args.into(),
        capture: Some(Capture {
            status: CaptureStatus::Interpreted,
            adapter: "claude-code".into(),
            adapter_version: 3,
            note: None,
        }),
        effect,
        outcome: None,
    }
}

fn exec(command: &str) -> ToolCall {
    call(
        "Bash",
        &format!(
            "{{\"command\":{}}}",
            serde_json::to_string(command).unwrap()
        ),
        Some(Effect {
            kind: EffectKind::Exec,
            path: None,
            content: None,
            written: None,
            written_unavailable: None,
        }),
    )
}

/// Die Demo-Session: lesen, schreiben, testen, delegieren — das Modell
/// nicht erfasst (Platzhalter `unknown`), das Transkript nicht gelesen.
fn demo() -> Inspection {
    let mut s = session(
        "Implementiere die Anforderung aus anforderung.md.\nNicht committen.",
        "2026-10-16T10:37:12Z",
    );
    s.model.id = "unknown".into();
    let mut lineage = Lineage::new("p");
    lineage.cwd = Some(CWD.into());
    lineage.started_at = Some("2026-10-16T10:37:12Z".into());
    lineage.ended_at = Some("2026-10-16T10:40:12Z".into());
    lineage.transcript = Some(minds_core::TranscriptStatus::NotReadByWitness);
    s.lineage = Some(lineage);
    s.turns.clear();
    s.turns.push(Turn {
        role: Role::User,
        text: "Implementiere die Anforderung aus anforderung.md.".into(),
        tool_calls: vec![],
        parent: None,
        at: Some("2026-10-16T10:37:12Z".into()),
    });
    let mut tests = exec("python -m unittest discover -s src");
    tests.outcome = Some(ExecOutcome {
        class: ExecClass::Test,
        runner: "unittest".into(),
        command: vec!["python".into(), "-m".into(), "unittest".into()],
        cwd: Some(".".into()),
        exit_code: None,
        tests: Some(TestCounts {
            passed: 8,
            failed: 0,
            ignored: 0,
        }),
        benches: Vec::new(),
    });
    s.turns.push(Turn {
        role: Role::Assistant,
        text: "Ich lese zuerst, dann schreibe ich.".into(),
        tool_calls: vec![
            exec(&format!("cd {CWD} && cat anforderung.md")),
            call(
                "Write",
                &format!("{{\"file_path\":\"{CWD}/src/sort.py\"}}"),
                Some(Effect {
                    kind: EffectKind::Write,
                    path: Some(format!("{CWD}/src/sort.py")),
                    content: None,
                    written: Some(minds_core::ContentHash::from_bytes([7u8; 32])),
                    written_unavailable: None,
                }),
            ),
            tests,
            call(
                "Task",
                "{\"description\":\"Review the tests\",\"prompt\":\"Lies src/sort.py\"}",
                None,
            ),
        ],
        parent: None,
        at: Some("2026-10-16T10:38:40Z".into()),
    });
    s.edges.push(Edge {
        kind: EdgeKind::Spawned,
        to: Endpoint::Session {
            agent: "claude-code".into(),
            local_id: "child".into(),
        },
        evidence: EvidenceMark::of(EvidenceSource::Observed),
    });
    let mut child = session("Review the tests", "2026-10-16T10:39:00Z");
    child.lineage = Some(Lineage::new("child"));
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), s);
    sessions.insert(sid('b'), child);
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "demo")
}

/// Öffnet die Story der Eltern-Session `a` — die Liste zeigt die jüngere
/// Kind-Session zuerst.
fn open_parent(app: &mut App) {
    while app.selected().map(|c| c.id) != Some(sid('a')) {
        app.reduce(Action::Down);
    }
    app.reduce(Action::Enter);
    assert!(matches!(app.top(), Some(View::Story { id, .. }) if *id == sid('a')));
}

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

/// Render 160×40: Verben korrekt, Testergebnis am Testlauf, kein absoluter
/// Pfad, der Prompt genau einmal, kein `unknown`, kein `⚠` am Review.
#[test]
fn c11_the_story_renders_steps_with_verbs_outcomes_and_no_absolute_path() {
    let (_dir, repo) = repo();
    let mut app = App::new(demo(), &repo, None);
    open_parent(&mut app);
    let out = render_sized(&mut app, 160, 40);
    let detail = detail_of(&out);
    assert!(detail.contains("STEPS (4)"), "{out}");
    for (verb, target) in [
        ("▶ ran", "cat anforderung.md"),
        ("✎ wrote", "src/sort.py"),
        ("▶ tests", "python -m unittest discover -s src"),
        ("◉ delegated", "Review the tests"),
    ] {
        let row = detail
            .lines()
            .find(|l| l.contains(verb) && l.contains(target));
        assert!(row.is_some(), "{verb} {target}\n{out}");
    }
    // Das Ergebnis nur, wo der Record eines kennt: am Testlauf, nicht am
    // Kommando daneben.
    let tests = detail.lines().find(|l| l.contains("▶ tests")).unwrap();
    assert!(tests.contains("✓ 8 passed"), "{out}");
    let ran = detail.lines().find(|l| l.contains("▶ ran")).unwrap();
    assert!(!ran.contains('✓') && !ran.contains('✗'), "{out}");
    // Die Schreibung ist berichtet, nicht bezeugt.
    let wrote = detail.lines().find(|l| l.contains("✎ wrote")).unwrap();
    assert!(wrote.contains("◇ reported"), "{out}");
    // Kein absoluter Pfad, nirgends (PII).
    assert!(!out.contains("/Users/"), "{out}");
    // Der Prompt genau einmal — nicht in YOU-Box, Knoten und TURN.
    assert_eq!(
        detail.matches("Implementiere die Anforderung").count(),
        1,
        "{out}"
    );
    assert!(detail.contains("PROMPT"), "{out}");
    // Nie `unknown`; der Grund steht (K1).
    assert!(!out.contains("unknown"), "{out}");
    assert!(out.contains("model not recorded"), "{out}");
    // Review offen ist der Normalzustand — nie ⚠.
    let result = detail.lines().find(|l| l.contains("RESULT")).unwrap();
    assert!(result.contains("◦ not reviewed yet"), "{out}");
    assert!(!result.contains('⚠'), "{out}");
    assert!(result.contains("commit 1111111"), "{out}");
    // Kein Baum, keine Zeitleiste, kein Absolutpfad.
    assert!(!out.contains("┣━"), "{out}");
    assert!(!out.contains("TIMELINE"), "{out}");
    // Zeit seit Start.
    assert!(detail.contains("+1:28"), "{out}");
    assert!(detail.contains("started 10:37:12Z"), "{out}");
}

/// Die Sub-Agent-Zeile hängt eingerückt unter dem delegierenden Schritt;
/// `Enter` öffnet die Kind-Story, `Esc` führt zurück.
#[test]
fn c11_a_subagent_is_a_nested_row_and_enter_opens_its_story_esc_returns() {
    let (_dir, repo) = repo();
    let mut app = App::new(demo(), &repo, None);
    open_parent(&mut app);
    let child_row = match app.top() {
        Some(View::Story { rows, .. }) => rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::Child { .. }))
            .expect("a child row"),
        _ => unreachable!(),
    };
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("↳ delegated → 3 steps"), "{out}");
    for _ in 0..child_row {
        app.reduce(Action::Down);
    }
    app.reduce(Action::Enter);
    assert!(
        matches!(app.top(), Some(View::Story { id, .. }) if *id == sid('b')),
        "the child's story"
    );
    assert_eq!(app.views.len(), 2);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("SESSION b3-bbbbbbbb…"), "{out}");
    app.reduce(Action::Back);
    assert!(
        matches!(app.top(), Some(View::Story { id, cursor, .. }) if *id == sid('a') && *cursor == child_row),
        "back on the child row of the parent"
    );
}

/// `Enter` auf einem Schreibschritt öffnet den Changes-Tab des Commits auf
/// der Datei — oder sagt, dass sie nicht im Diff liegt (hier: leeres Repo).
#[test]
fn c11_enter_on_a_written_file_opens_the_changes_tab() {
    let (_dir, repo) = repo();
    let mut app = App::new(demo(), &repo, None);
    open_parent(&mut app);
    app.reduce(Action::Down); // 2: wrote src/sort.py
    app.reduce(Action::Enter);
    assert_eq!(app.tab, Tab::Changes);
    assert_eq!(app.changes.as_ref().unwrap().commit(), Some(commit('1')));
    assert!(
        app.closed
            .as_deref()
            .is_some_and(|c| c.contains("src/sort.py")),
        "{:?}",
        app.closed
    );
    // Esc führt über den Verlauf zurück in die Story (B2).
    app.reduce(Action::Back);
    assert_eq!(app.tab, Tab::Sessions);
    assert!(matches!(app.top(), Some(View::Story { .. })));
}

/// `Enter` auf RESULT öffnet den Diff des Commits; auf einem Kommando sagt
/// die Fußzeile, dass es nichts zu öffnen gibt (B4).
#[test]
fn c11_enter_on_result_opens_the_diff_and_on_a_command_says_so() {
    let (_dir, repo) = repo();
    let mut app = App::new(demo(), &repo, None);
    open_parent(&mut app);
    app.reduce(Action::Enter); // 1: ran
    assert_eq!(app.tab, Tab::Sessions);
    assert!(
        app.closed
            .as_deref()
            .is_some_and(|c| c.contains("nothing to open")),
        "{:?}",
        app.closed
    );
    app.reduce(Action::End);
    app.reduce(Action::Enter);
    assert_eq!(app.tab, Tab::Changes);
    assert_eq!(app.changes.as_ref().unwrap().commit(), Some(commit('1')));
}

/// `t` ist frei (C11): keine Zeitleiste mehr — die Story ist sie.
#[test]
fn c11_t_is_free_and_there_is_no_timeline() {
    let t = crate::input::map(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('t'),
            crossterm::event::KeyModifiers::NONE,
        ),
        false,
    );
    assert_eq!(t, Action::None);
    let (_dir, repo) = repo();
    let mut app = App::new(demo(), &repo, None);
    open_parent(&mut app);
    app.reduce(t);
    assert!(matches!(app.top(), Some(View::Story { cursor: 0, .. })));
    let out = render_sized(&mut app, 160, 40);
    assert!(!out.contains("TIMELINE"), "{out}");
    assert!(!out.contains("t timeline"), "{out}");
}

/// Zoom 1 fasst gleichartige Folgen zusammen; der Cursor bleibt auf dem
/// Schritt (B5).
#[test]
fn c11_summary_zoom_folds_runs_and_keeps_the_step() {
    let (_dir, repo) = repo();
    let mut app = App::new(busy_tests(), &repo, None);
    app.reduce(Action::Enter);
    app.reduce(Action::Down);
    app.reduce(Action::Down); // 3: der dritte Testlauf
    app.reduce(Action::Zoom(1));
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("3 test runs"), "{out}");
    assert!(out.contains("✓ 3 passed"), "last outcome\n{out}");
    assert!(
        matches!(app.top(), Some(View::Story { rows, cursor, .. })
            if matches!(rows[*cursor].kind, RowKind::Run { first: 0, count: 3, .. })),
        "the run holds the step"
    );
}

/// Drei Testläufe hintereinander, das letzte Ergebnis 3 passed.
fn busy_tests() -> Inspection {
    let mut s = session("Run the tests", "2026-10-16T10:37:12Z");
    s.turns[0].tool_calls.clear();
    for passed in 1..=3 {
        let mut t = exec("pytest -q");
        t.outcome = Some(ExecOutcome {
            class: ExecClass::Test,
            runner: "pytest".into(),
            command: vec!["pytest".into(), "-q".into()],
            cwd: None,
            exit_code: None,
            tests: Some(TestCounts {
                passed,
                failed: 0,
                ignored: 0,
            }),
            benches: Vec::new(),
        });
        s.turns[0].tool_calls.push(t);
    }
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), s);
    Inspection::from_index(
        Index::from_parts(sessions, BTreeMap::new()),
        Vec::new(),
        "demo",
    )
}
