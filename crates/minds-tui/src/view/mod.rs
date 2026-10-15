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

use crate::app::{App, Tab, View};
use crate::changes::Focus;
use crate::theme;

pub mod activity;
mod banner;
pub mod changes;
pub mod evidence;
pub mod graph;
pub mod help;
mod intent;
mod overview;
pub mod trust;
pub mod verify;
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
    let banner_h = u16::from(app.tamper.is_some());
    let [head, alert, body, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(banner_h),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    header(frame, app, head);
    if let Some(tamper) = &app.tamper {
        banner::draw(frame, alert, tamper);
    }
    if app.tab == Tab::Changes {
        changes::draw(frame, app, body);
        footer(frame, app, foot);
        if app.help {
            app.help_scroll = help::draw(frame, frame.area(), app.tab, app.help_scroll);
        }
        return;
    }
    if app.tab == Tab::Verify {
        verify::draw(frame, app, body);
        footer(frame, app, foot);
        if app.help {
            app.help_scroll = help::draw(frame, frame.area(), app.tab, app.help_scroll);
        }
        return;
    }
    if app.tab == Tab::Overview {
        overview::draw(frame, app, body);
        footer(frame, app, foot);
        if app.help {
            app.help_scroll = help::draw(frame, frame.area(), app.tab, app.help_scroll);
        }
        return;
    }
    if app.tab == Tab::Intent {
        intent::draw(frame, app, body);
        footer(frame, app, foot);
        if app.help {
            app.help_scroll = help::draw(frame, frame.area(), app.tab, app.help_scroll);
        }
        return;
    }
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
        app.help_scroll = help::draw(frame, frame.area(), app.tab, app.help_scroll);
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
            ..
        } => why::draw(frame, area, chain, *cursor, *edge, inspector.as_deref()),
        View::Evidence {
            id,
            report,
            uninterpreted,
            artifacts,
            cursor,
        } => evidence::draw(
            frame,
            area,
            *id,
            report.as_ref(),
            *uninterpreted,
            artifacts,
            *cursor,
        ),
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
    // Die Tabs zuerst gerechnet: Sie fallen nie weg — unter 100 Spalten mit
    // Kurzlabel; Repo und Branch weichen vorher (mit „…").
    let short = area.width < 100;
    let mut tabs: Vec<Span> = Vec::new();
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let name = if short { tab.short() } else { tab.title() };
        let label = format!(" F{} {name} ", i + 1);
        // Der aktive invertiert, mit F-Taste — Tab/Shift-Tab wechselt.
        tabs.push(if *tab == app.tab {
            Span::styled(
                label,
                theme::title().add_modifier(ratatui::style::Modifier::REVERSED),
            )
        } else {
            Span::styled(label, theme::dim())
        });
        tabs.push(Span::raw(" "));
    }
    let tabs_w: usize = tabs.iter().map(|s| s.content.chars().count()).sum();
    let place = format!(
        "{} · {}",
        h.repo,
        h.branch.clone().unwrap_or_else(|| "(detached)".into())
    );
    let room = (area.width as usize).saturating_sub(tabs_w + "MINDS ".len() + 3);
    let mut title = vec![
        Span::styled("MINDS ", theme::title().fg(theme::AGENT)),
        Span::styled(clip(&place, room), theme::title()),
        Span::raw("   "),
    ];
    title.extend(tabs);
    let title = Line::from(title);
    // Zeile 2: der Breadcrumb (B2) — wo bin ich, über welche Sprünge. Von
    // links gekürzt; die globale Statistik steht dahinter, solange Platz
    // ist, und weicht als Erstes (C3 zieht sie in die Übersicht).
    let crumb = app.breadcrumb().join(" › ");
    let mut stats = format!(
        "{} Sessions · {} Changes · {:.0} % context coverage",
        h.sessions,
        h.changes,
        h.coverage.ratio() * 100.0
    );
    if h.degraded > 0 {
        stats.push_str(&format!(" · {} degraded", h.degraded));
    }
    let width = area.width as usize;
    let crumb_w = crumb.chars().count();
    let stats_w = stats.chars().count();
    // Die Statistik direkt hinter dem Pfad, gedimmt — nur mit Luft: Eine
    // Zeile, die exakt aufgeht, sähe aus wie abgeschnitten.
    let line = if crumb_w + 3 + stats_w < width {
        Line::from(vec![
            Span::styled(crumb, theme::title()),
            Span::styled("   ", theme::dim()),
            Span::styled(stats, theme::dim()),
        ])
    } else if crumb_w <= width {
        Line::from(Span::styled(crumb, theme::title()))
    } else {
        let tail: String = crumb
            .chars()
            .skip(crumb_w - width.saturating_sub(1))
            .collect();
        Line::from(Span::styled(format!("…{tail}"), theme::title()))
    };
    frame.render_widget(Paragraph::new(vec![title, line]), area);
}

