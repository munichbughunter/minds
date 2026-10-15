//! Der Graph einer Session: Kopf, die Absicht als Box, darunter die Spur —
//! und unter dem Cursor die Details des gewählten Knotens.

use minds_core::SessionId;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::app::App;
use crate::layout::Row;
use crate::theme;
use crate::view::{clip, offset, when};
use minds_reader::graph::NodeKind;
use minds_reader::model::evidence_sentence;

/// Zeichnet den Graphen. `cursor` ist `None` für die Vorschau neben der
/// Liste: kein Fokus, also keine hervorgehobene Zeile und kein Detailkasten
/// — die gehören der gelegten Ebene, die man mit ↑↓ bewegt.
pub fn draw(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    id: SessionId,
    rows: &[Row],
    cursor: Option<usize>,
    timeline: bool,
) {
    let Some(card) = app.inspection.card(id) else {
        frame.render_widget(
            Paragraph::new("Session unreadable.").style(theme::dim()),
            area,
        );
        return;
    };
    let focused = cursor.and_then(|c| rows.get(c));
    // Der Intent-Knoten trägt Auftrag, Constraints und verworfene Ansätze —
    // seine Box darf höher werden als die übrigen, wenn Platz ist.
    let detail_cap = match focused.map(|r| &r.kind) {
        Some(NodeKind::Intent) => (area.height / 2).max(12),
        _ => 12,
    };
    let detail_h = focused
        .and_then(|r| graph_detail_len(app, id, r))
        .map(|n| (n as u16 + 2).min(detail_cap))
        .unwrap_or(0);
    let you = you_lines(app, id, area.height >= 30);
    let [head, intent, body, detail] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(you.len() as u16 + 2),
        Constraint::Min(1),
        Constraint::Length(detail_h),
    ])
    .areas(area);

    // Kopf: Id · Akteur · Umfang · Beleg · Verdicts — auch der
    // Beweiszustand der Seals (ADR-0011) steht hier, nicht nur das Review.
    let (ev_glyph, ev_word, ev_style) = theme::evidence(card.evidence);
    let (v_glyph, v_word, v_style) = theme::verdict(card.review.verdict);
    let (s_glyph, s_word, s_style) = theme::provenance(&card.provenance);
    let mut seal_summary = card
        .evidence_state()
        .map(|s| format!(" · {}", s.summary()))
        .unwrap_or_default();
    // Die uebrigen Achsen und Beziehungen, wo vorhanden: Epoche, Deutung,
    // Content-Uebergaben — der Kopf zeigt Beweiszustaende, nicht nur Logs.
    if let Some((k, n)) = card.epoch_position {
        seal_summary.push_str(&format!(" · epoch {k}/{n}"));
    }
    if card.uninterpreted_calls > 0 {
        seal_summary.push_str(&format!(" · ◐ {} uninterpreted", card.uninterpreted_calls));
    }
    if card.handovers > 0 {
        seal_summary.push_str(&format!(" · ⇄ {} handover(s)", card.handovers));
    }
    // Die Summary wächst — geclippt, damit die hinten stehenden Verdikte
    // (Kanten-Beleg, Review) auf schmalen Terminals nicht abgeschnitten
    // werden.
    let seal_summary = clip(&seal_summary, 56);
    let short: String = card.id.to_string().chars().take(11).collect();
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!("SESSION {short}… "),
                    theme::title().fg(theme::AGENT),
                ),
                Span::styled(if timeline { "TIMELINE" } else { "GRAPH" }, theme::dim()),
            ]),
            Line::from(vec![
                Span::styled(
                    card.summary.actor.clone(),
                    Style::default().fg(theme::AGENT),
                ),
                Span::raw(format!(
                    " · {} · {} file(s) · {}/{} tokens · ",
                    when(card.started_at.as_deref()),
                    card.summary.files,
                    card.summary.input_tokens,
                    card.summary.output_tokens
                )),
                Span::styled(format!("{s_glyph} {s_word}{seal_summary} · "), s_style),
                Span::styled(format!("{ev_glyph} {ev_word}  "), ev_style),
                Span::styled(format!("{v_glyph} {v_word}"), v_style),
            ]),
        ]),
        head,
    );

    // Die Absicht.
    let inner = intent.width.saturating_sub(4) as usize;
    let you: Vec<Line> = you
        .into_iter()
        .map(|(text, style)| Line::from(Span::styled(clip(&text, inner), style)))
        .collect();
    frame.render_widget(
        Paragraph::new(you).block(
            Block::bordered()
                .title(" YOU ")
                .title_style(theme::title().fg(theme::HUMAN))
                .border_style(Style::default().fg(theme::HUMAN)),
        ),
        intent,
    );

    // Die Spur.
    let height = body.height as usize;
    let first = offset(cursor.unwrap_or(0), rows.len(), height);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, row)| {
            let (glyph, word, style) = theme::node(&row.kind);
            let mut spans = vec![
                Span::styled(row.prefix.clone(), theme::lane(row.depth)),
                Span::styled(format!("{glyph} "), style),
                Span::styled(format!("{word} "), style.patch(theme::title())),
                Span::raw(clip(
                    &row.label,
                    (body.width as usize)
                        .saturating_sub(row.prefix.chars().count() + word.len() + 20),
                )),
            ];
            if timeline && let Some(at) = &row.at {
                spans.push(Span::styled(format!("  {}", when(Some(at))), theme::dim()));
            }
            let mut line = Line::from(spans);
            if Some(i) == cursor {
                line = line.style(theme::cursor());
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);

    // Die Details unter dem Cursor.
    if detail_h > 0
        && let Some(row) = focused
        && let Some(lines) = graph_detail(app, id, row)
    {
        let (_, word, style) = theme::node(&row.kind);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::bordered()
                    .title(format!(" {word} "))
                    .title_style(style.patch(theme::title()))
                    .border_style(style),
            ),
            detail,
        );
    }
}

