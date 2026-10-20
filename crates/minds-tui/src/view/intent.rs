//! Der Requirements-Tab (C12.3): links die Anker als Register der
//! Anforderungen, rechts die gewählte — zuerst die Anforderung selbst, dann
//! ihr Status mit einem Satz je Zeile, dann die Sessions, Technik zuletzt.
//! Nie mehr behaupten als `minds intent show` und `minds verify`: „signed"
//! nur nach geprüfter Signatur, der Snapshot nur bei belegtem Anker,
//! Bindungen der Sessions als Record.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use minds_reader::assurance::IntentSignature;
use minds_reader::intent::AnchorRecord;

use crate::app::App;
use crate::intent::{IntentTab, bound_sessions, detail};
use crate::theme;
use crate::{IntentDetail, IntentInfo};

/// Zeichnet den Tab.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.intent else {
        return;
    };
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(area);
    draw_list(frame, app, state, left);
    draw_detail(frame, app, state, right);
}

/// Links: die Anker, je einer als Satz — Status, Kurz-Id, Quelle, Scope,
/// Sessions als Zahl.
fn draw_list(frame: &mut Frame, app: &App, state: &IntentTab, left: Rect) {
    let mut rows: Vec<Line> = Vec::new();
    match &state.entries {
        None => rows.push(Line::from(Span::styled("… reading anchors", theme::dim()))),
        Some(Err(why)) => rows.push(Line::from(Span::styled(
            format!("· anchors not readable ({why})"),
            theme::dim(),
        ))),
        Some(Ok(list)) if list.is_empty() => rows.push(Line::from(Span::styled(
            "No requirement anchors — bind one with: minds intent bind --file <path>",
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
            // Nur die Treffer des Filters aus `/` (B4).
            let shown = state.shown();
            if !state.filter.is_empty() {
                rows.push(Line::from(Span::styled(
                    format!(
                        "filter: {} ({} of {})",
                        state.filter,
                        shown.len(),
                        list.len()
                    ),
                    theme::dim(),
                )));
            }
            let height = height.saturating_sub(usize::from(!state.filter.is_empty()));
            let at = shown.iter().position(|&i| i == state.cursor).unwrap_or(0);
            let first = crate::view::offset(at, shown.len(), height);
            let inner = usize::from(left.width.saturating_sub(2));
            for &i in shown.iter().skip(first).take(height.max(1)) {
                let info = &list[i];
                let (glyph, style) = status_glyph(info, state.stale);
                let mut text = format!(
                    "{} · {}",
                    crate::view::overview::anchor_short(&info.id),
                    source_label(info, state.stale)
                );
                if let Some(d) = detail(info) {
                    text.push_str(" · ");
                    text.push_str(&scope_label(d));
                }
                let used = bound_sessions(&app.inspection, &info.id).len();
                if used > 0 {
                    text.push_str(&format!(" · {used} session{}", plural(used)));
                }
                if info.active {
                    text.push_str(" (local file)");
                }
                let line = Line::from(vec![
                    Span::styled(if i == state.cursor { "▸ " } else { "  " }, theme::title()),
                    Span::styled(format!("{glyph} "), style),
                    Span::raw(crate::view::clip(&text, inner.saturating_sub(4))),
                ]);
                rows.push(if i == state.cursor {
                    line.style(theme::cursor())
                } else {
                    line
                });
            }
        }
    }
    frame.render_widget(
        Paragraph::new(rows).block(
            Block::bordered()
                .title(" REQUIREMENTS ")
                .title_style(theme::title()),
        ),
        left,
    );
}

/// Rechts: die gewählte Anforderung in fester Reihenfolge — REQUIREMENT,
/// STATUS, SESSIONS, TECHNICAL. Keine Tastenhinweise außer dem Aufklapper.
fn draw_detail(frame: &mut Frame, app: &App, state: &IntentTab, right: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let inner = usize::from(right.width.saturating_sub(2));
    if let Some(info) = state.selected() {
        requirement_block(&mut lines, info, state);
        if let Ok(d) = &info.detail {
            lines.push(Line::raw(""));
            status_block(&mut lines, app, info, d, state.stale);
        }
        lines.push(Line::raw(""));
        sessions_block(&mut lines, app, info, inner);
        lines.push(Line::raw(""));
        technical_block(&mut lines, info, state);
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
                .title(" REQUIREMENT ")
                .title_style(theme::title()),
        ),
        right,
    );
}

/// REQUIREMENT — Kopf mit Kurz-Id und Quelle, darunter der Snapshot-Text
/// vollständig; nur bei belegtem Anker, sonst der Grund und nie der Text.
fn requirement_block(lines: &mut Vec<Line<'static>>, info: &IntentInfo, state: &IntentTab) {
    let mut head = vec![
        Span::styled("REQUIREMENT  ", theme::title()),
        Span::styled(
            format!("⚑ {}", crate::view::overview::anchor_short(&info.id)),
            theme::title(),
        ),
        Span::raw(format!(" · {}", source_label(info, state.stale))),
    ];
    if let Some(d) = detail(info) {
        head.push(Span::styled(
            format!(" · scope {}", scope_label(d)),
            theme::dim(),
        ));
    }
    lines.push(Line::from(head));
    match &info.detail {
        Err(why) => lines.push(Line::from(Span::styled(
            format!("  not readable — {why}"),
            Style::default().fg(theme::REVIEW),
        ))),
        Ok(d) => match &d.snapshot {
            // Nur belegt und frisch geprüft — auch wenn eine Quelle mehr
            // mitgäbe.
            _ if state.stale => lines.push(Line::from(Span::styled("  rechecking…", theme::dim()))),
            Some(snapshot) if d.proof.is_ok() => {
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
                "  text hidden — the anchor is not proven",
                theme::dim(),
            ))),
        },
    }
}

