//! C9.2 · Layout nach Breite, kein Zeilen-Hintergrund
//! (docs/spikes/auftrag-batch-2.md, Paket 4).

use ratatui::style::{Color, Modifier};

use super::*;
use crate::theme;

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

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

/// Die Zelle des Markers (`+`/`-`) links vom Text in Zeile `y`.
fn sign_left_of(buffer: &ratatui::buffer::Buffer, x: u16, y: u16) -> u16 {
    (0..x)
        .rev()
        .find(|&mx| matches!(buffer[(mx, y)].symbol(), "+" | "-"))
        .expect("a marker left of the text")
}

/// Die Breite des Rahmens, dessen Kopfzeile `needle` trägt — von `┌` bis
/// `┐` einschließlich.
fn box_width(out: &str, needle: &str) -> usize {
    let line = out
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle} missing:\n{out}"));
    let chars: Vec<char> = line.chars().collect();
    let at = line[..line.find(needle).unwrap()].chars().count();
    let start = (0..at).rev().find(|&i| chars[i] == '┌').unwrap();
    let end = (at..chars.len()).find(|&i| chars[i] == '┐').unwrap();
    end - start + 1
}

/// Der FILES-Titel zählt Dateien und Zeilen des Commits.
#[test]
fn c92_the_files_title_counts_files_and_lines() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let out = render(&mut app);
    assert!(out.contains("FILES · 2 files · +3 −1"), "{out}");
    assert!(!out.contains("FILES · 1111111"), "{out}");
}

/// Ab 160 Spalten: FILES 28 · Diff Rest · WHY 56; keine `…` in Zeilen bis
/// 80 Zeichen.
#[test]
fn c92_at_160_columns_files_take_28_and_why_56() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let out = render_sized(&mut app, 160, 40);
    assert_eq!(box_width(&out, "FILES · 2 files"), 28, "{out}");
    assert_eq!(box_width(&out, "WHY THIS LINE?"), 56, "{out}");
    for needle in ["fn order() {", "sort_by_key();", "// manuell nachgezogen"] {
        let line = out.lines().find(|l| l.contains(needle)).unwrap();
        assert!(!line.contains('…'), "{line}");
    }
    // Der Diff bekommt den Rest: Dateiliste, Diff und Why-Box füllen die
    // Breite.
    let diff = out.lines().find(|l| l.contains("@@ -1,3 +1,4 @@")).unwrap();
    assert_eq!(diff.chars().count(), 160 + 2, "{diff}");
}

/// 120–159 Spalten: FILES als Zeile über dem Diff, WHY 48.
#[test]
fn c92_at_120_columns_files_is_a_line_and_why_48() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let out = render_sized(&mut app, 120, 30);
    let files = out
        .lines()
        .find(|l| l.contains("FILES · 2 files · +3 −1"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(files.contains("src/sort/mod.rs +2"), "{files}");
    assert!(files.contains("tests/t.rs +1"), "{files}");
    assert!(files.contains(" · "), "{files}");
    assert!(!out.contains("┌ FILES"), "no file pane:\n{out}");
    assert!(!out.contains("┌ ▸ FILES"), "no file pane:\n{out}");
    assert_eq!(box_width(&out, "WHY THIS LINE?"), 48, "{out}");
    // Die Dateiliste bleibt bedienbar: Esc fokussiert die Zeile, ↓ wechselt.
    app.reduce(Action::Back);
    app.reduce(Action::Down);
    assert_eq!(
        app.changes
            .as_ref()
            .unwrap()
            .current()
            .map(|f| f.path.as_str()),
        Some("tests/t.rs")
    );
}

/// Unter 120 Spalten: FILES-Zeile + Diff; WHY als Ebene per `Enter`, `Esc`
/// schließt sie.
#[test]
fn c92_below_120_columns_why_is_a_layer() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let out = render_sized(&mut app, 80, 24);
    assert!(out.contains("FILES · 2 files"), "{out}");
    assert!(!out.contains("WHY THIS LINE?"), "{out}");
    assert!(out.contains("@@ -1,3 +1,4 @@"), "{out}");
    app.reduce(Action::Enter);
    let out = render_sized(&mut app, 80, 24);
    assert!(out.contains("WHY THIS LINE?"), "{out}");
    assert!(out.contains("Unchanged in this commit"), "{out}");
    app.reduce(Action::Back);
    let out = render_sized(&mut app, 80, 24);
    assert!(!out.contains("WHY THIS LINE?"), "{out}");
}

