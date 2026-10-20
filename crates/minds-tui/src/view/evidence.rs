//! Der Evidence-Report einer Session (C12.1): **ein Satz für Menschen** —
//! je Achse ein Satzteil, kein Gesamturteil —, darunter das Urteil von
//! `minds verify` in dessen Wörtern (oder „sealed, not yet verified"), dann
//! genau die **drei Achsen** Integrität · Coverage · Deutung, die Grenze der
//! Stufe als Satz und die Technik für Auditoren hinter Space.
//!
//! Die TUI rechnet hier nichts nach: Alles kommt fertig aus
//! [`EvidenceReport`] ([`minds_reader`]) — derselben Rechnung, die auch
//! `minds verify` und das Audit-Bundle tragen. Die Stufe (INTEGRITY, LIMITS)
//! kommt aus [`App::session_rating`]: dem Urteil von `minds verify`, sobald
//! es da ist — dasselbe Wort wie in der Übersicht —, davor der Stufe, die
//! das Repository allein trägt. Und sie behauptet nie mehr
//! als das Modell: „vollständig" heißt vollständig **innerhalb** der
//! Beobachtungsgrenze; was außerhalb liegt, ist `— nicht erfasst`, keine
//! Lücke — und eine Lücke ist kein Beweis, dass etwas geschah.
//!
//! Der Abgleich der Commits, die die Session tragen (EA-03), steht als
//! zweite Zeile unter COVERAGE — mit den Zustandswörtern aus C9.4
//! (`reported`, `observed`, `not observed`), nie als „explained". Die Zahlen
//! kommen aus derselben Reader-Rechnung wie Overview und Changes (C3.1).
//! Unerklärt ist nie ein Fehler — es steht im neutralen Stil „not observed
//! in the session", nie in Rot.

use minds_core::SessionId;
use minds_git::CommitId;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use minds_reader::artifact::{ArtifactState, CommitArtifact, NOT_OBSERVED, provenance_note};
use minds_reader::assurance::Assurance;
use minds_reader::changes::LineClasses;
use minds_reader::model::{
    CoverageAxis, EpochLink, EpochReport, EvidenceReport, EvidenceVerdict, InterpretationAxis,
    LEGACY_SENTENCE, Provenance, TrustAxes,
};
use minds_reader::reconcile::ReconClass;

use crate::app::App;
use crate::theme;
use crate::view::trust;

/// Die Zeilenklassen je abgeglichenem Commit — aus derselben Rechnung wie
/// `◦n` im Changes-Tab und COVERAGE in der Übersicht.
pub type CommitLines = (CommitId, Result<LineClasses, String>);

/// Die Sektionen, in Anzeige-Reihenfolge — muss zu
/// [`crate::app::EVIDENCE_SECTIONS`] passen (testfixiert).
const SECTIONS: [&str; crate::app::EVIDENCE_SECTIONS] = [
    "INTEGRITY",
    "COVERAGE",
    "INTERPRETATION",
    "LIMITS",
    "TECHNICAL",
];

/// Die Sektion TECHNICAL — Space klappt sie auf.
const TECHNICAL: usize = 4;

/// Breite der Label-Spalte vor den Achsen.
const LABEL: usize = 16;

