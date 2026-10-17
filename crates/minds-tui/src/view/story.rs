//! Die Story einer Session (C11): Kopf, der Prompt als Aussage, die
//! Anforderung, die nummerierte Schrittliste mit Ergebnis und Zeit, die
//! RESULT-Zeile — und unter dem Cursor die Detailbox des gewählten Schritts.
//!
//! Die Frage der Seite: *Was hat der Agent zwischen Prompt und Commit
//! getan — in welcher Reihenfolge, mit welchem Ergebnis?* Ergebnisse stehen
//! nur, wo der Record sie kennt; alles Aufgezeichnete trägt `◌`.

use minds_core::SessionId;
use minds_reader::story::{Outcome, Step, Story, Verb};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::app::App;
use crate::layout::{Row, RowKind};
use crate::theme;
use crate::view::trust::{self, Density};
use crate::view::{clip, offset, when};

/// So viele Zeilen zeigt der PROMPT-Abschnitt höchstens; der Rest wird
/// gezählt.
const PROMPT_MAX: usize = 6;
/// Höchstens so viele Einträge je Claim-Abschnitt (Constraints, verworfene
/// Ansätze); der Rest wird gezählt (A3).
const SECTION_MAX: usize = 5;
/// Die Breite der Spalte links in Kopf- und Abschnittszeilen.
const LABEL_W: usize = 13;
/// Die Breite der Ergebnis-/Evidenz-Spalte rechts in der Schrittliste.
const RIGHT_W: usize = 16;
/// Die Breite der Zeitspalte (`+12:34`).
const TIME_W: usize = 7;

/// Zeichnet die Story. `cursor` ist `None` für die Vorschau neben der
/// Liste: kein Fokus, keine hervorgehobene Zeile, keine Detailbox.
pub fn draw(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    id: SessionId,
    story: &Story,
    rows: &[Row],
    cursor: Option<usize>,
) {
    let Some(card) = app.inspection.card(id) else {
        frame.render_widget(
            Paragraph::new("Session unreadable.").style(theme::dim()),
            area,
        );
        return;
    };
    let width = area.width as usize;
    let focused = cursor.and_then(|c| rows.get(c));
    let detail_lines = focused.and_then(|row| detail(app, id, story, row, width.saturating_sub(2)));
    let intro = intro_lines(app, id, story, width, area.height);
    // Die Schrittliste behält mindestens sechs Zeilen: Das Detail bekommt,
    // was danach bleibt — höchstens die halbe Höhe (mindestens 12).
    let room = area
        .height
        .saturating_sub(2 + intro.len() as u16 + 6)
        .min((area.height / 2).max(12));
    let detail_h = detail_lines
        .as_ref()
        .map(|l| (l.len() as u16 + 2).min(room))
        .filter(|h| *h >= 3)
        .unwrap_or(0);
    let [head, intro_area, body, detail_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(intro.len() as u16),
        Constraint::Min(1),
        Constraint::Length(detail_h),
    ])
    .areas(area);

    // Kopf, Zeile 1: Identität — Id, Akteur (K1: nie `unknown`), Zeitraum.
    let short: String = id.to_string().chars().take(11).collect();
    let actor = app
        .inspection
        .index()
        .session(id)
        .map(minds_reader::summary::actor_line)
        .unwrap_or_else(|| card.summary.actor.clone());
    let mut identity = vec![
        Span::styled(
            format!("SESSION {short}… "),
            theme::title().fg(theme::AGENT),
        ),
        // Begrenzt: ein langer K1-Grund schöbe sonst den Zeitraum aus der Zeile.
        Span::styled(
            format!("· {}", clip(&actor, 64)),
            Style::default().fg(theme::AGENT),
        ),
    ];
    if let Some(span) = span_of(story) {
        identity.push(Span::styled(format!(" · {span}"), theme::dim()));
    }
    // Zeile 2: Ergebnis zuerst (A4/A6) — Trust Strip, Review, Beleg.
    let (v_glyph, v_word, v_style) = theme::verdict(card.review.verdict);
    let (ev_glyph, ev_word, ev_style) = theme::evidence(card.evidence);
    let mut state = trust::spans(&card.axes(), Density::Short);
    state.push(Span::raw("   "));
    state.push(Span::styled(format!("{v_glyph} {v_word}"), v_style));
    state.push(Span::raw("   "));
    state.push(Span::styled(format!("{ev_glyph} {ev_word}"), ev_style));
    if let Some((k, n)) = card.epoch_position {
        state.push(Span::styled(format!("   epoch {k}/{n}"), theme::dim()));
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(fit(identity, width)),
            Line::from(fit(state, width)),
        ]),
        head,
    );
    frame.render_widget(Paragraph::new(intro), intro_area);

    // Die Schrittliste.
    let height = body.height as usize;
    let first = offset(cursor.unwrap_or(0), rows.len(), height);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, row)| {
            let mut line = Line::from(row_spans(app, story, row, &card, width));
            if Some(i) == cursor {
                line = line.style(theme::cursor());
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);

    // Die Details unter dem Cursor.
    if detail_h > 0
        && let (Some(row), Some(lines)) = (focused, detail_lines)
    {
        let (title, style) = detail_title(story, row);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::bordered()
                    .title(format!(" {title} "))
                    .title_style(style.patch(theme::title()))
                    .border_style(style),
            ),
            detail_area,
        );
    }
}

