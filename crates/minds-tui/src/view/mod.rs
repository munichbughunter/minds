//! Das Zeichnen. Jede Ebene hat ihr Modul; dieses verteilt Rahmen, Kopf und
//! Fuß und legt die Hilfe darüber.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use minds_reader::model::evidence_sentence;

use crate::app::{App, View};
use crate::theme;

pub mod activity;
pub mod evidence;
pub mod graph;
pub mod help;
pub mod why;

#[cfg(test)]
mod tests;

/// Zeichnet den ganzen Bildschirm.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    header(frame, app, head);
    // Eine Seite = sichtbare Zeilen: Die Liste verliert an Rahmen und
    // Kopfzeile drei Zeilen, die übrigen Ebenen zwei.
    app.page = (body.height as usize)
        .saturating_sub(if app.top().is_none() { 3 } else { 2 })
        .max(1);
    match app.top() {
        None => activity::draw(frame, app, body),
        Some(View::Graph {
            id,
            rows,
            cursor,
            timeline,
            ..
        }) => graph::draw(frame, app, body, *id, rows, *cursor, *timeline),
        Some(View::Why {
            chain,
            cursor,
            edge,
            inspector,
        }) => why::draw(frame, body, chain, *cursor, *edge, inspector.as_deref()),
        Some(View::Evidence {
            id,
            report,
            uninterpreted,
            cursor,
        }) => evidence::draw(frame, body, *id, report.as_ref(), *uninterpreted, *cursor),
    }
    footer(frame, app, foot);
    if app.help {
        help::draw(frame, frame.area());
    }
}

fn header(frame: &mut Frame, app: &App, area: Rect) {
    let h = app.inspection.header();
    let title = Line::from(vec![
        Span::styled("MINDS ", theme::title().fg(theme::AGENT)),
        Span::styled(h.repo.clone(), theme::title()),
        Span::raw(" · "),
        Span::raw(h.branch.clone().unwrap_or_else(|| "(detached)".into())),
    ]);
    let mut stats = vec![
        Span::raw(format!("{} Sessions", h.sessions)),
        Span::raw(" · "),
        Span::raw(format!("{} Changes", h.changes)),
        Span::raw(" · "),
        Span::raw(format!(
            "{:.0} % context coverage",
            h.coverage.ratio() * 100.0
        )),
    ];
    if h.degraded > 0 {
        stats.push(Span::raw(" · "));
        stats.push(Span::styled(
            format!("{} degraded", h.degraded),
            theme::dim(),
        ));
    }
    frame.render_widget(Paragraph::new(vec![title, Line::from(stats)]), area);
}