/// Zeichnet den Report. `report: None` ist Legacy — ein ehrlicher Zustand
/// mit einem Satz, kein leerer Bildschirm.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    id: SessionId,
    report: Option<&EvidenceReport>,
    uninterpreted: usize,
    artifacts: &[CommitArtifact],
    lines: &[CommitLines],
    cursor: usize,
    technical: bool,
) {
    let short: String = id.to_string().chars().take(11).collect();
    let Some(report) = report else {
        legacy(frame, area, &short, uninterpreted, artifacts, lines);
        return;
    };

    let inner = area.width.saturating_sub(4) as usize;
    let check = app.evidence_check(id, artifacts);
    let intact = intact(&check);
    // Die Stufe: einmal berechnet, für INTEGRITY und LIMITS — aus dem
    // Urteil von `minds verify`, wenn es diese Session nennt.
    let (rating, checked) = app.session_rating(id);

    // Kopf: Identität, Trust Strip, Prüfzeit — dann der Satz für Menschen
    // und das Urteil von `minds verify`.
    let story = wrap(&sentence(report, uninterpreted, lines, intact), inner);
    let head_h = 2 + 1 + story.len() as u16 + 1;
    let (_, v_word, v_style) = theme::provenance(&Provenance::Chained(report.state));
    let axes = TrustAxes::of(&Provenance::Chained(report.state), uninterpreted);
    let mut first = vec![Span::styled(
        format!("EVIDENCE · session {short}…  "),
        theme::title().fg(theme::AGENT),
    )];
    first.extend(trust::spans(&axes, trust::Density::Glyphs));
    if let Some((_, _, Some(at))) = &check {
        let used: usize = first.iter().map(|s| s.content.chars().count()).sum();
        let stamp = format!("checked {}Z", super::clock(*at));
        let pad = inner.saturating_sub(used + stamp.chars().count());
        first.push(Span::raw(" ".repeat(pad)));
        first.push(Span::styled(stamp, theme::dim()));
    }
    let mut head_lines = vec![Line::from(first)];
    head_lines.extend(story.into_iter().map(|l| Line::from(Span::raw(l))));
    let (words, style) = verdict_words(report, check, app.evidence_checking(artifacts), true);
    head_lines.push(Line::from(vec![
        Span::styled("Verdict of minds verify: ", theme::dim()),
        Span::styled(words, style),
    ]));

    // Die Achsen: drei Zeilen, der Abgleich unter COVERAGE, LIMITS als
    // Satz, TECHNICAL zu- oder aufgeklappt.
    let rows = rows(
        report,
        &rating,
        uninterpreted,
        artifacts,
        lines,
        inner,
        technical,
        cursor,
        intact,
    );
    let rows_h = rows.len() as u16 + 2;

    let [head, rows_area, detail_area] = Layout::vertical([
        Constraint::Length(head_h),
        Constraint::Length(rows_h),
        Constraint::Min(1),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(head_lines).block(
            Block::bordered()
                .title(format!(" SESSION {} ", v_word.to_uppercase()))
                .title_style(v_style.patch(theme::title()))
                .border_style(v_style),
        ),
        head,
    );
    frame.render_widget(
        Paragraph::new(rows).block(
            Block::bordered()
                .title(" EVIDENCE ")
                .title_style(theme::title()),
        ),
        rows_area,
    );

    // Das Detail folgt dem Fokus — wie bisher, ohne dass Enter etwas
    // verspricht.
    let (title, lines) = detail(
        report,
        &rating,
        checked,
        uninterpreted,
        artifacts,
        lines,
        cursor,
    );
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(format!(" {title} "))
                .title_style(theme::title()),
        ),
        detail_area,
    );
}

/// Ob `minds verify` den Record geprüft und für intakt befunden hat:
/// nur dann heißt INTEGRITY `✓ record intact`. Ohne Urteil, nach einem
/// Fehler der Prüfung oder bei NOT VERIFIABLE ist der Record **sealed** —
/// sealed ist nicht verified (Design-Regel 2: dieselbe Darstellung wie
/// `… checking` in der Übersicht, nie ✓ davor).
fn intact(check: &Option<(CommitId, Result<&crate::CommitVerify, String>, Option<i64>)>) -> bool {
    match check {
        Some((_, Ok(c), _)) => {
            matches!(
                c.verdict,
                Ok(crate::VerifyVerdict::Verified | crate::VerifyVerdict::Incomplete)
            ) && !c.sessions.iter().any(|s| s.tampered)
        }
        _ => false,
    }
}

/// Die Wörter des Urteils (C12.1): **die von `minds verify`**, wörtlich,
/// mit der Prüfzeit — oder „sealed, not yet verified", solange keine
/// Signaturprüfung lief. Nie VERIFIED davor. `checking`: die Prüfung
/// läuft gerade (`v`) — dann `… checking`, kein Urteil.
/// `with_time` hängt die Prüfzeit an (Kopf); die Fußzeile lässt sie weg.
pub(crate) fn verdict_words(
    report: &EvidenceReport,
    check: Option<(CommitId, Result<&crate::CommitVerify, String>, Option<i64>)>,
    checking: bool,
    with_time: bool,
) -> (String, Style) {
    use crate::verify::Overall;
    match check {
        None if checking => ("… checking".into(), theme::dim()),
        None => {
            if report.state.verdict == EvidenceVerdict::Tampered {
                let (glyph, word, style) = theme::tampered();
                (
                    format!("{glyph} {word} — seal material altered · v: run verify"),
                    style,
                )
            } else {
                (
                    "sealed, not yet verified — v: run verify".into(),
                    theme::dim(),
                )
            }
        }
        Some((_, Err(why), _)) => (
            format!("not checked — {why}"),
            Style::default().fg(theme::REVIEW),
        ),
        Some((_, Ok(c), at)) => {
            let tampered = report.state.verdict == EvidenceVerdict::Tampered
                || c.sessions.iter().any(|s| s.tampered);
            let overall = match &c.verdict {
                _ if tampered => Overall::Tampered,
                Ok(crate::VerifyVerdict::Verified) => Overall::Verified,
                Ok(crate::VerifyVerdict::Incomplete) => Overall::Incomplete,
                Ok(crate::VerifyVerdict::Tampered) => Overall::Tampered,
                Ok(crate::VerifyVerdict::NotVerifiable) => Overall::NotVerifiable,
                Err(why) => {
                    return (
                        format!("not checked — {why}"),
                        Style::default().fg(theme::REVIEW),
                    );
                }
            };
            let (words, style) = trust::verify_headline(overall);
            match at.filter(|_| with_time) {
                Some(at) => (
                    format!("{words} (signatures checked {}Z)", super::clock(at)),
                    style,
                ),
                None => (words, style),
            }
        }
    }
}