/// STATUS — drei Zeilen mit je einem Satz: belegt, signiert, Bindung (A1).
fn status_block(
    lines: &mut Vec<Line<'static>>,
    app: &App,
    info: &IntentInfo,
    d: &IntentDetail,
    stale: bool,
) {
    lines.push(Line::from(Span::styled("STATUS", theme::title())));
    let (glyph, text, style) = proven_status(d, stale);
    lines.push(mark(glyph, text, style));
    let (glyph, text, style) = signed_status(info, d, stale);
    lines.push(mark(glyph, text, style));
    let sessions = bound_sessions(&app.inspection, &info.id);
    // Die Stufe der Bindung: die der Sessions, die den Anker im
    // Witness-Event nennen — aus derselben Ableitung wie `minds verify` und
    // die Übersicht (`App::session_rating`). Bei mehreren die schwächste
    // **geprüfte** Stufe; `Unchecked` ist keine niedrigere Stufe, sondern
    // keine — sie zählt nur, wenn keine der Sessions geprüft ist.
    let rating = sessions
        .iter()
        .filter(|(_, record)| *record == AnchorRecord::WitnessEvent)
        .map(|(id, _)| app.session_rating(*id).0)
        .min_by_key(|rating| {
            (
                matches!(rating, minds_reader::assurance::Assurance::Unchecked { .. }),
                rating.level(),
            )
        });
    lines.push(mark(
        "·",
        crate::copy::binding_sentence(rating.as_ref(), info.active),
        theme::dim(),
    ));
}

/// Eine Statuszeile: Glyph, dann der Satz — `  ✓ proven — …`.
fn mark(glyph: &'static str, text: String, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{glyph} "), style),
        Span::styled(text, style),
    ])
}

/// Der Beleg als Satz — Glyph, Text, Stil.
fn proven_status(d: &IntentDetail, stale: bool) -> (&'static str, String, Style) {
    if stale {
        return ("?", "rechecking…".into(), theme::dim());
    }
    match &d.proof {
        Ok(()) => (
            "✓",
            crate::copy::proven_sentence(blob_of(&d.source), &d.source),
            Style::default().fg(theme::OK),
        ),
        Err(why) => (
            "✗",
            format!("not proven — {why}"),
            Style::default().fg(theme::REVIEW),
        ),
    }
}