fn footer(frame: &mut Frame, app: &App, area: Rect) {
    // Erste Zeile: was der Fokus bedeutet — der Evidenz-Satz zur gewählten
    // Karte bzw. die Lücken der Kette. Zweite Zeile: die Tasten.
    let status = match app.top() {
        None => app
            .selected()
            .map(|card| {
                if card.is_degraded() {
                    Line::from(Span::styled(
                        "Degraded: the payload is unreadable — forgotten or damaged; the reference stays resolvable.",
                        theme::dim(),
                    ))
                } else {
                    let (glyph, word, style) = theme::evidence(card.evidence);
                    Line::from(vec![
                        Span::styled(format!("{glyph} {word}  "), style),
                        Span::styled(
                            evidence_sentence(card.evidence).to_string(),
                            theme::dim(),
                        ),
                    ])
                }
            })
            .unwrap_or_default(),
        Some(View::Graph { .. }) => Line::from(Span::styled(
            "Graph: intent → agent → effects → change → review. Details under the cursor.",
            theme::dim(),
        )),
        Some(View::Why { chain, .. }) => {
            let gaps = chain.gaps();
            if gaps.is_empty() {
                Line::from(Span::styled(
                    "✓ no gap — every link is attested",
                    Style::default().fg(theme::OK),
                ))
            } else {
                Line::from(Span::styled(
                    format!(
                        "⚠ {} {} in the chain — see the block below",
                        gaps.len(),
                        if gaps.len() == 1 { "gap" } else { "gaps" }
                    ),
                    Style::default().fg(theme::REVIEW),
                ))
            }
        }
        Some(View::Evidence { report, .. }) => Line::from(Span::styled(
            report
                .as_ref()
                .map(|r| r.sentence())
                .unwrap_or(minds_reader::model::LEGACY_SENTENCE),
            theme::dim(),
        )),
    };
    let keys = if app.searching {
        Line::from(vec![
            Span::styled("/", theme::title()),
            Span::raw(app.query.clone()),
            Span::styled("▏", Style::default()),
            Span::styled(
                format!(
                    "  {}/{} match(es) · Enter apply · Esc clear",
                    app.visible.len(),
                    app.cards.len()
                ),
                theme::dim(),
            ),
        ])
    } else {
        let mut spans = Vec::new();
        // Der Verdikt-Badge: der Beweiszustand der fokussierten Session als
        // invertierter Block, immer an derselben Stelle unten links — auf
        // einer manipulierten Session springt er sichtbar auf ✗ TAMPERED.
        // Glyph UND Wort, nie nur Farbe (Farbschwäche-Regel aus `theme`).
        let focused = match app.top() {
            None => app.selected().map(|card| card.provenance),
            Some(View::Graph { id, .. }) | Some(View::Evidence { id, .. }) => {
                app.inspection.card(*id).map(|card| card.provenance)
            }
            Some(View::Why { .. }) => None,
        };
        if let Some(provenance) = focused {
            let (glyph, word, style) = theme::provenance(&provenance);
            spans.push(Span::styled(
                format!(" {glyph} {word} "),
                style.add_modifier(ratatui::style::Modifier::REVERSED),
            ));
            spans.push(Span::raw("  "));
        }
        if !app.query.is_empty() {
            spans.push(Span::styled(
                format!("[{}] ", app.query),
                theme::title().fg(theme::EDIT),
            ));
        }
        let keys = match app.top() {
            None => {
                "↑↓ select  Enter graph  w why  e evidence  / search  1·2·3 zoom  ? help  q quit"
            }
            Some(View::Graph { .. }) => {
                "↑↓ select  Enter descend  w why  e evidence  t timeline  1·2·3 zoom  Esc back  ? help"
            }
            Some(View::Why { .. }) => "↑↓ select  Enter open  Esc back  ? help",
            Some(View::Evidence { .. }) => "↑↓ section  Esc back  ? help",
        };
        spans.push(Span::styled(keys, theme::dim()));
        spans.push(Span::styled(
            format!("  Zoom {}", app.zoom.digit()),
            theme::dim(),
        ));
        Line::from(spans)
    };
    frame.render_widget(Paragraph::new(vec![status, keys]), area);
}

/// `DD.MM. HH:MMZ` aus dem RFC-3339-Präfix; `—`, wenn keine Zeit erfasst ist.
/// UTC, wie der Zeitstempel selbst — ohne Datums-Crate keine Ortszeit.
pub fn when(ts: Option<&str>) -> String {
    // `get` statt Index-Slicing: Der Wert kann fremdbestimmt sein (etwa die
    // Zeitzeile eines handgebauten Seals) — ein Multi-Byte-Zeichen an der
    // falschen Stelle darf die Oberfläche nicht panicken.
    match ts {
        Some(ts) => match (ts.get(8..10), ts.get(5..7), ts.get(11..16)) {
            (Some(d), Some(m), Some(hm)) => format!("{d}.{m}. {hm}Z"),
            _ => ts.to_string(),
        },
        None => "—".into(),
    }
}

/// Kürzt auf `max` Zeichen mit Ellipse.
pub fn clip(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Zeilenfenster um den Cursor: die erste gezeigte Zeile.
pub fn offset(cursor: usize, len: usize, height: usize) -> usize {
    if height == 0 || len <= height {
        return 0;
    }
    cursor
        .saturating_sub(height / 2)
        .min(len.saturating_sub(height))
}