/// Der Satz für Menschen: je Achse ein Satzteil — Integrität, Coverage,
/// Abgleich, Deutung —, nie ein Gesamturteil.
/// `intact` sagt, ob `minds verify` das Siegel schon bestätigt hat — davor
/// ist der Record sealed, nicht intact.
fn sentence(
    report: &EvidenceReport,
    uninterpreted: usize,
    lines: &[CommitLines],
    intact: bool,
) -> String {
    let state = &report.state;
    let axes = TrustAxes::of(&Provenance::Chained(*state), uninterpreted);
    let mut out = if state.verdict == EvidenceVerdict::Tampered {
        "This session's seal material was altered — the record is not intact".to_string()
    } else {
        format!(
            "This session's record is {} and {}",
            if intact { "intact" } else { "sealed" },
            signed_words(state.signed, state.seals)
        )
    };
    let boundary = boundary_word(report.scope.as_deref());
    out.push_str(&match axes.coverage {
        CoverageAxis::Complete => format!(
            ", and all {} {} lie inside the {boundary}.",
            state.events,
            plural(state.events as usize, "event", "events")
        ),
        CoverageAxis::Incomplete => format!(
            "; {} {} interrupt the {} events recorded inside the {boundary}.",
            state.gaps,
            plural(state.gaps as usize, "gap", "gaps"),
            state.events
        ),
        CoverageAxis::NotAssessable => "; coverage cannot be assessed on altered material.".into(),
        CoverageAxis::NotRecorded => "; no events were recorded.".into(),
    });
    out.push(' ');
    out.push_str(&artifact_sentence(lines));
    out.push(' ');
    out.push_str(&match axes.interpretation {
        InterpretationAxis::Interpreted => "Every tool call was interpreted.".to_string(),
        InterpretationAxis::Uninterpreted(n) => format!(
            "{n} tool {} observed but not interpreted.",
            plural(n, "call was", "calls were")
        ),
    });
    out
}

/// Der Satzteil zum Abgleich: die Zeilen des Commits nach Zustand.
fn artifact_sentence(lines: &[CommitLines]) -> String {
    let counted: Vec<&LineClasses> = lines.iter().filter_map(|(_, l)| l.as_ref().ok()).collect();
    if lines.is_empty() {
        return "No commit carries this session, so no lines were reconciled.".into();
    }
    let Some(c) = counted.first() else {
        return "The commit's lines were not reconciled.".into();
    };
    let c = if counted.len() == 1 {
        (*c).clone()
    } else {
        counted.iter().fold(LineClasses::default(), |mut acc, c| {
            // Sättigend: Zahlen aus dem Store, kein Überlauf-Panic.
            acc.added = acc.added.saturating_add(c.added);
            acc.explained = acc.explained.saturating_add(c.explained);
            acc.fs_only = acc.fs_only.saturating_add(c.fs_only);
            acc.stated = acc.stated.saturating_add(c.stated);
            acc.unexplained = acc.unexplained.saturating_add(c.unexplained);
            acc.not_assessed = acc.not_assessed.saturating_add(c.not_assessed);
            acc
        })
    };
    let observed = c.explained + c.fs_only;
    if c.added == 0 {
        "The commit adds no lines.".into()
    } else if c.unexplained == 0 && c.stated == 0 && c.not_assessed == 0 {
        format!("All {} added lines were observed.", c.added)
    } else if c.unexplained > 0 {
        format!(
            "{} of {} added lines {} not observed in the session; {} reported by the agent and {} observed.",
            c.unexplained,
            c.added,
            plural(c.unexplained, "is", "are"),
            c.stated,
            observed
        )
    } else {
        format!(
            "The {} added lines are reported by the agent; {} observed by a witness.",
            c.added,
            if observed == 0 {
                "none were".to_string()
            } else {
                format!("{observed} were")
            }
        )
    }
}

fn signed_words(signed: usize, seals: usize) -> String {
    if signed == 0 {
        "unsigned".into()
    } else if signed == seals {
        "signed".into()
    } else {
        format!("signed in {signed} of {seals} seals")
    }
}

