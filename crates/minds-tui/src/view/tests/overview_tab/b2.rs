//! B2 · Rückweg über Tab-Grenzen und Breadcrumb.

use super::*;

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

/// Das AC der Spec: ein Tab mit Zeile → Enter (Session) → `e` → Esc → Esc
/// landet wieder dort auf derselben Zeile. (Bis C10 war die zweite Ebene
/// die Why-Kette, jetzt der Evidence-Report; bis C12.2 war der Ursprung
/// der Verify-Tab, der in der Übersicht aufgegangen ist — der Rückweg ist
/// derselbe.)
#[test]
fn b2_esc_returns_across_tabs_to_the_same_row() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    select_commit_with_sessions(&mut app);
    let row = app.overview.as_ref().unwrap().cursor;
    let commit = overview_commit(&app);
    app.reduce(Action::Enter);
    // Mehrere Sessions: erst wählen (Fokus rechts), dann Enter.
    if app.tab == Tab::Overview {
        app.reduce(Action::Enter);
    }
    assert_eq!(app.tab, Tab::Sessions);
    assert!(matches!(app.top(), Some(View::Story { .. })));
    app.reduce(Action::Evidence);
    assert!(matches!(app.top(), Some(View::Evidence { .. })));
    let out = render_sized(&mut app, 160, 40);
    let crumb = out.lines().nth(1).unwrap();
    assert!(crumb.contains("Overview"), "{out}");
    assert!(crumb.contains("› Session b3-"), "{out}");
    // Der Pfad endet am Ziel: … › Session … › Evidence (die Statistik
    // rechts daneben ist kein Teil des Pfads).
    assert!(crumb.contains("› Evidence"), "{crumb}");
    assert!(
        crumb.find("Overview").unwrap() < crumb.find("› Evidence").unwrap(),
        "{crumb}"
    );
    app.reduce(Action::Back); // Evidence schließt
    assert!(matches!(app.top(), Some(View::Story { .. })));
    app.reduce(Action::Back); // die Ebene des Sprungs: zurück zum Ursprung
    assert_eq!(app.tab, Tab::Overview);
    assert_eq!(app.overview.as_ref().unwrap().cursor, row);
    assert_eq!(overview_commit(&app), commit);
    // Kein weiterer Ursprung: nichts.
    app.reduce(Action::Back);
    assert_eq!(app.tab, Tab::Overview);
    assert!(!app.quit);
}

/// Der Commit der gewählten Zeile.
fn overview_commit(app: &App) -> Option<CommitId> {
    match app.overview.as_ref()?.selected()? {
        crate::overview::Selected::Commit(row) => Some(row.id),
        crate::overview::Selected::Wip(_) => None,
    }
}

fn select_commit_with_sessions(app: &mut App) {
    for _ in 0..20 {
        if matches!(
            app.overview.as_ref().and_then(|s| s.selected()),
            Some(crate::overview::Selected::Commit(row)) if !row.sessions.is_empty()
        ) {
            return;
        }
        app.reduce(Action::Down);
    }
    panic!("no commit with a session");
}

/// Der Breadcrumb wird von links gekürzt — das Ziel bleibt lesbar.
#[test]
fn b2_the_breadcrumb_is_shortened_from_the_left() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    // Overview → Enter: die Sessions des Commits bzw. der Graph.
    select_commit_with_sessions(&mut app);
    app.reduce(Action::Enter);
    if app.tab == Tab::Overview {
        app.reduce(Action::Enter);
    }
    assert_eq!(app.tab, Tab::Sessions);
    app.reduce(Action::Evidence);
    let out = render_sized(&mut app, 30, 24);
    let crumb = out.lines().nth(1).unwrap().trim_matches('"').trim_end();
    assert!(crumb.starts_with('…'), "{crumb}");
    assert!(crumb.ends_with("› Evidence"), "{crumb}");
    let out = render_sized(&mut app, 160, 40);
    let crumb = out.lines().nth(1).unwrap();
    assert!(crumb.contains("Overview ›"), "{crumb}");
}
