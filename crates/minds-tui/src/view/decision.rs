//! Der Decisions-Tab: links die Entscheidungen als Register — Stand,
//! Kurz-Id, Thema —, rechts die gewählte: zuerst das Thema und der Stand in
//! einem Satz, dann der gewählte Weg und die Alternativen mit ihren Gründen
//! und dem Stand der Annahme dahinter, dann jede Annahme mit ihrer Probe,
//! dann Scope, Historie, Sessions. Nie mehr behaupten als `minds
//! decisions`: Jeder Stand ist eine Ableitung zur Lesezeit über
//! Aussagen des Agenten (◌), kein Beleg.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use minds_core::decision::{Probe, Reason};
use minds_reader::decisions::{Assessment, Standing};

use crate::app::App;
use crate::decision::{DecisionTab, naming_sessions};
use crate::theme;
use crate::view::overview::anchor_short;

/// Zeichnet den Tab.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.decisions else {
        return;
    };
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(area);
    draw_list(frame, state, left);
    draw_detail(frame, app, state, right);
}

/// Links: die Spitzen, je eine Zeile — Stand, Kurz-Id, Thema.
fn draw_list(frame: &mut Frame, state: &DecisionTab, left: Rect) {
    let mut rows: Vec<Line> = Vec::new();
    match &state.entries {
        None => rows.push(Line::from(Span::styled(
            "… reading decisions",
            theme::dim(),
        ))),
        Some(Err(why)) => rows.push(Line::from(Span::styled(
            format!("· decisions not readable ({why})"),
            theme::dim(),
        ))),
        Some(Ok(_)) if state.heads().is_empty() => rows.push(Line::from(Span::styled(
            "No decisions recorded — minds decide --template prints the form",
            theme::dim(),
        ))),
        Some(Ok(_)) if state.shown().is_empty() => rows.push(Line::from(Span::styled(
            "no decision matches the filter",
            theme::dim(),
        ))),
        Some(Ok(_)) => {
            let heads = state.heads();
            let shown = state.shown();
            let inner = usize::from(left.width.saturating_sub(2));
            let height = usize::from(left.height.saturating_sub(2)).max(1);
            let at = shown.iter().position(|i| *i == state.cursor).unwrap_or(0);
            let first = crate::view::offset(at, shown.len(), height);
            for &i in shown.iter().skip(first).take(height) {
                let a = heads[i];
                // Nach einem Neuladen zeigt auch die Liste keinen alten
                // Stand mehr — `?`, bis neu gerechnet ist.
                let (glyph, word, style) = if state.stale {
                    ("?", "rechecking", theme::dim())
                } else {
                    theme::standing(a.standing)
                };
                let focus = if i == state.cursor { "▸ " } else { "  " };
                let text = format!(
                    "{focus}{glyph} {}  {}",
                    anchor_short(&a.id),
                    crate::view::clip(&a.topic, inner.saturating_sub(16 + word.len()))
                );
                let mut spans = vec![Span::styled(text, style)];
                spans.push(Span::styled(format!("  {word}"), theme::dim()));
                let mut line = Line::from(spans);
                if i == state.cursor {
                    line = line.style(theme::cursor());
                }
                rows.push(line);
            }
            if state.unreadable > 0 {
                rows.push(Line::from(Span::styled(
                    format!(
                        "  {} record{} not readable — minds decisions names them",
                        state.unreadable,
                        if state.unreadable == 1 { "" } else { "s" }
                    ),
                    theme::dim(),
                )));
            }
            if state.truncated {
                rows.push(Line::from(Span::styled(
                    "  not every record read — see minds decisions",
                    theme::dim(),
                )));
            }
        }
    }
    let heads = state.heads();
    let mut title = format!(" Decisions · {} ", heads.len());
    // Nach einem Neuladen zählt der Titel nichts mehr — die Zahlen wären
    // der alte Stand.
    if state.stale {
        title.push_str("· rechecking… ");
    } else {
        for (standing, word) in [
            (Standing::Open, "open"),
            (Standing::Review, "to re-evaluate"),
            (Standing::ReCheck, "to re-check"),
            (Standing::Unchecked, "unchecked"),
            (Standing::InForce, "in force"),
        ] {
            let n = state.count(standing);
            if n > 0 {
                title.push_str(&format!("· {n} {word} "));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(rows).block(Block::bordered().title(title).title_style(theme::title())),
        left,
    );
}

/// Rechts: die gewählte Entscheidung in fester Reihenfolge.
fn draw_detail(frame: &mut Frame, app: &App, state: &DecisionTab, right: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let inner = usize::from(right.width.saturating_sub(2));
    if let Some(a) = state.selected() {
        lines.extend(detail_lines(app, state, a, inner));
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let inner_width = right.width.saturating_sub(2);
    let inner_height = usize::from(right.height.saturating_sub(2));
    let rendered = paragraph.line_count(inner_width);
    let max = u16::try_from(rendered.saturating_sub(inner_height)).unwrap_or(u16::MAX);
    state.max_scroll.set(max);
    frame.render_widget(
        paragraph.scroll((state.scroll.min(max), 0)).block(
            Block::bordered()
                .title(" DECISION ")
                .title_style(theme::title()),
        ),
        right,
    );
}

/// Die Zeilen des Details — auch für Tests ohne Frame.
pub(crate) fn detail_lines(
    app: &App,
    state: &DecisionTab,
    a: &Assessment,
    inner: usize,
) -> Vec<Line<'static>> {
    let (claim_glyph, _, claim_style) = theme::claim();
    let r = &a.record;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let day = minds_reader::decisions::day_of(&r.at);

    // DECISION — Thema, Kurz-Id, wer und wann; als Aussage gekennzeichnet.
    lines.push(Line::from(vec![
        Span::styled("DECISION  ", theme::title()),
        Span::styled(
            format!("{} {}", theme::decision().0, anchor_short(&a.id)),
            theme::title(),
        ),
        Span::styled(
            format!(
                " · {} · {}",
                minds_reader::decisions::by_word(r.decided_by),
                day
            ),
            theme::dim(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!("  {claim_glyph} "), claim_style),
        Span::styled(a.topic.clone(), claim_style),
    ]));
    lines.push(Line::raw(""));

    // STANDING — der Satz, dann die Vermerke.
    lines.push(Line::from(Span::styled("STANDING", theme::title())));
    let (glyph, _, style) = theme::standing(a.standing);
    let headline = if state.stale {
        "rechecking…".to_owned()
    } else {
        a.headline()
    };
    lines.push(mark(glyph, headline, style));
    if !state.stale {
        for note in &a.notes {
            lines.push(mark("·", note.clone(), theme::dim()));
        }
    }
    lines.push(Line::raw(""));

    // CHOSEN / OPTIONS — der Weg und die Annahmen, auf denen er ruht.
    let result_of = |id: &str| -> (&'static str, String, Style) {
        if state.stale {
            return ("?", "rechecking".into(), theme::dim());
        }
        a.assumptions
            .iter()
            .find(|s| s.id == minds_reader::sanitize(id))
            .map(|s| theme::probe(s.result))
            .unwrap_or(("?", "unknown assumption".into(), theme::dim()))
    };
    let reasons = |lines: &mut Vec<Line<'static>>, label: &str, list: &[Reason]| {
        for (i, reason) in list.iter().enumerate() {
            let (glyph, word, style) = result_of(&reason.assumption);
            let statement = r
                .assumption(&reason.assumption)
                .map(|x| minds_reader::sanitize(&x.statement))
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {:<9} ", if i == 0 { label } else { "" }),
                    theme::dim(),
                ),
                Span::raw(format!(
                    "{}  {} ",
                    minds_reader::sanitize(&reason.assumption),
                    crate::view::clip(&statement, inner.saturating_sub(40).max(12))
                )),
                Span::styled(format!("({}) ", reason.weight.as_str()), theme::dim()),
                Span::styled(format!("{glyph} {word}"), style),
            ]));
        }
    };
    match (&a.chosen, r.chosen.as_ref()) {
        (Some(chosen), Some(c)) => {
            lines.push(Line::from(Span::styled("CHOSEN", theme::title())));
            lines.push(Line::from(vec![
                Span::styled(format!("  {claim_glyph} "), claim_style),
                Span::styled(chosen.clone(), claim_style),
            ]));
            reasons(&mut lines, "rests on", &c.rests_on);
        }
        _ => {
            lines.push(Line::from(Span::styled(
                format!(
                    "OPTIONS ({}) — the choice is with a human",
                    r.alternatives.len()
                ),
                theme::title(),
            )));
        }
    }
    let label = if r.open { "option" } else { "rejected" };
    for (i, alternative) in r.alternatives.iter().enumerate() {
        if !r.open && i == 0 {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled("REJECTED", theme::title())));
        }
        let approach = a
            .alternatives
            .get(i)
            .cloned()
            .unwrap_or_else(|| minds_reader::sanitize(&alternative.approach));
        let reconsider = a.reconsider.iter().any(|x| x.alternative == approach);
        let mut head = vec![
            Span::styled(format!("  {claim_glyph} "), claim_style),
            Span::styled(approach, claim_style),
        ];
        if reconsider && !state.stale {
            head.push(Span::styled(
                "  ✗ reconsider",
                Style::default().fg(theme::DELETE),
            ));
        }
        if r.open {
            head.insert(0, Span::styled(format!("  {label} "), theme::dim()));
        }
        lines.push(Line::from(head));
        reasons(&mut lines, "because", &alternative.rejected_because);
        reasons(&mut lines, "rests on", &alternative.rests_on);
    }
    lines.push(Line::raw(""));

    // ASSUMPTIONS — jede mit Ergebnis; die Probe im Wortlaut unter TECHNICAL.
    lines.push(Line::from(Span::styled(
        format!("ASSUMPTIONS ({})", a.assumptions.len()),
        theme::title(),
    )));
    if a.assumptions.is_empty() {
        lines.push(mark("·", "none recorded".into(), theme::dim()));
    }
    for s in &a.assumptions {
        let (glyph, word, style) = if state.stale {
            ("?", "rechecking".to_owned(), theme::dim())
        } else {
            theme::probe(s.result)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {glyph} "), style),
            Span::raw(format!(
                "{}  {}",
                s.id,
                crate::view::clip(&s.statement, inner.saturating_sub(30).max(12))
            )),
            Span::styled(format!("  {word}"), style),
        ]));
    }
    if !r.scope.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("SCOPE  ", theme::title()),
            Span::raw(
                r.scope
                    .iter()
                    .map(|g| minds_reader::sanitize(g))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ]));
    }

    // HISTORY — die Kette: wen dieser Record ablöst, wer ihn abgelöst hat.
    let mut history: Vec<Line<'static>> = Vec::new();
    let mut at = r.supersedes.clone();
    let mut guard = 0;
    while let Some(old_id) = at.take() {
        guard += 1;
        if guard > 64 {
            break;
        }
        match state.by_id(&old_id) {
            Some(old) => {
                let old_day = minds_reader::decisions::day_of(&old.record.at);
                history.push(mark(
                    "⚖",
                    format!(
                        "supersedes {} · {} · {} — {}",
                        anchor_short(&old_id),
                        old_day,
                        minds_reader::decisions::by_word(old.record.decided_by),
                        crate::view::clip(
                            old.chosen.as_deref().unwrap_or(&old.topic),
                            inner.saturating_sub(40).max(12)
                        )
                    ),
                    theme::dim(),
                ));
                at = old.record.supersedes.clone();
            }
            None => history.push(mark(
                "⚖",
                format!("supersedes {} (not in this store)", anchor_short(&old_id)),
                theme::dim(),
            )),
        }
    }
    if let Some(by) = &a.superseded_by {
        history.push(mark(
            "⚖",
            format!("superseded by {}", anchor_short(by)),
            theme::dim(),
        ));
    }
    if !history.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled("HISTORY", theme::title())));
        lines.extend(history);
    }

    // SESSIONS — wer die Entscheidung aufgenommen hat (Record).
    let sessions = naming_sessions(&app.inspection, &a.id);
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        format!("SESSIONS ({})", sessions.len()),
        theme::title(),
    )));
    if sessions.is_empty() {
        lines.push(mark(
            "·",
            "no session names this decision — recorded outside a session, or not yet captured"
                .into(),
            theme::dim(),
        ));
    }
    for id in sessions.iter().take(5) {
        let actor = app
            .inspection
            .index()
            .session(*id)
            .map(minds_reader::summary::actor_line)
            .unwrap_or_default();
        let short: String = id.to_string().chars().take(11).collect();
        lines.push(mark(
            claim_glyph,
            format!(
                "{short}… · {}",
                crate::view::clip(&actor, inner.saturating_sub(20))
            ),
            claim_style,
        ));
    }
    if sessions.len() > 5 {
        lines.push(mark(
            "·",
            format!("… {} more", sessions.len() - 5),
            theme::dim(),
        ));
    }

    // TECHNICAL — Ids und Proben im Wortlaut; Space klappt auf.
    lines.push(Line::raw(""));
    if state.technical_open {
        lines.push(Line::from(Span::styled("TECHNICAL", theme::title())));
        lines.push(mark("·", format!("id {}", a.id), theme::dim()));
        for (s, assumption) in a.assumptions.iter().zip(&r.assumptions) {
            let probe = assumption
                .probe
                .as_ref()
                .map(describe_probe)
                .unwrap_or_else(|| "no probe".to_owned());
            lines.push(mark("·", format!("{}: {probe}", s.id), theme::dim()));
        }
        lines.push(mark(
            "·",
            format!("re-evaluate with: minds decide --supersedes {}", a.id),
            theme::dim(),
        ));
    } else {
        lines.push(Line::from(Span::styled(
            "  ▸ Space: technical (ids, probes, the command to supersede)",
            theme::dim(),
        )));
    }
    lines
}

