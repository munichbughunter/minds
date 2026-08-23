//! Die Herkunftskette: von der Zeile zum Intent und zur Bewertung, ein
//! Block je Glied — und auf Wunsch der Inspector, der sagt, **warum** eine
//! Kante im Index steht.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use minds_reader::model::{
    EvidenceExplanation, LinkEvidence, WhyChain, WhyStep, evidence_sentence,
};

use crate::theme;
use crate::view::{clip, offset, when};

/// Zeichnet die Kette: jedes Glied mit ✓ oder ⚠, darunter die Lücken als
/// eigener Block — und, sobald der Cursor auf dem Evidence-Glied steht, der
/// Inspector, der jede Kante erklärt.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    chain: &WhyChain,
    cursor: usize,
    inspector: Option<&[LinkEvidence]>,
) {
    let gaps = chain.gaps();
    let gaps_h = if area.height < 14 {
        0
    } else {
        (2 + gaps.len().max(1) as u16 * 2).min(10)
    };
    let inspector_h = match inspector {
        Some(links) if area.height >= 20 => (3 + links.len().max(1) as u16 * 3).min(12),
        _ => 0,
    };
    let [body, panel, gaps_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(inspector_h),
        Constraint::Length(gaps_h),
    ])
    .areas(area);

    let width = body.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    for (i, step) in chain.steps.iter().enumerate() {
        starts.push(lines.len());
        let (_, word, style, text) = describe(step, width.saturating_sub(8));
        let is_gap = gaps.iter().any(|g| g.step == i);
        let (mark, mark_style) = if is_gap {
            ("⚠", Style::default().fg(theme::REVIEW))
        } else {
            ("✓", Style::default().fg(theme::OK))
        };
        let selected = i == cursor;
        let head_style = if selected {
            style.patch(theme::title()).patch(theme::cursor())
        } else {
            style.patch(theme::title())
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{mark} "), mark_style),
            Span::styled(word.to_string(), head_style),
            Span::styled(
                if selected && enterable(step) {
                    "   Enter ↵"
                } else {
                    ""
                },
                theme::dim(),
            ),
        ]));
        for t in text {
            lines.push(Line::from(vec![Span::raw("     "), Span::raw(t)]));
        }
        if i + 1 < chain.steps.len() {
            lines.push(Line::from(Span::styled("  │", theme::dim())));
            lines.push(Line::from(Span::styled("  ▼", theme::dim())));
        }
    }
    let height = body.height as usize;
    let anchor = starts.get(cursor).copied().unwrap_or(0);
    let first = offset(anchor, lines.len(), height);
    let shown: Vec<Line> = lines.into_iter().skip(first).take(height).collect();
    frame.render_widget(Paragraph::new(shown), body);

    if inspector_h > 0
        && let Some(links) = inspector
    {
        let mut lines: Vec<Line> = Vec::new();
        if links.is_empty() {
            lines.push(Line::from(Span::styled(
                "Keine Kante — dieser Commit trägt keine Session.",
                theme::dim(),
            )));
        }
        for link in links {
            let (glyph, word, style) = theme::evidence(Some(link.evidence));
            let short: String = link.commit.to_string().chars().take(10).collect();
            let sess: String = link.session.to_string().chars().take(11).collect();
            lines.push(Line::from(vec![
                Span::styled(format!("{glyph} {word}"), style.patch(theme::title())),
                Span::raw(format!("   {short} ↔ {sess}…")),
            ]));
            lines.push(Line::from(Span::styled(
                format!("   {}", evidence_sentence(Some(link.evidence))),
                theme::dim(),
            )));
            lines.push(Line::from(Span::raw(format!(
                "   {}",
                explanation(&link.why)
            ))));
        }
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::bordered()
                    .title(" WHY IS THIS LINKED? ")
                    .title_style(theme::title().fg(theme::REVIEW))
                    .border_style(Style::default().fg(theme::REVIEW)),
            ),
            panel,
        );
    }

    if gaps_h > 0 {
        let (title, style) = if gaps.is_empty() {
            (" KEINE LÜCKE ".to_string(), Style::default().fg(theme::OK))
        } else {
            (
                format!(
                    " {} {} ",
                    gaps.len(),
                    if gaps.len() == 1 { "LÜCKE" } else { "LÜCKEN" }
                ),
                Style::default().fg(theme::REVIEW),
            )
        };
        let mut lines: Vec<Line> = Vec::new();
        if gaps.is_empty() {
            lines.push(Line::from(Span::styled(
                "Jedes Glied ist belegt — die Kette schließt sich ohne Vermutung.",
                theme::dim(),
            )));
        }
        for gap in &gaps {
            let (_, word, _, _) = describe(&chain.steps[gap.step], 0);
            lines.push(Line::from(vec![
                Span::styled("⚠ ", style),
                Span::styled(word.to_string(), theme::title()),
            ]));
            lines.push(Line::from(Span::raw(format!("  {}", gap.text))));
        }
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::bordered()
                    .title(title)
                    .title_style(style.patch(theme::title()))
                    .border_style(style),
            ),
            gaps_area,
        );
    }
}

fn enterable(step: &WhyStep) -> bool {
    match step {
        WhyStep::Evidence { .. } => true,
        WhyStep::Sessions { cards } => !cards.is_empty(),
        WhyStep::Commit { id, .. } => id.is_some(),
        _ => false,
    }
}