fn boundary_word(scope: Option<&str>) -> &'static str {
    match scope {
        Some(s) if s.contains("witness") => "witness boundary",
        Some(s) if s.contains("hook") => "hook boundary",
        _ => "recorded boundary",
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// Zeilenumbruch an Wortgrenzen auf höchstens `width` Zeichen.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for word in text.split_whitespace() {
        let w = word.chars().count();
        if used > 0 && used + 1 + w > width {
            out.push(std::mem::take(&mut current));
            used = 0;
        }
        if used > 0 {
            current.push(' ');
            used += 1;
        }
        current.push_str(word);
        used += w;
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

/// Eine Achsenzeile: Label (16 Spalten), dann Glyph und Text.
fn axis(label: &str, glyph: &str, text: String, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {label:<LABEL$}"), theme::dim()),
        Span::styled(format!("{glyph} {text}"), style),
    ])
}

/// Eine eingerückte Folgezeile unter einer Achse.
fn sub(glyph: &str, text: String, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(LABEL + 1)),
        Span::styled(format!("{glyph} {text}"), style),
    ])
}

/// Die Achsenzeilen — mit dem Cursor auf `cursor`. `intact`: ob `minds
/// verify` das Siegel bestätigt hat — erst dann `✓ record intact`, davor
/// `◈ record sealed` (der Trust-Glyph der Achse). `rating`: die Stufe aus
/// [`App::session_rating`] — INTEGRITY nennt ihr Wort, LIMITS ihren Satz.
#[allow(clippy::too_many_arguments)]
fn rows(
    report: &EvidenceReport,
    rating: &Assurance,
    uninterpreted: usize,
    artifacts: &[CommitArtifact],
    lines: &[CommitLines],
    width: usize,
    technical: bool,
    cursor: usize,
    intact: bool,
) -> Vec<Line<'static>> {
    let state = &report.state;
    let axes = TrustAxes::of(&Provenance::Chained(*state), uninterpreted);
    let [i, c, d] = trust::slots(&axes);
    let ok = Style::default().fg(theme::OK);
    let warn = Style::default().fg(theme::REVIEW);
    let mut out: Vec<(usize, Line<'static>)> = Vec::new();

    // INTEGRITY: Record, Seals und Signatur, Kette und Stufe — in einem.
    if state.verdict == EvidenceVerdict::Tampered {
        let (glyph, word, style) = theme::tampered();
        out.push((
            0,
            axis(
                SECTIONS[0],
                glyph,
                format!("{word} — seal material altered"),
                style,
            ),
        ));
    } else {
        let mut text = format!(
            "record {} · {} {}, {} · chain {} ({} {})",
            if intact { "intact" } else { "sealed" },
            state.seals,
            plural(state.seals, "seal", "seals"),
            signed_words(state.signed, state.seals),
            if state.chain_closed { "closed" } else { "open" },
            state.seals,
            plural(state.seals, "epoch", "epochs")
        );
        // Sealed ist nicht verified: ✓ erst mit dem Urteil, davor der
        // Trust-Glyph der Achse (`◈`) — wie die Übersicht nie ✓ vor dem
        // Urteil zeigt.
        let (mut glyph, mut style) = if intact {
            ("✓", ok)
        } else {
            (i.glyph, i.style)
        };
        if !state.chain_closed {
            glyph = "!";
            style = warn;
        }
        if state.rejected {
            text.push_str(" · block seal in the chain");
            glyph = "!";
            style = warn;
        }
        text.push_str(&format!(" · {}", rating.word()));
        out.push((0, axis(SECTIONS[0], glyph, text, style)));
    }

    // COVERAGE: die Zahlen, darunter der Abgleich mit den C9.4-Wörtern.
    let coverage = match axes.coverage {
        CoverageAxis::Complete => format!(
            "{} · {} {} · {} {}",
            c.word,
            state.events,
            plural(state.events as usize, "event", "events"),
            state.gaps,
            plural(state.gaps as usize, "gap", "gaps")
        ),
        CoverageAxis::Incomplete => format!(
            "{} · {} events · {} {} · {} pre-chain",
            c.word,
            state.events,
            state.gaps,
            plural(state.gaps as usize, "gap", "gaps"),
            state.pre_chain
        ),
        CoverageAxis::NotAssessable => format!("{} — the seal material is altered", c.word),
        CoverageAxis::NotRecorded => c.word.to_string(),
    };
    out.push((1, axis(SECTIONS[1], c.glyph, coverage, c.style)));
    for (glyph, text, style) in artifact_rows(artifacts, lines) {
        out.push((1, sub(glyph, text, style)));
    }

    // INTERPRETATION: die dritte Achse, getrennt von den beiden anderen.
    let interpretation = match axes.interpretation {
        InterpretationAxis::Interpreted => format!("all tool calls {}", d.word),
        InterpretationAxis::Uninterpreted(n) => {
            format!("{n} {} observed but {}", plural(n, "call", "calls"), d.word)
        }
    };
    out.push((2, axis(SECTIONS[2], d.glyph, interpretation, d.style)));

    // LIMITS: die Grenze der Stufe als Satz, nicht als Zahl — aus der
    // Variante der Stufe, nicht aus dieser Ansicht.
    let limit = rating.limit();
    let text_width = width.saturating_sub(LABEL + 1);
    for (i, part) in wrap(&limit, text_width).into_iter().enumerate() {
        if i == 0 {
            out.push((
                3,
                Line::from(vec![
                    Span::styled(format!(" {:<LABEL$}", SECTIONS[3]), theme::dim()),
                    Span::raw(part),
                ]),
            ));
        } else {
            out.push((
                3,
                Line::from(vec![Span::raw(" ".repeat(LABEL + 1)), Span::raw(part)]),
            ));
        }
    }

    // TECHNICAL: zugeklappt eine Zeile; Space klappt die Technik auf.
    let label = format!(" {} (for auditors)", SECTIONS[4]);
    let hint = if technical { "▾ Space" } else { "▸ Space" };
    let pad = width.saturating_sub(label.chars().count() + hint.chars().count());
    out.push((
        TECHNICAL,
        Line::from(vec![
            Span::styled(label, theme::dim()),
            Span::raw(" ".repeat(pad)),
            Span::styled(hint, theme::dim()),
        ]),
    ));
    if technical {
        for line in integrity(report) {
            let mut spans = vec![Span::raw("   ")];
            spans.extend(line.spans);
            out.push((usize::MAX, Line::from(spans)));
        }
    }

    // Der Cursor liegt auf der ersten Zeile seiner Sektion.
    let mut seen = std::collections::HashSet::new();
    out.into_iter()
        .map(|(section, line)| {
            if section == cursor && seen.insert(section) {
                line.style(theme::cursor())
            } else {
                line
            }
        })
        .collect()
}

