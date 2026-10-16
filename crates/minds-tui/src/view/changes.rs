//! Der Changes-Tab: die Dateien des Commits, der Diff wie in `git diff`,
//! GitHub oder GitLab — Unified mit Hunk-Köpfen, alten und neuen
//! Zeilennummern, `+` grün und `-` rot auf Marker und Nummer, oder Split
//! nebeneinander. Daneben, für die Zeile unter dem Cursor: warum sie
//! existiert.
//!
//! Layout nach Breite (C9.2): ab 160 Spalten FILES als Bereich (28), Diff
//! und WHY (56) daneben; von 120 bis 159 FILES als Zeile über dem Diff,
//! WHY 48; darunter FILES-Zeile und Diff, WHY als Ebene per `Enter`.
//!
//! Farbe trägt nie allein: `+`/`-` stehen immer da, die Klasse einer Zeile
//! als Glyph am Rand. Kein Zeilen-Hintergrund — der ist dem Cursor
//! vorbehalten. Unerklärt ist kein Fehler (kein Rot, siehe
//! [`theme::recon`]); der Rand-Glyph ist gedimmt, solange er dem Zustand
//! der Datei entspricht, und steht fett in voller Farbe, wo er abweicht.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use minds_git::DiffKind;
use minds_reader::changes::{DiffRow, FileDiff};
use minds_reader::reconcile::{Gap, ReconClass};

use crate::app::App;
use crate::changes::{ChangesState, Focus};
use crate::theme;
use crate::view::{clip, offset};

/// Ab dieser Breite des Diffs gibt es Split.
pub const SPLIT_MIN: u16 = 160;
/// Ab dieser Terminalbreite ist FILES ein Bereich links (C9.2); darunter
/// eine Zeile über dem Diff.
pub const FILES_PANE_MIN: u16 = 160;
/// Breite des FILES-Bereichs (mit Rahmen).
const FILES_WIDTH: u16 = 28;
/// Unter dieser Terminalbreite ist „Warum diese Zeile?" keine Spalte,
/// sondern eine Ebene über dem Diff (`Enter` auf der Zeile, `Esc` zurück).
pub const WHY_LAYER_MAX: u16 = 120;
/// Breite der Spalte „Warum diese Zeile?" (mit Rahmen) ab 160 Spalten …
const WHY_WIDTH_WIDE: u16 = 56;
/// … und von 120 bis 159 Spalten.
const WHY_WIDTH: u16 = 48;
/// Die Ebene „Warum diese Zeile?" unter 120 Spalten: höchstens so breit und
/// so hoch, sonst das Terminal minus Rand.
const WHY_LAYER_SIZE: (u16, u16) = (60, 16);

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
    // Breakpoints (C9.2): ab 160 Spalten FILES als Bereich, sonst als Zeile.
    let right = if area.width >= FILES_PANE_MIN {
        let [files, right] =
            Layout::horizontal([Constraint::Length(FILES_WIDTH), Constraint::Min(1)]).areas(area);
        draw_files(frame, state, files);
        right
    } else {
        let [line, right] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        draw_files_line(frame, state, line);
        right
    };

    let file = state.current()?;
    let split = split_shown(state, area.width);
    // Der Diff trägt den Rahmen seines Fokus (B7): hell, wenn der Cursor
    // hier liegt, sonst gedimmt — die Marke im Titel sagt es auch ohne Farbe.
    let (border, _) = theme::pane(state.focus == Focus::Diff);
    let block = Block::bordered()
        .title(file_title(state, file, area.width))
        .border_style(border);
    let inner = block.inner(right);
    frame.render_widget(block, right);
    let [head, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
    draw_file_head(frame, file, head);
    // Die Spalte „Warum diese Zeile?": 56 ab 160 Spalten, 48 darunter, nie
    // weniger — der Diff bekommt den Rest. Unter 120 Spalten ist sie keine
    // Spalte, sondern eine Ebene. Im Split braucht der Diff die ganze
    // Breite: Die Begründung steht dann darunter.
    let why_w = if area.width >= FILES_PANE_MIN {
        WHY_WIDTH_WIDE
    } else {
        WHY_WIDTH
    };
    let (diff, why) = if !state.why || area.width < WHY_LAYER_MAX {
        (rest, None)
    } else if !split {
        let [d, w] =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(why_w)]).areas(rest);
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
    if area.width < WHY_LAYER_MAX && state.why_layer {
        // Die Ebene liegt mittig über dem Diff; `Esc` schließt sie.
        let (w, h) = WHY_LAYER_SIZE;
        let w = w.min(area.width.saturating_sub(2));
        let h = h.min(area.height.saturating_sub(2));
        let layer = Rect {
            x: area.x + (area.width.saturating_sub(w)) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        };
        frame.render_widget(Clear, layer);
        draw_why(frame, app, state, file, layer);
    }
    Some((diff.height as usize).max(1))
}

