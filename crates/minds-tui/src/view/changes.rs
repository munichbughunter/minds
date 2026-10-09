//! Der Changes-Tab: links die Dateien des Commits, rechts der Diff wie in
//! `git diff`, GitHub oder GitLab — Unified mit Hunk-Köpfen, alten und
//! neuen Zeilennummern, `+` auf grünem und `-` auf rotem Grund, oder Split
//! nebeneinander. Daneben, für die Zeile unter dem Cursor: warum sie
//! existiert.
//!
//! Farbe trägt nie allein: `+`/`-` stehen immer da, die Klasse einer Zeile
//! als Glyph am Rand. Unerklärt ist kein Fehler (kein Rot, siehe
//! [`theme::recon`]) — aber kräftig markiert, fett und invertiert, damit es
//! im Grün nicht untergeht.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use minds_git::DiffKind;
use minds_reader::changes::{DiffRow, FileDiff};
use minds_reader::reconcile::{Gap, ReconClass};

use crate::app::App;
use crate::changes::{ChangesState, Focus};
use crate::theme;
use crate::view::{clip, offset};

/// Grund einer hinzugefügten Zeile — gedämpft, damit Text und Glyph lesbar
/// bleiben (wie `delta` oder GitHubs Dark Mode).
pub const ADD_BG: Color = Color::Indexed(22);
/// Grund einer entfernten Zeile.
pub const DEL_BG: Color = Color::Indexed(52);
/// Ab dieser Breite des Diffs gibt es Split.
pub const SPLIT_MIN: u16 = 160;
/// Ab dieser Breite rechts steht „Warum diese Zeile?" neben dem Diff.
const WHY_BESIDE_MIN: u16 = 110;
/// Breite der Spalte „Warum diese Zeile?".
const WHY_WIDTH: u16 = 46;

/// Zeichnet den Tab in `area`; setzt `app.page` auf die Höhe des Diffs und
/// `app.width` auf die Breite (ob Split passt).
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    // Geliehen, nicht kopiert: Der Zustand trägt alle Diff-Zeilen des
    // Commits — ihn je Frame zu klonen kostete bei großen Commits Sekunden.
    let Some(state) = app.changes.take() else {
        return;
    };
    app.width = area.width;
    if let Some(page) = draw_state(frame, app, &state, area) {
        app.page = page;
    }
    app.changes = Some(state);
}

/// Ob Split gezeichnet wird: gewählt und breit genug.
fn split_shown(state: &ChangesState, width: u16) -> bool {
    state.split && width >= SPLIT_MIN
}

fn draw_state(frame: &mut Frame, app: &App, state: &ChangesState, area: Rect) -> Option<usize> {
    let set = match &state.set {
        Ok(set) => set,
        Err(err) => {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled("Changes unavailable", theme::title())),
                    Line::from(Span::styled(err.clone(), theme::dim())),
                ])
                .block(Block::bordered().title(" CHANGES ")),
                area,
            );
            return None;
        }
    };
    if set.files.is_empty() {
        frame.render_widget(
            Paragraph::new("This commit changes no files.")
                .style(theme::dim())
                .block(Block::bordered().title(" CHANGES ")),
            area,
        );
        return None;
    }
    let list_w = (area.width * 28 / 100).clamp(24, 44);
    let [files, right] =
        Layout::horizontal([Constraint::Length(list_w), Constraint::Min(1)]).areas(area);
    draw_files(frame, state, files);

    let [head, rest] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(right);
    let file = state.current()?;
    let split = split_shown(state, area.width);
    draw_file_head(frame, state, file, head, area.width);
    // Im Split braucht der Diff die ganze Breite: Die Begründung steht dann
    // darunter.
    let (diff, why) = if !state.why {
        (rest, None)
    } else if !split && rest.width >= WHY_BESIDE_MIN {
        let [d, w] =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(WHY_WIDTH)]).areas(rest);
        (d, Some(w))
    } else {
        let [d, w] = Layout::vertical([Constraint::Min(3), Constraint::Length(9)]).areas(rest);
        (d, Some(w))
    };
    if let Some(note) = &file.note {
        frame.render_widget(
            Paragraph::new(Span::styled(format!("No line view: {note}."), theme::dim())),
            diff,
        );
    } else if split {
        draw_split(frame, state, file, diff);
    } else {
        draw_unified(frame, state, file, diff);
    }
    if let Some(why) = why {
        draw_why(frame, app, state, file, why);
    }
    Some((diff.height as usize).max(1))
}