/// Die Abgleich-Zeilen unter COVERAGE: je abgeglichenem Commit eine.
fn artifact_rows(
    artifacts: &[CommitArtifact],
    lines: &[CommitLines],
) -> Vec<(&'static str, String, Style)> {
    if artifacts.is_empty() {
        return vec![(
            "·",
            "no commit carries this session — nothing to reconcile".into(),
            theme::dim(),
        )];
    }
    let many = artifacts.len() > 1;
    let mut out = Vec::new();
    let mut skipped = 0;
    for artifact in artifacts {
        let counted = lines
            .iter()
            .find(|(c, _)| *c == artifact.commit)
            .map(|(_, l)| l);
        match (&artifact.state, counted) {
            (ArtifactState::Assessed(_), Some(Ok(classes))) => {
                let (glyph, mut text, style) = super::class_words(classes);
                if many {
                    text = format!("{} · {text}", short_commit(artifact));
                }
                let note = provenance_note(artifact);
                if !note.is_empty() {
                    text.push_str(&format!(" · {note}"));
                }
                out.push((glyph, text, style));
            }
            (ArtifactState::Assessed(_), counted) => {
                let mut text = match counted {
                    Some(Err(why)) => format!("lines not counted — {why}"),
                    _ => "lines not counted".to_string(),
                };
                if many {
                    text = format!("{} · {text}", short_commit(artifact));
                }
                let note = provenance_note(artifact);
                if !note.is_empty() {
                    text.push_str(&format!(" · {note}"));
                }
                out.push(("·", text, theme::dim()));
            }
            (ArtifactState::Unavailable(why), _) => {
                if many {
                    skipped += 1;
                } else {
                    out.push(("·", format!("not assessed ({why})"), theme::dim()));
                }
            }
            (ArtifactState::Failed(_), _) => {
                if many {
                    skipped += 1;
                } else {
                    out.push(("·", "not assessed (error)".into(), theme::dim()));
                }
            }
        }
    }
    if many {
        let mut text = format!("{} commits", artifacts.len());
        if skipped > 0 {
            text.push_str(&format!(" · {skipped} not assessed"));
        }
        if artifacts.iter().any(|a| a.inferred) {
            text.push_str(" · partly inferred links");
        }
        out.push(("·", text, theme::dim()));
    }
    out
}

fn legacy(
    frame: &mut Frame,
    area: Rect,
    short: &str,
    uninterpreted: usize,
    artifacts: &[CommitArtifact],
    lines: &[CommitLines],
) {
    let mut out = vec![
        Line::from(vec![
            Span::styled(
                format!("EVIDENCE · session {short}…  "),
                theme::title().fg(theme::AGENT),
            ),
            {
                let slot = trust::integrity(minds_reader::model::IntegrityAxis::Legacy);
                Span::styled(
                    format!("{} {}", slot.glyph, slot.word),
                    slot.style.patch(theme::title()),
                )
            },
        ]),
        Line::default(),
        Line::from(Span::raw(LEGACY_SENTENCE)),
        Line::default(),
    ];
    // Die drei Achsen in der vollen Dichte (A4): Legacy hat keine
    // Integrität und keine Coverage zu prüfen — die Deutung schon.
    out.extend(trust::render(
        &TrustAxes::of(&Provenance::Legacy, uninterpreted),
        trust::Density::Full,
    ));
    out.extend([
        Line::default(),
        Line::from(Span::styled(
            "This session was captured before the evidence chain. It never gets a chain \
             attributed after the fact — its honest answer is this state.",
            theme::dim(),
        )),
    ]);
    // Der Abgleich hängt nicht an der Chain: Auch eine Legacy-Session kann
    // Zeilen eines Commits erklären — oder eben nicht.
    if !artifacts.is_empty() {
        out.push(Line::default());
        out.extend(artifact(artifacts, lines));
    }
    frame.render_widget(
        Paragraph::new(out).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(" SESSION · LEGACY ")
                .title_style(theme::dim().patch(theme::title()))
                .border_style(theme::dim()),
        ),
        area,
    );
}

