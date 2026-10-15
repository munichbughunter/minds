//! B4 im Verify-Tab: PgUp/PgDn blättern seitenweise, nicht an Anfang/Ende.

use super::*;

#[test]
fn b4_page_keys_page_in_verify() {
    let (_dir, repo) = repo();
    let mut app = app(&repo);
    app.reduce(Action::TabTo(2));
    app.page = 1;
    let rows = app.verify.as_ref().unwrap().rows();
    assert!(rows > 2, "{rows}");
    app.reduce(Action::PageDown);
    assert_eq!(app.verify.as_ref().unwrap().cursor, 1);
    app.reduce(Action::PageDown);
    assert_eq!(app.verify.as_ref().unwrap().cursor, 2);
    app.reduce(Action::PageUp);
    assert_eq!(app.verify.as_ref().unwrap().cursor, 1);
}