/// Der FILES-Titel: Zahl der Dateien, hinzugefügte und entfernte Zeilen des
/// Commits — aus den Zählern des Readers, denselben wie in der Übersicht.
fn files_title(state: &ChangesState) -> String {
    let (files, added, removed) = match &state.set {
        Ok(set) => (set.files.len(), set.added(), set.removed()),
        Err(_) => (0, 0, 0),
    };
    let (_, mark) = theme::pane(state.focus == Focus::Files);
    let mut title = format!(
        "{mark}FILES · {files} file{} · +{added} −{removed}",
        if files == 1 { "" } else { "s" }
    );
    if !state.filter.is_empty() {
        title.push_str(&format!(
            " · filter: {} ({} of {})",
            state.filter,
            state.shown().len(),
            files
        ));
    }
    title
}

/// Die Dateien als eine Zeile über dem Diff (unter 160 Spalten): Titel,
/// dann je Datei Glyph, Pfad, `+n` und `◦n`, durch ` · ` getrennt; die
/// gewählte trägt den Cursor, wenn die Liste den Fokus hat.
fn draw_files_line(frame: &mut Frame, state: &ChangesState, area: Rect) {
    let files = state.files();
    let mut spans = vec![
        Span::styled(format!(" {}", files_title(state)), theme::title()),
        Span::styled("   ", theme::dim()),
    ];
    for (n, &i) in state.shown().iter().enumerate() {
        let file = &files[i];
        if n > 0 {
            spans.push(Span::styled(" · ", theme::dim()));
        }
        let (glyph, _, style) = match file.class {
            Some(class) => theme::recon(class),
            None => ("·", "", theme::dim()),
        };
        let unexplained = file.unexplained();
        let mut item = vec![
            Span::styled(format!("{glyph} "), style),
            Span::raw(file.path.clone()),
            Span::styled(format!(" +{}", file.added), theme::dim()),
        ];
        if unexplained > 0 {
            item.push(Span::styled(
                format!(" ◦{unexplained}"),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
        if i == state.file {
            let style = if state.focus == Focus::Files {
                theme::cursor()
            } else {
                Style::default().add_modifier(Modifier::UNDERLINED)
            };
            item = item.into_iter().map(|s| s.patch_style(style)).collect();
        }
        spans.extend(item);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_files(frame: &mut Frame, state: &ChangesState, area: Rect) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;
    let files = state.files();
    // Nur die Treffer des Filters aus `/` (B4).
    let shown = state.shown();
    let at = shown.iter().position(|&i| i == state.file).unwrap_or(0);
    let first = offset(at, shown.len(), height);
    let lines: Vec<Line> = shown
        .iter()
        .skip(first)
        .take(height)
        .map(|&i| (i, &files[i]))
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
    let title = format!(" {} ", files_title(state));
    frame.render_widget(
        Paragraph::new(lines).block({
            let (border, _) = theme::pane(state.focus == Focus::Files);
            Block::bordered()
                .title(title)
                .title_style(theme::title())
                .border_style(border)
        }),
        area,
    );
}

/// Der Titel des Diff-Bereichs: Fokus-Marke, Datei, Zahlen, Modus.
fn file_title(state: &ChangesState, file: &FileDiff, width: u16) -> Line<'static> {
    let mode = match (state.split, split_shown(state, width)) {
        (true, true) => "split".to_string(),
        (true, false) => format!("split needs ≥{SPLIT_MIN} columns — unified"),
        _ => "unified".to_string(),
    };
    Line::from(vec![
        Span::styled(
            format!(
                " {}{}",
                theme::pane(state.focus == Focus::Diff).1,
                file.path
            ),
            theme::title(),
        ),
        Span::raw("   "),
        Span::styled(format!("+{}", file.added), Style::default().fg(theme::OK)),
        Span::raw(" "),
        Span::styled(
            format!("−{}", file.removed),
            Style::default().fg(theme::DELETE),
        ),
        Span::styled(format!("   [{mode}] "), theme::dim()),
    ])
}

/// Die Zählung der `+`-Zeilen einer Datei nach Zustand: beobachtet, nur im
/// Dateisystem, berichtet, nicht beobachtet.
fn class_counts(file: &FileDiff) -> [usize; 4] {
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
    counts
}

/// Die Legende über dem Diff: je Zustand Glyph, Wort und Zahl.
fn draw_file_head(frame: &mut Frame, file: &FileDiff, area: Rect) {
    let counts = class_counts(file);
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
    frame.render_widget(Paragraph::new(vec![classes]), area);
}

/// Die Zusammenfassung einer Datei für den Hunk-Kopf (C9.1): Pfad und Zahl
/// der hinzugefügten Zeilen, dann je Zustand eine Zeile mit der Zahl —
/// `reported` mit dem Werkzeug und Turn, wenn alle Claims aus einem Aufruf
/// stammen. Zeilenweise, damit im schmalen Panel nichts mitten im Satz
/// umbricht.
fn file_summary(app: &App, file: &FileDiff) -> Vec<String> {
    let [observed, fs_only, reported, open] = class_counts(file);
    let mut calls: std::collections::BTreeSet<(usize, usize, String)> = Default::default();
    for row in file.rows.iter().filter(|r| r.kind == DiffKind::Added) {
        if row.class == Some(ReconClass::ReportedOnly)
            && let Some(reason) = row.source.and_then(|s| app.inspection.line_reason(s))
        {
            calls.insert((reason.turn, reason.call, reason.tool));
        }
    }
    let mut parts = vec![format!(
        "{} · {} added line{}",
        file.path,
        file.added,
        if file.added == 1 { "" } else { "s" }
    )];
    if reported > 0 {
        let by = match calls.len() {
            0 => String::new(),
            1 => {
                let (turn, _, tool) = calls.iter().next().expect("one call");
                format!(" by one {tool} (turn {})", turn + 1)
            }
            n => {
                let tools: std::collections::BTreeSet<&str> =
                    calls.iter().map(|(_, _, t)| t.as_str()).collect();
                match tools.len() {
                    1 => format!(" by {n} {} calls", tools.iter().next().expect("one tool")),
                    _ => format!(" by {n} tool calls"),
                }
            }
        };
        parts.push(format!("{reported} reported{by}"));
    }
    if fs_only > 0 {
        parts.push(format!("{fs_only} fs only"));
    }
    parts.push(format!("{observed} observed"));
    if open > 0 {
        parts.push(format!("{open} not observed"));
    }
    parts
}

/// Der Rand einer Zeile: Cursor und Klasse. Der Glyph ist gedimmt, solange
/// er dem Zustand der Datei (`file_class`) entspricht; eine Abweichung steht
/// fett in voller Farbe — nie invertiert (das ist der Cursor), nie in einer
/// Warnfarbe.
fn margin(row: &DiffRow, cursor: bool, file_class: Option<ReconClass>) -> Vec<Span<'static>> {
    let pointer = Span::styled(
        if cursor { "▸" } else { " " },
        Style::default().add_modifier(Modifier::BOLD),
    );
    let glyph = match (row.kind, row.class) {
        (DiffKind::Added, Some(class)) if Some(class) == file_class => {
            Span::styled(theme::recon(class).0, theme::dim())
        }
        (DiffKind::Added, Some(ReconClass::Unexplained)) => Span::styled(
            theme::recon(ReconClass::Unexplained).0,
            Style::default().add_modifier(Modifier::BOLD),
        ),
        (DiffKind::Added, Some(class)) => {
            let (glyph, _, style) = theme::recon(class);
            Span::styled(glyph, style.add_modifier(Modifier::BOLD))
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

/// Marker und Farbe einer Zeilenart — nur als Vordergrund auf Marker und
/// Zeilennummer; der Text bleibt ungefärbt, der Hintergrund dem Cursor
/// vorbehalten (C9.2).
fn row_style(kind: DiffKind) -> (char, Style) {
    match kind {
        DiffKind::Added => ('+', Style::default().fg(theme::OK)),
        DiffKind::Removed => ('-', Style::default().fg(theme::DELETE)),
        DiffKind::Context => (' ', Style::default()),
        DiffKind::Hunk => (' ', theme::dim()),
    }
}

/// Die Zeilennummer in der Farbe ihrer Art, wenn die Art sie betrifft (alt
/// bei `-`, neu bei `+`), sonst gedimmt.
fn number_span(n: Option<u32>, kind: DiffKind, new_side: bool) -> Span<'static> {
    let coloured = matches!(
        (kind, new_side),
        (DiffKind::Added, true) | (DiffKind::Removed, false)
    );
    let style = if coloured {
        row_style(kind).1
    } else {
        theme::dim()
    };
    Span::styled(number(n), style)
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
            let mut spans = margin(row, cursor, file.class);
            if row.kind == DiffKind::Hunk {
                spans.push(Span::styled(
                    padded(&row.text, width.saturating_sub(3)),
                    Style::default().fg(Color::Cyan),
                ));
            } else {
                let (sign, style) = row_style(row.kind);
                spans.push(number_span(row.old, row.kind, false));
                spans.push(Span::raw(" "));
                spans.push(number_span(row.new, row.kind, true));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(sign.to_string(), style));
                spans.push(Span::raw(format!(" {}", padded(&row.text, text_w))));
            }
            let line = Line::from(spans);
            if cursor {
                // Der eine Auswahlstil (B7) über die ganze Zeile — vom Rand
                // bis zum aufgefüllten Zeilenende.
                line.style(theme::cursor())
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
                    number_span(n, row.kind, new_side),
                    Span::raw(" "),
                    Span::styled(sign.to_string(), style),
                    Span::raw(format!(" {} ", padded(&row.text, text_w))),
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
            let mut spans = margin(marker_row, cursor, file.class);
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
            let line = Line::from(spans);
            if cursor {
                line.style(theme::cursor())
            } else {
                line
            }
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
            // Der Hunk-Kopf zeigt die Datei im Ganzen — nie nur einen
            // Tastenhinweis (C9.1).
            lines.extend(
                file_summary(app, file)
                    .into_iter()
                    .map(|part| Line::from(Span::styled(part, theme::dim()))),
            );
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
                        let how = match anchor.record {
                            minds_reader::intent::AnchorRecord::WitnessEvent => "witness event",
                            minds_reader::intent::AnchorRecord::LocalFile => "local file",
                        };
                        let id = anchor.id.to_string();
                        lines.push(Line::from(vec![
                            label("Intent"),
                            Span::raw(format!("anchor {}… ({how})", &id[..id.len().min(14)])),
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
