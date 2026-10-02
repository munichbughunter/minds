//! Das Zeichnen. Jede Ebene hat ihr Modul; dieses verteilt Rahmen, Kopf und
//! Fuß und legt die Hilfe darüber.
//!
//! Ist das Terminal breit genug, liegt die Liste dauerhaft links und rechts
//! daneben das Detail: die oberste Ebene des Stapels — oder, solange der
//! Stapel leer ist, der Graph der Karte unter dem Cursor als Vorschau, die
//! mit dem Cursor wandert. Der Stapel selbst weiß davon nichts: Was `Enter`,
//! `Esc` und die Suche tun, ändert sich nicht, nur wie viel Platz es bekommt.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

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

/// Eine Spalte Luft zwischen Listenrahmen und Detail.
const GUTTER_WIDTH: u16 = 1;
/// Was das Detail neben der Liste mindestens braucht: Kopf, YOU-Box und
/// eine Spur, in der ein Pfad noch lesbar ist.
const DETAIL_MIN_WIDTH: u16 = 55;
/// Ab dieser Breite liegen Liste und Detail nebeneinander; darunter füllt
/// wie bisher eine Fläche den Bildschirm.
pub const SPLIT_MIN_WIDTH: u16 = activity::COMPACT_WIDTH + GUTTER_WIDTH + DETAIL_MIN_WIDTH;
/// Der Anteil der Liste an der Breite — begrenzt auf das, was ihre Spalten
/// brauchen, und auf das, was ihnen noch etwas bringt.
const LIST_SHARE_PERCENT: u32 = 40;

/// Die Meldung zu einer degradierten Karte — Fußzeile und Vorschau sagen
/// dasselbe.
const DEGRADED: &str =
    "Degraded: the payload is unreadable — forgotten or damaged; the reference stays resolvable.";

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
    // Kopfzeile drei Zeilen, die übrigen Ebenen zwei. Die Höhe teilt sich
    // nicht, nur die Breite — die Teilung ändert daran nichts.
    app.page = (body.height as usize)
        .saturating_sub(if app.top().is_none() { 3 } else { 2 })
        .max(1);
    match split(body) {
        Some((list, detail)) => {
            activity::draw(frame, app, list);
            match app.top() {
                None => preview(frame, app, detail),
                Some(view) => view_draw(frame, app, detail, view),
            }
        }
        None => match app.top() {
            None => activity::draw(frame, app, body),
            Some(view) => view_draw(frame, app, body, view),
        },
    }
    footer(frame, app, foot);
    if app.help {
        help::draw(frame, frame.area());
    }
}

/// Liste links, Detail rechts — oder `None`, wenn beides nebeneinander
/// keinen Platz hat.
fn split(body: Rect) -> Option<(Rect, Rect)> {
    if body.width < SPLIT_MIN_WIDTH {
        return None;
    }
    let share = (u32::from(body.width) * LIST_SHARE_PERCENT / 100) as u16;
    let list_w = share.clamp(activity::COMPACT_WIDTH, activity::WIDE_WIDTH);
    let [list, detail] = Layout::horizontal([Constraint::Length(list_w), Constraint::Min(1)])
        .spacing(GUTTER_WIDTH)
        .areas(body);
    Some((list, detail))
}

/// Zeichnet eine gelegte Ebene in `area`.
fn view_draw(frame: &mut Frame, app: &App, area: Rect, view: &View) {
    match view {
        View::Graph {
            id,
            rows,
            cursor,
            timeline,
            ..
        } => graph::draw(frame, app, area, *id, rows, Some(*cursor), *timeline),
        View::Why {
            chain,
            cursor,
            edge,
            inspector,
        } => why::draw(frame, area, chain, *cursor, *edge, inspector.as_deref()),
        View::Evidence {
            id,
            report,
            uninterpreted,
            cursor,
        } => evidence::draw(frame, area, *id, report.as_ref(), *uninterpreted, *cursor),
    }
}

/// Die Vorschau: der Graph der Karte unter dem Cursor, gezeichnet wie eine
/// gelegte Ebene, aber ohne Cursor und ohne Stapel. Eine leere Liste lässt
/// die Fläche leer — ihren Leerzustand sagt die Liste selbst.
fn preview(frame: &mut Frame, app: &App, area: Rect) {
    let Some(card) = app.selected() else {
        return;
    };
    if card.is_degraded() {
        frame.render_widget(
            Paragraph::new(DEGRADED)
                .style(theme::dim())
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    if let Some(rows) = app.preview_graph(card.id) {
        graph::draw(frame, app, area, card.id, &rows, None, false);
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
                    Line::from(Span::styled(DEGRADED, theme::dim()))
                } else {
                    let (glyph, word, style) = theme::evidence(card.evidence);
                    Line::from(vec![
                        Span::styled(format!("{glyph} {word}  "), style),
                        Span::styled(evidence_sentence(card.evidence).to_string(), theme::dim()),
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
                "↑↓ select  Enter descend  w why  e evidence  / search  1·2·3 zoom  ? help  q quit"
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
