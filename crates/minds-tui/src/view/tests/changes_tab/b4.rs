//! B4 im Changes-Tab: `w` öffnet die Why-Kette, `c` schaltet die Spalte.

use super::*;

#[test]
fn b4_w_opens_the_why_chain_and_c_toggles_the_column() {
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
    app.reduce(Action::Why);
    // w schaltet die Spalte nicht mehr um (alter Zustand) …
    assert_eq!(
        app.changes.as_ref().unwrap().why,
        toggled,
        "w no longer toggles"
    );
    // … sondern öffnet die Why-Kette der Zeile — oder sagt, warum nicht.
    assert!(
        matches!(app.top(), Some(View::Why { .. })) || app.closed.is_some(),
        "w opens the why chain or names why not"
    );
}
