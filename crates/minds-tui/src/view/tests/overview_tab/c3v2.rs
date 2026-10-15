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