fn draw_files(frame: &mut Frame, state: &ChangesState, area: Rect) {
    let short = state
        .commit()
        .map(|c| c.to_string()[..7].to_string())
        .unwrap_or_default();
    let inner_w = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;
    let files = state.files();
    let first = offset(state.file, files.len(), height);
    let lines: Vec<Line> = files
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, file)| {
            let (glyph, _, style) = match file.class {
                Some(class) => theme::recon(class),
                None => ("·", "", theme::dim()),
            };
            let counts = if file.note.is_some() {
                "·".to_string()
            } else {
                format!("+{} −{}", file.added, file.removed)
            };
            let unexplained = file.unexplained();
            let flag = if unexplained > 0 {
                format!(" ◦{unexplained}")
            } else {
                String::new()
            };
            let tail = format!(" {counts}{flag}");
            let name_w = inner_w.saturating_sub(2 + tail.chars().count());
            let mut spans = vec![
                Span::styled(format!("{glyph} "), style),
                Span::raw(format!("{:<name_w$}", clip(&file.path, name_w))),
                Span::styled(format!(" {counts}"), theme::dim()),
            ];
            if unexplained > 0 {
                spans.push(Span::styled(
                    flag,
                    Style::default().add_modifier(Modifier::BOLD),
                ));
            }
            let line = Line::from(spans);
            if i == state.file {
                let style = if state.focus == Focus::Files {
                    theme::cursor()
                } else {
                    Style::default().add_modifier(Modifier::UNDERLINED)
                };
                line.style(style)
            } else {
                line
            }
        })
        .collect();
    let title = format!(
        " FILES · {short} · {}/{} ",
        state.at + 1,
        state.commits.len()
    );
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title).title_style(theme::title())),
        area,
    );
}

fn draw_file_head(
    frame: &mut Frame,
    state: &ChangesState,
    file: &FileDiff,
    area: Rect,
    width: u16,
) {
    let mut counts = [0usize; 4];
    for row in file.rows.iter().filter(|r| r.kind == DiffKind::Added) {
        match row.class {
            Some(ReconClass::Explained) => counts[0] += 1,
            Some(ReconClass::ExplainedFsOnly) => counts[1] += 1,
            Some(ReconClass::ReportedOnly) => counts[2] += 1,
            Some(ReconClass::Unexplained) => counts[3] += 1,
            None => {}
        }
    }
    let mode = match (state.split, split_shown(state, width)) {
        (true, true) => "split".to_string(),
        (true, false) => format!("split needs ≥{SPLIT_MIN} columns — unified"),
        _ => "unified".to_string(),
    };
    let title = Line::from(vec![
        Span::styled(format!(" {}", file.path), theme::title()),
        Span::raw("   "),
        Span::styled(format!("+{}", file.added), Style::default().fg(theme::OK)),
        Span::raw(" "),
        Span::styled(
            format!("−{}", file.removed),
            Style::default().fg(theme::DELETE),
        ),
        Span::styled(format!("   [{mode}]"), theme::dim()),
    ]);
    let classes = Line::from(vec![
        Span::styled(" ● observed ", Style::default().fg(theme::OK)),
        Span::raw(format!("{}  ", counts[0])),
        Span::styled("◍ fs only ", Style::default().fg(theme::OK)),
        Span::raw(format!("{}  ", counts[1])),
        Span::raw("◇ reported "),
        Span::raw(format!("{}  ", counts[2])),
        Span::styled(
            "◦ not observed ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{}", counts[3])),
    ]);
    frame.render_widget(Paragraph::new(vec![title, classes]), area);
}

