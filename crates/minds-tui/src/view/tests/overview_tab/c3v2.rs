//! C3 · Overview v2 — der Einstieg beantwortet drei Fragen: Welcher Commit
//! stammt von einem Agenten, was war der Auftrag, wie weit ist das belegt.
//! Jede Zahl ist konsistent: Was oben steht, lässt sich unten nachzählen.

use super::*;

fn lines_at(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect()
}

// ---------------------------------------------------------------------
// C3.1 · Eine Quelle für alle Zahlen
// ---------------------------------------------------------------------

/// Die Titelzahl „with an agent session" ist die Zahl der Zeilen mit
/// Agent-Badge; die Zahl steht genau einmal — im Boxtitel —, ohne Prozent
/// und ohne „Changes".
#[test]
fn c31_the_title_counts_what_the_list_shows() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    for (w, h) in [(80u16, 24u16), (120, 30), (160, 40)] {
        let lines = lines_at(&mut app, w, h);
        let out = lines.join("\n");
        let title = &lines[2];
        assert!(title.contains("OVERVIEW · 3 commits"), "{w}: {out}");
        let state = app.overview.as_ref().unwrap();
        let counts = state.data.as_ref().unwrap().counts();
        assert!(
            title.contains(&format!(
                "{} with an agent session",
                counts.with_agent_session
            )),
            "{w}: {out}"
        );
        // Nur die Liste links zählt — das Detail rechts nennt die Sessions.
        let badges = lines
            .iter()
            .filter(|l| l.split("││").next().unwrap_or(l).contains("◉CC"))
            .count();
        // Die Session ohne Commit trägt auch ein Agent-Badge.
        let wip = state.data.as_ref().unwrap().wip.len();
        assert_eq!(badges, counts.with_agent_session + wip, "{w}: {out}");
        // Kopf und Titel: keine Prozente, keine Change-Ids, keine Doppelung.
        // (Zeile 1 trägt den Tab „Changes" — gemeint ist die Statistik.)
        for line in &lines[..3] {
            assert!(!line.contains("% of commits"), "{w}: {out}");
            assert!(!line.contains("Changes ·"), "{w}: {out}");
        }
        for line in &lines[1..3] {
            assert!(!line.contains("Changes"), "{w}: {out}");
        }
        assert!(!lines[1].contains("Sessions"), "{w}: {out}");
        assert_eq!(
            out.matches("with an agent session").count(),
            1,
            "{w}: {out}"
        );
    }
}

/// Kopfzeile 2 ist nur der Breadcrumb — die Statistik ist aus dem Kopf
/// verschwunden, in jedem Tab.
#[test]
fn c31_the_second_header_line_is_only_the_breadcrumb() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let lines = lines_at(&mut app, 300, 30);
    assert_eq!(lines[1].trim_end(), app.breadcrumb().join(" › "));
    // Die Zahl der Sessions bleibt erreichbar — im Rahmen der Liste.
    assert!(
        lines.iter().any(|l| l.contains(" SESSIONS · 2 captured")),
        "{}",
        lines.join("\n")
    );
}

/// Gekappt: Der Titel sagt bei der Zahl, dass nur das Gelesene zählt.
#[test]
fn c31_a_capped_title_says_what_was_read() {
    use minds_reader::overview::OverviewCounts;
    let counts = OverviewCounts {
        commits: 500,
        with_agent_session: 7,
        without_agent_session: 493,
        sessions_without_commit: 0,
        limited: true,
    };
    let text = crate::view::overview::counts_title(&counts, false);
    assert_eq!(text, "500 commits read (limit) · 7 with an agent session");
    let text = crate::view::overview::counts_title(&counts, true);
    assert_eq!(
        text,
        "500 commits read (read budget reached) · 7 with an agent session"
    );
    let open = OverviewCounts {
        limited: false,
        commits: 4,
        with_agent_session: 1,
        without_agent_session: 3,
        ..counts
    };
    assert_eq!(
        crate::view::overview::counts_title(&open, false),
        "4 commits · 1 with an agent session"
    );
    assert!(!crate::view::overview::counts_title(&open, false).contains('%'));
}

// ---------------------------------------------------------------------
// C3.2 · Die Liste: Betreff vorn, Auftrag sichtbar, Status leise
// ---------------------------------------------------------------------

/// Das Raster der Liste — die Zeilen links der Grenze zum Detail.
fn list_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.split("││").next().unwrap_or(l).to_string())
        .collect()
}

