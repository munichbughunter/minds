//! C5 im Verify-Tab: `v` startet die Prüfung direkt.

use super::*;

#[test]
fn c5_v_starts_the_check_again() {
    let (_dir, repo) = repo();
    let mut app = app(&repo);
    app.reduce(Action::TabTo(2));
    let commit = app.verify.as_ref().unwrap().commit();
    if let Some(state) = app.verify.as_mut() {
        state.pending = false;
        state.urgent = false;
    }
    app.reduce(Action::Verify);
    let state = app.verify.as_ref().unwrap();
    assert_eq!(app.tab, Tab::Verify);
    assert_eq!(state.commit(), commit, "same commit");
    assert!(
        state.pending && state.urgent,
        "the check runs on the next tick"
    );
}