/// Der Rand einer Zeile: Cursor und Klasse. Unerklärte `+`-Zeilen fett und
/// invertiert — kräftig, aber ohne Warnfarbe.
fn margin(row: &DiffRow, cursor: bool) -> Vec<Span<'static>> {
    let pointer = Span::styled(
        if cursor { "▸" } else { " " },
        Style::default().add_modifier(Modifier::BOLD),
    );
    let glyph = match (row.kind, row.class) {
        (DiffKind::Added, Some(ReconClass::Unexplained)) => Span::styled(
            "◦",
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ),
        (DiffKind::Added, Some(class)) => {
            let (glyph, _, style) = theme::recon(class);
            Span::styled(glyph, style)
        }
        _ => Span::raw(" "),
    };
    vec![pointer, glyph, Span::raw(" ")]
}

fn number(n: Option<u32>) -> String {
    n.map(|n| format!("{n:>5}"))
        .unwrap_or_else(|| "     ".into())
}

/// Text auf `width` aufgefüllt, damit der Grund bis zum Rand reicht.
/// Nach **Anzeigebreite**, nicht nach Zeichen: Ein breites Zeichen (CJK,
/// Vollbreite) belegt zwei Spalten — sonst schöbe eine Zeile aus breiten
/// Zeichen die neue Seite des Splits aus dem Bild.
fn padded(text: &str, width: usize) -> String {
    let columns = |s: &str| Span::raw(s.to_string()).width();
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = columns(c.encode_utf8(&mut [0; 4]));
        if used + w > width {
            // Der letzte Platz zeigt, dass gekürzt wurde.
            while used + 1 > width && out.pop().is_some() {
                used = columns(&out);
            }
            out.push('…');
            used += 1;
            break;
        }
        out.push(c);
        used += w;
    }
    out.push_str(&" ".repeat(width.saturating_sub(used)));
    out
}

fn row_style(kind: DiffKind) -> (char, Style) {
    match kind {
        DiffKind::Added => ('+', Style::default().bg(ADD_BG)),
        DiffKind::Removed => ('-', Style::default().bg(DEL_BG)),
        DiffKind::Context => (' ', Style::default()),
        DiffKind::Hunk => (' ', theme::dim()),
    }
}