/// Glyph, Wort, Stil und Textzeilen eines Glieds.
fn describe(step: &WhyStep, width: usize) -> (&'static str, &'static str, Style, Vec<String>) {
    match step {
        WhyStep::Line { path, line } => (
            "▸",
            "LINE",
            Style::default().fg(theme::EXEC),
            vec![format!("{path}:{line}")],
        ),
        WhyStep::Commit { id, subject } => match id {
            Some(id) => (
                "◆",
                "COMMIT",
                Style::default().fg(theme::CHANGE),
                vec![format!(
                    "{}  {}",
                    id.to_string().chars().take(10).collect::<String>(),
                    clip(subject.as_deref().unwrap_or(""), width)
                )],
            ),
            None => (
                "◆",
                "COMMIT",
                theme::dim(),
                vec!["Blame kennt die Zeile nicht (leerer HEAD oder nicht eingecheckt)".into()],
            ),
        },
        WhyStep::Change { id } => match id {
            Some(id) => (
                "◆",
                "CHANGE",
                Style::default().fg(theme::CHANGE),
                vec![id.to_string()],
            ),
            None => (
                "◆",
                "CHANGE",
                theme::dim(),
                vec!["kein Minds-Change-Id-Trailer".into()],
            ),
        },
        WhyStep::Sessions { cards } => {
            if cards.is_empty() {
                (
                    "●",
                    "SESSION",
                    theme::dim(),
                    vec!["kein Kontext erfasst".into()],
                )
            } else {
                (
                    "●",
                    "SESSION",
                    Style::default().fg(theme::AGENT),
                    cards
                        .iter()
                        .map(|c| {
                            format!(
                                "{}…  {}  {}",
                                c.id.to_string().chars().take(11).collect::<String>(),
                                when(c.started_at.as_deref()),
                                clip(&c.summary.headline, width.saturating_sub(30))
                            )
                        })
                        .collect(),
                )
            }
        }
        WhyStep::Agent {
            name,
            version,
            model,
        } => (
            "◉",
            "AGENT",
            Style::default().fg(theme::AGENT),
            vec![format!("{name} {version} · {model}")],
        ),
        WhyStep::Intent {
            request,
            constraints,
            discarded,
        } => {
            let mut text: Vec<String> = request.lines().take(4).map(|l| clip(l, width)).collect();
            if !constraints.is_empty() {
                text.push(format!("Constraints: {}", constraints.len()));
            }
            if !discarded.is_empty() {
                text.push(format!("Verworfen: {}", discarded.len()));
            }
            ("●", "INTENT", Style::default().fg(theme::HUMAN), text)
        }
        WhyStep::Evidence { links } => {
            if links.is_empty() {
                ("·", "EVIDENCE", theme::dim(), vec!["keine Kante".into()])
            } else {
                let best = links.iter().map(|l| l.evidence).max();
                let (_, _, style) = theme::evidence(best);
                (
                    "●",
                    "EVIDENCE",
                    style,
                    links
                        .iter()
                        .map(|l| {
                            let (glyph, word, _) = theme::evidence(Some(l.evidence));
                            format!(
                                "{glyph} {word}  {}",
                                l.commit.to_string().chars().take(10).collect::<String>()
                            )
                        })
                        .collect(),
                )
            }
        }
        WhyStep::Review { state } => {
            let (glyph, word, style) = theme::verdict(state.verdict);
            let mut text = vec![word.to_string()];
            text.extend(state.notes.iter().map(|n| {
                format!(
                    "{} · {}{} · {}",
                    n.reviewer,
                    n.decision.as_str(),
                    if n.signed { " (signiert)" } else { "" },
                    clip(&n.summary, width.saturating_sub(30))
                )
            }));
            (glyph, "REVIEW", style, text)
        }
    }
}

pub(crate) fn explanation(why: &EvidenceExplanation) -> String {
    match why {
        EvidenceExplanation::Trailer { commit } => format!(
            "Der Commit {} trägt den Trailer Minds-Session-Id — beobachtet, kein Raten.",
            commit.to_string().chars().take(10).collect::<String>()
        ),
        EvidenceExplanation::Declared => "Ein Mensch hat die Verbindung erklärt (--after).".into(),
        EvidenceExplanation::Content => {
            "Nachrechenbar über den Inhalt: gelesene Bytes sind geschriebene.".into()
        }
        EvidenceExplanation::Heuristic {
            shared_files,
            seconds_apart,
            in_window,
        } => {
            let files = if shared_files.is_empty() {
                "keine gemeinsame Datei".to_string()
            } else {
                format!(
                    "{} gemeinsame Datei(en): {}",
                    shared_files.len(),
                    shared_files.join(", ")
                )
            };
            let time = match (seconds_apart, in_window) {
                (Some(s), Some(true)) => format!("Commit {s} s nach Session-Ende, im Fenster"),
                (Some(s), Some(false)) => {
                    format!("Commit {s} s nach Session-Ende, außerhalb des Fensters")
                }
                _ => "Zeitabstand nicht bestimmbar".to_string(),
            };
            format!("Nachgerechnet, nicht protokolliert: {files}; {time}.")
        }
        EvidenceExplanation::Unknown { reason } => {
            format!("Gründe nicht rekonstruierbar: {reason}.")
        }
    }
}
