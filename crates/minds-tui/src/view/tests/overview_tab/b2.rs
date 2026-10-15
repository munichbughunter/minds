//! B2 · Rückweg über Tab-Grenzen und Breadcrumb.

use super::*;

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

/// Die Verify-Zeile einer Session (sonst `None`).
fn session_row(app: &App) -> Option<usize> {
    let state = app.verify.as_ref()?;
    (!state.sessions.is_empty()).then_some(0)
}

/// Das AC der Spec: Verify → Enter (Session) → `w` → Esc → Esc landet wieder
/// in Verify auf derselben Zeile.
#[test]
fn b2_esc_returns_across_tabs_to_the_same_row() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    // Ein Commit mit Session wählen (B3: F3 öffnet den aktiven Commit).
    select_commit_with_sessions(&mut app);
    app.reduce(Action::TabTo(2));
    let row = session_row(&app).expect("a commit with a session");
    app.verify.as_mut().unwrap().cursor = row;
    let commit = app.verify.as_ref().unwrap().commit();
    app.reduce(Action::Enter);
    assert_eq!(app.tab, Tab::Sessions);
    app.reduce(Action::Why);
    assert!(matches!(app.top(), Some(View::Why { .. })));
    let out = render_sized(&mut app, 160, 40);
    let crumb = out.lines().nth(1).unwrap();
    assert!(crumb.contains("Verify"), "{out}");
    assert!(crumb.contains("› Session b3-"), "{out}");
    // Der Pfad endet am Ziel: … › Session … › Why (die Statistik rechts
    // daneben ist kein Teil des Pfads).
    assert!(crumb.contains("› Why"), "{crumb}");
    assert!(
        crumb.find("Verify").unwrap() < crumb.find("› Why").unwrap(),
        "{crumb}"
    );
    app.reduce(Action::Back); // Why schließt
    assert!(matches!(app.top(), Some(View::Graph { .. })));
    app.reduce(Action::Back); // die Ebene des Sprungs: zurück zum Ursprung
    assert_eq!(app.tab, Tab::Verify);
    assert_eq!(app.verify.as_ref().unwrap().cursor, row);
    assert_eq!(app.verify.as_ref().unwrap().commit(), commit);
    // Kein weiterer Ursprung: nichts.
    app.reduce(Action::Back);
    assert_eq!(app.tab, Tab::Verify);
    assert!(!app.quit);
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
    app.reduce(Action::Why);
    let out = render_sized(&mut app, 30, 24);
    let crumb = out.lines().nth(1).unwrap().trim_matches('"').trim_end();
    assert!(crumb.starts_with('…'), "{crumb}");
    assert!(crumb.ends_with("› Why"), "{crumb}");
    let out = render_sized(&mut app, 160, 40);
    let crumb = out.lines().nth(1).unwrap();
    assert!(crumb.contains("Overview ›"), "{crumb}");
}