/// Eine Statuszeile: Glyph, dann der Satz.
fn mark(glyph: &'static str, text: String, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{glyph} "), style),
        Span::styled(text, style),
    ])
}

/// Eine Probe im Wortlaut, entschärft.
pub(crate) fn describe_probe(probe: &Probe) -> String {
    let s = minds_reader::sanitize;
    match probe {
        Probe::Pattern {
            path,
            glob,
            must_match,
            must_not_match,
        } => {
            let mut out = format!(
                "pattern in {}",
                s(path.as_deref().or(glob.as_deref()).unwrap_or("?"))
            );
            if let Some(m) = must_match {
                out.push_str(&format!(" must match {}", s(m)));
            }
            if let Some(m) = must_not_match {
                out.push_str(&format!(" must not match {}", s(m)));
            }
            out
        }
        Probe::Path { path, exists } => format!(
            "path {} {}",
            s(path),
            if *exists { "is tracked" } else { "is absent" }
        ),
        Probe::Blob { path, blob } => format!(
            "blob {} unchanged ({})",
            s(path),
            s(&blob.chars().take(8).collect::<String>())
        ),
    }
}

/// Der Stand einer Entscheidung in Wörtern für die Fußzeile (C2).
pub(crate) fn status_words(a: &Assessment, stale: bool) -> String {
    if stale {
        return "rechecking…".to_owned();
    }
    a.headline()
}