/// Der Detail-Block der fokussierten Sektion.
#[allow(clippy::too_many_arguments)]
fn detail(
    report: &EvidenceReport,
    rating: &Assurance,
    checked: bool,
    uninterpreted: usize,
    artifacts: &[CommitArtifact],
    lines: &[CommitLines],
    cursor: usize,
) -> (&'static str, Vec<Line<'static>>) {
    match cursor {
        0 => {
            let mut out = epochs(report);
            out.push(Line::default());
            out.extend(signature(report));
            ("INTEGRITY", out)
        }
        1 => {
            let mut out = artifact(artifacts, lines);
            out.push(Line::default());
            out.extend(coverage(report));
            ("COVERAGE", out)
        }
        2 => ("INTERPRETATION", interpretation(uninterpreted)),
        3 => ("LIMITS", limitations(rating, checked)),
        _ => ("TECHNICAL", integrity(report)),
    }
}

/// Höchstens so breit wird die Pfadspalte; längere Pfade werden von vorn
/// gekürzt — das Ende (Dateiname) trägt die Aussage.
const PATH_WIDTH: usize = 60;

/// Ein Pfad zur Anzeige: entschärft und auf [`PATH_WIDTH`] gekürzt.
fn shown_path(path: &str) -> String {
    let path = minds_reader::sanitize(path);
    if path.chars().count() <= PATH_WIDTH {
        return path;
    }
    let tail: Vec<char> = path.chars().rev().take(PATH_WIDTH - 1).collect();
    format!("…{}", tail.into_iter().rev().collect::<String>())
}

fn short_commit(artifact: &CommitArtifact) -> String {
    artifact.commit.to_string().chars().take(7).collect()
}

/// Das Detail des Abgleichs: je Commit die Zeilen nach Zustand, dann je
/// Datei Klasse, Pfad und was unerklärt ist. Pfade sind fremdbestimmt und
/// laufen durch die Terminal-Härtung des Readers.
fn artifact(artifacts: &[CommitArtifact], lines: &[CommitLines]) -> Vec<Line<'static>> {
    if artifacts.is_empty() {
        return vec![
            Line::from(Span::raw(
                "No commit carries this session — nothing to reconcile.",
            )),
            Line::default(),
            Line::from(Span::styled(
                "Reconciliation compares a commit with the session's stored writes.",
                theme::dim(),
            )),
        ];
    }
    let (neutral_glyph, _, neutral) = theme::not_observed();
    let mut out = Vec::new();
    for (i, artifact) in artifacts.iter().enumerate() {
        if i > 0 {
            out.push(Line::default());
        }
        let mut head = vec![
            Span::styled(format!("{:<12} ", "Commit"), theme::dim()),
            Span::styled(short_commit(artifact), Style::default().fg(theme::CHANGE)),
        ];
        // Der Betreff ist im Index schon entschärft.
        if let Some(subject) = &artifact.subject {
            head.push(Span::raw(format!("  {subject}")));
        }
        let note = provenance_note(artifact);
        if !note.is_empty() {
            head.push(Span::styled(format!("  · {note}"), theme::dim()));
        }
        out.push(Line::from(head));
        let assessed = match &artifact.state {
            ArtifactState::Assessed(assessed) => assessed,
            ArtifactState::Unavailable(why) => {
                out.push(Line::from(Span::styled(
                    format!("{:<12} not assessed ({why})", ""),
                    theme::dim(),
                )));
                continue;
            }
            ArtifactState::Failed(err) => {
                out.push(Line::from(Span::styled(
                    format!("{:<12} not assessed (error: {err})", ""),
                    theme::dim(),
                )));
                continue;
            }
        };
        let counted = lines
            .iter()
            .find(|(c, _)| *c == artifact.commit)
            .map(|(_, l)| l);
        let (glyph, text, style) = match counted {
            Some(Ok(classes)) => super::class_words(classes),
            Some(Err(why)) => ("·", format!("lines not counted — {why}"), theme::dim()),
            None => ("·", "lines not counted".into(), theme::dim()),
        };
        out.push(Line::from(vec![
            Span::raw(format!("{:<12} ", "")),
            Span::styled(format!("{glyph} {text}"), style.patch(theme::title())),
        ]));
        let width = assessed
            .recon
            .files
            .iter()
            .map(|f| shown_path(&f.path).chars().count())
            .chain(
                assessed
                    .structural
                    .iter()
                    .map(|s| shown_path(&s.path).chars().count()),
            )
            .max()
            .unwrap_or(0);
        for file in &assessed.recon.files {
            let (glyph, word, style) = theme::recon(file.class);
            let mut spans = vec![
                Span::styled(format!("  {glyph} "), style),
                Span::styled(format!("{word:<20}"), style),
                Span::raw(format!("{:<width$}", shown_path(&file.path))),
            ];
            let note = file.note();
            if !note.is_empty() {
                let note_style = if file.class == ReconClass::Unexplained {
                    neutral
                } else {
                    theme::dim()
                };
                spans.push(Span::styled(format!("  {note}"), note_style));
            }
            out.push(Line::from(spans));
        }
        for structural in &assessed.structural {
            let (glyph, word, style) = theme::recon(ReconClass::Unexplained);
            out.push(Line::from(vec![
                Span::styled(format!("  {glyph} "), style),
                Span::styled(format!("{word:<20}"), style),
                Span::raw(format!("{:<width$}", shown_path(&structural.path))),
                Span::styled(format!("  {} {NOT_OBSERVED}", structural.what), neutral),
            ]));
        }
    }
    // Die Legende nur, wo Dateizeilen stehen — nicht unter „not assessed".
    if artifacts
        .iter()
        .any(|a| matches!(a.state, ArtifactState::Assessed(_)))
    {
        out.push(Line::default());
        out.push(Line::from(vec![
            Span::styled(format!("{neutral_glyph} "), neutral),
            Span::styled(NOT_OBSERVED, neutral),
            Span::styled(
                "  ·  ◇ reported only: a tool claim, no witness",
                theme::dim(),
            ),
        ]));
    }
    out
}