/// Die Fußzeile des Changes-Tabs: der Commit und die Tasten des Fokus.
fn changes_footer(frame: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.changes else {
        return;
    };
    let status = match &state.set {
        Ok(set) => {
            let (glyph, word, style) = theme::verdict(set.review.verdict);
            let mut spans = vec![
                Span::styled(
                    set.commit.to_string()[..7].to_string(),
                    Style::default().fg(theme::CHANGE),
                ),
                Span::raw(format!(" {}  ", set.subject.clone().unwrap_or_default())),
                Span::styled(format!("{glyph} {word}"), style),
            ];
            if let Some(why) = &set.unassessed {
                spans.push(Span::styled(
                    format!("  · not assessed: {why}"),
                    theme::dim(),
                ));
            }
            Line::from(spans)
        }
        Err(err) => Line::from(Span::styled(err.clone(), theme::dim())),
    };
    let keys = actions(app).join("  ");
    let keys = Line::from(vec![
        freshness(app),
        Span::raw("  "),
        Span::styled(keys, theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(vec![
            status_line(with_context(app, status, area.width), area.width),
            pinned(keys, area.width, app.searching),
        ]),
        area,
    );
}

fn footer(frame: &mut Frame, app: &App, area: Rect) {
    if app.tab == Tab::Changes {
        changes_footer(frame, app, area);
        return;
    }
    if app.tab == Tab::Verify {
        // Zeile 1: der Satz zum Urteil (C1) — kein statischer Erklärsatz.
        let word = crate::copy::overall(app.verify.as_ref().map(|v| v.overall()));
        let status = explain(word);
        let keys = Line::from(vec![
            freshness(app),
            Span::raw("  "),
            Span::styled(actions(app).join("  "), theme::dim()),
        ]);
        frame.render_widget(
            Paragraph::new(vec![
                status_line(with_context(app, status, area.width), area.width),
                pinned(keys, area.width, app.searching),
            ]),
            area,
        );
        return;
    }
    if app.tab == Tab::Overview {
        // Zeile 1: der Satz zum Seal-Zustand des gewählten Commits (C1);
        // die Legende der Pillen steht in der Hilfe.
        let word = match app.overview.as_ref().and_then(|o| o.selected()) {
            Some(crate::overview::Selected::Commit(row)) => crate::copy::seal(row.seal),
            Some(crate::overview::Selected::Wip(_)) => "observed",
            None => crate::copy::seal(None),
        };
        let status = explain(word);
        let mut keys = vec![
            freshness(app),
            Span::raw("  "),
            Span::styled(actions(app).join("  "), theme::dim()),
        ];
        if let Some(closed) = &app.closed {
            keys.push(Span::styled(format!("  · {closed}"), theme::dim()));
        }
        frame.render_widget(
            Paragraph::new(vec![
                status_line(with_context(app, status, area.width), area.width),
                pinned(Line::from(keys), area.width, app.searching),
            ]),
            area,
        );
        return;
    }
    if app.tab == Tab::Intent {
        // Zeile 1 (C1): Bindungen an einen Anker sind aufgezeichnet — Aussagen.
        let status = explain("claim");
        let mut keys = vec![
            freshness(app),
            Span::raw("  "),
            Span::styled(actions(app).join("  "), theme::dim()),
        ];
        if let Some(closed) = &app.closed {
            keys.push(Span::styled(format!("  · {closed}"), theme::dim()));
        }
        frame.render_widget(
            Paragraph::new(vec![
                status_line(with_context(app, status, area.width), area.width),
                pinned(Line::from(keys), area.width, app.searching),
            ]),
            area,
        );
        return;
    }
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
                    // Der Evidenz-Satz vorn, dahinter der Satz zum Seal-Zustand
                    // (C1) — Wörter ohne Glyph, der Badge steht in Zeile 2.
                    Line::from(vec![
                        Span::styled(format!("{glyph} {word}  "), style),
                        // Nur der erste Satz: Der Prüfstatus steht im
                        // Glossar („observed is not checked").
                        Span::styled(
                            first_sentence(&evidence_sentence(card.evidence)),
                            theme::dim(),
                        ),
                        Span::styled("  ·  ", theme::dim()),
                        Span::styled(integrity_sentence(card.axes()), theme::dim()),
                    ])
                }
            })
            .unwrap_or_default(),
        // Graph (C1): der Satz zum Knoten unter dem Cursor — der Auftrag ist
        // eine Aussage, ein ungedeuteter Aufruf beobachtet, sonst der
        // Seal-Zustand der Session.
        Some(View::Graph {
            id, rows, cursor, ..
        }) => {
            let word = match rows.get(*cursor).map(|r| &r.kind) {
                Some(minds_reader::graph::NodeKind::Intent) => Some("claim"),
                Some(minds_reader::graph::NodeKind::Tool(
                    minds_reader::graph::ToolKind::Uninterpreted,
                )) => Some(crate::copy::interpretation(
                    minds_reader::model::InterpretationAxis::Uninterpreted(1),
                )),
                _ => None,
            };
            match word {
                Some(word) => explain(word),
                None => Line::from(Span::styled(
                    app.inspection
                        .card(*id)
                        .map(|c| integrity_sentence(c.axes()))
                        .unwrap_or_default(),
                    theme::dim(),
                )),
            }
        }
        Some(View::Why { chain, .. }) => {
            let gaps = chain.gaps();
            if gaps.is_empty() {
                Line::from(Span::styled(
                    "✓ no gap in the evidence links · intent is a recorded claim",
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
        // Evidence: der Satz des Reports, dahinter der Glossar-Satz der
        // fokussierten Achse (C1).
        Some(View::Evidence {
            report,
            uninterpreted,
            cursor,
            ..
        }) => {
            let axes = report.as_ref().map(|r| {
                minds_reader::model::TrustAxes::of(
                    &minds_reader::model::Provenance::Chained(r.state),
                    *uninterpreted,
                )
            });
            let word = axes.and_then(|axes| match cursor {
                0 => Some(crate::copy::integrity(axes.integrity)),
                1 => Some(crate::copy::coverage(axes.coverage)),
                5 => Some(crate::copy::interpretation(axes.interpretation)),
                _ => None,
            });
            let mut spans = vec![Span::styled(
                report
                    .as_ref()
                    .map(|r| r.sentence())
                    .unwrap_or(minds_reader::model::LEGACY_SENTENCE),
                theme::dim(),
            )];
            if let Some(sentence) = word.and_then(crate::copy::sentence) {
                spans.push(Span::styled(format!("  ·  {sentence}"), theme::dim()));
            }
            Line::from(spans)
        }
    };
    let keys = if app.searching {
        Line::from(vec![
            freshness(app),
            Span::raw("  "),
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
        // Die Frische vor den Tasten: Auf einem schmalen Terminal werden die
        // Tasten abgeschnitten, nie die Warnung, dass der Stand alt ist.
        spans.push(freshness(app));
        spans.push(Span::raw("  "));
        if !app.query.is_empty() {
            spans.push(Span::styled(
                format!("[{}] ", app.query),
                theme::title().fg(theme::EDIT),
            ));
        }
        let keys = actions(app).join("  ");
        spans.push(Span::styled(keys, theme::dim()));
        spans.push(Span::styled(
            format!("  Zoom {}", app.zoom.digit()),
            theme::dim(),
        ));
        Line::from(spans)
    };
    frame.render_widget(
        Paragraph::new(vec![
            status_line(with_context(app, status, area.width), area.width),
            pinned(keys, area.width, app.searching),
        ]),
        area,
    );
}

/// Die häufigsten Aktionen des Fokus, nach Priorität — 4 bis 6 (C2); alles
/// Weitere steht in der Hilfe. `? help  q quit` hängt [`pinned`] rechts an.
pub(crate) fn actions(app: &App) -> Vec<&'static str> {
    match app.tab {
        Tab::Overview => {
            if app
                .overview
                .as_ref()
                .is_some_and(|o| o.focus == crate::overview::OverviewFocus::Sessions)
            {
                vec!["↑↓ session", "Enter its graph", "e evidence", "Esc back"]
            } else {
                vec![
                    "↑↓ commit",
                    "Enter how it was made",
                    "w why",
                    "v verify",
                    "d diff",
                    "/ filter",
                ]
            }
        }
        Tab::Sessions => match app.top() {
            None => vec![
                "↑↓ select",
                "Enter descend",
                "w why",
                "e evidence",
                "/ search",
                "z Z zoom",
            ],
            Some(View::Graph { .. }) => {
                vec![
                    "↑↓ select",
                    "Enter descend",
                    "w why",
                    "e evidence",
                    "z Z zoom",
                    "Esc back",
                ]
            }
            Some(View::Why { .. }) => vec!["↑↓ select", "Enter open", "e evidence", "Esc back"],
            Some(View::Evidence { .. }) => vec!["↑↓ section", "Enter open", "w why", "Esc back"],
        },
        Tab::Verify => vec!["↑↓ row", "Enter open", "[ ] commit", "d diff", "w why"],
        Tab::Changes => match app.changes.as_ref().map(|c| c.focus) {
            Some(Focus::Diff) => vec![
                "↑↓ line",
                "] [ hunk",
                "n N unexplained",
                "s split",
                "w why chain",
                "Esc files",
            ],
            _ => vec![
                "↑↓ file",
                "Enter diff",
                "[ ] commit",
                "n next unexplained",
                "c why column",
                "/ filter",
            ],
        },
        Tab::Intent => vec!["↑↓ anchor", "Enter graph", "PgUp PgDn scroll", "/ filter"],
    }
}

/// Zeile 1, vorn: der fokussierte Gegenstand — Session mit Trust Strip,
/// Commit mit seiner Kurz-Id, Anker mit seiner (C2).
fn context(app: &App, wide: bool) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let session = match app.tab {
        Tab::Sessions => match app.top() {
            None => app.selected().filter(|c| !c.is_degraded()).map(|c| c.id),
            Some(View::Graph { id, .. }) | Some(View::Evidence { id, .. }) => Some(*id),
            Some(View::Why { .. }) => None,
        },
        _ => None,
    };
    if let Some(card) = session.and_then(|id| app.inspection.card(id)) {
        let id = card.id.to_string();
        spans.push(Span::styled(
            format!("{}… ", id.get(..11).unwrap_or(&id)),
            theme::title().fg(theme::AGENT),
        ));
        // Der Trust Strip: die Integrität als invertierter Block (A4), ab
        // 120 Spalten mit Wort je Achse (Short), darunter als Glyph.
        let axes = card.axes();
        let [integrity, _, _] = trust::slots(&axes);
        spans.push(Span::styled(
            format!(" {} {}", integrity.glyph, integrity.word),
            integrity
                .style
                .add_modifier(ratatui::style::Modifier::REVERSED),
        ));
        let density = if wide {
            trust::Density::Short
        } else {
            trust::Density::Compact
        };
        spans.extend(trust::spans(&axes, density).into_iter().skip(1));
        spans.push(Span::raw("  "));
    }
    let commit = match app.tab {
        Tab::Overview => match app.overview.as_ref().and_then(|o| o.selected()) {
            Some(crate::overview::Selected::Commit(row)) => Some(row.id),
            _ => None,
        },
        Tab::Verify => app.verify.as_ref().and_then(|v| v.commit()),
        _ => None,
    };
    if let Some(commit) = commit {
        spans.push(Span::styled(
            format!("{} ", &commit.to_string()[..7]),
            Style::default().fg(theme::CHANGE),
        ));
    }
    if app.tab == Tab::Intent
        && let Some(anchor) = app.intent.as_ref().and_then(|i| i.selected())
    {
        let id = anchor.id.to_string();
        spans.push(Span::styled(
            format!("{}… ", id.get(..14).unwrap_or(&id)),
            theme::title(),
        ));
    }
    spans
}

/// Zeile 1: Kontext vor dem Satz.
fn with_context(app: &App, status: Line<'static>, width: u16) -> Line<'static> {
    let mut spans = context(app, width >= 120);
    spans.extend(status.spans);
    Line::from(spans)
}

/// Die rechts fixierten Tasten der Fußzeile: Hilfe und Ausgang fallen nie
/// weg (A6).
const PINNED: &str = "? help  q quit";
/// Beim Tippen einer Suche sind `?` und `q` Zeichen der Suche — dort stehen
/// rechts die Suchaktionen (C2: im Suchmodus haben sie Vorrang).
const PINNED_SEARCH: &str = "Enter apply  Esc clear";

/// Der Glossar-Satz zur Integrität einer Session.
fn integrity_sentence(axes: minds_reader::model::TrustAxes) -> String {
    crate::copy::sentence(crate::copy::integrity(axes.integrity))
        .unwrap_or_default()
        .to_string()
}

/// Der erste Satz eines Texts (bis zum ersten „. ").
fn first_sentence(text: &str) -> String {
    match text.find(". ") {
        Some(at) => text[..=at].to_string(),
        None => text.to_string(),
    }
}

/// Der Glossar-Satz zu `word` als Statuszeile (C1).
fn explain(word: &str) -> Line<'static> {
    Line::from(Span::styled(
        crate::copy::sentence(word).unwrap_or_default(),
        theme::dim(),
    ))
}

/// Kürzt eine Zeile auf `width` Zeichen — von rechts, mit „…", nie hart.
fn fit(line: Line<'static>, width: usize) -> (Vec<Span<'static>>, usize) {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in line.spans {
        let n = span.content.chars().count();
        if used + n <= width {
            used += n;
            out.push(span);
        } else {
            let rest = width.saturating_sub(used);
            if rest > 0 {
                out.push(Span::styled(clip(&span.content, rest), span.style));
                used += rest;
            } else if let Some(last) = out.pop() {
                // Kein Platz mehr für den nächsten Span: Das „…" ersetzt das
                // letzte Zeichen des vorigen — nie ein harter Schnitt.
                let n = last.content.chars().count();
                let keep: String = last.content.chars().take(n.saturating_sub(1)).collect();
                out.push(Span::styled(format!("{keep}…"), last.style));
            }
            break;
        }
    }
    (out, used)
}

/// Zeile 1 der Fußzeile, auf die Breite gekürzt (A6).
fn status_line(line: Line<'static>, width: u16) -> Line<'static> {
    Line::from(fit(line, width as usize).0)
}

/// Zeile 2 der Fußzeile: links Kontext und Tasten in Prioritätsfolge — von
/// rechts gekürzt, mit „…" —, rechts fixiert [`PINNED`].
fn pinned(line: Line<'static>, width: u16, searching: bool) -> Line<'static> {
    let width = width as usize;
    let pin = if searching { PINNED_SEARCH } else { PINNED };
    let right = pin.chars().count();
    let room = width.saturating_sub(right + 2);
    let stripped = Line::from(
        line.spans
            .into_iter()
            .map(|span| {
                let text = span
                    .content
                    .replace("  ? help  q quit", "")
                    .replace("  ? help", "")
                    .replace("  q quit", "");
                Span::styled(text, span.style)
            })
            .collect::<Vec<_>>(),
    );
    let (left, used) = fit(stripped, room);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used + right))));
    spans.push(Span::styled(pin, theme::title()));
    Line::from(spans)
}

