//! Die Hilfe — ein Overlay über allem.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::theme;

const TEXT: &str = "\
j / k  ↑ / ↓      one line
PgUp / PgDn        one page
g / G              start / end
Enter / l          open, descend
Esc / h            back; on the list: clear the search, then quit

/                  search (terms AND-combined, over prompt, agent, paths, ids)
w                  why — the provenance chain
e                  evidence — verdict, coverage, epochs, signature, limits
t                  graph ↔ timeline
1 / 2 / 3          zoom: summary / normal / verbose
r                  reload now (it also reloads by itself when the store changes)
Tab / F1 … F5      switch tab: Overview · Sessions · Verify · Changes · Intent
?                  this help
q / Ctrl-C         quit

Changes:  Enter diff · ] [ hunk (commit in the file list) · n N next unexplained
          s unified ↔ split · w why this line · Enter why chain · Esc back
Overview: ↑↓ commit · Enter its session(s) / graph · w why chain · Esc back (sessions)
Intent:   ↑↓ anchor · PgUp PgDn scroll · Enter graph of a session naming it
          in why: Enter on the anchor";

/// Die Legende, aus dem Glyph-Register erzeugt (A1): erst die Zustände
/// (epistemisch), dann die Arten (strukturell), auf `width` umbrochen.
pub fn legend(width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for (label, class) in [
        ("State:  ", theme::GlyphClass::Epistemic),
        ("Kinds:  ", theme::GlyphClass::Structural),
    ] {
        let mut line = label.to_string();
        for entry in theme::REGISTER.iter().filter(|e| e.class == class) {
            let item = format!("{} {}", entry.symbol, entry.word);
            let fresh = line.chars().count() == label.len();
            let sep = if fresh { "" } else { "   " };
            if !fresh && line.chars().count() + sep.len() + item.chars().count() > width {
                lines.push(line);
                line = " ".repeat(label.len());
                line.push_str(&item);
            } else {
                line.push_str(sep);
                line.push_str(&item);
            }
        }
        lines.push(line);
    }
    lines
}

/// Zeichnet die Hilfe mittig.
pub fn draw(frame: &mut Frame, area: Rect) {
    let width = 84.min(area.width);
    // So hoch wie der Inhalt (Tasten, Leerzeile, Legende), höchstens das
    // Terminal — die Legende fällt nicht mehr unter den Rand, wo Platz ist.
    let content = TEXT.lines().count() + 1 + legend(width.saturating_sub(2) as usize).len();
    let height = (content as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new({
            let mut text: Vec<ratatui::text::Line> =
                TEXT.lines().map(ratatui::text::Line::from).collect();
            text.push(ratatui::text::Line::from(""));
            text.extend(
                legend(width.saturating_sub(2) as usize)
                    .into_iter()
                    .map(ratatui::text::Line::from),
            );
            text
        })
        .block(
            Block::bordered()
                .title(" Help ")
                .title_style(theme::title()),
        ),
        rect,
    );
}
