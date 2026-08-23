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
    app.page = (body.height as usize).saturating_sub(2).max(1);
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
            inspector,
        }) => why::draw(frame, body, chain, *cursor, inspector.as_deref()),
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
        Span::raw(h.branch.clone().unwrap_or_else(|| "(losgelöst)".into())),
    ]);
    let mut stats = vec![
        Span::raw(format!("{} Sessions", h.sessions)),
        Span::raw(" · "),
        Span::raw(format!("{} Changes", h.changes)),
        Span::raw(" · "),
        Span::raw(format!(
            "{:.0} % Kontext-Abdeckung",
            h.coverage.ratio() * 100.0
        )),
    ];
    if h.degraded > 0 {
        stats.push(Span::raw(" · "));
        stats.push(Span::styled(
            format!("{} degradiert", h.degraded),
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
                        "Degradiert: Die Nutzlast ist nicht lesbar — vergessen oder defekt; die Referenz bleibt auflösbar.",
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
            "Graph: Absicht → Agent → Effekte → Änderung → Review. Details unter dem Cursor.",
            theme::dim(),
        )),
        Some(View::Why { chain, .. }) => {
            let gaps = chain.gaps();
            if gaps.is_empty() {
                Line::from(Span::styled(
                    "✓ keine Lücke — jedes Glied ist belegt",
                    Style::default().fg(theme::OK),
                ))
            } else {
                Line::from(Span::styled(
                    format!(
                        "⚠ {} {} in der Kette — siehe Block unten",
                        gaps.len(),
                        if gaps.len() == 1 { "Lücke" } else { "Lücken" }
                    ),
                    Style::default().fg(theme::REVIEW),
                ))
            }
        }
    };
    let keys = if app.searching {
        Line::from(vec![
            Span::styled("/", theme::title()),
            Span::raw(app.query.clone()),
            Span::styled("▏", Style::default()),
            Span::styled(
                format!(
                    "  {}/{} Treffer · Enter übernehmen · Esc löschen",
                    app.visible.len(),
                    app.cards.len()
                ),
                theme::dim(),
            ),
        ])
    } else {
        let mut spans = Vec::new();
        if !app.query.is_empty() {
            spans.push(Span::styled(
                format!("[{}] ", app.query),
                theme::title().fg(theme::EDIT),
            ));
        }
        let keys = match app.top() {
            None => "↑↓ wählen  Enter Graph  w Why  / Suche  1·2·3 Zoom  ? Hilfe  q Ende",
            Some(View::Graph { .. }) => {
                "↑↓ wählen  Enter hinein  w Why  t Zeitleiste  1·2·3 Zoom  Esc zurück  ? Hilfe"
            }
            Some(View::Why { .. }) => "↑↓ wählen  Enter öffnen  Esc zurück  ? Hilfe",
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
    match ts {
        Some(ts) if ts.len() >= 16 => format!("{}.{}. {}Z", &ts[8..10], &ts[5..7], &ts[11..16]),
        Some(ts) => ts.to_string(),
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
