//! Das Zeichnen. Jede Ebene hat ihr Modul; dieses verteilt Rahmen, Kopf und
//! Fuß und legt die Hilfe darüber.
//!
//! Ist das Terminal breit genug, liegt die Liste dauerhaft links und rechts
//! daneben das Detail: die oberste Ebene des Stapels — oder, solange der
//! Stapel leer ist, die Story der Karte unter dem Cursor als Vorschau, die
//! mit dem Cursor wandert. Liegt eine Ebene, schrumpft die Liste auf eine
//! schmale Spalte (C11) oder verschwindet unter 140 Spalten. Der Stapel selbst weiß davon nichts: Was `Enter`,
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
pub mod help;
mod intent;
mod overview;
pub mod story;
pub mod trust;

#[cfg(test)]
mod tests;

/// Eine Spalte Luft zwischen Listenrahmen und Detail.
const GUTTER_WIDTH: u16 = 1;
/// Was das Detail neben der Liste mindestens braucht: Kopf, YOU-Box und
/// eine Spur, in der ein Pfad noch lesbar ist — bei 120 Spalten genau das,
/// was neben einer Liste mit 30 Zeichen Überschrift bleibt (A6).
const DETAIL_MIN_WIDTH: u16 = 46;
/// Die schmalste Liste neben der Vorschau: die kompakte Stufe mit 30 Zeichen
/// Überschrift statt ihres Minimums (A6) — das Ziel bei 120 Spalten.
const LIST_MIN_WIDTH: u16 = activity::COMPACT_WIDTH + 14;
/// Ab dieser Breite liegen Liste und Detail nebeneinander; darunter füllt
/// wie bisher eine Fläche den Bildschirm.
pub const SPLIT_MIN_WIDTH: u16 = LIST_MIN_WIDTH + GUTTER_WIDTH + DETAIL_MIN_WIDTH;
/// Der Anteil der Liste an der Breite — begrenzt auf das, was ihre Spalten
/// brauchen, und auf das, was ihnen noch etwas bringt.
const LIST_SHARE_PERCENT: u32 = 40;
/// Ab dieser Breite bleibt neben einer geöffneten Story die schmale Liste
/// (C11); darunter bekommt die Story den ganzen Bildschirm.
pub const STORY_SPLIT_MIN: u16 = 140;

/// Die Meldung zu einer degradierten Karte — Fußzeile und Vorschau sagen
/// dasselbe.
const DEGRADED: &str =
    "Degraded: the payload is unreadable — forgotten or damaged; the reference stays resolvable.";

/// Zeichnet den ganzen Bildschirm.
pub fn draw(frame: &mut Frame, app: &mut App) {
    draw_inner(frame, app);
    // `NO_COLOR` (C6): alle fg/bg-Farben fallen weg, Modifier (fett,
    // invertiert, unterstrichen) bleiben — Glyph und Wort tragen jeden
    // Zustand, der Cursor bleibt invertiert, der Fokus trägt `▸`.
    if app.no_color {
        let area = frame.area();
        let buffer = frame.buffer_mut();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &mut buffer[(x, y)];
                cell.set_fg(ratatui::style::Color::Reset);
                cell.set_bg(ratatui::style::Color::Reset);
            }
        }
    }
}