fn short_hash(hash: &minds_core::ContentHash) -> String {
    hash.to_string().chars().take(14).collect()
}

/// Die Technik für Auditoren (TECHNICAL, hinter Space): Algorithmus und
/// Kontexte, Seal-Ids und Roots, die Grenze des Proof-Modells —
/// unverändert die früheren INTEGRITY-Zeilen.
fn integrity(report: &EvidenceReport) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{:<12} ", "Algorithm"), theme::dim()),
            Span::raw("blake3 · derive_key, contexts minds/evidence/v1/*"),
        ]),
        Line::default(),
    ];
    for epoch in &report.epochs {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<12} ", "Seal"), theme::dim()),
            Span::raw(format!(
                "{}…  root {}…  {} event(s)",
                short_hash(&epoch.seal_id),
                short_hash(&epoch.root),
                epoch.events
            )),
        ]));
    }
    lines.push(Line::default());
    // Die Grenze des Proof-Modells, hier wo sie hingehört: extern prüfbar
    // sind Identität und Signatur — die Chain selbst nur lokal.
    lines.push(Line::from(Span::styled(
        "✓ Externally verifiable: seal identity (seal_id = hash of the seal text) and signature.",
        Style::default().fg(theme::OK),
    )));
    lines.push(Line::from(Span::styled(
        "— Chain root: reproducible only locally with journal + session salt (anti-oracle).",
        theme::dim(),
    )));
    lines
}

fn coverage(report: &EvidenceReport) -> Vec<Line<'static>> {
    let state = &report.state;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{:<12} ", "Scope"), theme::dim()),
            Span::raw(format!(
                "{} · {} event(s) · {} gap(s) · {} pre-chain",
                report.scope.clone().unwrap_or_else(|| "?".into()),
                state.events,
                state.gaps,
                state.pre_chain
            )),
        ]),
        Line::from(Span::styled("Observation boundary", theme::title())),
        Line::from(Span::styled(
            "✓ agent hook events (scope in the seal)",
            Style::default().fg(theme::OK),
        )),
    ];
    // „Nicht erfasst" ist KEINE Lücke: Es liegt außerhalb des Scopes —
    // visuell ein anderer Zustand (— statt !).
    for outside in [
        "— subprocesses outside the hook boundary  · not captured, not a gap",
        "— network activity                        · not captured, not a gap",
        "— the window between append and seal      · not captured, not a gap",
    ] {
        lines.push(Line::from(Span::styled(outside, theme::dim())));
    }
    lines.push(Line::from(Span::raw(
        "Missing evidence does not prove that nothing happened — it means: Minds cannot attest it.",
    )));
    lines
}