/// `2026-10-16 10:37–10:40 UTC (3 min)` aus Beginn, Ende und Dauer.
fn span_of(story: &Story) -> Option<String> {
    let start = story.started_at.as_deref()?;
    // Nur ein lesbarer UTC-Stempel (`…Z`) heißt „UTC“ — ein Offset würde
    // sonst falsch beschriftet.
    if !start.ends_with('Z') || minds_metrics::epoch_seconds(start).is_none() {
        return None;
    }
    let date = start.get(0..10)?;
    let from = start.get(11..16)?;
    let mut text = format!("{date} {from}");
    if let Some(to) = story
        .ended_at
        .as_deref()
        .filter(|e| e.ends_with('Z') && minds_metrics::epoch_seconds(e).is_some())
        .and_then(|e| e.get(11..16))
    {
        text.push_str(&format!("–{to}"));
    }
    text.push_str(" UTC");
    if let Some(secs) = story.duration_secs {
        let minutes = secs / 60;
        text.push_str(&format!(
            " ({} {})",
            if minutes > 0 { minutes } else { secs },
            if minutes > 0 { "min" } else { "s" }
        ));
    }
    Some(text)
}

/// PROMPT (Aussage, umgebrochen, gekappt), Constraints und verworfene
/// Ansätze (A3), REQUIREMENT, dann die STEPS-Überschrift.
fn intro_lines(
    app: &App,
    id: SessionId,
    story: &Story,
    width: usize,
    height: u16,
) -> Vec<Line<'static>> {
    let (claim_glyph, _, claim_style) = theme::claim();
    let label = |key: &str| Span::styled(format!("{key:<LABEL_W$}"), theme::dim());
    let indent = || Span::raw(" ".repeat(LABEL_W));
    let room = width.saturating_sub(LABEL_W + 2).max(8);
    let mut lines: Vec<Line> = Vec::new();
    // Bei wenig Höhe bleibt der Prompt eine Zeile — die Schritte haben Vorrang.
    let cap = if height >= 30 { PROMPT_MAX } else { 1 };
    let prompt: Vec<String> = story
        .prompt
        .lines()
        .filter(|l| !l.trim().is_empty())
        .flat_map(|l| wrap_words(l, room))
        .collect();
    let shown = prompt.len().min(cap);
    for (i, text) in prompt.iter().take(shown).enumerate() {
        let mut spans = vec![if i == 0 { label("PROMPT") } else { indent() }];
        if i == 0 {
            spans.push(Span::styled(format!("{claim_glyph} "), claim_style));
        } else {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(text.clone(), claim_style));
        lines.push(Line::from(spans));
    }
    if prompt.is_empty() {
        lines.push(Line::from(vec![
            label("PROMPT"),
            Span::styled(format!("{claim_glyph} (no prompt captured)"), claim_style),
        ]));
    }
    if prompt.len() > shown {
        lines.push(Line::from(vec![
            indent(),
            Span::styled(
                format!("  … {} more lines", prompt.len() - shown),
                theme::dim(),
            ),
        ]));
    }
    // Leer ist leer: Ein Abschnitt ohne Einträge erscheint nicht (A3).
    if let Some(session) = app.inspection.index().session(id) {
        for (title, entries) in [
            ("CONSTRAINTS", &session.intent.constraints),
            ("DISCARDED", &session.intent.discarded),
        ] {
            if entries.is_empty() {
                continue;
            }
            for (i, entry) in entries.iter().take(SECTION_MAX).enumerate() {
                let entry = minds_reader::sanitize(entry);
                lines.push(Line::from(vec![
                    if i == 0 { label(title) } else { indent() },
                    Span::styled(format!("{claim_glyph} "), claim_style),
                    Span::styled(clip(&entry, room), claim_style),
                ]));
            }
            if entries.len() > SECTION_MAX {
                lines.push(Line::from(vec![
                    indent(),
                    Span::styled(
                        format!("  … {} more", entries.len() - SECTION_MAX),
                        theme::dim(),
                    ),
                ]));
            }
        }
    }
    // REQUIREMENT: die Anker, die diese Session nennt — Kurzform wie in der
    // Übersicht, der Bereich, wenn die Übersicht ihn gelesen hat.
    let anchors: Vec<minds_core::ContentHash> = crate::intent::named_anchors(&app.inspection)
        .into_iter()
        .filter(|a| {
            crate::intent::bound_sessions(&app.inspection, a)
                .iter()
                .any(|(s, _)| *s == id)
        })
        .collect();
    for (i, anchor) in anchors.iter().enumerate() {
        let mut spans = vec![if i == 0 {
            label("REQUIREMENT")
        } else {
            indent()
        }];
        spans.push(Span::raw(format!(
            "⚑ {}",
            crate::view::overview::anchor_short(anchor)
        )));
        if let Some(scope) = app.requirement_scope(anchor) {
            spans.push(Span::styled(
                format!(
                    " · scope {}",
                    clip(&scope.join(", "), room.saturating_sub(20))
                ),
                theme::dim(),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(""));
    // STEPS (n), rechts der Start.
    let mut heading = vec![Span::styled(
        format!("STEPS ({})", story.steps.len()),
        theme::title(),
    )];
    if let Some(started) = crate::view::clock_z(story.started_at.as_deref()) {
        let text = format!("started {started}");
        let used: usize = heading.iter().map(|s| s.content.chars().count()).sum();
        let pad = width.saturating_sub(used + text.len() + 1);
        heading.push(Span::raw(" ".repeat(pad)));
        heading.push(Span::styled(text, theme::dim()));
    }
    lines.push(Line::from(heading));
    lines
}

/// Die Spans einer Zeile der Schrittliste.
fn row_spans(
    app: &App,
    story: &Story,
    row: &Row,
    card: &minds_reader::model::SessionCard,
    width: usize,
) -> Vec<Span<'static>> {
    let (claim_glyph, _, claim_style) = theme::claim();
    match &row.kind {
        RowKind::Step(i) => match story.steps.get(*i) {
            Some(step) => step_spans(step, format!("{:>3}", step.number), width),
            None => Vec::new(),
        },
        RowKind::Run {
            first,
            count,
            verb,
            target,
            outcome,
        } => {
            let (glyph, _, style) = theme::verb(*verb);
            let numbers = format!("{:>3}", format!("{}–{}", first + 1, first + count));
            let what = match (verb, target) {
                (Verb::Wrote, Some(t)) => format!("{count} writes of {t}"),
                (Verb::Edited, Some(t)) => format!("{count} edits of {t}"),
                (Verb::Read, Some(t)) => format!("{count} reads of {t}"),
                (Verb::Deleted, Some(t)) => format!("{count} deletes of {t}"),
                (Verb::Tests, _) => format!("{count} test runs"),
                (Verb::Lint, _) => format!("{count} lint runs"),
                (Verb::Build, _) => format!("{count} builds"),
                (Verb::Bench, _) => format!("{count} bench runs"),
                (Verb::Ran, _) => format!("{count} commands"),
                (Verb::Searched, _) => format!("{count} searches"),
                (Verb::Fetched, _) => format!("{count} fetches"),
                (Verb::Delegated, _) => format!("{count} delegations"),
                (v, Some(t)) => format!("{count} × {} {t}", v.word()),
                (v, None) => format!("{count} × {}", v.word()),
            };
            let (right, right_style) = outcome_span(*outcome);
            let last = story.steps.get(first + count - 1);
            let time = last
                .and_then(|s| s.offset_secs)
                .map(plus)
                .unwrap_or_default();
            let left = format!("{numbers}  {glyph} {:<10}", verb.word());
            columns(left, style, &what, right, right_style, time, width)
        }
        RowKind::Turn(t) => match story.turns.get(*t) {
            Some(turn) => {
                let who = match turn.role {
                    minds_core::Role::User => "you",
                    minds_core::Role::Assistant => "agent",
                    minds_core::Role::System => "system",
                    minds_core::Role::Tool => "tool",
                };
                let text = turn.text.lines().next().unwrap_or("").to_string();
                let room = width.saturating_sub(5 + who.len() + 5);
                vec![
                    Span::raw("     "),
                    Span::styled(
                        format!("{claim_glyph} {who} · {}", clip(&text, room)),
                        claim_style,
                    ),
                ]
            }
            None => Vec::new(),
        },
        RowKind::Child { child, .. } => {
            let steps = app.inspection.story(*child).map(|s| s.steps.len());
            let mut spans = vec![
                Span::raw("       "),
                Span::styled("↳ delegated → ", Style::default().fg(theme::AGENT)),
            ];
            match steps {
                Some(n) => spans.push(Span::raw(format!(
                    "{n} step{} · ",
                    if n == 1 { "" } else { "s" }
                ))),
                None => spans.push(Span::styled("not readable · ", theme::dim())),
            }
            if let Some(card) = app.inspection.card(*child) {
                spans.extend(trust::spans(&card.axes(), Density::Glyphs));
                spans.push(Span::raw("  "));
                spans.push(Span::raw(clip(
                    &card.summary.headline,
                    width.saturating_sub(40),
                )));
            } else {
                let id = child.to_string();
                spans.push(Span::styled(
                    id.get(..11).unwrap_or(&id).to_string(),
                    theme::dim(),
                ));
            }
            spans
        }
        RowKind::Handover(h) => {
            let (glyph, word, style) = theme::handover();
            let other: String = h.other.to_string().chars().take(11).collect();
            let direction = if h.incoming {
                "reads from"
            } else {
                "writes for"
            };
            vec![
                Span::raw("     "),
                Span::styled(format!("{glyph} {word} "), style),
                Span::raw(format!(
                    "{direction} {} · {other}…",
                    clip(&h.path, width.saturating_sub(40))
                )),
            ]
        }
        RowKind::Result => {
            let mut spans = vec![Span::styled("RESULT   ", theme::title())];
            spans.push(Span::raw(format!(
                "{} file{} claimed",
                card.summary.files,
                if card.summary.files == 1 { "" } else { "s" }
            )));
            let commits = app.inspection.index().commits_of(card.id);
            match commits.first() {
                Some(commit) => {
                    let short: String = commit.to_string().chars().take(7).collect();
                    let subject = app
                        .inspection
                        .index()
                        .subject_of(*commit)
                        .map(|s| format!(" „{}“", clip(s, 40)))
                        .unwrap_or_default();
                    spans.push(Span::styled(" · commit ", theme::dim()));
                    spans.push(Span::styled(short, Style::default().fg(theme::CHANGE)));
                    spans.push(Span::raw(subject));
                    if commits.len() > 1 {
                        spans.push(Span::styled(
                            format!(" +{}", commits.len() - 1),
                            theme::dim(),
                        ));
                    }
                }
                None => spans.push(Span::styled(" · no commit linked yet", theme::dim())),
            }
            spans.push(Span::styled(" · ", theme::dim()));
            spans.push(crate::view::review_span(card.review.verdict));
            spans
        }
    }
}

/// Eine Schrittzeile: Nummer, Glyph und Verb, Ziel, rechts Ergebnis oder
/// Evidenzzustand, ganz rechts die Zeit seit Start.
fn step_spans(step: &Step, number: String, width: usize) -> Vec<Span<'static>> {
    let (glyph, word, style) = theme::verb(step.verb);
    let (right, right_style) = right_of(step);
    let time = step.offset_secs.map(plus).unwrap_or_default();
    let left = format!("{number}  {glyph} {word:<10}");
    columns(left, style, &step.target, right, right_style, time, width)
}

