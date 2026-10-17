//! C10 · Die Why-Kette ist abgeschafft: ihre Versprechen leben in der
//! Why-Box des Changes-Tabs weiter — jede Probe nennt den Test, aus dem sie
//! umgezogen ist.

use super::c93::on_the_reported_line;
use super::*;
use crate::theme;
use ratatui::style::Color;

fn buffer_at(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal.backend().buffer().clone()
}

/// Die Zelle des ersten `needle` in einer Zeile, die `line_marker` enthält.
fn cell_in_line(
    buffer: &ratatui::buffer::Buffer,
    line_marker: &str,
    needle: &str,
) -> Option<(String, Color, ratatui::style::Modifier)> {
    let area = buffer.area;
    for y in 0..area.height {
        let line: String = (0..area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        if line.contains(line_marker)
            && let Some(byte) = line.find(needle)
        {
            let x = line[..byte].chars().count() as u16;
            let cell = &buffer[(x, y)];
            return Some((
                cell.symbol().to_string(),
                cell.fg,
                cell.style().add_modifier,
            ));
        }
    }
    None
}

/// Aussage != Beweis (ADR-0011): Was der Agent aufgezeichnet hat, trägt den
/// Claim-Marker, nie das ✓ eines Belegs — umgezogen aus
/// `why_shows_the_chain_and_a_missing_link_is_named_not_hidden` (INTENT war
/// `◌ INTENT`, nie `✓ INTENT`) und `a2_the_intent_link_is_a_claim_never_a_check`.
#[test]
fn c10_asked_for_and_agent_said_are_claims_never_checks() {
    let (_dir, repo) = repo();
    let mut app = on_the_reported_line(&repo);
    let out = render_at(&mut app, 200);
    let asked = out
        .lines()
        .find(|l| l.contains("ASKED FOR"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(asked.contains("◌"), "{out}");
    assert!(!asked.contains('✓'), "{out}");
    let said = out
        .lines()
        .find(|l| l.contains("AGENT SAID"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(said.contains("◌"), "{out}");
    assert!(!said.contains('✓'), "{out}");
    assert!(!out.contains("✓ ASKED FOR"), "{out}");
    assert!(!out.contains("✓ INTENT"), "{out}");
    // Der Marker trägt den Claim-Stil, nie das OK-Grün eines Belegs.
    let buffer = buffer_at(&mut app, 200, 30);
    let (glyph, color, _) = cell_in_line(&buffer, "ASKED FOR", "◌").expect("claim marker");
    assert_eq!(glyph, "◌");
    assert_ne!(color, theme::OK, "a claim is never OK-green");
    assert_eq!(color, theme::claim().2.fg.unwrap());
}

/// Das CLAIM-Label ist GESTYLT (theme::claim: HUMAN + DIM) — nie Default:
/// Eine Aussage, die aussieht wie der Text daneben, waere kein Label.
/// Umgezogen aus `the_claim_label_carries_its_theme_style` (Why-Kette).
#[test]
fn c10_the_claim_label_carries_its_theme_style() {
    let (_dir, repo) = repo();
    let mut app = on_the_reported_line(&repo);
    let buffer = buffer_at(&mut app, 200, 30);
    let (_, color, modifier) = cell_in_line(&buffer, "ASKED FOR", "◌").expect("claim label");
    assert_eq!(color, Color::Cyan);
    assert!(modifier.contains(ratatui::style::Modifier::DIM));
}

/// Eine Lücke wird benannt, nicht versteckt — umgezogen aus
/// `why_shows_the_chain_and_a_missing_link_is_named_not_hidden`: Die
/// Why-Box sagt zu einer nicht beobachteten Zeile, dass kein Beleg sie
/// deckt, und warum.
#[test]
fn c10_a_gap_in_the_why_box_is_named_not_hidden() {
    let (_dir, repo) = repo();
    let mut app = app_with_changes(&repo);
    // Die zweite hinzugefügte Zeile von `src/sort/mod.rs` stammt von
    // niemandem (`set()`): `n` springt auf sie.
    app.reduce(Action::Unexplained(true));
    let out = render_at(&mut app, 200);
    assert!(out.contains("◦ NOT OBSERVED"), "{out}");
    // Der Satz bricht in der Box um — Anfang und Grund stehen.
    assert!(
        out.contains("No evidence of the sessions linked to this commit"),
        "{out}"
    );
    assert!(out.contains("Minds cannot attest this line."), "{out}");
    // Kein Rot, kein Fehler: unerklärt ist offen (Invariante).
    assert!(!out.contains("TAMPERED"), "{out}");
}

/// `minds inspect <datei>:<zeile>` landet seit C10 im Changes-Tab auf der
/// Zeile — und in einem leeren Repo nicht in einem Fehler, sondern mit
/// einem Hinweis (umgezogen aus
/// `why_line_in_an_empty_repo_ends_at_the_commit_not_in_an_error`).
#[test]
fn c10_a_start_at_a_line_lands_in_changes_not_in_an_error() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.open_line("src/http/retry.rs", 42);
    assert_eq!(app.tab, Tab::Changes);
    let closed = app.closed.clone().unwrap_or_default();
    assert!(closed.contains("src/http/retry.rs:42"), "{closed}");
    let out = render_at(&mut app, 200);
    assert!(out.contains("src/http/retry.rs:42"), "{out}");
    // Mit einem Diff, der die Datei enthält: der Cursor auf der Zeile
    // (`open_line` liest den Diff von HEAD; hier liegt er schon vor).
    let mut app = app_with_changes(&repo);
    app.place_line("src/sort/mod.rs", 2);
    assert_eq!(app.tab, Tab::Changes);
    assert_eq!(app.changes.as_ref().unwrap().focus, Focus::Diff);
    let state = app.changes.as_ref().unwrap();
    assert_eq!(state.current().unwrap().rows[state.row].new, Some(2));
}
