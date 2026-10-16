//! C9.3 · Why-Box in fester Reihenfolge, mit Satz und Brücke
//! (docs/spikes/auftrag-batch-2.md, Paket 5).

use minds_reader::graph::{NodeKind, ToolKind};

use super::*;
use crate::theme;

/// Der Render ohne die zwei Fußzeilen — die Tasten stehen dort, nie im Panel.
fn panel(out: &str) -> String {
    let lines: Vec<&str> = out.lines().collect();
    lines[..lines.len().saturating_sub(2)].join("\n")
}

/// Die Byte-Position von `needle` in `out`, oder ein sprechender Fehler.
fn at(out: &str, needle: &str) -> usize {
    out.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} missing:\n{out}"))
}

/// Die App mit dem Cursor auf der `◇ reported`-Zeile (`sort_by_key`).
fn on_the_reported_line(repo: &Repo) -> App<'_> {
    let mut app = app_with_changes(repo);
    app.reduce(Action::Down);
    app.reduce(Action::Down);
    assert_eq!(app.changes.as_ref().unwrap().row, 3);
    app
}

/// Kopf, Satz, dann WRITTEN BY · SESSION · ASKED FOR · AGENT SAID · REVIEW in
/// dieser Reihenfolge; REQUIREMENT fehlt, weil die Session keinen Anker
/// nennt; keine Tastenhinweise; Commit und Change-Id nur als Schlusszeile.
#[test]
fn c93_the_why_box_has_a_fixed_order_and_no_key_hints() {
    let (_dir, repo) = repo();
    let mut app = on_the_reported_line(&repo);
    let out = panel(&render_at(&mut app, 260));
    let head = at(&out, "src/sort/mod.rs:2  ◇ REPORTED");
    let sentence = at(&out, "reported this content");
    let written = at(&out, "WRITTEN BY   ");
    let session = at(&out, "SESSION      ");
    let asked = at(&out, "ASKED FOR    ");
    let said = at(&out, "AGENT SAID   ");
    let review = at(&out, "REVIEW       ");
    // Die Fixture hat keine Change-Id: nur der Commit steht in der Schlusszeile.
    let tail = at(&out, "commit 1111111");
    assert!(head < sentence && sentence < written, "{out}");
    assert!(written < session && session < asked, "{out}");
    assert!(asked < said && said < review && review < tail, "{out}");
    assert!(!out.contains("REQUIREMENT"), "no anchor named:\n{out}");
    for hint in ["] / [", "Enter", "Esc", "press", "jump between hunks"] {
        assert!(!out.contains(hint), "{hint} is a key hint:\n{out}");
    }
    // Die alten Labels und der alte Kopf sind weg.
    for old in [
        "REPORTED ONLY",
        "Session  ",
        "Agent    ",
        "Step     ",
        "Intent   anchor",
    ] {
        assert!(!out.contains(old), "{old} is the old form:\n{out}");
    }
}

/// WRITTEN BY nennt Agent-Badge, Turn, Uhrzeit mit Relativangabe und das
/// Werkzeug mit Datei und Zeilenzahl; SESSION die Kurzform der Übersicht
/// mit dem Record-Zustand; ASKED FOR und AGENT SAID zitieren als Aussage.
#[test]
fn c93_written_by_session_asked_for_and_agent_said() {
    let (_dir, repo) = repo();
    let mut app = on_the_reported_line(&repo);
    let out = render_at(&mut app, 260);
    assert!(
        out.contains("WRITTEN BY   ◉CC claude-code · turn 1 · 14:10:00Z · "),
        "{out}"
    );
    assert!(out.contains("ago"), "{out}");
    assert!(!out.contains("2026-07-25T14:10:00Z"), "no RFC 3339:\n{out}");
    assert!(out.contains("Edit src/sort/mod.rs (1 line)"), "{out}");
    assert!(out.contains("SESSION      b3-aaaaaaaa · "), "{out}");
    assert!(out.contains("record "), "{out}");
    assert!(
        out.contains("ASKED FOR    ◌ \"Fix retry handling\""),
        "{out}"
    );
    assert!(
        out.contains("AGENT SAID   ◌ \"Ich lese und ändere.\""),
        "{out}"
    );
}

