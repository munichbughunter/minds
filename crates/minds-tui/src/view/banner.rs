//! Das Tamper-Banner: Manipulation ist der lauteste Zustand der Oberfläche —
//! eine Zeile über die volle Breite, rot und invertiert, mit Glyph **und**
//! Wort, damit sie auch ohne Farbe nicht zu übersehen ist.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::TamperAlert;
use crate::theme;
use crate::view::clip;

/// Die Tasten stehen rechts und fallen nie weg — gekürzt wird links.
const KEYS: &str = "  Enter: open  Esc: dismiss ";

/// Zeichnet das Banner in `area` (eine Zeile).
pub fn draw(frame: &mut Frame, area: Rect, alert: &TamperAlert) {
    let (glyph, word, _) = theme::tampered();
    let what = match alert.sessions.as_slice() {
        [one] => {
            let short: String = one.to_string().chars().take(11).collect();
            let why = match &alert.since {
                Some(at) => format!("seal material changed since {at}"),
                None => "seal material does not match its seals".to_string(),
            };
            format!(" {glyph} {word} — {why} · session {short}…")
        }
        many => format!(
            " {glyph} {} sessions {word} — seal material does not match · first: {}…",
            many.len(),
            many.first()
                .map(|id| id.to_string().chars().take(11).collect::<String>())
                .unwrap_or_default()
        ),
    };
    let width = area.width as usize;
    let keys = if width >= KEYS.chars().count() + 24 {
        KEYS
    } else {
        " Esc: dismiss "
    };
    let room = width.saturating_sub(keys.chars().count());
    let left = clip(&what, room);
    let pad = room.saturating_sub(left.chars().count());
    let style = Style::default()
        .fg(theme::DELETE)
        .add_modifier(Modifier::REVERSED | Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(left),
            Span::raw(" ".repeat(pad)),
            Span::raw(keys),
        ]))
        .style(style),
        area,
    );
}
