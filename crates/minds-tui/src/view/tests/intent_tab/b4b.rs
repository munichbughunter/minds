//! B4(b) · `/` global: im Intent-Tab filtert es Anker.

use super::*;

#[test]
fn b4b_slash_filters_the_anchors() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    let total = app.intent.as_ref().unwrap().list().len();
    assert!(total >= 2, "{total}");
    let first = app.intent.as_ref().unwrap().list()[1].id.to_string();
    let needle: String = first.chars().skip(3).take(8).collect();
    app.reduce(Action::SearchStart);
    for c in needle.chars() {
        app.reduce(Action::SearchInput(c));
    }
    app.reduce(Action::SearchCommit);
    let state = app.intent.as_ref().unwrap();
    assert_eq!(
        state.selected().map(|a| a.id.to_string()),
        Some(first.clone())
    );
    assert_eq!(state.shown().len(), 1, "only the match is shown");
    app.reduce(Action::Back);
    assert_eq!(app.intent.as_ref().unwrap().shown().len(), total);
}
