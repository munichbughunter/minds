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
use minds_reader::overview::{CommitRow, OverviewCounts, RefLabel, WipSession, WipState, one_line};

use crate::overview::MAX_SESSION_LINES;

use crate::app::App;
use crate::overview::{ConformanceMark, OverviewFocus, Selected, initials};
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

/// Das Badge: Initialen des Agenten (magenta, mit `◉` bzw. Roboter-Icon —
/// nicht nur die Farbe unterscheidet) oder des Autors (cyan).
fn badge(spans: &mut Vec<Span<'static>>, name: &str, agent: bool, nerd: bool) {
    if agent {
        let mark = if nerd {
            "\u{f06a9} "
        } else {
            theme::node(&minds_reader::graph::NodeKind::Agent).0
        };
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
/// Die Refs eines Commits als kompakte Pillen — direkt hinter dem Badge
/// (C3).
fn refs(spans: &mut Vec<Span<'static>>, row: &CommitRow, nerd: bool) {
    // Refs tragen immer ein Präfix (Icon oder `@`/`#`), nie eine
    // Belegfarbe — sonst könnte ein Branch-Name einen Status vortäuschen.
    let (branch, tag) = if nerd {
        ("\u{e0a0} ", "\u{f02b} ")
    } else {
        ("@", "#")
    };
    for label in &row.refs {
        let (text, bg) = match label {
            RefLabel::Head(name) => (format!("{branch}{name} {} HEAD", theme::HEAD), HEAD_REF),
            RefLabel::Branch(name) => (format!("{branch}{name}"), BRANCH_REF),
            RefLabel::Tag(name) => (format!("{tag}{name}"), TAG_REF),
            RefLabel::Detached => (format!("{} HEAD (detached)", theme::HEAD), HEAD_REF),
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
}

/// Was Minds über den Commit weiß, als Pillen — rechtsbündig in der Zeile
/// (C3): Conformance, Seal-Zustand, abweichendes Review, Intent, mehrere
/// Sessions. Normalzustände bekommen keine Pille.
fn states(
    spans: &mut Vec<Span<'static>>,
    row: &CommitRow,
    conformance: Option<ConformanceMark>,
    nerd: bool,
) {
    // Die Conformance gilt jedem Commit, auch einem ohne Session — eine
    // Pille, sobald sie gerechnet ist (gewählter Commit, HEAD). Farbe nie
    // allein: Glyph und Wort tragen das Ergebnis.
    if let Some(mark) = conformance {
        pill(spans, conformance_pill(mark), conformance_color(mark), nerd);
    }
    // Was Minds weiß — nur an Commits, die Minds kennt (Session, Change-Id
    // oder ein Review), sonst bliebe jeder fremde Commit „open".
    if row.sessions.is_empty() && row.change.is_none() && row.review == Verdict::Open {
        return;
    }
    if let Some(seal) = row.seal {
        let (glyph, word, style) = theme::seal(seal);
        pill(
            spans,
            format!("{glyph} {word}"),
            style.fg.unwrap_or(Color::Gray),
            nerd,
        );
    }
    // Review „open" ist der Normalzustand — keine Pille; nur Abweichungen
    // (approved, needs work, rejected) sind laut (A4).
    if row.review != Verdict::Open {
        let (glyph, word, style) = theme::verdict(row.review);
        pill(
            spans,
            format!("{glyph} {word}"),
            style.fg.unwrap_or(Color::Gray),
            nerd,
        );
    }
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

/// Der Text der Conformance-Pille.
pub(crate) fn conformance_pill(mark: ConformanceMark) -> String {
    use minds_reader::conformance::Outcome;
    match mark.outcome {
        Outcome::Pass => "✓ conform".to_owned(),
        Outcome::Drift => format!("✗ drift {}", mark.drift),
        Outcome::Unknown => format!("? conform unknown {}", mark.unknown),
    }
}

fn conformance_color(mark: ConformanceMark) -> Color {
    use minds_reader::conformance::Outcome;
    match mark.outcome {
        Outcome::Pass => theme::OK,
        Outcome::Drift => theme::DELETE,
        Outcome::Unknown => Color::Gray,
    }
}

/// Die Zellen des Graphen als Spans: je Lane das Zeichen und, wo eine
/// waagrechte Linie weiterläuft, `─`.
fn graph_spans(row: &CommitRow, width: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for cell in &row.graph {
        let style = Style::default().fg(lane(cell.lane));
        // Der Reader zeichnet den Commit-Knoten als `●`; die Oberfläche
        // zeigt ihn mit dem Register-Glyph für „Commit" (A1) — `●` heißt
        // dort „beobachtet".
        let glyph = if cell.glyph == '●' {
            let (commit, _, _) = theme::node(&minds_reader::graph::NodeKind::Commit(row.id));
            Span::styled(commit, style.add_modifier(Modifier::BOLD))
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
        Span::styled(theme::PENDING, Style::default().fg(theme::AGENT)),
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

/// Die Zahlen des Titels (C3.1): keine Prozente, die den Nenner verstecken;
/// gekappt sagt die Zahl, dass sie nur das Gelesene zählt.
pub(crate) fn counts_title(counts: &OverviewCounts, budget_hit: bool) -> String {
    let commits = if counts.limited {
        let why = if budget_hit {
            "read budget reached"
        } else {
            "limit"
        };
        format!("{} commits read ({why})", counts.commits)
    } else {
        format!("{} commits", counts.commits)
    };
    format!(
        "{commits} · {} with an agent session",
        counts.with_agent_session
    )
}

/// Zeichnet den Tab.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.overview else {
        return;
    };
    let nerd = app.nerd;
    // Unter 100 Spalten nur eine Fläche (C3): die Liste — oder, mit dem
    // Fokus rechts, das Detail.
    let narrow = area.width < 100;
    let [left, right] = if narrow {
        [area, area]
    } else {
        Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)]).areas(area)
    };
    if narrow && state.focus == OverviewFocus::Sessions {
        draw_detail(frame, app, area);
        return;
    }
    // Die Zeilenbreite: Rahmen links/rechts und die Cursor-Spalte.
    let row_w = usize::from(left.width.saturating_sub(3));

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
    // Nur die Treffer des Filters aus `/` (B4); ohne Filter alle.
    let shown = state.shown();
    // Was fehlt, steht im Titel — nie still.
    let height = usize::from(left.height.saturating_sub(2));
    let at = shown.iter().position(|&i| i == state.cursor).unwrap_or(0);
    let first = crate::view::offset(at, shown.len(), height);
    let mut lines: Vec<Line> = Vec::new();
    if total == 0 {
        lines.push(Line::from(Span::styled(
            "No commits yet — the history appears here once there is one.",
            theme::dim(),
        )));
    }
    for &i in shown.iter().skip(first).take(height.max(1)) {
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
                // Lanes · Badge · Refs · **Betreff** · Auftrag (gedimmt) —
                // rechtsbündig die Zustände (C3): Was man überfliegt, steht
                // vorn; gekürzt wird der Text, nie die Zustände.
                refs(&mut spans, row, nerd);
                let mark = state.conformance.get(&row.id).copied().flatten();
                let mut right: Vec<Span<'static>> = Vec::new();
                states(&mut right, row, mark, nerd);
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                let right_w: usize = right.iter().map(|s| s.content.chars().count()).sum();
                let room = row_w.saturating_sub(used + right_w + 1);
                let mut text = row.subject.clone();
                if let Some(request) = &row.request {
                    text.push_str(&format!("  • {request}"));
                }
                let text = crate::view::clip(&text, room);
                let subject_len = row.subject.chars().count().min(text.chars().count());
                let subject: String = text.chars().take(subject_len).collect();
                let rest: String = text.chars().skip(subject_len).collect();
                let pad = room.saturating_sub(text.chars().count());
                spans.push(Span::raw(subject));
                spans.push(Span::styled(rest, theme::dim()));
                spans.push(Span::raw(" ".repeat(pad + 1)));
                spans.extend(right);
                spans
            }
        };
        // Der eine Auswahlstil (B7): theme::cursor(), wie überall.
        if i == state.cursor {
            spans.insert(0, Span::raw(" "));
            lines.push(Line::from(spans).style(theme::cursor()));
        } else {
            spans.insert(0, Span::raw(" "));
            lines.push(Line::from(spans));
        }
    }
    let (_, mark) = theme::pane(state.focus == OverviewFocus::Graph);
    // Die Zahlen genau einmal, hier (C3.1) — aus den gelesenen Zeilen.
    let mut title = format!(
        " {}OVERVIEW · {}",
        mark,
        counts_title(&overview.counts(), overview.budget_hit)
    );
    if !state.filter.is_empty() {
        title.push_str(&format!(
            " · filter: {} ({} of {total})",
            state.filter,
            shown.len()
        ));
    }
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
        Paragraph::new(lines).block({
            let (border, _) = theme::pane(state.focus == OverviewFocus::Graph);
            Block::bordered()
                .title(title)
                .title_style(theme::title())
                .border_style(border)
        }),
        left,
    );

    if !narrow {
        draw_detail(frame, app, right);
    }
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
                    Span::raw(crate::view::when(Some(started))),
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
            // Im Detail absolut, in UTC (C4).
            lines.push(Line::from(vec![
                label("Date"),
                Span::raw(format!("{} UTC", row.date.trim_end_matches('Z'))),
            ]));
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
            // „Kann ich diesem Commit trauen?" (C3): Seal-Zustand, Review —
            // getrennt, nie verschmolzen.
            if let Some(seal) = row.seal {
                let (glyph, word, style) = theme::seal(seal);
                lines.push(Line::from(vec![
                    label("Seal"),
                    Span::styled(format!("{glyph} {word}"), style),
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
            // Die nächste Aktion.
            lines.push(Line::from(Span::styled(
                "Enter: how it was made · v: verify · d: diff",
                theme::dim(),
            )));
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
        Paragraph::new(lines).block({
            let (border, mark) = theme::pane(state.focus == OverviewFocus::Sessions);
            Block::bordered()
                .title(format!(" {mark}DETAIL "))
                .title_style(theme::title())
                .border_style(border)
        }),
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
