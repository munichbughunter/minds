//! B3 · Ein aktiver Commit für Overview und Changes (seit C12.2 ohne
//! Verify-Tab — sein Urteil steht in der Übersicht).

use super::*;
use crate::overview::Selected;

fn overview_commit(app: &App) -> Option<CommitId> {
    match app.overview.as_ref()?.selected()? {
        Selected::Commit(row) => Some(row.id),
        Selected::Wip(_) => None,
    }
}

#[test]
fn b3_one_active_commit_across_overview_and_changes() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = opened(&repo, inspection);
    // Eine Commit-Zeile wählen, die nicht die erste ist.
    while overview_commit(&app).is_none() {
        app.reduce(Action::Down);
    }
    app.reduce(Action::Down);
    let chosen = overview_commit(&app).expect("a commit row");
    assert_eq!(app.active_commit, Some(chosen));
    // F2 öffnet ihn.
    app.reduce(Action::TabTo(1));
    assert_eq!(app.changes.as_ref().unwrap().commit(), Some(chosen));
    // `[ ]` verschiebt ihn tabübergreifend.
    app.reduce(Action::Bracket(true));
    if app.changes.as_ref().unwrap().commit() == Some(chosen) {
        app.reduce(Action::Bracket(false));
    }
    assert_eq!(app.active_commit, app.changes.as_ref().unwrap().commit());
    let active = app.active_commit.unwrap();
    app.reduce(Action::TabTo(0));
    if commits.contains(&active) {
        assert_eq!(overview_commit(&app), Some(active));
    }
}