/// Jeder Zustand hat seinen Satz; die Brücke „With a witness …" steht nur
/// bei `reported` und `fs only`.
#[test]
fn c93_each_state_has_its_sentence_and_the_bridge_only_where_a_witness_would_help() {
    let (_dir, repo) = repo();
    let bridge = "With a witness this line would be ● observed.";
    let with_class = |class: Option<ReconClass>| {
        let mut app = on_the_reported_line(&repo);
        if let Ok(set) = &mut app.changes.as_mut().unwrap().set {
            set.files[0].rows[3].class = class;
        }
        panel(&render_at(&mut app, 260))
    };
    let reported = with_class(Some(ReconClass::ReportedOnly));
    assert!(reported.contains("◇ REPORTED"), "{reported}");
    assert!(
        reported.contains("The agent's Edit tool reported this content"),
        "{reported}"
    );
    assert!(reported.contains("No witness observed"), "{reported}");
    assert!(reported.contains(bridge), "{reported}");

    let fs_only = with_class(Some(ReconClass::ExplainedFsOnly));
    assert!(fs_only.contains("◍ FS ONLY"), "{fs_only}");
    assert!(fs_only.contains("tool call claims this line"), "{fs_only}");
    assert!(fs_only.contains(bridge), "{fs_only}");

    let observed = with_class(Some(ReconClass::Explained));
    assert!(observed.contains("● OBSERVED"), "{observed}");
    assert!(
        observed.contains("A witness observed this content"),
        "{observed}"
    );
    assert!(!observed.contains(bridge), "{observed}");

    // Nicht beobachtet: der bestehende Satz und der Grund, wörtlich.
    let open = with_class(Some(ReconClass::Unexplained));
    assert!(open.contains("◦ NOT OBSERVED"), "{open}");
    assert!(
        open.contains("No evidence of the sessions linked"),
        "{open}"
    );
    assert!(open.contains("Minds cannot attest this line."), "{open}");
    assert!(!open.contains(bridge), "{open}");
}

/// Review ohne Gelb: `◦ not reviewed yet` im Panel und in Fußzeile 1.
#[test]
fn c93_review_reads_not_reviewed_yet_without_yellow() {
    let (_dir, repo) = repo();
    let mut app = on_the_reported_line(&repo);
    let out = render_at(&mut app, 260);
    assert!(out.contains("REVIEW       ◦ not reviewed yet"), "{out}");
    assert!(!out.contains("⚠ open"), "{out}");
    let footer = out.lines().rev().nth(1).unwrap();
    assert!(footer.contains("◦ not reviewed yet"), "{footer}");
    let buffer = render_buffer(&mut app);
    let y = buffer.area.height - 2;
    let line: String = (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect();
    let x = line[..line.find("not reviewed").unwrap()].chars().count() as u16;
    assert_ne!(buffer[(x, y)].fg, theme::REVIEW, "never yellow");
}

/// Die Schlusszeile ist gedimmt und trägt Commit und Change-Id in Kurzform.
#[test]
fn c93_the_last_line_names_commit_and_change_dimmed() {
    let (_dir, repo) = repo();
    let mut app = on_the_reported_line(&repo);
    if let Ok(set) = &mut app.changes.as_mut().unwrap().set {
        set.change = Some(format!("I{}", "d".repeat(40)).parse().unwrap());
    }
    let buffer = render_buffer(&mut app);
    let needle = "commit 1111111 · change Idddddddddddd…";
    let (x, y) = (0..buffer.area.height)
        .find_map(|y| {
            let line: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect();
            line.find(needle)
                .map(|b| (line[..b].chars().count() as u16, y))
        })
        .unwrap_or_else(|| panic!("{needle} not drawn"));
    assert_eq!(buffer[(x, y)].fg, theme::DIM);
}

/// `Enter` auf einer Codezeile öffnet den Graph der Session mit dem Cursor
/// auf diesem Tool-Aufruf; `Esc` führt über den Verlauf zurück.
#[test]
fn c93_enter_opens_the_graph_on_the_tool_call_and_esc_returns() {
    let (_dir, repo) = repo();
    let mut app = on_the_reported_line(&repo);
    render_at(&mut app, 200);
    app.reduce(Action::Enter);
    assert_eq!(app.tab, Tab::Sessions);
    let Some(View::Graph {
        id, rows, cursor, ..
    }) = app.top()
    else {
        panic!("a graph is open");
    };
    assert_eq!(*id, sid('a'));
    assert_eq!(
        rows[*cursor].kind,
        NodeKind::Tool(ToolKind::Edit),
        "{rows:?}"
    );
    assert!(rows[*cursor].label.contains("src/http/retry.rs"));
    app.reduce(Action::Back);
    assert_eq!(app.tab, Tab::Changes);
    assert_eq!(app.changes.as_ref().unwrap().row, 3);
    assert_eq!(app.changes.as_ref().unwrap().focus, Focus::Diff);
}

/// Eine Zeile ohne aufgezeichneten Aufruf sagt es auf `Enter`, statt still
/// nichts zu tun (B4).
#[test]
fn c93_enter_on_a_line_without_a_call_says_so() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    render_at(&mut app, 200);
    assert_eq!(app.changes.as_ref().unwrap().row, 1);
    app.reduce(Action::Enter);
    assert_eq!(app.tab, Tab::Changes);
    assert!(
        app.closed
            .as_deref()
            .is_some_and(|s| s.contains("no tool call")),
        "{:?}",
        app.closed
    );
}

/// `10:39:09Z` aus dem RFC-3339-Zeitstempel; nichts aus Unlesbarem.
#[test]
fn c93_clock_z_is_the_time_of_day_in_utc() {
    assert_eq!(
        crate::view::clock_z(Some("2026-10-02T10:39:09Z")).as_deref(),
        Some("10:39:09Z")
    );
    assert_eq!(
        crate::view::clock_z(Some("2026-10-02T10:39:09.123Z")).as_deref(),
        Some("10:39:09Z")
    );
    assert_eq!(crate::view::clock_z(Some("garbage")), None);
    assert_eq!(crate::view::clock_z(None), None);
}