fn draw_inner(frame: &mut Frame, app: &mut App) {
    // Schmal ist das Detail der Übersicht eine Ebene hinter Enter (C3.4).
    let narrow = frame.area().width < 100;
    if let Some(state) = app.overview.as_mut() {
        state.narrow = narrow;
    }
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
    if app.tab == Tab::Overview {
        overview::draw(frame, app, body);
        footer(frame, app, foot);
        if app.help {
            app.help_scroll = help::draw(frame, frame.area(), app.tab, app.help_scroll);
        }
        return;
    }
    if app.tab == Tab::Requirements {
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
    match split(body, app.top().is_some()) {
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
/// keinen Platz hat. Liegt eine Ebene (`open`), bleibt der Liste nur die
/// schmale Spalte aus Zeit und Kurz-Überschrift (C11) — unter 140 Spalten
/// keine.
fn split(body: Rect, open: bool) -> Option<(Rect, Rect)> {
    if open {
        if body.width < STORY_SPLIT_MIN {
            return None;
        }
        let [list, detail] = Layout::horizontal([
            Constraint::Length(activity::NARROW_WIDTH),
            Constraint::Min(1),
        ])
        .spacing(GUTTER_WIDTH)
        .areas(body);
        return Some((list, detail));
    }
    if body.width < SPLIT_MIN_WIDTH {
        return None;
    }
    let share = (u32::from(body.width) * LIST_SHARE_PERCENT / 100) as u16;
    let list_w = share.clamp(LIST_MIN_WIDTH, activity::WIDE_WIDTH);
    let [list, detail] = Layout::horizontal([Constraint::Length(list_w), Constraint::Min(1)])
        .spacing(GUTTER_WIDTH)
        .areas(body);
    Some((list, detail))
}

/// Zeichnet eine gelegte Ebene in `area`.
fn view_draw(frame: &mut Frame, app: &App, area: Rect, view: &View) {
    match view {
        View::Story {
            id,
            story,
            rows,
            cursor,
        } => story::draw(frame, app, area, *id, story, rows, Some(*cursor)),
        View::Evidence {
            id,
            report,
            uninterpreted,
            artifacts,
            lines,
            cursor,
            technical,
        } => evidence::draw(
            frame,
            area,
            app,
            *id,
            report.as_ref(),
            *uninterpreted,
            artifacts,
            lines,
            *cursor,
            *technical,
        ),
    }
}

/// Die Vorschau: die Story der Karte unter dem Cursor, gezeichnet wie eine
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
    if let Some((story, rows)) = app.preview_story(card.id) {
        story::draw(frame, app, area, card.id, &story, &rows, None);
    }
}

/// Der Branch im Kopf als Kurzname (C3.5): `main`, nicht `refs/heads/main`
/// — der Reader liefert den vollen Ref-Namen, Kürzen ist Darstellung.
pub(crate) fn branch_label(branch: Option<&str>) -> String {
    match branch {
        Some(name) => name.strip_prefix("refs/heads/").unwrap_or(name).to_string(),
        None => "(detached)".into(),
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
    let place = format!("{} · {}", h.repo, branch_label(h.branch.as_deref()));
    let room = (area.width as usize).saturating_sub(tabs_w + "MINDS ".len() + 3);
    let mut title = vec![
        Span::styled("MINDS ", theme::title().fg(theme::AGENT)),
        Span::styled(clip(&place, room), theme::title()),
        Span::raw("   "),
    ];
    title.extend(tabs);
    let title = Line::from(title);
    // Zeile 2: nur der Breadcrumb (B2, C3.1) — wo bin ich, über welche
    // Sprünge. Von links gekürzt. Die Zahlen stehen genau einmal: die der
    // Historie im Titel der Übersicht, die der Sessions im Rahmen der Liste.
    let crumb = app.breadcrumb().join(" › ");
    let width = area.width as usize;
    let crumb_w = crumb.chars().count();
    let line = if crumb_w <= width {
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
            let mut spans = vec![
                Span::styled(
                    set.commit.to_string()[..7].to_string(),
                    Style::default().fg(theme::CHANGE),
                ),
                Span::raw(format!(" {}  ", set.subject.clone().unwrap_or_default())),
                review_span(set.review.verdict),
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
    if app.tab == Tab::Overview {
        // Zeile 1 (C3.5): die Kurz-Id (aus `context`) und der Filterzustand —
        // kein Erklärsatz; „sealed is not verified" steht im Detail und in
        // der Hilfe.
        let status = Line::from(Span::styled(overview_state(app), theme::dim()));
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
    if app.tab == Tab::Requirements {
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
        // Story (C11): der gewählte Schritt mit Verb und Ergebnis (`step 5 ·
        // tests · ✓ 8 passed`); ein Zug ist eine Aussage; sonst der
        // Seal-Zustand der Session.
        Some(View::Story {
            id,
            story,
            rows,
            cursor,
        }) => {
            let row = rows.get(*cursor);
            let sentence = match row.and_then(|row| story::footer_sentence(story, row)) {
                Some((text, style)) => Line::from(Span::styled(text, style)),
                None => match row.map(|r| &r.kind) {
                    Some(crate::layout::RowKind::Turn(_)) => explain("claim"),
                    _ => Line::from(Span::styled(
                        app.inspection
                            .card(*id)
                            .map(|c| integrity_sentence(c.axes()))
                            .unwrap_or_default(),
                        theme::dim(),
                    )),
                },
            };
            // Eine lange Story in der Zusammenfassung sagt, dass mehr da ist (C7).
            let full = app.story_rows_normal(*id);
            if app.zoom == crate::Zoom::Summary && full > crate::app::LARGE_STORY {
                let mut spans = vec![Span::styled(
                    format!("Summary of a long story: {full} rows at zoom 2 — z shows more  ·  "),
                    theme::title(),
                )];
                spans.extend(sentence.spans);
                Line::from(spans)
            } else {
                sentence
            }
        }
        // Evidence (C12.1): nur Id, Trust Strip (aus `context`) und das
        // Urteil von `minds verify` — keine Sätze; Legacy sagt sein Wort.
        Some(View::Evidence {
            report, artifacts, ..
        }) => match report.as_ref() {
            Some(report) => {
                let (words, style) =
                    evidence::verdict_words(report, app.evidence_check(artifacts), false);
                Line::from(Span::styled(words, style))
            }
            None => {
                let slot = trust::integrity(minds_reader::model::IntegrityAxis::Legacy);
                Line::from(Span::styled(
                    format!("{} {}", slot.glyph, slot.word),
                    slot.style,
                ))
            }
        },
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
pub(crate) fn actions(app: &App) -> Vec<String> {
    if app.tab == Tab::Changes {
        return changes_actions(app);
    }
    fixed_actions(app).into_iter().map(String::from).collect()
}

/// Die Tasten des Changes-Tabs (C9.1): `n` nennt den Zustand und die Zahl
/// (`n next not-observed (29)`) — ohne solche Zeilen fällt die Taste weg.
/// Intern heißt der Zustand weiter „unexplained"; nur das Wort folgt ihm.
fn changes_actions(app: &App) -> Vec<String> {
    let state = app.changes.as_ref();
    let open = state.map_or(0, |s| s.not_observed());
    let next = (open > 0).then(|| format!("n next not-observed ({open})"));
    let (head, tail): (&[&str], &[&str]) = match state.map(|c| c.focus) {
        Some(Focus::Diff) => (&["↑↓ line", "] [ hunk", "Enter story"], &["s split"]),
        _ => (
            &["↑↓ file", "Enter diff", "[ ] commit"],
            &["c why column", "/ filter"],
        ),
    };
    // In der Dateizeile (unter 160 Spalten) wechselt `Tab` die Datei (C9.2).
    let tab_file = (app.width > 0 && app.width < crate::view::changes::FILES_PANE_MIN)
        .then(|| "Tab file".to_string());
    // Esc geht den genommenen Weg zurück: nach einem Sprung in den Diff zum
    // Ursprung (`Esc back`), sonst in die Dateiliste (`Esc files`).
    let esc = matches!(state.map(|c| c.focus), Some(Focus::Diff)).then(|| {
        if app.at_jump_base() {
            "Esc back".to_string()
        } else {
            "Esc files".to_string()
        }
    });
    head.iter()
        .map(|k| k.to_string())
        .chain(tab_file)
        .chain(next)
        .chain(tail.iter().map(|k| k.to_string()))
        .chain(esc)
        .collect()
}

/// Die Tasten der übrigen Tabs — fest, ohne Zahlen.
fn fixed_actions(app: &App) -> Vec<&'static str> {
    match app.tab {
        Tab::Overview => {
            if app
                .overview
                .as_ref()
                .is_some_and(|o| o.focus == crate::overview::OverviewFocus::Sessions)
            {
                // Ohne Session öffnet Enter den Diff (schmales Detail).
                let none = app
                    .overview
                    .as_ref()
                    .is_some_and(|o| o.sessions().is_empty());
                let mut keys = vec![
                    "↑↓ session",
                    if none {
                        "Enter its diff"
                    } else {
                        "Enter its story"
                    },
                ];
                if let Some(hint) = space_hint(app) {
                    keys.push(hint);
                }
                keys.extend(["e evidence", "Esc back"]);
                keys
            } else {
                // Die sechs häufigsten (C2, C3.5) — `/ filter` und, mit
                // Space, `d diff` stehen in der Hilfe.
                let mut keys = vec!["↑↓ commit", "Enter how it was made", "a agent only/all"];
                if let Some(hint) = space_hint(app) {
                    keys.push(hint);
                }
                keys.extend(["v verify", "d diff", "e evidence"]);
                keys.truncate(6);
                keys
            }
        }
        Tab::Sessions => match app.top() {
            None => vec![
                "↑↓ select",
                "Enter descend",
                "e evidence",
                "d diff",
                "/ search",
                "z Z zoom",
            ],
            Some(View::Story { .. }) => {
                vec![
                    "↑↓ step",
                    "Enter open",
                    "e evidence",
                    "d diff",
                    "z Z zoom",
                    "Esc back",
                ]
            }
            Some(View::Evidence { .. }) => vec![
                "↑↓ section",
                "Space technical",
                "Enter open",
                "d diff",
                "Esc back",
            ],
        },
        Tab::Changes => Vec::new(),
        Tab::Requirements => vec![
            "↑↓ requirement",
            "Enter story",
            "Space technical",
            "PgUp PgDn scroll",
            "/ filter",
        ],
    }
}

/// Zeile 1, vorn: der fokussierte Gegenstand — Session mit Trust Strip,
/// Commit mit seiner Kurz-Id, Anker mit seiner (C2).
fn context(app: &App, wide: bool) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let session = match app.tab {
        Tab::Sessions => match app.top() {
            None => app.selected().filter(|c| !c.is_degraded()).map(|c| c.id),
            Some(View::Story { id, .. }) | Some(View::Evidence { id, .. }) => Some(*id),
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
        _ => None,
    };
    if let Some(commit) = commit {
        spans.push(Span::styled(
            format!("{} ", &commit.to_string()[..7]),
            Style::default().fg(theme::CHANGE),
        ));
    }
    if app.tab == Tab::Requirements
        && let Some(anchor) = app.intent.as_ref().and_then(|i| i.selected())
    {
        // Kurz-Id wie überall, dann der Status in Wörtern (C12.3).
        let stale = app.intent.as_ref().is_some_and(|i| i.stale);
        spans.push(Span::styled(
            format!("{} · ", overview::anchor_short(&anchor.id)),
            theme::title(),
        ));
        spans.push(Span::raw(format!(
            "{}  ",
            intent::status_words(anchor, stale)
        )));
    }
    spans
}

/// Ob Space im Detail der Übersicht etwas aufklappt: ein belegter Anker
/// mit Text (C3.4), sonst ein gekürzter Prompt (C3.6) — `None`, wenn es
/// nichts aufzuklappen gibt.
fn space_hint(app: &App) -> Option<&'static str> {
    let state = app.overview.as_ref()?;
    if state.has_requirement_text() {
        return Some("Space requirement");
    }
    // Gekürzt hat der letzte Draw — dort ist die Höhe bekannt.
    (state.agent_commit().is_some() && state.prompt_clipped.get()).then_some("Space prompt")
}

/// Der Filterzustand der Übersicht für Zeile 1 der Fußzeile (C3.5).
fn overview_state(app: &App) -> String {
    let Some(state) = app.overview.as_ref() else {
        return String::new();
    };
    let mut text = match state.selected() {
        Some(crate::overview::Selected::Wip(_)) => "session without a commit · ".to_string(),
        Some(crate::overview::Selected::Commit(_)) => "· ".to_string(),
        None => String::new(),
    };
    text.push_str(if state.agent_only {
        "showing agent sessions only"
    } else {
        "all commits"
    });
    if !state.filter.is_empty() {
        text.push_str(&format!(" · filter: {}", state.filter));
    }
    // Die nächste Aktion (C3.7) — dieselbe wie die NEXT-Zeile im Detail.
    if let Some(next) = overview::next_action_of(app) {
        text.push_str(&format!(" · NEXT → {next}"));
    }
    text
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

/// `HH:MM:SSZ` aus dem RFC-3339-Zeitstempel (C9.3) — die Uhrzeit, nicht
/// der volle Stempel mit Millisekunden; `None` für Unlesbares.
pub fn clock_z(ts: Option<&str>) -> Option<String> {
    let ts = ts?;
    minds_metrics::epoch_seconds(ts)?;
    ts.get(11..19).map(|hms| format!("{hms}Z"))
}

/// Das Urteil als Span: offen heißt `◦ not reviewed yet` — gedimmt, nie in
/// der Warnfarbe (C9.3); die übrigen Urteile wie [`theme::verdict`].
pub(crate) fn review_span(verdict: minds_reader::model::Verdict) -> Span<'static> {
    if verdict == minds_reader::model::Verdict::Open {
        let (glyph, _, style) = theme::open();
        Span::styled(format!("{glyph} not reviewed yet"), style)
    } else {
        let (glyph, word, style) = theme::verdict(verdict);
        Span::styled(format!("{glyph} {word}"), style)
    }
}

/// Tokens kurz (C5): `999 tok`, `5.7k tok`, `1.2M tok`.
pub fn tokens(n: u64) -> String {
    match n {
        n if n < 1_000 => format!("{n} tok"),
        n if n < 1_000_000 => format!("{:.1}k tok", n as f64 / 1_000.0),
        n => format!("{:.1}M tok", n as f64 / 1_000_000.0),
    }
}

/// Wie lange her, für Listen (C4): `just now`, `5m ago`, `3h ago`, `2d ago`
/// — gemessen an `now` (Sekunden seit Epoch). In der Zukunft oder unlesbar:
/// die absolute Form aus [`when`].
pub fn ago(ts: Option<&str>, now: i64) -> String {
    let Some(at) = ts.and_then(minds_metrics::epoch_seconds) else {
        return when(ts);
    };
    if now < at {
        return when(ts);
    }
    ago_secs(at, now)
}

/// [`ago`] für einen Zeitpunkt in Sekunden seit Epoch — dieselben Wörter;
/// in der Zukunft `just now` (Uhrenversatz zwischen Rechnern).
pub fn ago_secs(at: i64, now: i64) -> String {
    match now - at {
        s if s < 60 => "just now".into(),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// Warum eine Kante Commit ↔ Session im Index steht — der Satz zur
/// [`EvidenceExplanation`] des Readers, für das Detail einer Änderung.
pub(crate) fn explanation(why: &minds_reader::model::EvidenceExplanation) -> String {
    use minds_reader::model::EvidenceExplanation;
    match why {
        EvidenceExplanation::Trailer { commit } => format!(
            "Commit {} carries the Minds-Session-Id trailer — observed, no guessing.",
            commit.to_string().chars().take(10).collect::<String>()
        ),
        EvidenceExplanation::Declared => "A human declared the connection (--after).".into(),
        EvidenceExplanation::Content => {
            "Recomputable from content: the bytes read are the bytes written.".into()
        }
        EvidenceExplanation::Heuristic {
            shared_files,
            seconds_apart,
            in_window,
        } => {
            let files = if shared_files.is_empty() {
                "no shared file".to_string()
            } else {
                format!(
                    "{} shared file(s): {}",
                    shared_files.len(),
                    shared_files.join(", ")
                )
            };
            let time = match (seconds_apart, in_window) {
                (Some(s), Some(true)) => {
                    format!("commit {s} s after session end, within the window")
                }
                (Some(s), Some(false)) => {
                    format!("commit {s} s after session end, outside the window")
                }
                _ => "time distance not determinable".to_string(),
            };
            format!("Recomputed, not recorded: {files}; {time}.")
        }
        EvidenceExplanation::Unknown { reason } => {
            format!("Reasons not reconstructable: {reason}.")
        }
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
