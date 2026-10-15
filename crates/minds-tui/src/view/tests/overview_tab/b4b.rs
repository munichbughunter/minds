//! B4(b) · `/` global: in der Übersicht filtert es Commits.

use super::*;

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

#[test]
fn b4b_slash_filters_the_overview_commits() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("init: empty"), "{out}");
    app.reduce(Action::SearchStart);
    for c in "sort".chars() {
        app.reduce(Action::SearchInput(c));
    }
    app.reduce(Action::SearchCommit);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("feat(sort): agent work"), "{out}");
    assert!(!out.contains("init: empty"), "filtered out\n{out}");
    assert!(out.contains("filter: sort"), "{out}");
    // Die Auswahl steht auf einem Treffer.
    match app.overview.as_ref().and_then(|s| s.selected()) {
        Some(crate::overview::Selected::Commit(row)) => {
            assert!(row.subject.contains("sort"), "{}", row.subject)
        }
        other => panic!("{:?}", other.is_some()),
    }
    // Esc leert den Filter (B1, Schritt 2) — die Sessions-Suche bleibt leer.
    app.reduce(Action::Back);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("init: empty"), "{out}");
    assert!(app.query.is_empty());
}
