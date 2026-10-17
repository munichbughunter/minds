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
use minds_reader::overview::{CommitRow, OverviewCounts, RefLabel, SealMark, WipSession, WipState};
use minds_reader::reconcile::ReconClass;

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
            theme::AGENT_GLYPH
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

/// Die Refs eines Commits als leise Textspalte (C3.2) — keine Pillen, nie
/// eine Belegfarbe: Ein Branch, der `✓approved` heißt, darf nicht wie ein
/// Review aussehen. Jede Ref trägt ihr Präfix (Icon oder `@`/`#`).
fn refs_text(row: &CommitRow, nerd: bool) -> String {
    let (branch, tag) = if nerd {
        ("\u{e0a0} ", "\u{f02b} ")
    } else {
        ("@", "#")
    };
    row.refs
        .iter()
        .map(|label| match label {
            RefLabel::Head(name) => format!("{branch}{name} {} HEAD", theme::HEAD),
            RefLabel::Branch(name) => format!("{branch}{name}"),
            RefLabel::Tag(name) => format!("{tag}{name}"),
            RefLabel::Detached => format!("{} HEAD (detached)", theme::HEAD),
            RefLabel::More(n) => format!("+{n}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Nur HEAD, wenn die Refs nicht passen.
fn head_text(row: &CommitRow) -> String {
    row.refs
        .iter()
        .find_map(|label| match label {
            RefLabel::Head(_) | RefLabel::Detached => Some(format!("{} HEAD", theme::HEAD)),
            _ => None,
        })
        .unwrap_or_default()
}

/// Die Status-Spalte eines Commits (C3.2): Conformance, Seal-Zustand (Glyph
/// **und** Wort — „sealed" nur, wenn jedes Seal signiert ist), abweichendes
/// Review, `⚑` für eine gebundene Anforderung, `◉N` bei mehreren Sessions.
/// Normalzustände (Review „open") bleiben leise. Nur Vordergrundfarben —
/// die Auswahl ist ein einziger Balken.
fn status(row: &CommitRow, conformance: Option<ConformanceMark>) -> Vec<Span<'static>> {
    let mut parts: Vec<Span<'static>> = Vec::new();
    // Die Conformance gilt jedem Commit, auch einem ohne Session — sobald
    // sie gerechnet ist (gewählter Commit, HEAD). Farbe nie allein.
    if let Some(mark) = conformance {
        parts.push(Span::styled(
            conformance_pill(mark),
            Style::default().fg(conformance_color(mark)),
        ));
    }
    // Was Minds weiß — nur an Commits, die Minds kennt (Session, Change-Id
    // oder ein Review), sonst bliebe jeder fremde Commit „open".
    if !(row.sessions.is_empty() && row.change.is_none() && row.review == Verdict::Open) {
        if let Some(seal) = row.seal {
            let (glyph, word, style) = theme::seal(seal);
            parts.push(Span::styled(format!("{glyph} {word}"), style));
        }
        if row.review != Verdict::Open {
            let (glyph, word, style) = theme::verdict(row.review);
            parts.push(Span::styled(format!("{glyph} {word}"), style));
        }
        if row.intent {
            parts.push(Span::styled("⚑", Style::default().fg(theme::HUMAN)));
        }
        if row.sessions.len() > 1 {
            parts.push(Span::styled(
                format!("◉{}", row.sessions.len()),
                Style::default().fg(theme::AGENT),
            ));
        }
    }
    let mut spans = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(part);
    }
    spans
}

/// Das Badge in der Liste als Text, ohne eigenen Hintergrund (C3.2):
/// Initialen des Agenten mit `◉` (bzw. Roboter-Icon) oder des Autors —
/// nicht nur die Farbe unterscheidet. Vier Spalten breit.
fn badge_text(name: &str, agent: bool, nerd: bool) -> Span<'static> {
    let text = if agent {
        let mark = if nerd {
            "\u{f06a9}"
        } else {
            theme::AGENT_GLYPH
        };
        format!("{mark}{}", initials(name))
    } else {
        initials(name)
    };
    let pad = 4usize.saturating_sub(text.chars().count());
    let color = if agent { theme::AGENT } else { theme::HUMAN };
    Span::styled(
        format!("{text}{}", " ".repeat(pad)),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn width_of(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
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
            Span::styled(theme::COMMIT_GLYPH, style.add_modifier(Modifier::BOLD))
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

/// Die Lanes unter einem Commit — für die zweite Zeile eines
/// Agent-Commits: `│`, wo ein Zweig zur nächsten Zeile weiterläuft.
fn graph_through(next: Option<&CommitRow>, width: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for i in 0..width {
        let cell = next.and_then(|n| n.graph.get(i));
        match cell {
            Some(cell) if matches!(cell.glyph, '●' | '│' | '┼' | '╯' | '╰' | '┤' | '├' | '…') =>
            {
                spans.push(Span::styled("│ ", Style::default().fg(lane(cell.lane))));
            }
            _ => spans.push(Span::raw("  ")),
        }
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
    spans.push(badge_text(&wip.agent, true, nerd));
    spans.push(Span::raw(" "));
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

/// Wie lange her, kurz für die Liste (C3.2): `now`, `5m`, `3h`, `2d`.
fn ago_short(at: i64, now: i64) -> String {
    match now - at {
        s if s < 60 => "now".into(),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

/// So viele Zeichen soll der Betreff mindestens bekommen, bevor die Refs
/// weichen (C3.2).
const SUBJECT_MIN: usize = 30;

/// Der Titel der Liste und, rechts, was `a` jetzt bewirkt (C3.3). Die
/// Zahlen genau einmal, hier (C3.1) — aus den gelesenen Zeilen.
fn list_title(state: &crate::overview::OverviewState) -> (String, &'static str) {
    let hint = if state.agent_only {
        " a: all "
    } else {
        " a: agent sessions only "
    };
    let Ok(overview) = state.data.as_ref() else {
        return (" OVERVIEW ".into(), hint);
    };
    let (_, mark) = theme::pane(state.focus == OverviewFocus::Graph);
    let counts = overview.counts();
    let shown = state.shown();
    let total = overview.wip.len() + overview.rows.len();
    let mut title = if state.agent_only {
        let visible = shown.iter().filter(|&&i| i >= overview.wip.len()).count();
        format!(
            " {}OVERVIEW · agent sessions · {visible} of {} commits",
            mark, counts.commits
        )
    } else {
        format!(
            " {}OVERVIEW · {}",
            mark,
            // „(limit)" nur, wenn eine Grenze griff; eine lückenhafte
            // Historie sagt der Titel weiter hinten selbst.
            counts_title(
                &OverviewCounts {
                    limited: overview.truncated || overview.budget_hit,
                    ..counts
                },
                overview.budget_hit
            )
        )
    };
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
    (title, hint)
}

/// So breit wird das Detail mindestens (C3.4).
const DETAIL_MIN: u16 = 60;

/// So breit wäre die Liste, wenn alles ungekürzt passte: Lead, Lanes,
/// Badge, längster Betreff, Status, Refs, Zeit — mindestens der Titel.
fn natural_width(app: &App) -> u16 {
    let Some(Ok(overview)) = app.overview.as_ref().map(|s| s.data.as_ref()) else {
        return u16::MAX;
    };
    let state = app.overview.as_ref().expect("checked above");
    let now = app.now_secs();
    let rows = &overview.rows;
    let max = |f: &dyn Fn(&CommitRow) -> usize| rows.iter().map(f).max().unwrap_or(0);
    let status_w = max(&|r| width_of(&status(r, state.conformance.get(&r.id).copied().flatten())));
    let refs_w = max(&|r| refs_text(r, app.nerd).chars().count()).min(32);
    let subject_w = max(&|r| r.subject.chars().count());
    let time_w = max(&|r| ago_short(r.time, now).chars().count());
    let wip_w = overview
        .wip
        .iter()
        .map(|w| 40 + w.request.chars().count())
        .max()
        .unwrap_or(0);
    let commits =
        1 + overview.lanes.max(1) * 2 + 5 + subject_w + 1 + status_w + 1 + refs_w + 1 + time_w + 1;
    let (title, hint) = list_title(state);
    let mut title = title.chars().count() + hint.chars().count() + 4;
    if state.agent_only {
        // Der Leerzustand des Filters steht in einer Zeile.
        title = title.max(72);
    }
    u16::try_from(commits.max(wip_w).max(title)).unwrap_or(u16::MAX)
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
    // Das Detail bekommt mindestens 60 Spalten (C3.4); die Liste den Rest,
    // höchstens so breit, wie ihr Inhalt braucht.
    let list_w = area
        .width
        .saturating_sub(DETAIL_MIN)
        .min(natural_width(app).saturating_add(3));
    let [left, right] = if narrow {
        [area, area]
    } else {
        Layout::horizontal([Constraint::Length(list_w), Constraint::Min(DETAIL_MIN)]).areas(area)
    };
    if narrow && state.focus == OverviewFocus::Sessions {
        draw_detail(frame, app, area);
        return;
    }
    if narrow {
        // Ohne gezeichnetes Detail kürzt dieser Draw nichts — kein
        // veralteter `Space prompt` aus einem früheren breiten Draw.
        state.prompt_clipped.set(false);
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
    let height = usize::from(left.height.saturating_sub(2));
    let now = app.now_secs();
    // Feste, rechtsbündige Spalten (C3.2): Status, Refs, Zeit — so breit
    // wie ihr breitester Eintrag; der Betreff bekommt den Rest. Wird es eng,
    // weichen zuerst die Refs (sekundär), nie der Status.
    let commit_rows: Vec<&CommitRow> = shown
        .iter()
        .filter_map(|&i| i.checked_sub(overview.wip.len()))
        .filter_map(|at| overview.rows.get(at))
        .collect();
    let conformance = |row: &CommitRow| state.conformance.get(&row.id).copied().flatten();
    let status_w = commit_rows
        .iter()
        .map(|row| width_of(&status(row, conformance(row))))
        .max()
        .unwrap_or(0);
    let refs_full = commit_rows
        .iter()
        .map(|row| refs_text(row, nerd).chars().count())
        .max()
        .unwrap_or(0)
        .min(32);
    let time_w = commit_rows
        .iter()
        .map(|row| ago_short(row.time, now).chars().count())
        .max()
        .unwrap_or(0);
    // Lead, Lanes, Badge (4) und Luft.
    let lead_w = 1 + width * 2 + 5;
    let fixed = |refs: usize| lead_w + status_w + 1 + refs + usize::from(refs > 0) + time_w + 1;
    // Der Betreff höchstens so breit, wie der längste ihn braucht — dann
    // rücken Status, Refs und Zeit heran, statt weit rechts zu stehen.
    let longest = commit_rows
        .iter()
        .map(|row| row.subject.chars().count())
        .max()
        .unwrap_or(0);
    // Gekürzt kostet die Ellipse ein Zeichen: 30 sichtbare plus `…`.
    let needed = if longest > SUBJECT_MIN {
        SUBJECT_MIN + 1
    } else {
        longest
    };
    // Passen die Refs nicht, bleibt HEAD (`▲ HEAD`) — auffindbar, auch
    // wenn Branch-Namen weichen.
    let head_w = commit_rows
        .iter()
        .map(|row| head_text(row).chars().count())
        .max()
        .unwrap_or(0);
    let (refs_w, head_only) = if row_w.saturating_sub(fixed(refs_full)) >= needed {
        (refs_full, false)
    } else if head_w > 0 && row_w.saturating_sub(fixed(head_w)) >= needed {
        (head_w, true)
    } else {
        (0, false)
    };
    let subject_w = row_w.saturating_sub(fixed(refs_w)).min(longest).max(1);

    // Jeder Eintrag als eine oder zwei Bildschirmzeilen; das Fenster
    // rechnet in Bildschirmzeilen, die Auswahl bleibt ganz sichtbar.
    let mut entries: Vec<(usize, Vec<Line>)> = Vec::new();
    let mut hidden: Vec<&CommitRow> = Vec::new();
    // Mit `a` (C3.3) wird Ausgeblendetes zusammengefasst, nie spurlos
    // entfernt: eine gedimmte Zeile je Lauf, mit den Refs darin.
    let visible: std::collections::HashSet<usize> = shown.iter().copied().collect();
    let order: Vec<usize> = if state.agent_only {
        (0..total).collect()
    } else {
        shown.clone()
    };
    let flush = |hidden: &mut Vec<&CommitRow>, entries: &mut Vec<(usize, Vec<Line>)>| {
        if hidden.is_empty() {
            return;
        }
        let n = hidden.len();
        let mut text = format!("┆ {n} commit{} hidden", if n == 1 { "" } else { "s" });
        let refs: Vec<String> = hidden
            .iter()
            .map(|row| refs_text(row, nerd))
            .filter(|r| !r.is_empty())
            .collect();
        if !refs.is_empty() {
            text.push_str(" · ");
            text.push_str(&refs.join(" · "));
        }
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(crate::view::clip(&text, row_w), theme::dim()),
        ]);
        entries.push((usize::MAX, vec![line]));
        hidden.clear();
    };
    for i in order {
        if !visible.contains(&i) {
            if let Some(row) = i
                .checked_sub(overview.wip.len())
                .and_then(|at| overview.rows.get(at))
            {
                hidden.push(row);
            }
            continue;
        }
        flush(&mut hidden, &mut entries);
        let selected = i == state.cursor;
        let mut lines = match i.checked_sub(overview.wip.len()) {
            None => {
                let mut spans = vec![Span::raw(" ")];
                spans.extend(wip_spans(&overview.wip[i], width, nerd));
                vec![Line::from(spans)]
            }
            Some(at) => {
                let row = &overview.rows[at];
                let mut spans = vec![Span::raw(" ")];
                spans.extend(graph_spans(row, width));
                spans.push(match &row.agent {
                    Some(agent) => badge_text(agent, true, nerd),
                    None => badge_text(&row.author, false, nerd),
                });
                spans.push(Span::raw(" "));
                let subject = crate::view::clip(&row.subject, subject_w);
                let pad = subject_w.saturating_sub(subject.chars().count());
                spans.push(Span::raw(subject));
                spans.push(Span::raw(" ".repeat(pad + 1)));
                let state_spans = status(row, conformance(row));
                let gap = status_w.saturating_sub(width_of(&state_spans));
                spans.push(Span::raw(" ".repeat(gap)));
                spans.extend(state_spans);
                if refs_w > 0 {
                    let refs = if head_only {
                        head_text(row)
                    } else {
                        refs_text(row, nerd)
                    };
                    let refs = crate::view::clip(&refs, refs_w);
                    let gap = refs_w.saturating_sub(refs.chars().count());
                    spans.push(Span::raw(" ".repeat(gap + 1)));
                    spans.push(Span::styled(refs, theme::dim()));
                }
                let time = ago_short(row.time, now);
                let gap = time_w.saturating_sub(time.chars().count());
                spans.push(Span::raw(" ".repeat(gap + 1)));
                spans.push(Span::styled(time, theme::dim()));
                let mut lines = vec![Line::from(spans)];
                // Der Auftrag eines Agent-Commits darunter, gedimmt (C3.2).
                if let (Some(_), Some(request)) = (&row.agent, &row.request) {
                    let mut below = vec![Span::raw(" ")];
                    below.extend(graph_through(overview.rows.get(at + 1), width));
                    below.push(Span::raw("     "));
                    let room = row_w.saturating_sub(lead_w);
                    below.push(Span::styled(
                        crate::view::clip(request, room),
                        theme::dim().add_modifier(Modifier::ITALIC),
                    ));
                    lines.push(Line::from(below));
                }
                lines
            }
        };
        // Der eine Auswahlstil (B7): theme::cursor() über alle Zeilen des
        // Eintrags, ohne Teil-Hintergründe.
        if selected {
            lines = lines
                .into_iter()
                .map(|line| {
                    let used = width_of(&line.spans);
                    let mut line = line;
                    line.spans
                        .push(Span::raw(" ".repeat(row_w.saturating_sub(used) + 1)));
                    line.style(theme::cursor())
                })
                .collect();
        }
        entries.push((i, lines));
    }
    flush(&mut hidden, &mut entries);
    let counts = overview.counts();
    let mut lines: Vec<Line> = Vec::new();
    if total == 0 {
        lines.push(Line::from(Span::styled(
            "No commits yet — the history appears here once there is one.",
            theme::dim(),
        )));
    } else if state.agent_only && counts.with_agent_session == 0 && overview.wip.is_empty() {
        // Ein Leerzustand, der sagt, was los ist und was hilft (C3.3).
        lines.push(Line::from(Span::styled(
            format!(
                " No commit with an agent session in the {} commits read. a shows all.",
                counts.commits
            ),
            theme::dim(),
        )));
    }
    let all: usize = entries.iter().map(|(_, l)| l.len()).sum();
    let (start, len) = entries
        .iter()
        .scan(0usize, |at, (i, l)| {
            let here = *at;
            *at += l.len();
            Some((*i, here, l.len()))
        })
        .find(|(i, _, _)| *i == state.cursor)
        .map(|(_, here, len)| (here, len))
        .unwrap_or((0, 1));
    let first = crate::view::offset(start + len - 1, all, height).min(start);
    lines.extend(
        entries
            .into_iter()
            .flat_map(|(_, l)| l)
            .skip(first)
            .take(height.max(1)),
    );
    let (title, hint) = list_title(state);
    // Der Hinweis rechts ist sekundär: Wird es eng, weicht er vor den Zahlen.
    let roomy = usize::from(left.width) >= title.chars().count() + hint.chars().count() + 4;
    let hint = if roomy { hint } else { "" };
    frame.render_widget(
        Paragraph::new(lines).block({
            let (border, _) = theme::pane(state.focus == OverviewFocus::Graph);
            Block::bordered()
                .title(title)
                .title(Line::from(Span::styled(hint, theme::dim())).right_aligned())
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
        }
        Some(Selected::Commit(row)) => {
            commit_detail(&mut lines, app, row, inner, usize::from(area.height));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((state.detail_scroll, 0))
            .block({
                let (border, mark) = theme::pane(state.focus == OverviewFocus::Sessions);
                Block::bordered()
                    .title(format!(" {mark}DETAIL "))
                    .title_style(theme::title())
                    .border_style(border)
            }),
        area,
    );
}

/// Die Kurzform eines Ankers: acht Hex-Zeichen ohne Präfix — dieselbe in
/// der Übersicht und in „Warum diese Zeile?".
pub(crate) fn anchor_short(id: &minds_core::ContentHash) -> String {
    let text = id.to_string();
    // `b3-<hex>` (oder `<algo>:<hex>`): nur die Hex-Ziffern.
    let hex = text.rsplit(['-', ':']).next().unwrap_or(&text);
    hex.chars().take(8).collect()
}

/// `HH:MM` (UTC) eines Zeitpunkts in Sekunden seit Epoch.
fn clock(secs: i64) -> String {
    let day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3_600, day % 3_600 / 60)
}

/// `HH:MM` aus einem RFC-3339-Text, sonst nichts.
fn clock_of(ts: Option<&str>) -> Option<String> {
    ts.and_then(minds_metrics::epoch_seconds).map(clock)
}

/// Eine Zeile des EVIDENCE-Blocks: Label (16 Spalten), dann Glyph + Wort.
fn axis(label: &str, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut all = vec![Span::styled(format!("{label:<16}"), theme::dim())];
    all.extend(spans);
    Line::from(all)
}

/// Eine eingerückte Folgezeile unter einer Achse (Aktion, Unterzeile).
fn under(text: String, style: Style) -> Line<'static> {
    Line::from(vec![Span::raw(" ".repeat(16)), Span::styled(text, style)])
}

fn mark(glyph: &str, word: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(format!("{glyph} {}", word.into()), style)
}

fn ok() -> Style {
    Style::default().fg(theme::OK)
}

/// Das Detail eines Commits (C3.4): Kopf; mit Agent-Session dann
/// REQUIREMENT, SESSIONS und EVIDENCE — eine Zeile je Achse, nie
/// zusammengefasst. Keine Tastenhinweise: Tasten stehen in der Fußzeile.
fn commit_detail(
    lines: &mut Vec<Line<'static>>,
    app: &App,
    row: &CommitRow,
    inner: usize,
    height: usize,
) {
    let Some(state) = &app.overview else {
        return;
    };
    let id = row.id.to_string();
    lines.push(Line::from(vec![
        Span::styled(
            id.get(..7).unwrap_or(&id).to_string(),
            Style::default()
                .fg(theme::CHANGE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            crate::view::clip(&format!(" · {}", row.subject), inner.saturating_sub(7)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    // Leise: Branches, die ihn enthalten, Autor, Datum (UTC), Change-Id.
    let mut facts: Vec<String> = Vec::new();
    if !row.branches.is_empty() {
        let mut on = format!("on {}", row.branches.join(", "));
        if row.branches_more > 0 {
            on.push_str(&format!(" +{}", row.branches_more));
        }
        facts.push(on);
    }
    facts.push(row.author.clone());
    facts.push(format!("{} UTC", row.date.trim_end_matches('Z')));
    for part in wrapped(&facts.join(" · "), inner, 2) {
        lines.push(Line::from(Span::styled(part, theme::dim())));
    }
    if let Some(change) = &row.change {
        lines.push(Line::from(Span::styled(
            crate::view::clip(&format!("change {change}"), inner),
            theme::dim(),
        )));
    }
    if row.parents.len() > 1 {
        lines.push(Line::from(Span::styled(
            format!("merge of {} parents", row.parents.len()),
            theme::dim(),
        )));
    }
    lines.push(Line::raw(""));
    if !row.has_agent_session() {
        lines.push(Line::from(Span::styled(
            "No agent session linked to this commit.",
            theme::dim(),
        )));
        return;
    }
    let index = app.inspection.index();

    // REQUIREMENT — nur, wenn eine Session einen Anker nennt.
    if !row.anchors.is_empty() {
        lines.push(Line::from(Span::styled("REQUIREMENT", theme::title())));
        for anchor in &row.anchors {
            let mut head = format!("⚑ anchor {}", anchor_short(anchor));
            let detail = state.requirements.get(anchor);
            if let Some(Ok(d)) = detail {
                head.push_str(&format!(" · {}", d.source));
                if let Some(version) = &d.version {
                    head.push_str(&format!(" ({version})"));
                }
            }
            lines.push(Line::from(Span::styled(
                crate::view::clip(&head, inner),
                Style::default().fg(theme::HUMAN),
            )));
            match detail {
                None => lines.push(Line::from(Span::styled(
                    "  … reading the anchor",
                    theme::dim(),
                ))),
                Some(Err(why)) => lines.push(Line::from(Span::styled(
                    crate::view::clip(&format!("  not readable — {why}"), inner),
                    theme::dim(),
                ))),
                Some(Ok(d)) => {
                    if !d.scope.is_empty() {
                        lines.push(Line::from(Span::styled(
                            crate::view::clip(&format!("  scope  {}", d.scope.join(", ")), inner),
                            theme::dim(),
                        )));
                    }
                    // Der Text nur, wenn der Beleg gilt — ein unbelegter
                    // Snapshot könnte gepflanzter Klartext sein.
                    match (&d.proof, &d.snapshot) {
                        (Ok(()), Some(text)) if state.requirement_open => {
                            for line in text {
                                for part in wrapped(line, inner.saturating_sub(2).max(1), 50) {
                                    lines.push(Line::raw(format!("  {part}")));
                                }
                            }
                            if d.snapshot_clipped {
                                lines.push(Line::from(Span::styled(
                                    "  … clipped — minds intent show <id> has it all",
                                    theme::dim(),
                                )));
                            }
                        }
                        (Ok(()), Some(text)) => lines.push(Line::from(Span::styled(
                            format!("  ▸ Space: show text ({} lines)", text.len()),
                            theme::dim(),
                        ))),
                        (Err(why), _) => lines.push(Line::from(Span::styled(
                            crate::view::clip(&format!("  text hidden — {why}"), inner),
                            theme::dim(),
                        ))),
                        (Ok(()), None) => lines.push(Line::from(Span::styled(
                            "  text hidden — snapshot does not match the anchor",
                            theme::dim(),
                        ))),
                    }
                }
            }
        }
        lines.push(Line::raw(""));
    }

    // PROMPT — der Auftrag des Menschen als eigener Abschnitt (C3.6), vor
    // den Sessions, je Session in Session-Reihenfolge. Als Claim
    // gekennzeichnet: aufgezeichnet, nicht belegt. Volltext, umgebrochen,
    // zugeklappt bis PROMPT_LINES Zeilen — Space klappt auf.
    // Bei genau einer Session steht er vor SESSIONS; bei mehreren als
    // Unterabschnitt in jeder Session-Karte — so scrollt er mit dem
    // Fenster über die Sessions, und die gewählte bleibt sichtbar.
    let listed = row.sessions.len().min(MAX_SESSION_LINES);
    let single = row.sessions.len() == 1;
    // Die Dateien des Commits — den Dateiclaims der Session gegenüber (C3.6).
    let commit_files = state
        .lines
        .get(&row.id)
        .and_then(|r| r.as_ref().ok())
        .map(|c| c.files);
    let cards: Vec<Option<minds_reader::model::SessionCard>> = row
        .sessions
        .iter()
        .map(|sid| app.inspection.card(*sid))
        .collect();
    // Der Widerspruch „keine Dateiclaims, aber der Commit ändert Dateien"
    // bekommt eine eigene Zeile unter den Fakten.
    let contradicts = |card: &Option<minds_reader::model::SessionCard>| matches!((card, commit_files), (Some(c), Some(changed)) if c.summary.files == 0 && changed > 0);
    let any_contradiction = usize::from(cards.iter().any(contradicts));
    // EVIDENCE wird zuerst gebaut (es hängt nicht vom Prompt ab) und am
    // Ende gezeigt — so rechnet der Platz für den Prompt mit seiner echten
    // Höhe, nicht mit einer Schätzung.
    let evidence = evidence_lines(state, row, &cards, inner);
    // Die Markerzeilen des Session-Fensters: `… N above` und `… N more`
    // — bei zwei Sessions höchstens eine davon, ab drei beide.
    let markers = match listed {
        0 | 1 => 0,
        2 => 1,
        _ => 2,
    };
    // Zugeklappt nur so viele Zeilen, dass SESSIONS (eine Karte) und
    // EVIDENCE darunter sichtbar bleiben — der Prompt drängt die
    // Trust-Achsen nie aus dem Bild, notfalls bleibt nur sein Kopf. Rahmen,
    // das Bisherige, PROMPT-Kopf und -Hinweis, bei einer Session die
    // Leerzeile nach PROMPT (bei mehreren die Marker), SESSIONS-Kopf, eine
    // Karte, die Leerzeile vor EVIDENCE, EVIDENCE.
    let fixed = 2
        + lines.len()
        + 1
        + 1
        + if single { 1 } else { markers }
        + 1
        + 2
        + any_contradiction
        + 1
        + evidence.len();
    // Aufgeklappt (Prompt oder Anforderung) ist das Detail scrollbar —
    // dann wird nichts gekappt, und der Hinweis verspricht nichts, was
    // Space nicht hält.
    let cap = prompt_cap(
        row.sessions.len(),
        state.prompt_open || state.requirement_open,
        height.saturating_sub(fixed),
    );
    // Ob dieser Draw einen Prompt kürzt — für Space und die Fußzeile.
    state.prompt_clipped.set(false);
    let indent = if single { 0 } else { 4 };
    let prompts: Vec<Vec<String>> = row
        .sessions
        .iter()
        .take(listed)
        .map(|sid| {
            index
                .session(*sid)
                .map(|s| prompt_lines(&s.intent.request, inner.saturating_sub(indent).max(1)))
                .unwrap_or_default()
        })
        .collect();
    let (claim_glyph, _, claim_style) = theme::claim();
    // Der PROMPT-Block: Kopf, Zeilen bis `cap`, Kürzungshinweis.
    let prompt_block = |lines: &mut Vec<Line<'static>>, i: usize, head: String| {
        let pad = " ".repeat(indent);
        lines.push(Line::from(vec![
            Span::raw(pad.clone()),
            Span::styled("PROMPT  ", theme::title()),
            Span::styled(
                crate::view::clip(&head, inner.saturating_sub(indent + 8)),
                claim_style,
            ),
        ]));
        let text = &prompts[i];
        if text.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("{pad}no prompt recorded"),
                theme::dim(),
            )));
        }
        let shown = text.len().min(cap);
        for part in &text[..shown] {
            lines.push(Line::raw(format!("{pad}{part}")));
        }
        if text.len() > shown {
            // Der Hinweis steht, wo gekürzt wird — unabhängig davon, ob die
            // Fußzeile gerade Platz für `Space prompt` hat.
            state.prompt_clipped.set(true);
            let word = if shown == 0 { "lines" } else { "more lines" };
            lines.push(Line::from(Span::styled(
                format!("{pad}… {} {word} · Space shows all", text.len() - shown),
                theme::dim(),
            )));
        }
    };
    if single && index.session(row.sessions[0]).is_some() {
        let short = row.sessions[0].to_string();
        prompt_block(
            lines,
            0,
            format!(
                "{claim_glyph} as recorded by the agent · session {}",
                short.get(..11).unwrap_or(&short)
            ),
        );
        lines.push(Line::raw(""));
    }

    // SESSIONS — je Session Kopf und Fakten; der Auftrag steht unter
    // PROMPT (C3.6), nichts steht doppelt.
    let focused = state.focus == OverviewFocus::Sessions;
    lines.push(Line::from(Span::styled(
        format!("SESSIONS ({})", row.sessions.len()),
        theme::title(),
    )));
    // Zeilen je Session: Kopf und Fakten (und ggf. der Widerspruch) — bei
    // mehreren dazu der PROMPT-Unterabschnitt, so hoch wie der höchste.
    let per = 2
        + any_contradiction
        + if single {
            0
        } else {
            // Kopf, gezeigte Zeilen (oder „no prompt recorded"), Hinweis.
            1 + prompts
                .iter()
                .map(|t| {
                    let shown = t.len().min(cap);
                    let body = if t.is_empty() { 1 } else { shown };
                    body + usize::from(t.len() > shown)
                })
                .max()
                .unwrap_or(1)
        };
    // Platz für die Marker, die Leerzeile und EVIDENCE (echte Höhe) bleibt
    // frei — bei bindendem Cap geht das Fenster genau auf.
    let room = height
        .saturating_sub(2 + lines.len() + markers + 1 + evidence.len())
        .checked_div(per)
        .unwrap_or(0)
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
        let session = index.session(*sid);
        let chosen = focused && i == state.session;
        let mut head = vec![if chosen {
            Span::styled("▸ ", theme::title())
        } else {
            Span::raw("")
        }];
        match session {
            Some(s) => {
                // Agent und Modell, wie der Reader sie erfasst sieht (C3.6):
                // ein Platzhalter aus `minds-capture` wird nie gedruckt.
                let actor = minds_reader::Actor::of(s);
                match &actor.agent {
                    Some(agent) => {
                        badge(&mut head, agent, true, app.nerd);
                        head.push(Span::raw(format!("{} · ", crate::view::clip(agent, 40))));
                    }
                    // Kein Badge für einen Platzhalter — er ist kein Agent.
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
                    // K1: der Grund aus der Lineage folgt in der nächsten Zeile.
                    None => head.push(Span::styled("model not recorded", theme::dim())),
                }
                let short = sid.to_string();
                let mut text = format!(" · {}", short.get(..11).unwrap_or(&short));
                let lineage = s.lineage.as_ref();
                if let Some(start) = clock_of(lineage.and_then(|l| l.started_at.as_deref())) {
                    text.push_str(&format!(" · {start}"));
                    if let Some(end) = clock_of(lineage.and_then(|l| l.ended_at.as_deref())) {
                        text.push_str(&format!("–{end}"));
                    }
                }
                let used = width_of(&head);
                head.push(Span::raw(crate::view::clip(
                    &text,
                    inner.saturating_sub(used),
                )));
            }
            None => head.push(Span::styled("session not readable", theme::dim())),
        }
        let line = Line::from(head);
        lines.push(if chosen {
            line.style(Style::default().add_modifier(Modifier::BOLD))
        } else {
            line
        });
        // K1: Fehlt das Modell und kennt der Record den Grund, steht er in
        // einer eigenen gedimmten Zeile — der Kopf behält Id und Zeit.
        if let Some(reason) = session
            .map(minds_reader::Actor::of)
            .filter(|actor| actor.model.is_none())
            .and_then(|actor| actor.transcript)
            .filter(|status| *status != minds_core::TranscriptStatus::Read)
            .map(minds_reader::summary::transcript_reason)
        {
            lines.push(Line::from(Span::styled(
                crate::view::clip(&format!("    — {reason}"), inner),
                theme::dim(),
            )));
        }
        if let Some(card) = &cards[i] {
            let claimed = card.summary.files;
            let plural = |n: usize| if n == 1 { "" } else { "s" };
            let mut facts = vec![format!("{claimed} file{}", plural(claimed))];
            if let Some(e) = card.evidence_state() {
                facts.push(format!("{} events", e.events));
                facts.push(format!("{} gaps", e.gaps));
            }
            let (glyph, word, style) = theme::provenance(&card.provenance);
            // Der Record-Zustand (Vertrauen) weicht nie — die Fakten davor
            // werden mit `…` gekürzt, nie hart abgeschnitten; die zwei
            // Trennleerzeichen stehen außerhalb des Schnitts.
            let record = format!("{glyph} record {word}");
            let room = inner.saturating_sub(record.chars().count() + 2);
            let mut spans = vec![Span::styled(
                format!(
                    "{}  ",
                    crate::view::clip(&format!("    {}", facts.join(" · ")), room)
                ),
                theme::dim(),
            )];
            spans.push(Span::styled(record, style));
            lines.push(Line::from(spans));
            // Ein Widerspruch wird benannt, nicht nur gezählt (C3.6): Der
            // Commit ändert Dateien, die Session trägt keinen Dateiclaim —
            // eine Lücke, kein Manipulationsbefund (gedimmt, nie rot).
            if let (true, Some(changed)) = (contradicts(&cards[i]), commit_files) {
                lines.push(Line::from(Span::styled(
                    crate::view::clip(
                        &format!(
                            "    0 files claimed · commit changes {changed} file{}",
                            plural(changed)
                        ),
                        inner,
                    ),
                    theme::dim(),
                )));
            }
        }
        // Mehrere Sessions: der Prompt als Unterabschnitt der Karte (C3.6).
        if !single && session.is_some() {
            prompt_block(lines, i, format!("{claim_glyph} as recorded by the agent"));
        }
    }
    let below = row
        .sessions
        .len()
        .saturating_sub(first + room.min(listed.saturating_sub(first)));
    if below > 0 {
        lines.push(Line::from(Span::styled(
            format!("  … {below} more"),
            theme::dim(),
        )));
    }
    lines.push(Line::raw(""));
    lines.extend(evidence);
}

/// EVIDENCE — eine Zeile je Achse, nie zusammengefasst, kein Gesamtwert.
/// Als eigener Block gebaut, damit die Platzrechnung des Prompts seine
/// echte Höhe kennt.
fn evidence_lines(
    state: &crate::overview::OverviewState,
    row: &CommitRow,
    cards: &[Option<minds_reader::model::SessionCard>],
    inner: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let check = state.checks.get(&row.id);
    let mut heading = vec![Span::styled("EVIDENCE", theme::title())];
    if let Some(at) = state.checked_at.get(&row.id) {
        let pad = inner.saturating_sub("EVIDENCE".len() + 15);
        heading.push(Span::raw(" ".repeat(pad)));
        heading.push(Span::styled(
            format!("checked {}Z", clock(*at)),
            theme::dim(),
        ));
    }
    lines.push(Line::from(heading));
    lines.extend(integrity_lines(row, check));
    // Die Dateiclaims der Sessions — `0` erklärt die Lücke mit (C3.6).
    // Nur, wenn jede Karte gelesen ist: Eine ungelesene Session könnte
    // Dateien beanspruchen, über sie wird nichts behauptet.
    let claimed = cards
        .iter()
        .all(Option::is_some)
        .then(|| cards.iter().flatten().map(|c| c.summary.files).sum());
    lines.extend(coverage_lines_with(
        state.lines.get(&row.id),
        claimed,
        inner,
    ));
    lines.extend(interpretation_lines(row, cards));
    lines.extend(requirement_lines(row, state, check));
    let review = if row.review == Verdict::Open {
        let (glyph, _, style) = theme::open();
        mark(glyph, "not reviewed yet", style)
    } else {
        let (glyph, word, style) = theme::verdict(row.review);
        mark(glyph, word, style)
    };
    lines.push(axis("REVIEW", vec![review]));
    // NEXT (C3.7): die oberste Zeile, die nicht ✓ ist, bestimmt die nächste
    // Aktion — eine Zeile, nie mehr; dieselbe steht in Fußzeile 1.
    lines.push(axis(
        "NEXT",
        vec![Span::styled(
            format!("→ {}", next_action(state, row, cards)),
            Style::default().add_modifier(Modifier::BOLD),
        )],
    ));
    lines
}

/// Die nächste Aktion (C3.7): die oberste Zeile in EVIDENCE, die nicht ✓
/// ist, bestimmt sie — INTEGRITY vor COVERAGE vor INTERPRETATION vor
/// REQUIREMENT vor REVIEW. Nur Aktionen, die es gibt; die TUI schreibt
/// nicht, also stehen `minds intent sign` und `minds review` als Text.
/// Alles ✓: nur noch nachvollziehen (`Enter`).
pub(crate) fn next_action(
    state: &crate::overview::OverviewState,
    row: &CommitRow,
    cards: &[Option<minds_reader::model::SessionCard>],
) -> String {
    use crate::verify::Overall;
    use minds_reader::assurance::{IntentSignature, IntentState};
    let check = state.checks.get(&row.id);
    match overall_of(row, check) {
        Some(Overall::Tampered) => {
            return "e: evidence of the session — what was altered".into();
        }
        Some(Overall::Incomplete) => {
            return "e: evidence of the session — where the record ends".into();
        }
        Some(Overall::NotVerifiable) => {
            return "e: evidence of the session — nothing sealed to verify".into();
        }
        Some(Overall::Checking) | Some(Overall::NotChecked) | None => {
            return "v: run verify".into();
        }
        Some(Overall::Verified) | Some(Overall::NoSession) => {}
    }
    let plural = |n: usize| if n == 1 { "line" } else { "lines" };
    match state.lines.get(&row.id) {
        // Zählt noch: die Achse ist nicht ✓ — also auch nichts weiter unten
        // empfehlen, bis die Zahl steht.
        None => return "wait — the lines are still being counted".into(),
        Some(Err(_)) => return "d: open the diff — the lines could not be assessed".into(),
        Some(Ok(c)) if c.all_explained() || c.added == 0 => {}
        Some(Ok(c)) => {
            if c.unexplained > 0 {
                return format!(
                    "d: inspect the {} {} not observed (n jumps on)",
                    c.unexplained,
                    plural(c.unexplained)
                );
            }
            if c.stated > 0 {
                return format!(
                    "d: inspect the {} {} the agent claims without a hash",
                    c.stated,
                    plural(c.stated)
                );
            }
            if c.fs_only > 0 {
                return format!(
                    "d: inspect the {} {} seen in the file system only",
                    c.fs_only,
                    plural(c.fs_only)
                );
            }
            if c.not_assessed > 0 {
                return format!(
                    "d: open the diff — {} {} not assessed",
                    c.not_assessed,
                    plural(c.not_assessed)
                );
            }
            return "d: open the diff".into();
        }
    }
    // Eine ungelesene Session: über ihre Deutung ist nichts gesagt — die
    // Achse zeigt `no session readable`, NEXT führt zur Evidenz.
    if cards.iter().any(Option::is_none) {
        return "e: evidence — a session is not readable".into();
    }
    let uninterpreted: usize = cards.iter().flatten().map(|c| c.uninterpreted_calls).sum();
    if uninterpreted > 0 {
        return format!(
            "e: evidence — {uninterpreted} uninterpreted call{}",
            if uninterpreted == 1 { "" } else { "s" }
        );
    }
    if let Some(anchor) = row.anchors.first() {
        let bound = match check {
            Some(Ok(c)) => c.sessions.iter().find_map(|s| match &s.intent {
                IntentState::Bound { chained, .. } => Some(*chained),
                _ => None,
            }),
            _ => None,
        };
        if bound != Some(true) {
            return "i: Intent tab — the binding is not proven".into();
        }
        match state.requirements.get(anchor) {
            None => {}
            Some(Err(_)) => return "i: Intent tab — the anchor could not be read".into(),
            Some(Ok(d)) => {
                if d.proof.is_err() {
                    return "i: Intent tab — the requirement is not proven".into();
                }
                match d.signature {
                    IntentSignature::Valid(_) => {}
                    IntentSignature::Unsigned => return format!("minds intent sign {anchor}"),
                    IntentSignature::NotChecked => {
                        return "i: Intent tab — why the signature is not checked".into();
                    }
                    IntentSignature::Invalid => {
                        return "i: Intent tab — the signature is invalid".into();
                    }
                }
            }
        }
    }
    if row.review == Verdict::Open {
        let short: String = row.id.to_string().chars().take(7).collect();
        return format!("minds review {short} … — nobody has decided yet");
    }
    "Enter: how it was made — nothing is open".into()
}

/// Die nächste Aktion des gewählten Commits der Übersicht — für Fußzeile 1;
/// `None` ohne Commit mit Session.
pub(crate) fn next_action_of(app: &App) -> Option<String> {
    let state = app.overview.as_ref()?;
    let Some(Selected::Commit(row)) = state.selected() else {
        return None;
    };
    if row.sessions.is_empty() {
        return None;
    }
    let cards: Vec<Option<minds_reader::model::SessionCard>> = row
        .sessions
        .iter()
        .map(|sid| app.inspection.card(*sid))
        .collect();
    Some(next_action(state, row, &cards))
}

/// Das Gesamturteil der INTEGRITY-Zeile aus Seal-Zustand und Prüfung —
/// `None`, wenn die Prüfung selbst scheiterte.
fn overall_of(
    row: &CommitRow,
    check: Option<&Result<crate::CommitVerify, String>>,
) -> Option<crate::verify::Overall> {
    use crate::verify::Overall;
    let tampered_cli = matches!(check, Some(Ok(c)) if c.sessions.iter().any(|s| s.tampered));
    if row.seal == Some(SealMark::Tampered) || tampered_cli {
        return Some(Overall::Tampered);
    }
    match check {
        None => Some(Overall::Checking),
        Some(Err(_)) => None,
        Some(Ok(c)) => Some(match &c.verdict {
            Ok(crate::VerifyVerdict::Verified) => Overall::Verified,
            Ok(crate::VerifyVerdict::Incomplete) => Overall::Incomplete,
            Ok(crate::VerifyVerdict::Tampered) => Overall::Tampered,
            Ok(crate::VerifyVerdict::NotVerifiable) => Overall::NotVerifiable,
            Err(_) => Overall::NotChecked,
        }),
    }
}

/// INTEGRITY: das Urteil von `minds verify`, in seinen Wörtern. Vor dem
/// Urteil `… checking` — nie ✓; Manipulation immer ✗ und rot.
fn integrity_lines(
    row: &CommitRow,
    check: Option<&Result<crate::CommitVerify, String>>,
) -> Vec<Line<'static>> {
    use crate::verify::Overall;
    let overall = overall_of(row, check);
    let mut out = Vec::new();
    match overall {
        Some(Overall::Checking) => {
            out.push(axis("INTEGRITY", vec![mark("…", "checking", theme::dim())]));
        }
        Some(Overall::NotChecked) | None => {
            let why = match check {
                Some(Err(why)) => why.clone(),
                Some(Ok(c)) => c.verdict.clone().err().unwrap_or_default(),
                None => String::new(),
            };
            out.push(axis(
                "INTEGRITY",
                vec![mark(
                    "?",
                    format!("not checked — {why}"),
                    Style::default().fg(theme::REVIEW),
                )],
            ));
        }
        Some(other) => {
            let (text, style) = super::trust::verify_headline(other);
            out.push(axis("INTEGRITY", vec![Span::styled(text, style)]));
        }
    }
    // „sealed" ist nicht „verified" — gesagt, wo es zählt (C3.5).
    if let Some(seal @ (SealMark::Sealed | SealMark::Unsigned | SealMark::Partial)) = row.seal
        && !matches!(overall, Some(Overall::Verified | Overall::Tampered))
    {
        let (glyph, word, style) = theme::seal(seal);
        out.push(Line::from(vec![
            Span::raw(" ".repeat(16)),
            Span::styled(format!("{glyph} record {word}"), style),
            Span::styled(" — sealed is not verified", theme::dim()),
        ]));
    }
    // Mehrere Sessions: je Session eine Unterzeile, nie zu einem ✓ gemischt.
    if let Some(Ok(c)) = check
        && c.sessions.len() > 1
    {
        for s in &c.sessions {
            let (glyph, word, style) = super::trust::verify_session_mark(s.tampered);
            let id = s.session.to_string();
            out.push(Line::from(vec![
                Span::raw(" ".repeat(16)),
                Span::styled(format!("{} ", id.get(..11).unwrap_or(&id)), theme::dim()),
                Span::styled(format!("{glyph} {word}"), style),
            ]));
        }
    }
    out
}

/// COVERAGE: die hinzugefügten Zeilen nach Klasse (C3.4) — die Zahl der
/// unerklärten ist `◦n` des Changes-Tabs. ✓ nur, wenn jede Zeile beobachtet
/// ist; Ausgesagtes, nur im Dateisystem Gesehenes und nicht Bewertetes steht
/// für sich, nie als erklärt.
/// Nur Tests rufen die Form ohne Grund — die Oberfläche nennt ihn immer.
#[cfg(test)]
pub(crate) fn coverage_lines(
    count: Option<&Result<minds_reader::changes::LineClasses, String>>,
) -> Vec<Line<'static>> {
    coverage_lines_with(count, None, 80)
}

/// Wie [`coverage_lines`], mit dem Grund (C3.6): Jede `◦`-Zeile nennt den
/// häufigsten Grund mit Zahl, weitere als `+ N other reasons → d`; die
/// Gründe kommen aus derselben Rechnung wie `◦n` im Changes-Tab.
/// `claimed_files` sind die Dateiclaims der Sessions — `Some(0)` sagt
/// dazu, dass die Session keine Datei aufzeichnete. `width` ist die Breite
/// des Details für den Umbruch.
pub(crate) fn coverage_lines_with(
    count: Option<&Result<minds_reader::changes::LineClasses, String>>,
    claimed_files: Option<usize>,
    width: usize,
) -> Vec<Line<'static>> {
    let c = match count {
        None => {
            return vec![axis(
                "COVERAGE",
                vec![mark("…", "counting lines", theme::dim())],
            )];
        }
        Some(Err(why)) => {
            return vec![axis(
                "COVERAGE",
                vec![mark(
                    "?",
                    format!("not assessed — {why}"),
                    Style::default().fg(theme::REVIEW),
                )],
            )];
        }
        Some(Ok(c)) => c,
    };
    if c.all_explained() {
        return vec![axis(
            "COVERAGE",
            vec![mark(
                "✓",
                format!("all {} added lines observed", c.added),
                ok(),
            )],
        )];
    }
    // Der Kopf nennt den offensten Zustand mit seinem Wort (C9.4): nicht
    // beobachtete Zeilen, sonst berichtete (`◇ 95 of 95 added lines
    // reported, not observed`), sonst nur im Dateisystem gesehene, sonst
    // nicht bewertete; die Zeile `· N added lines` gibt es nicht mehr.
    let (open_glyph, _, open_style) = theme::open();
    let (reported_glyph, reported_word, reported_style) = theme::state(ReconClass::ReportedOnly);
    let (fs_glyph, fs_word, fs_style) = theme::state(ReconClass::ExplainedFsOnly);
    let of = |n: usize, what: &str| format!("{n} of {} added lines {what}", c.added);
    let (head, lead) = if c.unexplained > 0 {
        (
            mark(open_glyph, of(c.unexplained, "unexplained"), open_style),
            None,
        )
    } else if c.stated > 0 {
        (
            mark(
                reported_glyph,
                of(c.stated, &format!("{reported_word}, not observed")),
                reported_style,
            ),
            Some(ReconClass::ReportedOnly),
        )
    } else if c.fs_only > 0 {
        (
            mark(fs_glyph, of(c.fs_only, fs_word), fs_style),
            Some(ReconClass::ExplainedFsOnly),
        )
    } else if c.not_assessed > 0 {
        (
            mark(
                "?",
                of(c.not_assessed, "not assessed"),
                Style::default().fg(theme::REVIEW),
            ),
            None,
        )
    } else if c.skipped_files > 0 {
        // Nur Dateien ohne Zeilenansicht: das steht im Kopf, nicht darunter.
        (mark("·", not_diffed(c.skipped_files), theme::dim()), None)
    } else {
        (mark("·", "no added lines", theme::dim()), None)
    };
    let skipped_in_head = c.added == 0 && c.skipped_files > 0 && c.not_assessed == 0;
    let mut out = vec![axis("COVERAGE", vec![head])];
    let mut parts: Vec<String> = Vec::new();
    if c.explained > 0 && c.explained < c.added {
        parts.push(format!("● {} observed", c.explained));
    }
    if c.stated > 0 && lead != Some(ReconClass::ReportedOnly) {
        parts.push(format!("{reported_glyph} {} {reported_word}", c.stated));
    }
    if c.fs_only > 0 && lead != Some(ReconClass::ExplainedFsOnly) {
        parts.push(format!("{fs_glyph} {} {fs_word}", c.fs_only));
    }
    if c.not_assessed > 0 && (c.unexplained > 0 || c.stated > 0 || c.fs_only > 0) {
        parts.push(format!("? {} not assessed", c.not_assessed));
    }
    if c.skipped_files > 0 && !skipped_in_head {
        parts.push(not_diffed(c.skipped_files));
    }
    if !parts.is_empty() {
        out.push(under(parts.join(" · "), theme::dim()));
    }
    // Der Grund (C3.6) — nie eine nackte Zahl, die raten lässt.
    if c.unexplained > 0
        && let Some(top) = c.reasons.first()
    {
        let mut text = format!("— {}: ", top.reason);
        if top.lines == c.unexplained {
            text.push_str(&format!("all {}", top.lines));
        } else {
            text.push_str(&top.lines.to_string());
        }
        let others = c.reasons.len().saturating_sub(1);
        if others > 0 {
            text.push_str(&format!(
                " · + {others} other reason{} → d",
                if others == 1 { "" } else { "s" }
            ));
        }
        let room = width.saturating_sub(16).max(16);
        let mut parts = wrap_words(&text, room);
        if claimed_files == Some(0) {
            // Die Lücke erklärt sich aus denselben Daten: kein Dateiclaim.
            let note = "(session records 0 files)";
            match parts.last_mut() {
                Some(last) if last.chars().count() + 1 + note.chars().count() <= room => {
                    last.push(' ');
                    last.push_str(note);
                }
                _ => parts.push(note.into()),
            }
        }
        for part in parts {
            out.push(under(part, theme::dim()));
        }
    }
    if c.unexplained > 0 {
        out.push(under("→ d, then n".into(), theme::dim()));
    }
    out
}

/// `N file(s) not diffed` — Dateien ohne Zeilenansicht (binär, zu groß, …).
fn not_diffed(files: usize) -> String {
    format!(
        "{files} file{} not diffed",
        if files == 1 { "" } else { "s" }
    )
}

/// INTERPRETATION: gedeutete Tool-Aufrufe — je Session, wenn sie sich
/// unterscheiden.
fn interpretation_lines(
    row: &CommitRow,
    cards: &[Option<minds_reader::model::SessionCard>],
) -> Vec<Line<'static>> {
    let counts: Vec<(String, usize)> = row
        .sessions
        .iter()
        .zip(cards)
        .filter_map(|(sid, card)| {
            card.as_ref()
                .map(|c| (sid.to_string(), c.uninterpreted_calls))
        })
        .collect();
    let slot = |n: usize| {
        let axis = if n == 0 {
            minds_reader::model::InterpretationAxis::Interpreted
        } else {
            minds_reader::model::InterpretationAxis::Uninterpreted(n)
        };
        let slot = super::trust::interpretation(axis);
        mark(slot.glyph, slot.word, slot.style)
    };
    let same = counts.windows(2).all(|w| w[0].1 == w[1].1);
    match counts.first() {
        None => vec![axis(
            "INTERPRETATION",
            vec![mark("·", "no session readable", theme::dim())],
        )],
        Some((_, n)) if same => vec![axis("INTERPRETATION", vec![slot(*n)])],
        Some(_) => {
            let mut out = vec![axis(
                "INTERPRETATION",
                vec![Span::styled("per session:", theme::dim())],
            )];
            for (id, n) in counts {
                out.push(Line::from(vec![
                    Span::raw(" ".repeat(16)),
                    Span::styled(format!("{} ", id.get(..11).unwrap_or(&id)), theme::dim()),
                    slot(n),
                ]));
            }
            out
        }
    }
}

/// REQUIREMENT: gebunden · belegt · signiert — je für sich, nie verschmolzen.
fn requirement_lines(
    row: &CommitRow,
    state: &crate::overview::OverviewState,
    check: Option<&Result<crate::CommitVerify, String>>,
) -> Vec<Line<'static>> {
    use minds_reader::assurance::{IntentSignature, IntentState};
    let Some(anchor) = row.anchors.first() else {
        return vec![axis(
            "REQUIREMENT",
            vec![mark("·", "no requirement named", theme::dim())],
        )];
    };
    let (open_glyph, _, open_style) = theme::open();
    let dot = || Span::styled(" · ", theme::dim());
    // Gebunden: was `minds verify` sah — sonst nur, was die Session sagt.
    let bound = match check {
        Some(Ok(c)) => c.sessions.iter().find_map(|s| match &s.intent {
            IntentState::Bound { chained, .. } => Some(*chained),
            _ => None,
        }),
        _ => None,
    };
    let bound = match bound {
        Some(true) => mark("✓", "bound", ok()),
        Some(false) => mark(open_glyph, "bound by a local file only", open_style),
        None => {
            let (glyph, _, style) = theme::claim();
            mark(glyph, "named by the session", style)
        }
    };
    let mut spans = vec![bound, dot()];
    let mut next = None;
    match state.requirements.get(anchor) {
        None => spans.push(mark("…", "checking", theme::dim())),
        Some(Err(_)) => spans.push(mark(
            "?",
            "not readable",
            Style::default().fg(theme::REVIEW),
        )),
        Some(Ok(d)) => {
            spans.push(match &d.proof {
                Ok(()) => mark("✓", "proven", ok()),
                Err(_) => mark(open_glyph, "not proven", open_style),
            });
            spans.push(dot());
            spans.push(match &d.signature {
                IntentSignature::Valid(_) => mark("✓", "signed", ok()),
                IntentSignature::Unsigned => {
                    next = Some("→ minds intent sign <id> (Intent tab, i)".to_string());
                    mark(open_glyph, "not signed", open_style)
                }
                IntentSignature::NotChecked => mark(
                    "?",
                    "signature not checked",
                    Style::default().fg(theme::REVIEW),
                ),
                IntentSignature::Invalid => {
                    let (glyph, _, style) = theme::tampered();
                    mark(glyph, "signature invalid", style)
                }
            });
        }
    }
    let mut out = vec![axis("REQUIREMENT", spans)];
    if row.anchors.len() > 1 {
        out.push(under(
            format!(
                "+{} more anchors — see REQUIREMENT above",
                row.anchors.len() - 1
            ),
            theme::dim(),
        ));
    }
    if let Some(next) = next {
        out.push(under(next, theme::dim()));
    }
    out
}

/// So viele Zeilen zeigt PROMPT zugeklappt (C3.6) …
pub(crate) const PROMPT_LINES: usize = 12;
/// … und bei mehr als zwei Sessions je Session.
pub(crate) const PROMPT_LINES_MANY: usize = 4;
/// Wie viele Prompt-Zeilen das Detail zeigt: aufgeklappt alle; zugeklappt
/// die Vorgabe nach Session-Zahl, gekappt auf den Platz `avail`, der
/// SESSIONS und EVIDENCE übrig lässt — notfalls null (dann steht nur der
/// PROMPT-Kopf mit dem Hinweis). Der Vertrauenszustand hat Vorrang vor
/// dem Prompt, nie umgekehrt.
pub(crate) fn prompt_cap(sessions: usize, open: bool, avail: usize) -> usize {
    if open {
        return usize::MAX;
    }
    let base = if sessions <= 2 {
        PROMPT_LINES
    } else {
        PROMPT_LINES_MANY
    };
    base.min(avail)
}

/// Der Auftrag als Anzeigezeilen (C3.6): je Quellzeile entschärft und an
/// Wortgrenzen auf `width` umgebrochen; Leerzeilen innen bleiben, Leerraum
/// am Anfang und Ende des Textes fällt weg. Leer, wenn nichts erfasst ist.
/// Leerzeichenfolgen im Zeileninneren fallen zu einem zusammen — nur die
/// Einrückung einer Zeile bleibt; Spaltenausrichtung in Code-Blöcken
/// überlebt den Umbruch nicht (bewusst: die Anzeige ist kein Editor).
fn prompt_lines(request: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in request.lines() {
        let line = minds_reader::sanitize(raw.trim_end());
        if line.trim().is_empty() {
            if !out.is_empty() {
                out.push(String::new());
            }
            continue;
        }
        out.extend(wrap_words(&line, width));
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// Eine Zeile an Wortgrenzen auf höchstens `width` Zeichen umgebrochen;
/// die Einrückung der Zeile bleibt am ersten Stück, ein Wort länger als
/// die Breite wird hart geteilt. Nie leer.
fn wrap_words(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let indent: String = line
        .chars()
        .take_while(|c| *c == ' ')
        .take(width / 2)
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut current = indent.clone();
    let mut used = indent.chars().count();
    let mut fresh = true;
    for word in line.split_whitespace() {
        let w = word.chars().count();
        if !fresh && used + 1 + w > width {
            out.push(std::mem::take(&mut current));
            used = 0;
            fresh = true;
        }
        if w > width.saturating_sub(used) && w > width / 2 {
            // Ein Wort länger als die Restzeile: hart teilen.
            let chars: Vec<char> = word.chars().collect();
            for chunk in chars.chunks(width) {
                if !fresh {
                    out.push(std::mem::take(&mut current));
                }
                current = chunk.iter().collect();
                used = chunk.len();
                fresh = false;
            }
            continue;
        }
        if !fresh {
            current.push(' ');
            used += 1;
        }
        current.push_str(word);
        used += w;
        fresh = false;
    }
    if !fresh || out.is_empty() {
        out.push(current);
    }
    out
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
    use super::{prompt_lines, wrap_words, wrapped};

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

    /// C3.6: Der Prompt bricht an Wortgrenzen, kein Stück ist breiter als
    /// die Zeile, kein Wort geht verloren, Einrückung bleibt, Leerzeilen
    /// innen bleiben, außen fallen sie weg.
    #[test]
    fn the_prompt_wraps_at_words_and_keeps_every_word() {
        let parts = wrap_words("one two three four five", 9);
        assert_eq!(parts, ["one two", "three", "four five"]);
        assert!(parts.iter().all(|p| p.chars().count() <= 9));
        assert_eq!(wrap_words("  indented words", 12), ["  indented", "words"]);
        // Zeichen, nicht Bytes: Umlaute zählen einfach.
        assert_eq!(wrap_words("äöü ßß ñ", 4), ["äöü", "ßß ñ"]);
    }

    /// Review: der Cap zugeklappt — Vorgabe nach Session-Zahl, auf den
    /// Platz gekappt, notfalls null (Vertrauen vor Prompt); aufgeklappt
    /// alles.
    #[test]
    fn the_prompt_cap_respects_the_room() {
        use super::{PROMPT_LINES, PROMPT_LINES_MANY, prompt_cap};
        assert_eq!(prompt_cap(1, false, 0), 0);
        assert_eq!(prompt_cap(1, false, 5), 5);
        assert_eq!(prompt_cap(1, false, 100), PROMPT_LINES);
        assert_eq!(prompt_cap(2, false, 100), PROMPT_LINES);
        assert_eq!(prompt_cap(3, false, 100), PROMPT_LINES_MANY);
        assert_eq!(prompt_cap(3, true, 0), usize::MAX);
        assert_eq!(wrap_words(&"x".repeat(25), 10).len(), 3);
        assert_eq!(wrap_words("", 10), [""]);
        let lines = prompt_lines("\n\nfirst\n\n\nsecond line\n\n", 80);
        assert_eq!(lines, ["first", "", "", "second line"]);
        assert!(prompt_lines("   \n", 80).is_empty());
        // Entschärft: kein ANSI im Detail.
        assert!(!prompt_lines("a\u{1b}[31mb", 80)[0].contains('\u{1b}'));
    }
}