/// Die Signatur als Satz — nicht signiert ist offen, kein Fehler.
fn signed_status(
    info: &IntentInfo,
    d: &IntentDetail,
    stale: bool,
) -> (&'static str, String, Style) {
    if stale {
        return ("?", "rechecking…".into(), theme::dim());
    }
    match d.signature {
        IntentSignature::Valid(kind) => (
            "✓",
            crate::copy::signed_sentence(kind.word()),
            Style::default().fg(theme::OK),
        ),
        IntentSignature::Invalid => (
            "✗",
            "signature invalid — no trusted minds-intent signer verifies it".into(),
            Style::default().fg(theme::REVIEW),
        ),
        IntentSignature::NotChecked => (
            "?",
            "signature not checked — no usable trusted signers, or no ssh-keygen outside the checkout"
                .into(),
            theme::dim(),
        ),
        IntentSignature::Unsigned => {
            let (glyph, _, style) = theme::open();
            (
                glyph,
                format!("not signed — minds intent sign {} adds a signature", info.id),
                style,
            )
        }
    }
}

/// SESSIONS (n) — je Session Agent, Modell, Id, Zeit und Art der Bindung,
/// darunter der Auftrag als Claim. Wie das Overview-Detail; ein
/// Platzhalter aus `minds-capture` wird nie gedruckt.
fn sessions_block(lines: &mut Vec<Line<'static>>, app: &App, info: &IntentInfo, inner: usize) {
    let sessions = bound_sessions(&app.inspection, &info.id);
    lines.push(Line::from(Span::styled(
        format!("SESSIONS ({})", sessions.len()),
        theme::title(),
    )));
    if sessions.is_empty() {
        lines.push(Line::from(Span::styled("  none", theme::dim())));
    }
    let (claim_glyph, _, claim_style) = theme::claim();
    for (sid, record) in sessions.iter().take(8) {
        let id = sid.to_string();
        let short = id.get(..11).unwrap_or(&id).to_string();
        let how = match record {
            AnchorRecord::WitnessEvent => "witness event",
            AnchorRecord::LocalFile => "local file",
        };
        let mut head = vec![Span::raw("  ")];
        match app.inspection.index().session(*sid) {
            Some(s) => {
                let actor = minds_reader::Actor::of(s);
                match &actor.agent {
                    Some(agent) => {
                        head.push(crate::view::overview::badge_text(agent, true, app.nerd));
                        head.push(Span::raw(format!("{} · ", crate::view::clip(agent, 40))));
                    }
                    None => head.push(Span::styled("agent not recorded · ", theme::dim())),
                }
                match &actor.model {
                    Some(model) => {
                        let mut text = crate::view::clip(model, 40);
                        if let Some(provider) = &actor.provider {
                            text.push_str(&format!(" ({})", crate::view::clip(provider, 24)));
                        }
                        head.push(Span::raw(text));
                    }
                    None => head.push(Span::styled("model not recorded", theme::dim())),
                }
                head.push(Span::raw(format!(" · {short}")));
                let started = s.lineage.as_ref().and_then(|l| l.started_at.as_deref());
                if let Some(clock) = crate::view::clock_z(started) {
                    head.push(Span::raw(format!(" · {clock}")));
                }
                head.push(Span::styled(format!(" · ({how})"), theme::dim()));
                lines.push(Line::from(head));
                let request =
                    minds_reader::sanitize(&minds_core::without_pasted_wrapper(&s.intent.request));
                let request = request.lines().next().unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(format!("{claim_glyph} "), claim_style),
                    Span::styled(
                        crate::view::clip(request, inner.saturating_sub(8)),
                        claim_style,
                    ),
                ]));
            }
            None => {
                head.push(Span::raw(short));
                head.push(Span::styled(
                    format!(" · ({how}) · not readable"),
                    theme::dim(),
                ));
                lines.push(Line::from(head));
            }
        }
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
}

