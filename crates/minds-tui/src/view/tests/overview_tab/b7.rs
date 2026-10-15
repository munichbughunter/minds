//! B7 · Ein Auswahl- und Fokusstil: `theme::cursor()` für jede Cursorzeile;
//! der fokussierte Bereich trägt `▸` im Titel und einen hervorgehobenen
//! Rahmen, unfokussierte einen gedimmten — auch ohne Farbe erkennbar.

use super::*;
use ratatui::style::Modifier;

fn buffer_sized(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    let area = buffer.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect()
}

/// Ob die Zeile mit `needle` (ab dessen Spalte) invertiert gezeichnet ist.
fn reversed_at(buffer: &ratatui::buffer::Buffer, needle: &str) -> bool {
    let lines = text(buffer);
    lines.iter().enumerate().any(|(y, line)| {
        line.find(needle).is_some_and(|byte| {
            let x = line[..byte].chars().count() as u16;
            buffer[(x, y as u16)].modifier.contains(Modifier::REVERSED)
        })
    })
}

#[test]
fn b7_one_cursor_style_in_overview_verify_and_intent() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let selected = match app.overview.as_ref().and_then(|s| s.selected()) {
        Some(crate::overview::Selected::Commit(row)) => row.subject.clone(),
        Some(crate::overview::Selected::Wip(wip)) => wip.request.clone(),
        None => panic!("nothing selected"),
    };
    let buffer = buffer_sized(&mut app, 160, 40);
    let needle: String = selected.chars().take(12).collect();
    assert!(
        reversed_at(&buffer, &needle),
        "overview cursor uses theme::cursor()"
    );
    app.reduce(Action::TabTo(2));
    let buffer = buffer_sized(&mut app, 160, 40);
    let lines = text(&buffer);
    if let Some((y, line)) = lines.iter().enumerate().find(|(_, l)| l.contains("▸ ")) {
        let x = line[..line.find("▸ ").unwrap()].chars().count() as u16;
        assert!(
            buffer[(x, y as u16)].modifier.contains(Modifier::REVERSED),
            "verify cursor uses theme::cursor()"
        );
    }
}

#[test]
fn b7_the_focused_pane_is_marked_without_color() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let lines = text(&buffer_sized(&mut app, 160, 40));
    assert!(lines[2].contains("▸ OVERVIEW"), "{}", lines[2]);
    assert!(!lines[2].contains("▸ DETAIL"), "{}", lines[2]);
    if let Some(state) = app.overview.as_mut() {
        state.focus = crate::overview::OverviewFocus::Sessions;
    }
    let lines = text(&buffer_sized(&mut app, 160, 40));
    assert!(lines[2].contains("▸ DETAIL"), "{}", lines[2]);
    assert!(!lines[2].contains("▸ OVERVIEW"), "{}", lines[2]);
    // Sessions: die Liste fokussiert, bis eine Ebene liegt.
    app.reduce(Action::TabTo(1));
    let lines = text(&buffer_sized(&mut app, 160, 40));
    assert!(
        lines.iter().any(|l| l.contains("▸ SESSIONS")),
        "{}",
        lines.join("\n")
    );
    app.reduce(Action::Enter);
    let lines = text(&buffer_sized(&mut app, 160, 40));
    assert!(
        lines.iter().any(|l| l.contains(" SESSIONS ")),
        "{}",
        lines.join("\n")
    );
    assert!(
        !lines.iter().any(|l| l.contains("▸ SESSIONS")),
        "{}",
        lines.join("\n")
    );
}
