//! Die Liste: eine Zeile je Session — Zeit, Überschrift, Akteur, Umfang,
//! Beleg, Verdict.

use minds_reader::model::{CardState, SessionCard};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme;
use crate::view::{clip, offset, when};

/// Zeichnet die Liste.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let cards = app.visible_cards();
    if cards.is_empty() {
        let text = if app.cards.is_empty() {
            "Noch keine Sessions erfasst.\n\nminds enable richtet die Hooks ein; nach dem nächsten Commit steht die erste Session hier."
        } else {
            "Kein Treffer für die Suche.\n\nEsc löscht die Suche."
        };
        frame.render_widget(Paragraph::new(text).style(theme::dim()), area);
        return;
    }
    let height = area.height as usize;
    let first = offset(app.cursor, cards.len(), height);
    let width = area.width as usize;
    // Die Überschrift hat Vorrang: feste Spalten rechts, die Umfang-Spalte
    // nur, wenn das Terminal breit genug ist.
    let time_w = 13;
    let actor_w = 22;
    let mark_w = 26;
    let show_size = width >= 120;
    let size_w = if show_size { 20 } else { 0 };
    let headline_w = width
        .saturating_sub(time_w + actor_w + size_w + mark_w + 4)
        .max(24);

    let lines: Vec<Line> = cards
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, card)| {
            let degraded = card.is_degraded();
            let base = if degraded {
                theme::dim()
            } else {
                Style::default()
            };
            let mut spans = vec![
                Span::styled(
                    format!("{:<time_w$}", when(card.started_at.as_deref())),
                    base,
                ),
                Span::styled(
                    if degraded { "⌦ " } else { "● " },
                    if degraded {
                        theme::dim()
                    } else {
                        theme::lane(0)
                    },
                ),
                Span::styled(
                    format!("{:<headline_w$}", clip(&card.summary.headline, headline_w)),
                    base.patch(theme::title()),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<actor_w$}", clip(&card.summary.actor, actor_w)),
                    base.fg(theme::AGENT),
                ),
            ];
            if show_size {
                spans.push(Span::styled(
                    format!(
                        "{:<size_w$}",
                        format!(
                            "{} D · {}/{} T",
                            card.summary.files,
                            card.summary.input_tokens,
                            card.summary.output_tokens
                        )
                    ),
                    base,
                ));
            }
            if degraded {
                spans.push(Span::styled(state_word(card), theme::dim()));
            } else {
                let (ev_glyph, ev_word, ev_style) = theme::evidence(card.evidence);
                let (v_glyph, v_word, v_style) = theme::verdict(card.review.verdict);
                spans.push(Span::styled(format!("{ev_glyph} {ev_word}  "), ev_style));
                spans.push(Span::styled(format!("{v_glyph} {v_word}"), v_style));
            }
            let mut line = Line::from(spans);
            if i == app.cursor {
                line = line.style(theme::cursor());
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn state_word(card: &SessionCard) -> &'static str {
    match card.state {
        CardState::Ok => "",
        CardState::Forgotten { .. } => "⌦ vergessen",
        CardState::Unreadable { .. } => "? unlesbar",
    }
}
