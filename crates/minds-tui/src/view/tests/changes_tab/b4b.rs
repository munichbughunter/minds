//! B4(b) · `/` global: im Changes-Tab filtert es Dateien.

use super::*;

#[test]
fn b4b_slash_filters_the_changed_files() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let out = render(&mut app);
    assert!(
        out.contains("src/sort/mod.rs") && out.contains("tests/t.rs"),
        "{out}"
    );
    app.reduce(Action::SearchStart);
    for c in "tests".chars() {
        app.reduce(Action::SearchInput(c));
    }
    app.reduce(Action::SearchCommit);
    let out = render(&mut app);
    assert!(out.contains("tests/t.rs"), "{out}");
    assert!(!out.contains("src/sort/mod.rs"), "filtered out\n{out}");
    let state = app.changes.as_ref().unwrap();
    assert_eq!(
        state.current().map(|f| f.identity.as_str()),
        Some("tests/t.rs")
    );
    app.reduce(Action::Back);
    assert!(render(&mut app).contains("src/sort/mod.rs"));
}
