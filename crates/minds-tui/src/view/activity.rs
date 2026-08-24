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
    let mark_w = 36;
    let show_size = width >= 120;
    let size_w = if show_size { 20 } else { 0 };
    let headline_w = width
        .saturating_sub(time_w + actor_w + size_w + mark_w + 4)
        .max(24);

    let mut lines: Vec<Line> = cards
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
                // Drei Spalten Beweiszustand (ADR-0011): das Seal-Verdikt der
                // Session, der beste Kanten-Beleg (Glyph samt Status-Modifikator
                // — das Wort erklärt die Fußzeile für die fokussierte Karte)
                // und das Review-Verdict.
                let (s_glyph, s_word, s_style) = theme::provenance(&card.provenance);
                let (ev_glyph, _, ev_style) = theme::evidence(card.evidence);
                let (v_glyph, v_word, v_style) = theme::verdict(card.review.verdict);
                spans.push(Span::styled(format!("{s_glyph} {s_word}  "), s_style));
                spans.push(Span::styled(format!("{ev_glyph}  "), ev_style));
                spans.push(Span::styled(format!("{v_glyph} {v_word}"), v_style));
            }
            let mut line = Line::from(spans);
            if i == app.cursor {
                line = line.style(theme::cursor());
            }
            line
        })
        .collect();

    // Zurückgehaltene Sessions (Block-Seals, ADR-0011): eine Zeile INNERHALB
    // des Viewports — die Abwesenheit einer Session ist eine Aussage, und
    // eine Aussage, die unter dem Scroll-Fenster hängt, sieht niemand.
    // Details zeigt `minds fsck` bzw. `minds verify --evidence`.
    let rejected = app.inspection.rejected_seals();
    if app.query.is_empty() && !rejected.is_empty() {
        lines.truncate(height.saturating_sub(1));
        lines.push(Line::from(vec![
            Span::styled("⛔ ", Style::default().fg(theme::DELETE)),
            Span::styled(
                format!(
                    "{} Session(s) zurückgehalten (Redaction) — Coverage versiegelt, \
                     Details: minds fsck",
                    rejected.len()
                ),
                Style::default().fg(theme::REVIEW),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn state_word(card: &SessionCard) -> &'static str {
    match card.state {
        CardState::Ok => "",
        CardState::Forgotten { .. } => "⌦ vergessen",
        CardState::Unreadable { .. } => "? unlesbar",
    }
}