/// Spalten: Lanes · Badge · Betreff · Status · Refs · Zeit. Agent-Commits
/// tragen eine zweite, gedimmte Zeile mit dem Auftrag; menschliche bleiben
/// einzeilig. Der Graph läuft mit `│` an der zweiten Zeile vorbei.
#[test]
fn c32_subject_status_refs_time_and_the_request_below() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let lines = list_lines(&lines_at(&mut app, 160, 40));
    let out = lines.join("\n");
    let at = lines
        .iter()
        .position(|l| l.contains("feat(sort): agent work"))
        .expect("agent row");
    let row = &lines[at];
    let subject = row.find("feat(sort): agent work").unwrap();
    let status = row.find("· unsealed").expect("status");
    // Frisch angelegt: „just now" — sonst „3h ago" (dieselben Wörter wie
    // die Sessions-Liste, C4).
    let time = row
        .find("just now")
        .or_else(|| row.find(" ago"))
        .expect("relative time");
    assert!(subject < status && status < time, "{out}");
    // Der Auftrag steht nicht mehr in der Zeile, sondern darunter.
    assert!(!row.contains("Fix retry handling"), "{out}");
    assert!(!out.contains("• Fix retry handling"), "{out}");
    let below = &lines[at + 1];
    assert!(below.contains("Fix retry handling"), "{out}");
    assert!(
        below.trim_start_matches(['│', ' ']).starts_with("│") || below.contains("│ "),
        "{out}"
    );
    // HEAD-Zeile: Refs als leise Textspalte zwischen Status und Zeit.
    let head = lines
        .iter()
        .find(|l| l.contains("docs: two sessions"))
        .unwrap();
    let refs = head.find("@main ▲ HEAD").expect("refs column");
    assert!(head.find("◉2").unwrap() < refs, "{out}");
    assert!(refs < head.find("just now").unwrap(), "{out}");
    // Der menschliche Commit bleibt einzeilig.
    let human = lines
        .iter()
        .position(|l| l.contains("init: empty"))
        .unwrap();
    assert!(
        lines[human + 1]
            .trim_matches(['│', ' ', '└', '─', '┘'])
            .is_empty(),
        "{out}"
    );
    // Keine farbigen Pillen mehr: der Auftrag nicht hinter einem `•`, das
    // Requirement-Zeichen ohne Pillen-Wort.
    assert!(out.contains("⚑"), "{out}");
    assert!(!out.contains("⚑ intent"), "{out}");
}

/// Bei 120 Spalten (geteilt) bekommt der Betreff mindestens 30 Zeichen, und
/// die Status-Spalte bleibt vollständig; bei 80 und 160 ebenso vollständig.
#[test]
fn c32_the_subject_gets_thirty_columns_at_120() {
    let (dir, _, _, _) = history();
    let long = "feat(sort): a stable sort that keeps equal keys in input order";
    git(dir.path(), &["commit", "-q", "--allow-empty", "-m", long]);
    let head: CommitId = git(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid('a'),
        session("Fix retry handling", "2026-07-25T14:10:00Z"),
    );
    sessions.insert(sid('c'), session("Write the docs", "2026-07-25T15:00:00Z"));
    let mut links = BTreeMap::new();
    links.insert(head, vec![sid('a'), sid('c')]);
    let inspection = Inspection::from_index(Index::from_parts(sessions, links), Vec::new(), "demo");
    let mut app = opened(&repo, inspection);
    for (w, h) in [(80u16, 24u16), (120, 30), (160, 40)] {
        let lines = list_lines(&lines_at(&mut app, w, h));
        let out = lines.join("\n");
        let row = lines
            .iter()
            .find(|l| l.contains("feat(sort): a stable"))
            .unwrap_or_else(|| panic!("{w}: {out}"));
        assert!(row.contains("· unsealed"), "{w}: {out}");
        assert!(row.contains("◉2"), "{w}: {out}");
        if w >= 120 {
            let shown: String = long.chars().take(30).collect();
            assert!(row.contains(&shown), "{w}: {out}");
        }
    }
}

/// Ein Auswahlbalken über die volle Zeile (beide Zeilen beim Agent-Commit):
/// kein Span der Auswahl hat einen eigenen Hintergrund.
#[test]
fn c32_the_selection_is_one_bar() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text = |y: u16| -> String {
        (0..160)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    };
    let y = (0..40)
        .find(|y| text(*y).contains("docs: two sessions"))
        .unwrap();
    // Die Liste endet an ihrer rechten Rahmenkante (das zweite `│`).
    let edge = text(y)
        .chars()
        .enumerate()
        .filter(|(_, c)| *c == '│')
        .nth(1)
        .map(|(x, _)| x as u16)
        .unwrap();
    for row in [y, y + 1] {
        for x in 1..edge {
            let cell = &buffer[(x, row)];
            assert_eq!(
                cell.bg,
                ratatui::style::Color::Reset,
                "bg at {x},{row}: {}",
                text(row)
            );
            assert!(
                cell.modifier.contains(ratatui::style::Modifier::REVERSED),
                "cursor at {x},{row}: {}",
                text(row)
            );
        }
    }
    // Keine ▌-Fragmente.
    assert!(!(0..40).any(|y| text(y).contains('▌')));
}

/// Review „open" ist der Normalzustand und leise; ein freigegebener Commit
/// sagt es — mit dem Glyph des Registers (`⊕`, nicht `✓`: `✓` heißt
/// „Prüfung bestanden").
#[test]
fn c32_open_is_quiet_and_approved_is_said() {
    let (_dir, repo, commits, _) = history();
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid('a'),
        session("Fix retry handling", "2026-07-25T14:10:00Z"),
    );
    sessions.insert(sid('c'), session("Write the docs", "2026-07-25T15:00:00Z"));
    let mut links = BTreeMap::new();
    links.insert(commits[2], vec![sid('a')]);
    links.insert(commits[1], vec![sid('c')]);
    let review = minds_core::Review::new(
        minds_core::Subject::Session(sid('a').to_string()),
        minds_core::Decision::Approve,
        "pd",
        "ok",
        Some("2026-07-26T00:00:00Z".into()),
    );
    let inspection =
        Inspection::from_index(Index::from_parts(sessions, links), vec![review], "demo");
    let mut app = opened(&repo, inspection);
    let lines = list_lines(&lines_at(&mut app, 160, 40));
    let out = lines.join("\n");
    assert!(!out.contains("⚠ open"), "{out}");
    let approved = lines
        .iter()
        .find(|l| l.contains("docs: two sessions"))
        .unwrap();
    assert!(approved.contains("⊕ approved"), "{out}");
}