/// Wie frisch das Gezeigte ist — und, wenn es das nicht ist, warum (Glyph und
/// Wort, nie nur Farbe). Vorrang: gescheitertes Laden (samt abgeschaltetem
/// Live-Modus) vor abgeschaltetem Live-Modus vor einer geschlossenen Ebene;
/// ein Hinweis, der später wieder vorn steht, bleibt bis zur nächsten Taste.
/// Beim Tippen einer Suche fehlt der `r`-Hinweis — dort ist `r` ein Zeichen.
fn freshness(app: &App) -> Span<'static> {
    let warn = Style::default().fg(theme::REVIEW);
    let at = &app.loaded_at;
    let hint = !app.searching;
    match (&app.reload_error, app.live, &app.closed) {
        (Some(err), live, _) => Span::styled(
            format!(
                "⚠ reload failed{}, showing {at}: {}",
                if live { "" } else { " · live off" },
                clip(err, 40)
            ),
            warn,
        ),
        (None, false, _) if hint => Span::styled(format!("⚠ live off — r reloads · {at}"), warn),
        (None, false, _) => Span::styled(format!("⚠ live off · {at}"), warn),
        (None, true, Some(why)) => Span::styled(format!("⚠ closed {} · {at}", clip(why, 40)), warn),
        (None, true, None) if hint => {
            Span::styled(format!("updated {at} · r reload"), theme::dim())
        }
        (None, true, None) => Span::styled(format!("updated {at}"), theme::dim()),
    }
}