fn draw_unified(frame: &mut Frame, state: &ChangesState, file: &FileDiff, area: Rect) {
    let height = area.height as usize;
    let first = offset(state.row, file.rows.len(), height);
    let width = area.width as usize;
    let text_w = width.saturating_sub(3 + 12 + 2);
    let lines: Vec<Line> = file
        .rows
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, row)| {
            let cursor = i == state.row && state.focus == Focus::Diff;
            let mut spans = margin(row, cursor);
            if row.kind == DiffKind::Hunk {
                spans.push(Span::styled(
                    padded(&row.text, width.saturating_sub(3)),
                    Style::default().fg(Color::Cyan),
                ));
            } else {
                let (sign, style) = row_style(row.kind);
                spans.push(Span::styled(
                    format!("{} {} ", number(row.old), number(row.new)),
                    theme::dim(),
                ));
                spans.push(Span::styled(
                    format!("{sign} {}", padded(&row.text, text_w)),
                    style,
                ));
            }
            let line = Line::from(spans);
            if cursor {
                line.style(Style::default().add_modifier(Modifier::BOLD))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_split(frame: &mut Frame, state: &ChangesState, file: &FileDiff, area: Rect) {
    let pairs = ChangesState::split_pairs(&file.rows);
    let at = pairs
        .iter()
        .position(|(l, r)| *l == Some(state.row) || *r == Some(state.row))
        .unwrap_or(0);
    let height = area.height as usize;
    let first = offset(at, pairs.len(), height);
    let half = (area.width as usize).saturating_sub(3) / 2;
    let text_w = half.saturating_sub(5 + 4);
    let side = |index: Option<usize>, new_side: bool| -> Vec<Span<'static>> {
        match index.map(|i| &file.rows[i]) {
            Some(row) => {
                let (sign, style) = row_style(row.kind);
                let n = if new_side { row.new } else { row.old };
                vec![
                    Span::styled(format!("{} ", number(n)), theme::dim()),
                    Span::styled(format!("{sign} {} ", padded(&row.text, text_w)), style),
                ]
            }
            None => vec![Span::raw(format!("{:width$}", "", width = half))],
        }
    };
    let lines: Vec<Line> = pairs
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(p, (left, right))| {
            let cursor = p == at && state.focus == Focus::Diff;
            let marker_row = right
                .or(*left)
                .map(|i| &file.rows[i])
                .expect("a pair has one side");
            let mut spans = margin(marker_row, cursor);
            if marker_row.kind == DiffKind::Hunk {
                spans.push(Span::styled(
                    marker_row.text.clone(),
                    Style::default().fg(Color::Cyan),
                ));
            } else {
                spans.extend(side(*left, false));
                spans.push(Span::styled("│", theme::dim()));
                spans.extend(side(*right, true));
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// „Warum diese Zeile?" für die Cursorzeile.
fn draw_why(frame: &mut Frame, app: &App, state: &ChangesState, file: &FileDiff, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let mut tail: Vec<Line> = Vec::new();
    let label = |key: &str| Span::styled(format!("{key:<9}"), theme::dim());
    match file.rows.get(state.row) {
        None => {}
        Some(row) if row.kind == DiffKind::Hunk => {
            lines.push(Line::from(Span::styled(
                "Hunk header — ] / [ jump between hunks.",
                theme::dim(),
            )));
        }
        Some(row) if row.kind == DiffKind::Context => {
            lines.push(Line::from(Span::styled(
                "Unchanged in this commit — context only.",
                theme::dim(),
            )));
        }
        Some(row) if row.kind == DiffKind::Removed => {
            lines.push(Line::from(Span::styled(
                "Removed by this commit.",
                theme::dim(),
            )));
            lines.push(Line::from(Span::styled(
                "Removals are not attributed line by line.",
                theme::dim(),
            )));
        }
        Some(row) => {
            match row.class {
                Some(ReconClass::Unexplained) => {
                    lines.push(Line::from(Span::styled(
                        "◦ NOT OBSERVED",
                        Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
                    )));
                    lines.push(Line::raw(""));
                    lines.push(Line::raw(
                        "No evidence of the sessions linked to this commit backs this line.",
                    ));
                    // Warum — aus dem, was die Sessions über die Datei wissen;
                    // nie, wer die Zeile schrieb.
                    let (why, shell) = match file.gap {
                        Some(Gap::AfterAgent { later_shell }) => (
                            "The last tool claim on this file (write or delete) does not carry the version of this commit.",
                            later_shell,
                        ),
                        Some(Gap::Unhashed) => (
                            "A tool claim writes this file without a hash — whether it wrote this version is open.",
                            None,
                        ),
                        Some(Gap::Unmapped) => (
                            "A write or delete claim ends in this path but could not be mapped to this checkout — not counted.",
                            None,
                        ),
                        Some(Gap::WitnessOpaque) => (
                            "No tool claim for this file. The witness's latest observation of it is opaque (no hash).",
                            None,
                        ),
                        Some(Gap::WitnessOther) => (
                            "The witness's latest readable observation of this file shows another version.",
                            None,
                        ),
                        Some(Gap::Shell(source)) => (
                            "No tool claim for this file. A shell command mentions it (heuristic, not evidence).",
                            Some(source),
                        ),
                        Some(Gap::Untouched { complete }) => (
                            if complete {
                                "No tool claim or shell mention of this file was found in the linked sessions."
                            } else {
                                "No tool claim or shell mention of this file was found in the linked sessions (search incomplete)."
                            },
                            None,
                        ),
                        None => ("Minds cannot attest this line.", None),
                    };
                    lines.push(Line::from(Span::styled(why, theme::dim())));
                    if let Some(reason) = shell.and_then(|s| app.inspection.line_reason(s)) {
                        let id = reason.session.to_string();
                        lines.push(Line::raw(""));
                        lines.push(Line::from(Span::styled(
                            "Mentioned by a shell command (heuristic, not evidence):",
                            theme::dim(),
                        )));
                        lines.push(Line::from(vec![
                            label("Shell"),
                            Span::raw(format!(
                                "turn {} · {} (call {})",
                                reason.turn + 1,
                                reason.tool,
                                reason.call + 1
                            )),
                        ]));
                        lines.push(Line::from(vec![
                            label("Session"),
                            Span::raw(format!("{}… · {}", &id[..id.len().min(12)], reason.agent)),
                        ]));
                        if let Some(said) = reason.said {
                            tail.push(Line::raw(""));
                            tail.push(Line::from(Span::styled(
                                format!(
                                    "Agent said (turn {}, unverified)",
                                    reason.said_turn.unwrap_or(reason.turn) + 1
                                ),
                                theme::title(),
                            )));
                            tail.push(Line::raw(said));
                        }
                    }
                }
                Some(class) => {
                    let (glyph, word, style) = theme::recon(class);
                    lines.push(Line::from(Span::styled(
                        format!("{glyph} {}", word.to_uppercase()),
                        style.add_modifier(Modifier::BOLD),
                    )));
                }
                None => lines.push(Line::from(Span::styled("· not assessed", theme::dim()))),
            }
            match row.source.and_then(|s| app.inspection.line_reason(s)) {
                Some(reason) => {
                    let id = reason.session.to_string();
                    lines.push(Line::raw(""));
                    lines.push(Line::from(vec![
                        label("Session"),
                        Span::raw(format!("{}…", &id[..id.len().min(12)])),
                    ]));
                    lines.push(Line::from(vec![label("Agent"), Span::raw(reason.agent)]));
                    lines.push(Line::from(vec![
                        label("Step"),
                        Span::raw(format!(
                            "turn {} · {} (call {})",
                            reason.turn + 1,
                            reason.tool,
                            reason.call + 1
                        )),
                    ]));
                    if let Some(at) = reason.at {
                        lines.push(Line::from(vec![label("At"), Span::raw(at)]));
                    }
                    if let Some(anchor) = reason.anchor {
                        let anchor = anchor.to_string();
                        lines.push(Line::from(vec![
                            label("Intent"),
                            Span::raw(format!("anchor {}…", &anchor[..anchor.len().min(14)])),
                        ]));
                    }
                    // Prosa zuletzt (nach Commit und Review): Ein langer Text des
                    // Agenten darf die Metadaten nicht aus dem Feld schieben.
                    if let Some(said) = reason.said {
                        tail.push(Line::raw(""));
                        // Stammt der Text aus einem früheren Turn, steht das dabei.
                        let heading = match reason.said_turn {
                            Some(t) if t != reason.turn => format!("Agent said (turn {})", t + 1),
                            _ => "Agent said".to_string(),
                        };
                        tail.push(Line::from(Span::styled(heading, theme::title())));
                        tail.push(Line::raw(said));
                    }
                    tail.push(Line::raw(""));
                    tail.push(Line::from(Span::styled("Asked for", theme::title())));
                    tail.push(Line::raw(reason.request));
                }
                None if row.class == Some(ReconClass::ExplainedFsOnly) => {
                    lines.push(Line::raw(""));
                    lines.push(Line::from(Span::styled(
                        "Seen in the file system; no tool call claims this line.",
                        theme::dim(),
                    )));
                }
                None if row.class.is_some_and(|c| c != ReconClass::Unexplained) => {
                    lines.push(Line::raw(""));
                    lines.push(Line::from(Span::styled(
                        "Produced by the sessions' writes; the exact step could not be pinned down (repeated line or size budget).",
                        theme::dim(),
                    )));
                }
                None => {}
            }
        }
    }
    if let Ok(set) = &state.set {
        lines.push(Line::raw(""));
        let short = set.commit.to_string()[..7].to_string();
        let mut commit = vec![label("Commit"), Span::raw(short)];
        if let Some(subject) = &set.subject {
            commit.push(Span::raw(format!(" {subject}")));
        }
        lines.push(Line::from(commit));
        if let Some(change) = &set.change {
            lines.push(Line::from(vec![
                label("Change"),
                Span::raw(change.to_string()),
            ]));
        }
        let (glyph, word, style) = theme::verdict(set.review.verdict);
        lines.push(Line::from(vec![
            label("Review"),
            Span::styled(format!("{glyph} {word}"), style),
        ]));
        if let Some(why) = &set.unassessed {
            lines.push(Line::from(Span::styled(
                format!("not assessed: {why}"),
                theme::dim(),
            )));
        }
    }
    lines.extend(tail);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(" WHY THIS LINE? ")
                .title_style(theme::title()),
        ),
        area,
    );
}