/// Die Spalten einer Zeile: links Nummer, Glyph und Verb; das Ziel bekommt
/// den Rest; rechts Ergebnis bzw. Evidenz und die Zeit. Wird es eng (unter
/// 100 Spalten, etwa in der Vorschau), weicht erst die Zeit, dann die feste
/// Breite der rechten Spalte — nie der Zustand selbst (A6).
fn columns(
    left: String,
    style: Style,
    target: &str,
    right: String,
    right_style: Style,
    time: String,
    width: usize,
) -> Vec<Span<'static>> {
    let roomy = width >= 100;
    let time_w = if roomy { TIME_W + 1 } else { 0 };
    let right_w = if right.is_empty() {
        0
    } else if roomy {
        RIGHT_W + 2
    } else {
        right.chars().count() + 2
    };
    let room = width
        .saturating_sub(left.chars().count() + right_w + time_w)
        .max(8);
    let mut spans = vec![
        Span::styled(left, style),
        Span::raw(format!("{:<room$}", clip(target, room))),
    ];
    if right_w > 0 {
        let w = right_w - 2;
        spans.push(Span::styled(format!("  {right:<w$}"), right_style));
    }
    if time_w > 0 {
        spans.push(Span::styled(format!(" {time:>TIME_W$}"), theme::dim()));
    }
    spans
}

