//! Der Overview-Tab: links die Historie als Commit-Graph — Lanes, ein Badge
//! (Agent oder Mensch), Pillen für Refs, Review, Seal und Intent, der
//! Betreff und gedimmt der Auftrag der Session —, rechts der gewählte Commit
//! mit seinen Sessions. Mit Nerd Font (`--nerd-font` oder
//! `MINDS_NERD_FONT=1`) runde Pillen und Icons, sonst Blöcke, die in jedem
//! Terminal stehen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use minds_reader::model::Verdict;
use minds_reader::overview::{CommitRow, RefLabel, SealMark, WipSession, WipState, one_line};

use crate::overview::MAX_SESSION_LINES;

use crate::app::App;
use crate::overview::{OverviewFocus, Selected, initials};
use crate::theme;

/// Die Farben der Lanes, reihum.
const LANES: [Color; 7] = [
    Color::Cyan,
    Color::Magenta,
    Color::Green,
    Color::Yellow,
    Color::Indexed(75),
    Color::Indexed(208),
    Color::Indexed(141),
];

fn lane(index: usize) -> Color {
    LANES[index % LANES.len()]
}

/// Ein Pillen-Etikett: mit Nerd Font rund (`` … ``), sonst ein Block.
fn pill(spans: &mut Vec<Span<'static>>, text: String, bg: Color, nerd: bool) {
    let body = Style::default().fg(Color::Black).bg(bg);
    if nerd {
        spans.push(Span::styled("\u{e0b6}", Style::default().fg(bg)));
        spans.push(Span::styled(text, body));
        spans.push(Span::styled("\u{e0b4}", Style::default().fg(bg)));
    } else {
        spans.push(Span::styled(format!(" {text} "), body));
    }
    spans.push(Span::raw(" "));
}

/// Das Badge: Initialen des Agenten (magenta, mit `◆` bzw. Roboter-Icon —
/// nicht nur die Farbe unterscheidet) oder des Autors (cyan).
fn badge(spans: &mut Vec<Span<'static>>, name: &str, agent: bool, nerd: bool) {
    if agent {
        let mark = if nerd { "\u{f06a9} " } else { "◆" };
        pill(
            spans,
            format!("{mark}{}", initials(name)),
            theme::AGENT,
            nerd,
        );
    } else {
        pill(spans, initials(name), theme::HUMAN, nerd);
    }
}

/// Die Ref-Farben — nie das Grün der Belege (`theme::OK`): Ein Branch, der
/// `✓approved` heißt, darf nicht wie ein Review aussehen.
const HEAD_REF: Color = Color::Indexed(31);
const BRANCH_REF: Color = Color::Indexed(24);
const TAG_REF: Color = Color::Indexed(94);

/// Die Pillen eines Commits: Refs, dann was Minds weiß.
fn pills(spans: &mut Vec<Span<'static>>, row: &CommitRow, nerd: bool) {
    // Refs tragen immer ein Präfix (Icon oder `@`/`#`), nie eine
    // Belegfarbe — sonst könnte ein Branch-Name einen Status vortäuschen.
    let (branch, tag) = if nerd {
        ("\u{e0a0} ", "\u{f02b} ")
    } else {
        ("@", "#")
    };
    for label in &row.refs {
        let (text, bg) = match label {
            RefLabel::Head(name) => (format!("{branch}{name} ● HEAD"), HEAD_REF),
            RefLabel::Branch(name) => (format!("{branch}{name}"), BRANCH_REF),
            RefLabel::Tag(name) => (format!("{tag}{name}"), TAG_REF),
            RefLabel::Detached => ("● HEAD (detached)".into(), HEAD_REF),
            RefLabel::More(n) => (format!("+{n}"), Color::Indexed(238)),
        };
        let body = Style::default().fg(Color::White).bg(bg);
        if nerd {
            spans.push(Span::styled("\u{e0b6}", Style::default().fg(bg)));
            spans.push(Span::styled(text, body));
            spans.push(Span::styled("\u{e0b4}", Style::default().fg(bg)));
        } else {
            spans.push(Span::styled(format!(" {text} "), body));
        }
        spans.push(Span::raw(" "));
    }
    // Was Minds weiß — nur an Commits, die Minds kennt (Session, Change-Id
    // oder ein Review), sonst bliebe jeder fremde Commit „open".
    if row.sessions.is_empty() && row.change.is_none() && row.review == Verdict::Open {
        return;
    }
    if let Some(seal) = row.seal {
        let (text, color) = match seal {
            SealMark::Sealed => ("◈ sealed", theme::OK),
            SealMark::Unsigned => ("◇ sealed · unsigned", Color::Gray),
            SealMark::Partial => ("◇ sealed · first 64 checked", Color::Gray),
            SealMark::Incomplete => ("◇ incomplete", theme::REVIEW),
            SealMark::Unsealed => ("· unsealed", Color::Gray),
            SealMark::Tampered => ("✗ tampered", theme::DELETE),
        };
        pill(spans, text.into(), color, nerd);
    }
    let (glyph, word, style) = theme::verdict(row.review);
    pill(
        spans,
        format!("{glyph} {word}"),
        style.fg.unwrap_or(Color::Gray),
        nerd,
    );
    if row.intent {
        pill(spans, "⚑ intent".into(), theme::HUMAN, nerd);
    }
    if row.sessions.len() > 1 {
        pill(
            spans,
            format!("◉{}", row.sessions.len()),
            theme::AGENT,
            nerd,
        );
    }
}

