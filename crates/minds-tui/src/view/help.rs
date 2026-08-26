//! Die Hilfe — ein Overlay über allem.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::theme;

const TEXT: &str = "\
j / k  ↑ / ↓      eine Zeile
PgUp / PgDn        eine Seite
g / G              Anfang / Ende
Enter / l          öffnen, hinein
Esc / h            zurück; auf der Liste: Suche löschen, dann beenden

/                  Suche (Terme UND-verknüpft, über Prompt, Agent, Pfade, Ids)
w                  Why — die Herkunftskette
e                  Evidence — Verdikt, Coverage, Epochen, Signatur, Grenzen
t                  Graph ↔ Zeitleiste
1 / 2 / 3          Zoom: Übersicht / normal / ausführlich
?                  diese Hilfe
q / Ctrl-C         beenden

Belege:  ● observed   ◆ content   ◇ declared   ○ inferred [vermutet]   · unverknüpft
Review:  ⚠ offen   ✓ approved   ↻ needs work   ✕ rejected
Effekte: ◇ READ   ✎ EDIT   ▶ EXEC   ✕ DELETE";

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
                .title(" Hilfe ")
                .title_style(theme::title()),
        ),
        rect,
    );
}