fn epoch_link_word(link: EpochLink) -> &'static str {
    match link {
        EpochLink::Start => "chain start",
        EpochLink::Chained => "chained (previous attested)",
        EpochLink::RejectedBefore => "predecessor epoch rejected",
        EpochLink::Unresolved => "previous not resolvable — chain open",
    }
}

fn epochs(report: &EvidenceReport) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let total = report.epochs.len();
    for (i, epoch) in report.epochs.iter().enumerate() {
        lines.extend(epoch_lines(epoch, i, total));
        if i + 1 < total {
            lines.push(Line::from(Span::styled("  │", theme::dim())));
            lines.push(Line::from(Span::styled("  ▼", theme::dim())));
        }
    }
    lines
}

fn epoch_lines(epoch: &EpochReport, i: usize, total: usize) -> Vec<Line<'static>> {
    let seal_word = if epoch.stored {
        Span::styled("Seal ✓", Style::default().fg(theme::OK))
    } else {
        Span::styled(
            "block seal · payload rejected",
            Style::default().fg(theme::REVIEW),
        )
    };
    vec![
        Line::from(vec![
            Span::styled(format!("Epoch {}/{total}  ", i + 1), theme::title()),
            Span::raw(format!(
                "#{}–#{} · {} event(s) · {} gap(s)",
                epoch.first_seq, epoch.last_seq, epoch.events, epoch.gaps
            )),
        ]),
        Line::from(vec![
            Span::raw("  "),
            seal_word,
            Span::raw(format!(
                "  {}…  {} · {}",
                short_hash(&epoch.seal_id),
                if epoch.signed { "signed" } else { "unsigned" },
                epoch_link_word(epoch.link)
            )),
        ]),
    ]
}

fn signature(report: &EvidenceReport) -> Vec<Line<'static>> {
    let state = &report.state;
    if state.signed == 0 {
        return vec![
            Line::from(Span::styled("○ NOT SIGNED", theme::title())),
            Line::default(),
            Line::from(Span::raw(
                "The seals are cryptographically self-consistent (content-addressed), but nobody vouches for them with a key.",
            )),
            Line::from(Span::styled(
                "Unsigned is not invalid — `minds sign --seal` adds the signature after the fact.",
                theme::dim(),
            )),
        ];
    }
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "✓ {}/{} seal(s) carry a signature",
            state.signed, state.seals
        ),
        Style::default().fg(theme::OK).patch(theme::title()),
    ))];
    for (i, epoch) in report.epochs.iter().enumerate() {
        lines.push(Line::from(Span::raw(format!(
            "  epoch {}: {}",
            i + 1,
            if epoch.signed {
                "signed (SSH)"
            } else {
                "unsigned"
            }
        ))));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Presence is not verification: validity is checked by `minds verify` against an allowed_signers file from a trusted source.",
        theme::dim(),
    )));
    lines
}

fn interpretation(uninterpreted: usize) -> Vec<Line<'static>> {
    let axis = if uninterpreted == 0 {
        InterpretationAxis::Interpreted
    } else {
        InterpretationAxis::Uninterpreted(uninterpreted)
    };
    let slot = trust::interpretation(axis);
    let mut lines = vec![Line::from(Span::styled(
        format!("{} {} — {}", slot.glyph, slot.word, slot.sentence),
        slot.style,
    ))];
    if uninterpreted > 0 {
        lines.push(Line::from(Span::raw(
            "Observed means: name and raw arguments are evidence — the effect is not normalized.",
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Interpretation is separate from integrity and coverage: same evidence + same adapter ⇒ same interpretation; `minds reinterpret` shows the stored and the current interpretation side by side.",
        theme::dim(),
    )));
    lines
}

/// Der Hinweis unter der Überschrift: woher die Stufe stammt. Eine
/// ungeprüfte Witness-Signatur sagt es mit dem Satz der Variante —
/// `minds verify --signers` urteilt dann; sonst versprach der Hinweis
/// etwas, das nicht kommt.
fn limits_note(rating: &Assurance, checked: bool) -> String {
    match rating {
        Assurance::Unchecked { .. } => rating.limit(),
        _ if checked => "Level as minds verify rated it, signatures checked.".into(),
        _ => "Level from the repository alone.".into(),
    }
}

/// Die Grenzen der Stufe (EA-13) — die Stufe steht über der Liste, damit
/// niemand die Grenzen einer anderen Stufe unterstellt. `checked`: die
/// Stufe stammt aus der Signaturprüfung von `minds verify`.
pub(super) fn limitations(rating: &Assurance, checked: bool) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("At {}, Minds does NOT prove:", rating.word()),
            theme::title(),
        )),
        Line::from(Span::styled(limits_note(rating, checked), theme::dim())),
        Line::default(),
    ];
    for limit in rating.limitations() {
        lines.push(Line::from(vec![
            Span::styled("• ", theme::dim()),
            Span::raw(limit.text),
        ]));
    }
    lines
}