/// `+`/`-` nur als Vordergrund auf Marker und Zeilennummer; kein
/// Zeilen-Hintergrund — der ist dem Cursor vorbehalten.
#[test]
fn c92_diff_lines_have_no_background_only_coloured_markers() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let buffer = render_buffer(&mut app);
    let (x, y) = find(&buffer, "sort_by_key");
    assert_eq!(buffer[(x, y)].bg, Color::Reset);
    assert_eq!(buffer[(x, y)].fg, Color::Reset, "text keeps its colour");
    let sign = &buffer[(sign_left_of(&buffer, x, y), y)];
    assert_eq!(sign.symbol(), "+");
    assert_eq!(sign.fg, theme::OK);
    assert_eq!(sign.bg, Color::Reset);
    // Die neue Zeilennummer der `+`-Zeile trägt dieselbe Farbe.
    let (nx, _) = find(&buffer, "2 +     sort_by_key");
    assert_eq!(buffer[(nx, y)].fg, theme::OK);
    let (x, y) = find(&buffer, "old();");
    let sign = &buffer[(sign_left_of(&buffer, x, y), y)];
    assert_eq!(sign.symbol(), "-");
    assert_eq!(sign.fg, theme::DELETE);
    assert_eq!(buffer[(x, y)].bg, Color::Reset);
    // Der Cursor (Zeile 1) ist die einzige Zeile mit Hintergrund-Wirkung.
    let (x, y) = find(&buffer, "fn order");
    assert!(buffer[(x, y)].modifier.contains(Modifier::REVERSED));
}

/// Der Rand-Glyph ist gedimmt, solange er dem Dateizustand entspricht;
/// eine Abweichung steht in voller Farbe und fett.
#[test]
fn c92_the_gutter_glyph_is_dim_when_it_matches_the_file_state() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let buffer = render_buffer(&mut app);
    // Datei `src/sort/mod.rs` ist `◇ reported`: ihr ◇ ist gedimmt …
    let (x, y) = find(&buffer, "sort_by_key");
    let marker = (0..x)
        .rev()
        .find(|mx| buffer[(*mx, y)].symbol() == "◇")
        .unwrap();
    assert_eq!(buffer[(marker, y)].fg, theme::DIM);
    assert!(!buffer[(marker, y)].modifier.contains(Modifier::BOLD));
    // … ihr ◦ weicht ab: volle Farbe, fett, nie invertiert.
    let (x, y) = find(&buffer, "// manuell");
    let marker = (0..x)
        .rev()
        .find(|mx| buffer[(*mx, y)].symbol() == "◦")
        .unwrap();
    let cell = &buffer[(marker, y)];
    assert_ne!(cell.fg, theme::DIM);
    assert!(cell.modifier.contains(Modifier::BOLD));
    assert!(!cell.modifier.contains(Modifier::REVERSED));
    // Datei `tests/t.rs` ist `◦ not observed`: dort ist ◦ gedimmt.
    app.reduce(Action::Back);
    app.reduce(Action::Down);
    app.reduce(Action::Enter);
    let buffer = render_buffer(&mut app);
    let (x, y) = find(&buffer, "#[test] fn t() {}");
    let marker = (0..x)
        .rev()
        .find(|mx| buffer[(*mx, y)].symbol() == "◦")
        .unwrap();
    assert_eq!(buffer[(marker, y)].fg, theme::DIM);
}

/// Unter 160 Spalten ist FILES eine Zeile über dem Diff — dort wechselt
/// `Tab` zur nächsten, `Shift-Tab` zur vorigen Datei (umlaufend), der Cursor
/// landet auf der ersten Codezeile der Datei; der Tab-Wechsel bleibt den
/// Ziffern und F-Tasten.
#[test]
fn c92_tab_picks_the_next_file_in_the_files_line() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let _ = render_sized(&mut app, 120, 30);
    assert_eq!(app.changes.as_ref().unwrap().file, 0);
    app.reduce(Action::CycleTab(true));
    assert_eq!(app.tab, Tab::Changes, "Tab stays in the tab");
    let state = app.changes.as_ref().unwrap();
    assert_eq!(state.file, 1);
    assert_eq!(state.row, state.first_code_row());
    assert_ne!(state.row, 0, "never the hunk header");
    let out = render_sized(&mut app, 120, 30);
    assert!(out.contains("▸ tests/t.rs"), "{out}");
    assert!(!out.contains("▸ src/sort/mod.rs"), "{out}");
    // Umlaufend: hinter der letzten Datei die erste.
    app.reduce(Action::CycleTab(true));
    assert_eq!(app.changes.as_ref().unwrap().file, 0);
    app.reduce(Action::CycleTab(false));
    assert_eq!(app.changes.as_ref().unwrap().file, 1);
    app.reduce(Action::CycleTab(false));
    assert_eq!(app.changes.as_ref().unwrap().file, 0);
    // Die Fußzeile nennt die Taste nur in der Dateizeile.
    let out = render_sized(&mut app, 120, 30);
    assert!(out.contains("Tab file"), "{out}");
    // Der Verlauf wächst durch den Dateiwechsel nicht (Paket 2, Regel 1
    // gilt nur für echte Tab-Wechsel).
    assert!(app.history.is_empty());
}

/// Ab 160 Spalten ist FILES ein Pane — `Tab` wechselt den Tab wie überall.
#[test]
fn c92_tab_cycles_the_tab_when_files_is_a_pane() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    let out = render_sized(&mut app, 160, 40);
    assert!(!out.contains("Tab file"), "{out}");
    app.reduce(Action::CycleTab(true));
    assert_eq!(app.tab, Tab::Intent);
    assert_eq!(app.changes.as_ref().unwrap().file, 0);
    app.reduce(Action::CycleTab(false));
    assert_eq!(app.tab, Tab::Changes);
}
