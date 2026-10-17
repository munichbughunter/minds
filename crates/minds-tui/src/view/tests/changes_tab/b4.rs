//! B4 im Changes-Tab: `c` schaltet die Spalte; `w` ist seit C10 frei und
//! tut nichts — still, aber ohne Nebenwirkung auf die Spalte.

use super::*;

#[test]
fn b4_c_toggles_the_column_and_w_does_nothing() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    app.reduce(Action::Enter); // in den Diff
    let before = app.changes.as_ref().unwrap().why;
    app.reduce(Action::Context);
    assert_ne!(
        app.changes.as_ref().unwrap().why,
        before,
        "c toggles the column"
    );
    let toggled = app.changes.as_ref().unwrap().why;
    // `w` ist keine Aktion mehr (C10): die Spalte bleibt, keine Ebene
    // öffnet sich, der Tab bleibt.
    let w = crate::input::map(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('w'),
            crossterm::event::KeyModifiers::NONE,
        ),
        false,
    );
    assert_eq!(w, Action::None);
    app.reduce(w);
    assert_eq!(
        app.changes.as_ref().unwrap().why,
        toggled,
        "w leaves the column"
    );
    assert_eq!(app.tab, Tab::Changes);
    assert!(app.top().is_none());
}