/// Die rechte Spalte eines Schritts: das Ergebnis eines Kommandos, der
/// Evidenzzustand einer Schreibung (`◇ reported`: berichtet, nicht
/// bezeugt), `◐ uninterpreted` für einen ungedeuteten Aufruf.
pub(crate) fn right_of(step: &Step) -> (String, Style) {
    if step.uninterpreted {
        // Das Glyph und Wort der Deutungs-Achse (A4) — ohne Zahl: ein Schritt.
        let (glyph, word, style) = trust::uninterpreted_one();
        return (format!("{glyph} {word}"), style);
    }
    if step.verb.writes() {
        let (glyph, word, style) = theme::state(minds_reader::reconcile::ReconClass::ReportedOnly);
        return (format!("{glyph} {word}"), style);
    }
    outcome_span(step.outcome)
}

/// Das Ergebnis als Glyph und Wort — nur, was der Record kennt; Glyphen
/// aus dem Register über [`theme::outcome`].
fn outcome_span(outcome: Option<Outcome>) -> (String, Style) {
    match outcome {
        None => (String::new(), Style::default()),
        Some(outcome) => {
            let (glyph, word, style) = theme::outcome(outcome);
            (format!("{glyph} {word}").trim_start().to_string(), style)
        }
    }
}

/// `+m:ss` seit Session-Start.
fn plus(secs: i64) -> String {
    if secs < 0 {
        return format!("−{}:{:02}", (-secs) / 60, (-secs) % 60);
    }
    format!("+{}:{:02}", secs / 60, secs % 60)
}