/// TECHNICAL — Anker, Inhalts-Hash, Quelle in voller Länge und die Prüfungen
/// im Wortlaut; eingeklappt, Space klappt auf.
fn technical_block(lines: &mut Vec<Line<'static>>, info: &IntentInfo, state: &IntentTab) {
    lines.push(Line::from(vec![
        Span::styled("TECHNICAL (for auditors)", theme::title()),
        Span::styled(
            if state.technical_open {
                "  ▾ Space"
            } else {
                "  ▸ Space"
            },
            theme::dim(),
        ),
    ]));
    if !state.technical_open {
        return;
    }
    let label = |key: &str| Span::styled(format!("  {key:<10}"), theme::dim());
    lines.push(Line::from(vec![
        label("anchor"),
        Span::raw(info.id.to_string()),
    ]));
    let Ok(d) = &info.detail else {
        return;
    };
    lines.push(Line::from(vec![
        label("content"),
        Span::raw(d.content.to_string()),
    ]));
    lines.push(Line::from(vec![
        label("source"),
        Span::raw(d.source.clone()),
    ]));
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
        label("scope"),
        Span::raw(if d.scope.is_empty() {
            "none declared".to_string()
        } else {
            d.scope.join(", ")
        }),
    ]));
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
    lines.push(Line::from(vec![
        label("snapshot"),
        Span::raw(format!("{} bytes as stored", d.snapshot_len)),
    ]));
}

/// Das Status-Glyph der Liste — `✓` belegt und signiert, `◦` belegt, nicht
/// signiert, `✗` nicht belegt oder ungültig signiert, `?` nicht geprüft
/// oder neu zu prüfen, `!` nicht lesbar.
pub(crate) fn status_glyph(info: &IntentInfo, stale: bool) -> (&'static str, Style) {
    let Some(d) = detail(info) else {
        return ("!", Style::default().fg(theme::REVIEW));
    };
    if stale {
        return ("?", theme::dim());
    }
    if d.proof.is_err() {
        return ("✗", Style::default().fg(theme::REVIEW));
    }
    match d.signature {
        IntentSignature::Valid(_) => ("✓", Style::default().fg(theme::OK)),
        IntentSignature::Invalid => ("✗", Style::default().fg(theme::REVIEW)),
        IntentSignature::NotChecked => ("?", theme::dim()),
        IntentSignature::Unsigned => {
            let (glyph, _, style) = theme::open();
            (glyph, style)
        }
    }
}

/// Die Status-Wörter für die Fußzeile: `✓ proven · ✓ signed`.
pub(crate) fn status_words(info: &IntentInfo, stale: bool) -> String {
    let Some(d) = detail(info) else {
        return "! not readable".into();
    };
    if stale {
        return "? rechecking".into();
    }
    let proven = match d.proof {
        Ok(()) => "✓ proven",
        Err(_) => "✗ not proven",
    };
    let signed = match d.signature {
        IntentSignature::Valid(_) => "✓ signed",
        IntentSignature::Invalid => "✗ signature invalid",
        IntentSignature::NotChecked => "? signature not checked",
        IntentSignature::Unsigned => "◦ not signed",
    };
    format!("{proven} · {signed}")
}

/// Die Quelle lesbar: `docs/req.md (HEAD)` statt `file:docs/req.md@3f9c1e2a`;
/// Issue und Prompt wie sie sind. Nicht lesbar: der Grund der Quelle.
fn source_label(info: &IntentInfo, stale: bool) -> String {
    match &info.detail {
        Err(why) => why.clone(),
        Ok(d) => {
            let Some(rest) = d.source.strip_prefix("file:") else {
                return d.source.clone();
            };
            let path = rest.rsplit_once('@').map_or(rest, |(path, _)| path);
            match d.version.as_deref() {
                _ if stale => path.to_string(),
                Some("the version in HEAD") => format!("{path} (HEAD)"),
                Some(_) => format!("{path} (working tree)"),
                None => path.to_string(),
            }
        }
    }
}

/// Das Blob hinter `file:<pfad>@<blob>`, sonst nichts.
fn blob_of(source: &str) -> Option<&str> {
    source
        .strip_prefix("file:")
        .and_then(|rest| rest.rsplit_once('@'))
        .map(|(_, blob)| blob)
}

/// Der Scope als Text; leer: `no scope declared`.
fn scope_label(d: &IntentDetail) -> String {
    if d.scope.is_empty() {
        "no scope declared".to_string()
    } else {
        d.scope.join(", ")
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
