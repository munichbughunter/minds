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
?                  this help
q / Ctrl-C         quit

Evidence: ● observed   ◆ content   ◇ declared   ○ inferred   · unlinked
Review:   ⚠ open   ✓ approved   ↻ needs work   ✕ rejected
Effects:  ◇ READ   ✎ EDIT   ▶ EXEC   ✕ DELETE";

/// Zeichnet die Hilfe mittig.
pub fn draw(frame: &mut Frame, area: Rect) {
    let width = 84.min(area.width);
    let height = 20.min(area.height);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(TEXT).block(
            Block::bordered()
                .title(" Help ")
                .title_style(theme::title()),
        ),
        rect,
    );
}