/// `DD.MM. HH:MMZ` aus dem RFC-3339-Präfix; `—`, wenn keine Zeit erfasst ist.
/// UTC, wie der Zeitstempel selbst — ohne Datums-Crate keine Ortszeit.
pub fn when(ts: Option<&str>) -> String {
    // Im Detail absolut, in UTC und ohne Landesformat (C4):
    // `2026-10-14 09:00 UTC`. `get` statt Index-Slicing: Der Wert kann
    // fremdbestimmt sein (etwa die Zeitzeile eines handgebauten Seals) — ein
    // Multi-Byte-Zeichen an der falschen Stelle darf nicht panicken.
    match ts {
        Some(ts) => match (ts.get(0..10), ts.get(11..16)) {
            (Some(date), Some(hm)) if minds_metrics::epoch_seconds(ts).is_some() => {
                format!("{date} {hm} UTC")
            }
            _ => ts.to_string(),
        },
        None => "—".into(),
    }
}

/// Wie lange her, für Listen (C4): `just now`, `5m ago`, `3h ago`, `2d ago`
/// — gemessen an `now` (Sekunden seit Epoch). In der Zukunft oder unlesbar:
/// die absolute Form aus [`when`].
pub fn ago(ts: Option<&str>, now: i64) -> String {
    let Some(at) = ts.and_then(minds_metrics::epoch_seconds) else {
        return when(ts);
    };
    let secs = now - at;
    match secs {
        s if s < 0 => when(ts),
        s if s < 60 => "just now".into(),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        s => format!("{}d ago", s / 86_400),
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
