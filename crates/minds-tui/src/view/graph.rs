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

/// Zeichnet den Graphen.
pub fn draw(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    id: SessionId,
    rows: &[Row],
    cursor: usize,
    timeline: bool,
) {
    let Some(card) = app.inspection.card(id) else {
        frame.render_widget(
            Paragraph::new("Session nicht lesbar.").style(theme::dim()),
            area,
        );
        return;
    };
    let detail_h = rows
        .get(cursor)
        .and_then(|r| app.views.last().and_then(|_| graph_detail_len(app, id, r)))
        .map(|n| (n as u16 + 2).min(12))
        .unwrap_or(0);
    let [head, intent, body, detail] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(detail_h),
    ])
    .areas(area);

    // Kopf: Id · Akteur · Umfang · Beleg · Verdict.
    let (ev_glyph, ev_word, ev_style) = theme::evidence(card.evidence);
    let (v_glyph, v_word, v_style) = theme::verdict(card.review.verdict);
    let short: String = card.id.to_string().chars().take(11).collect();
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!("SESSION {short}… "),
                    theme::title().fg(theme::AGENT),
                ),
                Span::styled(if timeline { "ZEITLEISTE" } else { "GRAPH" }, theme::dim()),
            ]),
            Line::from(vec![
                Span::styled(
                    card.summary.actor.clone(),
                    Style::default().fg(theme::AGENT),
                ),
                Span::raw(format!(
                    " · {} · {} Dateien · {}/{} Token · ",
                    when(card.started_at.as_deref()),
                    card.summary.files,
                    card.summary.input_tokens,
                    card.summary.output_tokens
                )),
                Span::styled(format!("{ev_glyph} {ev_word}  "), ev_style),
                Span::styled(format!("{v_glyph} {v_word}"), v_style),
            ]),
        ]),
        head,
    );

    // Die Absicht.
    frame.render_widget(
        Paragraph::new(clip(
            &card.summary.headline,
            intent.width.saturating_sub(4) as usize,
        ))
        .block(
            Block::bordered()
                .title(" YOU ")
                .title_style(theme::title().fg(theme::HUMAN))
                .border_style(Style::default().fg(theme::HUMAN)),
        ),
        intent,
    );

    // Die Spur.
    let height = body.height as usize;
    let first = offset(cursor, rows.len(), height);
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
            if i == cursor {
                line = line.style(theme::cursor());
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);

    // Die Details unter dem Cursor.
    if detail_h > 0
        && let Some(row) = rows.get(cursor)
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

fn graph_detail_len(app: &App, id: SessionId, row: &Row) -> Option<usize> {
    graph_detail(app, id, row).map(|l| l.len())
}

fn graph_detail(app: &App, id: SessionId, row: &Row) -> Option<Vec<Line<'static>>> {
    let graph = app.inspection.graph(id)?;
    let node = graph.nodes.get(row.node)?;
    let mut lines: Vec<Line> = node
        .detail
        .iter()
        .filter(|(k, _)| k != "Beleg")
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k:<10} "), theme::dim()),
                Span::raw(v.lines().next().unwrap_or("").to_string()),
            ])
        })
        .collect();
    if let Some(at) = &node.at {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<10} ", "Zeit"), theme::dim()),
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
            Span::styled(format!("{:<10} ", "Beleg"), theme::dim()),
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
            format!("{} Aufrufe zusammengefasst — Zoom 2 zeigt jeden", row.count),
            theme::dim(),
        )));
    }
    if lines.is_empty() {
        return None;
    }
    Some(lines)
}