/// Die Zellen des Graphen als Spans: je Lane das Zeichen und, wo eine
/// waagrechte Linie weiterläuft, `─`.
fn graph_spans(row: &CommitRow, width: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for cell in &row.graph {
        let style = Style::default().fg(lane(cell.lane));
        let glyph = if cell.glyph == '●' {
            Span::styled("●", style.add_modifier(Modifier::BOLD))
        } else {
            Span::styled(cell.glyph.to_string(), style)
        };
        spans.push(glyph);
        spans.push(Span::styled(if cell.joint { "─" } else { " " }, style));
    }
    let used = row.graph.len() * 2;
    if used < width * 2 {
        spans.push(Span::raw(" ".repeat(width * 2 - used)));
    }
    spans
}

fn wip_spans(wip: &WipSession, width: usize, nerd: bool) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled("◌", Style::default().fg(theme::AGENT)),
        Span::styled("┄", theme::dim()),
    ];
    if width > 1 {
        spans.push(Span::raw(" ".repeat((width - 1) * 2)));
    }
    spans.push(Span::raw(" "));
    badge(&mut spans, &wip.agent, true, nerd);
    // Nur, was bekannt ist: `SessionEnd` gesehen oder nicht.
    let (word, style) = match wip.state {
        WipState::Ended => ("ended · no commit linked  ", theme::dim()),
        WipState::Open => (
            "open · no commit linked yet  ",
            Style::default()
                .fg(theme::AGENT)
                .add_modifier(Modifier::ITALIC),
        ),
        WipState::Unknown => ("no commit linked  ", theme::dim()),
    };
    spans.push(Span::styled(word, style));
    spans.push(Span::styled(wip.request.clone(), theme::dim()));
    spans
}

/// Zeichnet den Tab.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.overview else {
        return;
    };
    let nerd = app.nerd;
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)]).areas(area);

    let overview = match &state.data {
        Ok(overview) => overview,
        Err(why) => {
            frame.render_widget(
                Paragraph::new(format!("History not readable: {why}"))
                    .style(theme::dim())
                    .block(Block::bordered().title(" OVERVIEW ")),
                area,
            );
            return;
        }
    };
    let width = overview.lanes.max(1);
    let total = overview.wip.len() + overview.rows.len();
    // Was fehlt, steht im Titel — nie still.
    let height = usize::from(left.height.saturating_sub(2));
    let first = crate::view::offset(state.cursor, total, height);
    let mut lines: Vec<Line> = Vec::new();
    if total == 0 {
        lines.push(Line::from(Span::styled(
            "No commits yet — the history appears here once there is one.",
            theme::dim(),
        )));
    }
    for i in first..total.min(first + height.max(1)) {
        let mut spans = match i.checked_sub(overview.wip.len()) {
            None => wip_spans(&overview.wip[i], width, nerd),
            Some(at) => {
                let row = &overview.rows[at];
                let mut spans = graph_spans(row, width);
                spans.push(Span::raw(" "));
                match &row.agent {
                    Some(agent) => badge(&mut spans, agent, true, nerd),
                    None => badge(&mut spans, &row.author, false, nerd),
                }
                pills(&mut spans, row, nerd);
                spans.push(Span::raw(row.subject.clone()));
                if let Some(request) = &row.request {
                    spans.push(Span::styled(format!("  • {request}"), theme::dim()));
                }
                spans
            }
        };
        if i == state.cursor {
            spans.insert(0, Span::styled("▌", theme::title()));
            lines.push(
                Line::from(spans).style(
                    Style::default()
                        .bg(Color::Indexed(236))
                        .add_modifier(Modifier::BOLD),
                ),
            );
        } else {
            spans.insert(0, Span::raw(" "));
            lines.push(Line::from(spans));
        }
    }
    let mut title = if overview.budget_hit {
        format!(
            " OVERVIEW · newest {} commits (read budget reached)",
            overview.rows.len()
        )
    } else if overview.truncated {
        format!(" OVERVIEW · newest {} commits", overview.rows.len())
    } else {
        format!(" OVERVIEW · {} commits", overview.rows.len())
    };
    if overview.wip_more > 0 {
        title.push_str(&format!(
            " · {} more sessions without a commit",
            overview.wip_more
        ));
    }
    if overview.skipped_refs > 0 {
        title.push_str(&format!(" · {} ref(s) not shown", overview.skipped_refs));
    }
    if overview.refs_capped {
        title.push_str(" · not all branches/tags read");
    }
    if overview.incomplete {
        title.push_str(" · history incomplete (object missing, too large or not a commit)");
    }
    title.push(' ');
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title).title_style(theme::title())),
        left,
    );

    draw_detail(frame, app, right);
}