/// Höchstens so viele Zeilen je mehrzeiligem Detailwert; der Rest wird
/// gezählt, nicht verschwiegen.
const VALUE_MAX: usize = 8;

/// Der Inhalt der YOU-Box: bis zu drei Zeilen Auftrag (eine, wenn `roomy`
/// nicht gilt) und, wenn vorhanden, die gedimmte Claim-Zeile mit den
/// verworfenen Ansätzen und Constraints.
fn you_lines(app: &App, id: SessionId, roomy: bool) -> Vec<(String, Style)> {
    let detail = app.inspection.graph(id).and_then(|g| {
        g.nodes
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Intent))
            .map(|n| n.detail.clone())
    });
    let value = |key: &str| {
        detail
            .as_ref()
            .and_then(|d| d.iter().find(|(k, _)| k == key))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let count = |key: &str| value(key).lines().filter(|l| !l.trim().is_empty()).count();
    let mut lines: Vec<(String, Style)> = value("Prompt")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(if roomy { 3 } else { 1 })
        .map(|l| (l.to_string(), Style::default()))
        .collect();
    if lines.is_empty() {
        let headline = app
            .inspection
            .card(id)
            .map(|c| c.summary.headline.clone())
            .unwrap_or_default();
        lines.push((headline, Style::default()));
    }
    let (discarded, constraints) = (count("Discarded"), count("Constraints"));
    let mut parts = Vec::new();
    if discarded > 0 {
        parts.push(format!("{discarded} discarded"));
    }
    if constraints > 0 {
        parts.push(format!("{constraints} constraints"));
    }
    if !parts.is_empty() {
        let (glyph, _, style) = theme::claim();
        lines.push((
            format!("{glyph} {} — select YOU for details", parts.join(" · ")),
            style,
        ));
    }
    lines
}

fn graph_detail_len(app: &App, id: SessionId, row: &Row) -> Option<usize> {
    graph_detail(app, id, row).map(|l| l.len())
}

fn graph_detail(app: &App, id: SessionId, row: &Row) -> Option<Vec<Line<'static>>> {
    let graph = app.inspection.graph(id)?;
    let node = graph.nodes.get(row.node)?;
    // Was der Intent-Knoten trägt, ist aufgezeichnet, nicht beobachtet: im
    // Claim-Stil, nie im Stil eines Belegs.
    let value_style = if matches!(node.kind, NodeKind::Intent) {
        theme::claim().2
    } else {
        Style::default()
    };
    let mut lines: Vec<Line> = Vec::new();
    for (k, v) in node.detail.iter().filter(|(k, _)| k != "Evidence") {
        // Mehrzeilige Werte vollständig (gekappt mit Hinweis), leere gar
        // nicht — leer ist leer.
        let values: Vec<&str> = v.lines().filter(|l| !l.trim().is_empty()).collect();
        for (i, value) in values.iter().take(VALUE_MAX).enumerate() {
            let key = if i == 0 { k.as_str() } else { "" };
            lines.push(Line::from(vec![
                Span::styled(format!("{key:<10} "), theme::dim()),
                Span::styled(value.to_string(), value_style),
            ]));
        }
        if values.len() > VALUE_MAX {
            lines.push(Line::from(Span::styled(
                format!("{:<10} … {} more", "", values.len() - VALUE_MAX),
                theme::dim(),
            )));
        }
    }
    if let Some(at) = &node.at {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<10} ", "Time"), theme::dim()),
            Span::raw(when(Some(at))),
        ]));
    }
    // Eine Änderung erklärt ihren Beleg — Satz und, wo nötig, Nachrechnung.
    let commit = match &row.kind {
        NodeKind::Commit(commit) => Some(*commit),
        NodeKind::Change(change) => app.inspection.card(id).and_then(|card| {
            card.commits
                .iter()
                .copied()
                .find(|c| app.inspection.index().change_of(*c) == Some(change))
        }),
        _ => None,
    };
    if let Some(commit) = commit
        && let Some(link) = app.inspection.evidence(app.repo, commit, id)
    {
        let (glyph, word, style) = theme::evidence(Some(link.evidence));
        lines.push(Line::from(vec![
            Span::styled(format!("{:<10} ", "Evidence"), theme::dim()),
            Span::styled(format!("{glyph} {word}"), style),
        ]));
        lines.push(Line::from(Span::styled(
            format!("{:<10} {}", "", evidence_sentence(Some(link.evidence))),
            theme::dim(),
        )));
        lines.push(Line::from(Span::raw(format!(
            "{:<10} {}",
            "",
            crate::view::why::explanation(&link.why)
        ))));
    }
    if row.count > 1 {
        lines.push(Line::from(Span::styled(
            format!("{} calls merged — zoom 2 shows each one", row.count),
            theme::dim(),
        )));
    }
    if lines.is_empty() {
        return None;
    }
    Some(lines)
}
