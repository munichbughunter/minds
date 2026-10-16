//! C9.1 · Changes landet auf der ersten Codezeile, Cursor sichtbar
//! (docs/spikes/auftrag-batch-2.md, Paket 3).

use ratatui::style::Modifier;

use super::*;
use crate::theme;

/// Die Zelle, in der `needle` in `buffer` beginnt.
fn find(buffer: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16) {
    (0..buffer.area.height)
        .find_map(|y| {
            let line: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect();
            line.find(needle)
                .map(|byte| (line[..byte].chars().count() as u16, y))
        })
        .unwrap_or_else(|| panic!("{needle} not drawn"))
}

/// Beim Öffnen liegt der Fokus im Diff, auf der ersten Codezeile des ersten
/// Hunks — nie auf dem Kopf, nie in der Dateiliste.
#[test]
fn c91_changes_lands_in_the_diff_on_the_first_code_line() {
    let (_dir, repo) = repo();
    let app = app_with_changes(&repo);
    let state = app.changes.as_ref().unwrap();
    assert_eq!(state.focus, Focus::Diff);
    assert_eq!((state.file, state.row), (0, 1));
    assert_eq!(
        state.current().unwrap().rows[state.row].kind,
        DiffKind::Context,
        "a code line, not the hunk header"
    );
}

/// Der Cursor ist `theme::cursor()` über die ganze Zeile: Rand,
/// Zeilennummern, Text und Auffüllung bis zum Zeilenende.
#[test]
fn c91_the_cursor_covers_the_whole_diff_line() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let buffer = render_buffer(&mut app);
    let (x, y) = find(&buffer, "fn order() {");
    let cursor = theme::cursor().add_modifier;
    for dx in [-12i32, -8, 0, 5, 30] {
        let cx = (x as i32 + dx) as u16;
        assert!(
            buffer[(cx, y)].modifier.contains(cursor),
            "column {cx} of the cursor row is not reversed"
        );
    }
    // Die Zeile darunter trägt den Cursor nicht.
    assert!(!buffer[(x, y + 1)].modifier.contains(cursor));
}

/// Der fokussierte Bereich trägt die helle Marke aus B7, der andere ist
/// gedimmt — und das wechselt mit `Esc`.
#[test]
fn c91_the_focused_pane_has_the_bright_frame() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let out = render(&mut app);
    assert!(out.contains("▸ src/sort/mod.rs"), "{out}");
    assert!(!out.contains("▸ FILES"), "{out}");
    app.reduce(Action::Back);
    let out = render(&mut app);
    assert!(out.contains("▸ FILES"), "{out}");
    assert!(!out.contains("▸ src/sort/mod.rs"), "{out}");
    let buffer = render_buffer(&mut app);
    let (x, y) = find(&buffer, "FILES");
    assert!(
        buffer[(x.saturating_sub(3), y)]
            .modifier
            .contains(Modifier::BOLD),
        "the focused frame is bright"
    );
}

/// Die Fußzeile zählt die nicht beobachteten Zeilen; ohne solche fällt die
/// Taste aus der Fußzeile. Das Wort folgt dem Zustand (`not-observed`),
/// nie der internen Bezeichnung.
#[test]
fn c91_the_footer_counts_the_not_observed_lines() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let out = render(&mut app);
    assert!(out.contains("n next not-observed (2)"), "{out}");
    assert!(!out.contains("unexplained"), "{out}");
    app.reduce(Action::Back);
    let out = render(&mut app);
    assert!(out.contains("n next not-observed (2)"), "{out}");

    let mut all_reported = set();
    for file in &mut all_reported.files {
        for row in &mut file.rows {
            if row.class == Some(ReconClass::Unexplained) {
                row.class = Some(ReconClass::ReportedOnly);
            }
        }
    }
    let mut app = App::new(filled(), &repo, None);
    app.changes = Some(ChangesState::new(vec![commit('1')], 0, Ok(all_reported)));
    app.tab = Tab::Changes;
    let out = render(&mut app);
    assert!(!out.contains("n next"), "{out}");
}

/// Auf dem Hunk-Kopf zeigt die Why-Box die Datei-Zusammenfassung — nie nur
/// den Tastenhinweis.
#[test]
fn c91_the_hunk_header_shows_the_file_summary() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    app.reduce(Action::Bracket(false));
    assert_eq!(app.changes.as_ref().unwrap().row, 0);
    let out = render_at(&mut app, 260);
    assert!(out.contains("src/sort/mod.rs · 2 added lines"), "{out}");
    assert!(out.contains("1 reported by one Edit (turn 1)"), "{out}");
    assert!(out.contains("│0 observed"), "{out}");
    assert!(out.contains("│1 not observed"), "{out}");
    assert!(!out.contains("Hunk header —"), "{out}");
}

/// `d` aus der Übersicht landet auf der ersten Zeile im offenen Zustand
/// (`◦ not observed`), nicht auf der ersten Codezeile.
#[test]
fn c91_d_from_the_overview_lands_on_the_first_not_observed_line() {
    let (_dir, repo, inspection) = reconciled(true);
    let mut app = App::new(inspection, &repo, None);
    app.open_overview();
    app.reduce(Action::Diff);
    assert_eq!(app.tab, Tab::Changes);
    let state = app.changes.as_ref().unwrap();
    assert_eq!(state.focus, Focus::Diff);
    let row = &state.current().unwrap().rows[state.row];
    assert_eq!(row.class, Some(ReconClass::Unexplained), "{row:?}");
    assert!(row.text.contains("HUMAN"), "{row:?}");
}

/// `Enter` im Diff unter 120 Spalten öffnet „Warum diese Zeile?" als Ebene;
/// `Esc` schließt sie, erst das zweite `Esc` geht in die Dateiliste.
#[test]
fn c91_enter_below_120_columns_opens_the_why_layer_and_esc_closes_it() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    render_at(&mut app, 100);
    app.reduce(Action::Enter);
    assert!(app.changes.as_ref().unwrap().why_layer);
    app.reduce(Action::Back);
    let state = app.changes.as_ref().unwrap();
    assert!(!state.why_layer);
    assert_eq!(state.focus, Focus::Diff);
    app.reduce(Action::Back);
    assert_eq!(app.changes.as_ref().unwrap().focus, Focus::Files);
}