/// Rechts: der gewählte Commit (oder die WIP-Session) und seine Sessions.
fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.overview else {
        return;
    };
    // Ohne automatischen Umbruch: Jede Zeile ist genau eine Bildschirmzeile
    // (das Fenster über die Sessions rechnet damit); lange Texte bricht das
    // Detail selbst um, auf höchstens wenige Zeilen.
    let inner = usize::from(area.width.saturating_sub(2)).max(1);
    let label = |key: &str| Span::styled(format!("{key:<8}"), theme::dim());
    let mut lines: Vec<Line> = Vec::new();
    match state.selected() {
        None => {}
        Some(Selected::Wip(wip)) => {
            lines.push(Line::from(Span::styled(
                "SESSION WITHOUT A COMMIT",
                Style::default()
                    .fg(theme::AGENT)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                label("Agent"),
                Span::raw(wip.agent.clone()),
            ]));
            if let Some(started) = &wip.started {
                lines.push(Line::from(vec![
                    label("Started"),
                    Span::raw(started.clone()),
                ]));
            }
            lines.push(Line::from(vec![
                label("Commit"),
                Span::styled("none linked", theme::dim()),
            ]));
            lines.push(Line::from(vec![
                label("State"),
                Span::raw(match wip.state {
                    WipState::Ended => "ended (SessionEnd seen)",
                    WipState::Open => "open — no SessionEnd seen, it may still run",
                    WipState::Unknown => "unknown (no lineage recorded)",
                }),
            ]));
            lines.push(Line::raw(""));
            for part in wrapped(&wip.request, inner, 6) {
                lines.push(Line::raw(part));
            }
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "Enter: its graph in Sessions",
                theme::dim(),
            )));
        }
        Some(Selected::Commit(row)) => {
            let id = row.id.to_string();
            lines.push(Line::from(Span::styled(
                format!("COMMIT {}", id.get(..10).unwrap_or(&id)),
                Style::default()
                    .fg(theme::CHANGE)
                    .add_modifier(Modifier::BOLD),
            )));
            for part in wrapped(&row.subject, inner, 3) {
                lines.push(Line::raw(part));
            }
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                label("Author"),
                Span::raw(row.author.clone()),
            ]));
            lines.push(Line::from(vec![label("Date"), Span::raw(row.date.clone())]));
            if !row.refs.is_empty() {
                let names: Vec<String> = row
                    .refs
                    .iter()
                    .map(|label| match label {
                        RefLabel::Head(name) => format!("{name} (HEAD)"),
                        RefLabel::Branch(name) => name.clone(),
                        RefLabel::Tag(name) => format!("tag {name}"),
                        RefLabel::Detached => "HEAD (detached)".into(),
                        RefLabel::More(n) => format!("+{n} more"),
                    })
                    .collect();
                lines.push(Line::from(vec![
                    label("Refs"),
                    Span::raw(crate::view::clip(
                        &names.join(", "),
                        inner.saturating_sub(8),
                    )),
                ]));
            }
            if let Some(change) = &row.change {
                lines.push(Line::from(vec![
                    label("Change"),
                    Span::raw(change.to_string()),
                ]));
            }
            let (glyph, word, style) = theme::verdict(row.review);
            lines.push(Line::from(vec![
                label("Review"),
                Span::styled(format!("{glyph} {word}"), style),
            ]));
            if row.parents.len() > 1 {
                lines.push(Line::from(vec![
                    label("Merge"),
                    Span::raw(format!("{} parents", row.parents.len())),
                ]));
            }
            lines.push(Line::raw(""));
            let focused = state.focus == OverviewFocus::Sessions;
            lines.push(Line::from(Span::styled(
                format!("Sessions ({})", row.sessions.len()),
                theme::title(),
            )));
            if row.sessions.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  no session linked in this store",
                    theme::dim(),
                )));
                lines.push(Line::from(Span::styled(
                    "  Enter: the diff in Changes",
                    theme::dim(),
                )));
            }
            // Ein Fenster um den Cursor: so viele Sessions, wie unter dem Kopf
            // Platz haben (Rahmen, Hinweise) — die gewählte bleibt sichtbar.
            let listed = row.sessions.len().min(MAX_SESSION_LINES);
            let room = usize::from(area.height)
                .saturating_sub(2 + lines.len() + 4)
                .max(1);
            let first = crate::view::offset(state.session, listed, room);
            if first > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  … {first} above"),
                    theme::dim(),
                )));
            }
            for (i, sid) in row
                .sessions
                .iter()
                .enumerate()
                .take(listed)
                .skip(first)
                .take(room)
            {
                let session = app.inspection.index().session(*sid);
                // Vor dem Entschärfen gekürzt: kein Megabyte-Auftrag je Frame.
                let agent = session.map(|s| one_line(&s.agent.name, 40));
                let request = session
                    .map(|s| one_line(&s.intent.request, 120))
                    .unwrap_or_else(|| "not readable".into());
                let id = sid.to_string();
                let marker = if focused && i == state.session {
                    Span::styled("▸ ", theme::title())
                } else {
                    Span::raw("  ")
                };
                let mut spans = vec![marker];
                match &agent {
                    Some(agent) => badge(&mut spans, agent, true, app.nerd),
                    None => pill(&mut spans, "?".into(), Color::Gray, app.nerd),
                }
                spans.push(Span::raw(format!("{}…  ", id.get(..12).unwrap_or(&id))));
                // Auf die Restbreite gekürzt: eine Session, eine Zeile.
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                spans.push(Span::styled(
                    crate::view::clip(&request, inner.saturating_sub(used)),
                    theme::dim(),
                ));
                let line = Line::from(spans);
                lines.push(if focused && i == state.session {
                    line.style(Style::default().add_modifier(Modifier::BOLD))
                } else {
                    line
                });
            }
            let below = row
                .sessions
                .len()
                .saturating_sub(first + room.min(listed - first));
            if below > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  … {below} more"),
                    theme::dim(),
                )));
            }
            if !row.sessions.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(
                    if row.sessions.len() == 1 {
                        "Enter: the session's graph · w: the commit's why chain"
                    } else if focused {
                        "↑↓ session · Enter: its graph · Esc: back to the history"
                    } else {
                        "Enter: choose a session · w: the commit's why chain"
                    },
                    theme::dim(),
                )));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" DETAIL ")
                .title_style(theme::title()),
        ),
        area,
    );
}

/// Ein Text in Stücken zu höchstens `width` Zeichen, höchstens `max` Stück;
/// das letzte endet mit `…`, wenn etwas fehlt.
fn wrapped(text: &str, width: usize, max: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = chars
        .chunks(width.max(1))
        .take(max)
        .map(|c| c.iter().collect())
        .collect();
    if chars.len() > width.max(1) * max
        && let Some(last) = out.last_mut()
    {
        *last = last.chars().take(width.max(1) - 1).collect::<String>() + "…";
    }
    out
}

#[cfg(test)]
mod tests {
    use super::wrapped;

    /// Gekürzt endet das letzte Stück mit genau einer Ellipse.
    #[test]
    fn a_cut_text_ends_in_one_ellipsis() {
        let parts = wrapped(&"x".repeat(100), 10, 3);
        assert_eq!(parts.len(), 3);
        assert!(parts[2].ends_with('…'));
        assert!(!parts[2].ends_with("……"));
        assert_eq!(parts[2].chars().count(), 10);
        assert_eq!(wrapped("short", 10, 3), ["short"]);
    }
}