/// Titel und Stil der Detailbox.
fn detail_title(story: &Story, row: &Row) -> (String, Style) {
    match &row.kind {
        RowKind::Step(i) => match story.steps.get(*i) {
            Some(step) => {
                let (_, _, style) = theme::verb(step.verb);
                (
                    format!("STEP {} · {}", step.number, step.verb.word()),
                    style,
                )
            }
            None => ("STEP".into(), theme::dim()),
        },
        RowKind::Run { first, count, .. } => (
            format!("STEPS {}–{}", first + 1, first + count),
            theme::dim(),
        ),
        RowKind::Turn(_) => ("TURN".into(), theme::claim().2),
        RowKind::Child { .. } => ("SUBAGENT".into(), Style::default().fg(theme::AGENT)),
        RowKind::Handover(_) => ("HANDOVER".into(), theme::handover().2),
        RowKind::Result => ("RESULT".into(), Style::default().fg(theme::CHANGE)),
    }
}

/// Die Zeilen der Detailbox zur Zeile unter dem Cursor — `None`, wenn es
/// nichts zu sagen gibt.
fn detail(
    app: &App,
    id: SessionId,
    story: &Story,
    row: &Row,
    width: usize,
) -> Option<Vec<Line<'static>>> {
    let key = |k: &str| Span::styled(format!("{k:<10} "), theme::dim());
    let mut lines: Vec<Line> = Vec::new();
    match &row.kind {
        RowKind::Step(i) => {
            let step = story.steps.get(*i)?;
            for (k, v) in &step.detail {
                for (n, value) in v.lines().filter(|l| !l.trim().is_empty()).enumerate() {
                    lines.push(Line::from(vec![
                        key(if n == 0 { k } else { "" }),
                        Span::raw(value.to_string()),
                    ]));
                }
            }
            if let Some(at) = &step.at {
                let mut text = when(Some(at));
                if let Some(secs) = step.offset_secs {
                    text.push_str(&format!(" · {} after start", plus(secs)));
                }
                lines.push(Line::from(vec![key("Time"), Span::raw(text)]));
            }
            // Der Evidenzzustand einer Schreibung mit seinem Satz (C9.3).
            if let Some(claim) = step.claim {
                let (glyph, word, style) =
                    theme::state(minds_reader::reconcile::ReconClass::ReportedOnly);
                let hash = match claim {
                    minds_reader::story::WriteClaim::Hashed => "hash recorded".to_string(),
                    minds_reader::story::WriteClaim::Unhashed(reason) => format!(
                        "no hash{}",
                        reason
                            .map(|r| format!(" — {}", r.as_str()))
                            .unwrap_or_default()
                    ),
                };
                lines.push(Line::from(vec![
                    key("Evidence"),
                    Span::styled(format!("{glyph} {word}"), style),
                    Span::styled(format!(" · {hash}"), theme::dim()),
                ]));
                for sentence in crate::view::changes::state_sentence(
                    minds_reader::reconcile::ReconClass::ReportedOnly,
                    &step.tool,
                ) {
                    lines.push(Line::from(Span::styled(
                        format!("{:<10} {sentence}", ""),
                        theme::dim(),
                    )));
                }
            }
        }
        RowKind::Run { first, count, .. } => {
            lines.push(Line::from(Span::styled(
                format!("{count} steps merged — zoom 2 shows each one"),
                theme::dim(),
            )));
            for step in story.steps.iter().skip(*first).take(*count) {
                let (right, _) = right_of(step);
                lines.push(Line::from(vec![
                    key(&format!("{:>3}", step.number)),
                    Span::raw(format!(
                        "{}  {right}",
                        clip(&step.target, width.saturating_sub(32))
                    )),
                ]));
            }
        }
        RowKind::Turn(t) => {
            let turn = story.turns.get(*t)?;
            let (glyph, word, style) = theme::claim();
            lines.push(Line::from(Span::styled(
                format!("{glyph} {word} — as recorded, not verified evidence"),
                style,
            )));
            for text in turn.text.lines().filter(|l| !l.trim().is_empty()) {
                lines.push(Line::from(Span::styled(text.to_string(), style)));
            }
        }
        RowKind::Child { child, .. } => {
            let (glyph, _, style) = theme::claim();
            match app.inspection.story(*child) {
                Some(story) => {
                    lines.push(Line::from(vec![
                        key("Session"),
                        Span::raw(child.to_string()),
                    ]));
                    lines.push(Line::from(vec![
                        key("Steps"),
                        Span::raw(story.steps.len().to_string()),
                    ]));
                    for (n, text) in story.prompt.lines().take(4).enumerate() {
                        lines.push(Line::from(vec![
                            key(if n == 0 { "Prompt" } else { "" }),
                            Span::styled(format!("{glyph} {text}"), style),
                        ]));
                    }
                    lines.push(Line::from(Span::styled(
                        "Enter opens the story of the subagent".to_string(),
                        theme::dim(),
                    )));
                }
                None => lines.push(Line::from(Span::styled(
                    "The subagent's session is not readable.".to_string(),
                    theme::dim(),
                ))),
            }
        }
        RowKind::Handover(h) => {
            lines.push(Line::from(vec![
                key("Session"),
                Span::raw(h.other.to_string()),
            ]));
            lines.push(Line::from(vec![key("Path"), Span::raw(h.path.clone())]));
            lines.push(Line::from(vec![
                key("Evidence"),
                Span::raw("content [recomputed] — the same bytes, no timestamp needed"),
            ]));
        }
        RowKind::Result => {
            // Jede Änderung erklärt ihren Beleg — Satz und Nachrechnung.
            for commit in app.inspection.index().commits_of(id) {
                let short: String = commit.to_string().chars().take(10).collect();
                let subject = app
                    .inspection
                    .index()
                    .subject_of(commit)
                    .unwrap_or("")
                    .to_string();
                lines.push(Line::from(vec![
                    key("Commit"),
                    Span::raw(format!("{short} {subject}")),
                ]));
                if let Some(change) = app.inspection.index().change_of(commit) {
                    lines.push(Line::from(vec![
                        key("Change"),
                        Span::raw(clip(&change.to_string(), 14)),
                    ]));
                }
                if let Some(link) = app.inspection.evidence(app.repo, commit, id) {
                    let (glyph, word, style) = theme::evidence(Some(link.evidence));
                    lines.push(Line::from(vec![
                        key("Evidence"),
                        Span::styled(format!("{glyph} {word}"), style),
                    ]));
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{:<10} {}",
                            "",
                            minds_reader::model::evidence_sentence(Some(link.evidence))
                        ),
                        theme::dim(),
                    )));
                    lines.push(Line::from(Span::raw(format!(
                        "{:<10} {}",
                        "",
                        crate::view::explanation(&link.why)
                    ))));
                }
            }
            // Die Review-Notizen: unsigniert ist aufgezeichnet, nicht belegt
            // (A2) — Claim-Marker und -Stil.
            if let Some(card) = app.inspection.card(id) {
                let (glyph, _, claim_style) = theme::claim();
                for note in &card.review.notes {
                    let (prefix, style) = if note.signed {
                        (String::new(), Style::default())
                    } else {
                        (format!("{glyph} "), claim_style)
                    };
                    lines.push(Line::from(vec![
                        key(&note.reviewer),
                        Span::styled(
                            format!(
                                "{prefix}{}{} — {}",
                                note.decision.as_str(),
                                if note.signed { " (signed)" } else { "" },
                                note.summary
                            ),
                            style,
                        ),
                    ]));
                }
                if card.review.notes.is_empty() {
                    lines.push(Line::from(vec![
                        key("Review"),
                        crate::view::review_span(card.review.verdict),
                    ]));
                }
            }
            lines.push(Line::from(Span::styled(
                "Enter opens the diff of the commit".to_string(),
                theme::dim(),
            )));
        }
    }
    (!lines.is_empty()).then_some(lines)
}

/// Kürzt eine Span-Zeile auf `width` Zeichen — mit „…", nie hart.
fn fit(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    crate::view::fit(Line::from(spans), width).0
}

/// Bricht einen Text an Wortgrenzen auf `width` um.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// Der Satz der Fußzeile zur Zeile unter dem Cursor (C11: „step 5 · tests ·
/// ✓ 8 passed").
pub(crate) fn footer_sentence(story: &Story, row: &Row) -> Option<(String, Style)> {
    match &row.kind {
        RowKind::Step(i) => {
            let step = story.steps.get(*i)?;
            let (right, style) = right_of(step);
            let mut text = format!("step {} · {}", step.number, step.verb.word());
            if !right.is_empty() {
                text.push_str(&format!(" · {right}"));
            }
            Some((text, style.add_modifier(Modifier::BOLD)))
        }
        RowKind::Run { first, count, .. } => Some((
            format!("steps {}–{} · z shows each one", first + 1, first + count),
            theme::dim(),
        )),
        _ => None,
    }
}
