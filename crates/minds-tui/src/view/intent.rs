//! Der Intent-Tab: links die Anker, rechts der gewählte — was er sagt und
//! was davon geprüft ist. Nie mehr behaupten als `minds intent show` und
//! `minds verify`: „valid" nur nach geprüfter Signatur, der Snapshot nur bei
//! belegtem Anker, Bindungen der Sessions als Record.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use minds_reader::assurance::IntentSignature;
use minds_reader::intent::AnchorRecord;

use crate::app::App;
use crate::intent::{bound_sessions, detail};
use crate::theme;

/// Zeichnet den Tab.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.intent else {
        return;
    };
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(area);

    // Links: die Anker.
    let mut rows: Vec<Line> = Vec::new();
    match &state.entries {
        None => rows.push(Line::from(Span::styled("… reading anchors", theme::dim()))),
        Some(Err(why)) => rows.push(Line::from(Span::styled(
            format!("· anchors not readable ({why})"),
            theme::dim(),
        ))),
        Some(Ok(list)) if list.is_empty() => rows.push(Line::from(Span::styled(
            "No intent anchors — bind one with: minds intent bind --file <path>",
            theme::dim(),
        ))),
        Some(Ok(list)) => {
            // Gelesen sind nur die mit Inhalt oder Grund aus dem Store — nicht
            // die, die das Budget übersprang.
            let read = list.len().saturating_sub(state.skipped);
            if read < state.total {
                rows.push(Line::from(Span::styled(
                    format!(
                        "{read} of {} anchors read (anchors sessions name come first)",
                        state.total
                    ),
                    theme::dim(),
                )));
            }
            // Ein Fenster um den Cursor: bei vielen Ankern bleibt ▸ sichtbar.
            let height = (left.height as usize).saturating_sub(2 + rows.len());
            let first = crate::view::offset(state.cursor, list.len(), height);
            for (i, info) in list.iter().enumerate().skip(first).take(height.max(1)) {
                let id = info.id.to_string();
                let short = id.get(..14).unwrap_or(&id);
                let (glyph, style) = match detail(info).map(|d| d.signature) {
                    // Neu geladen, noch nicht neu geprüft: nichts als gültig.
                    Some(_) if state.stale => ("?", theme::dim()),
                    Some(IntentSignature::Valid(_)) => ("✓", Style::default().fg(theme::OK)),
                    Some(IntentSignature::Invalid) => ("✗", Style::default().fg(theme::REVIEW)),
                    Some(IntentSignature::NotChecked) => ("?", theme::dim()),
                    Some(IntentSignature::Unsigned) => ("·", theme::dim()),
                    None => ("!", Style::default().fg(theme::REVIEW)),
                };
                let source = detail(info)
                    .map(|d| d.source.clone())
                    .unwrap_or_else(|| info.detail.clone().err().unwrap_or_default());
                let used = bound_sessions(&app.inspection, &info.id).len();
                let mut spans = vec![
                    Span::styled(if i == state.cursor { "▸ " } else { "  " }, theme::title()),
                    Span::styled(format!("{glyph} "), style),
                    Span::raw(format!("{short}…  ")),
                    Span::raw(crate::view::clip(&source, 40)),
                ];
                if used > 0 {
                    spans.push(Span::styled(format!("  ◉{used}"), theme::dim()));
                }
                if info.active {
                    spans.push(Span::styled("  (local file)", theme::dim()));
                }
                let line = Line::from(spans);
                rows.push(if i == state.cursor {
                    line.style(Style::default().add_modifier(Modifier::BOLD))
                } else {
                    line
                });
            }
        }
    }
    frame.render_widget(
        Paragraph::new(rows).block(
            Block::bordered()
                .title(" INTENT ANCHORS ")
                .title_style(theme::title()),
        ),
        left,
    );

    // Rechts: der gewählte Anker.
    let mut lines: Vec<Line> = Vec::new();
    let label = |key: &str| Span::styled(format!("{key:<10}"), theme::dim());
    if let Some(info) = state.selected() {
        lines.push(Line::from(vec![
            label("anchor"),
            Span::raw(info.id.to_string()),
        ]));
        match &info.detail {
            Err(why) => lines.push(Line::from(vec![
                label("state"),
                Span::styled(why.clone(), Style::default().fg(theme::REVIEW)),
            ])),
            Ok(d) => {
                lines.push(Line::from(vec![
                    label("source"),
                    Span::raw(d.source.clone()),
                ]));
                lines.push(Line::from(vec![
                    label("content"),
                    Span::raw(d.content.to_string()),
                ]));
                let scope = if d.scope.is_empty() {
                    "none declared".to_string()
                } else {
                    d.scope.join(", ")
                };
                lines.push(Line::from(vec![label("scope"), Span::raw(scope)]));
                if let Some(version) = &d.version {
                    lines.push(Line::from(vec![
                        label("version"),
                        if state.stale {
                            Span::styled("rechecking…", theme::dim())
                        } else {
                            Span::raw(version.clone())
                        },
                    ]));
                }
                lines.push(Line::from(vec![
                    label("proof"),
                    match &d.proof {
                        _ if state.stale => Span::styled("rechecking…", theme::dim()),
                        Ok(()) => Span::styled(
                            "ok — snapshot matches, clean under the working-tree redaction policy, as minds intent show checks it",
                            Style::default().fg(theme::OK),
                        ),
                        Err(why) => Span::styled(
                            format!("NOT PROVEN — {why}"),
                            Style::default().fg(theme::REVIEW),
                        ),
                    },
                ]));
                let (text, style) = match d.signature {
                    _ if state.stale => ("rechecking…".to_string(), theme::dim()),
                    IntentSignature::Valid(kind) => (
                        format!(
                            "valid ({}) — checked against ~/.ssh/allowed_signers, as minds verify does without --signers",
                            kind.word()
                        ),
                        Style::default().fg(theme::OK),
                    ),
                    IntentSignature::Invalid => (
                        "invalid — no trusted minds-intent signer verifies it".to_string(),
                        Style::default().fg(theme::REVIEW),
                    ),
                    IntentSignature::NotChecked => (
                        "present — not checked (no usable trusted signers, or no ssh-keygen outside the checkout)"
                            .to_string(),
                        theme::dim(),
                    ),
                    IntentSignature::Unsigned => ("none".to_string(), theme::dim()),
                };
                lines.push(Line::from(vec![
                    label("signature"),
                    Span::styled(text, style),
                ]));
            }
        }
        lines.push(Line::from(vec![
            label("binding"),
            Span::raw(if info.active {
                "active in the local file (A1)"
            } else {
                "not the local file binding (A1)"
            }),
        ]));

        // Wer sich an den Anker gebunden nennt — als Record.
        let sessions = bound_sessions(&app.inspection, &info.id);
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("Sessions naming this anchor ({})", sessions.len()),
            theme::title(),
        )));
        if sessions.is_empty() {
            lines.push(Line::from(Span::styled("  none", theme::dim())));
        }
        for (sid, record) in sessions.iter().take(8) {
            let id = sid.to_string();
            let request = app
                .inspection
                .index()
                .session(*sid)
                .map(|s| minds_reader::sanitize(&s.intent.request))
                .unwrap_or_default();
            let how = match record {
                AnchorRecord::WitnessEvent => "witness event",
                AnchorRecord::LocalFile => "local file",
            };
            lines.push(Line::from(vec![
                Span::raw(format!("  {}…  ", id.get(..12).unwrap_or(&id))),
                Span::styled(format!("({how})  "), theme::dim()),
                Span::raw(crate::view::clip(&request, 60)),
            ]));
        }
        if sessions.len() > 8 {
            lines.push(Line::from(Span::styled(
                format!("  … {} more", sessions.len() - 8),
                theme::dim(),
            )));
        }
        lines.push(Line::from(Span::styled(
            "  as recorded in the sessions — minds verify checks chaining and signature",
            theme::dim(),
        )));

        // Der Snapshot — nur belegt.
        if let Ok(d) = &info.detail {
            lines.push(Line::raw(""));
            // Nur belegt und frisch geprüft — auch wenn eine Quelle mehr
            // mitgäbe.
            match &d.snapshot {
                _ if state.stale => lines.push(Line::from(Span::styled(
                    "Snapshot — rechecking…",
                    theme::dim(),
                ))),
                Some(snapshot) if d.proof.is_ok() => {
                    lines.push(Line::from(Span::styled(
                        format!("Snapshot ({} bytes, as stored)", d.snapshot_len),
                        theme::title(),
                    )));
                    for line in snapshot {
                        lines.push(Line::raw(format!("  {line}")));
                    }
                    if d.snapshot_clipped {
                        lines.push(Line::from(Span::styled(
                            format!("  … cut here — minds intent show {} for all of it", info.id),
                            theme::dim(),
                        )));
                    }
                }
                _ => lines.push(Line::from(Span::styled(
                    format!(
                        "Snapshot ({} bytes) not shown — the anchor is not proven",
                        d.snapshot_len
                    ),
                    theme::dim(),
                ))),
            }
        }
    }
    // Nicht ins Leere blättern: höchstens so weit, dass die letzte
    // gerenderte (umbrochene) Zeile unten steht.
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let inner_width = right.width.saturating_sub(2);
    let inner_height = usize::from(right.height.saturating_sub(2));
    let rendered = paragraph.line_count(inner_width);
    let max = u16::try_from(rendered.saturating_sub(inner_height)).unwrap_or(u16::MAX);
    state.max_scroll.set(max);
    frame.render_widget(
        paragraph.scroll((state.scroll.min(max), 0)).block(
            Block::bordered()
                .title(" ANCHOR — PgUp/PgDn scroll ")
                .title_style(theme::title()),
        ),
        right,
    );
}
