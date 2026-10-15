//! Der Verify-Tab: das Urteil zuerst, dann je Achse eine Zeile — was
//! `minds verify` als Textwand ausgibt, auf einen Blick. Enter auf einer
//! Session öffnet ihren Graphen, auf dem Artefakt den Diff, auf dem Scope
//! die erste Datei außerhalb des Bereichs.
//!
//! Leitsatz: nie mehr behaupten als `verify`. VERIFIED steht nur nach
//! durchgelaufener Signaturprüfung; was nicht geprüft ist, sagt das.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use minds_reader::assurance::{IntentSignature, IntentState};
use minds_reader::model::EvidenceVerdict;

use crate::app::App;
use crate::theme;
use crate::verify::{ClassCounts, Overall, VerifyState};

/// Breite des Artefakt-Balkens.
const BAR: usize = 24;

/// Zeichnet den Tab.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.verify else {
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    headline(state, app.spin, &mut lines);
    commit_line(state, &mut lines);
    lines.push(Line::raw(""));

    let label = |key: &str| Span::styled(format!("{key:<12}"), theme::dim());
    let pointer = |row: usize| {
        if row == state.cursor {
            Span::styled("▸ ", theme::title())
        } else {
            Span::raw("  ")
        }
    };

    // Sessions — je eine Zeile, wählbar.
    lines.push(Line::from(Span::styled("Sessions", theme::title())));
    if state.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No session is linked to this commit.",
            theme::dim(),
        )));
    }
    for (i, session) in state.sessions.iter().enumerate() {
        let id = session.id.to_string();
        let short = id.get(..12).unwrap_or(&id);
        let mut spans = vec![pointer(i)];
        let cli_tampered = state.assurance_of(session.id).is_some_and(|a| a.tampered);
        match &session.report {
            Some(report) => {
                // Kein eigenes Verdikt je Zeile — das sagt die Kopfzeile, von
                // `verify` selbst. Hier nur Fakten; ✗ nur bei Manipulation.
                let tampered = cli_tampered || report.state.verdict == EvidenceVerdict::Tampered;
                let (glyph, word, style) = crate::view::trust::verify_session_mark(tampered);
                spans.push(Span::styled(format!("{glyph} {word:<10}"), style));
                spans.push(Span::raw(format!(" {short}…  ")));
                spans.push(Span::styled(
                    format!(
                        "{} event(s) · {} seal(s) · {} gap(s)  ",
                        report.state.events, report.state.seals, report.state.gaps
                    ),
                    theme::dim(),
                ));
            }
            None => {
                spans.push(Span::styled("· no seal   ", theme::dim()));
                spans.push(Span::raw(format!(" {short}…  ")));
            }
        }
        // Bei Manipulation keine Stufe: `verify` fällt dann auf A0 und zeigt
        // keinen Scope — die Momentaufnahmen der CLI können älter sein.
        if let Some(assurance) = state
            .assurance_of(session.id)
            .filter(|_| state.overall() != Overall::Tampered)
        {
            spans.push(Span::raw(assurance.level.word().to_string()));
            if let Some(reason) = &assurance.reason {
                spans.push(Span::styled(format!(" — {reason}"), theme::dim()));
            }
        }
        lines.push(emphasis(Line::from(spans), i == state.cursor));
        lines.push(Line::from(Span::styled(
            format!("      {}", crate::view::clip(&session.request, 100)),
            theme::dim(),
        )));
    }
    lines.push(Line::raw(""));

    // Die Achsen aus dem Reader: Integrität und Coverage.
    let reports: Vec<_> = state
        .sessions
        .iter()
        .filter_map(|s| s.report.as_ref())
        .collect();
    let unsealed = state.sessions.len() - reports.len();
    let overall = state.overall();
    // Ohne geprüftes Urteil von `verify` weder ✓ noch „complete": Der Reader
    // sieht keine ungültige Witness-Signatur und nicht alle Lücken
    // (`trust::verify_integrity`).
    let pending_word = if state.pending {
        "signatures checking…"
    } else {
        "signatures not checked"
    };
    if !reports.is_empty() {
        let seals: usize = reports.iter().map(|r| r.state.seals).sum();
        let signed: usize = reports.iter().map(|r| r.state.signed).sum();
        let without = if unsealed > 0 {
            format!(" · {unsealed} session(s) without a seal")
        } else {
            String::new()
        };
        let (g, style, text) =
            crate::view::trust::verify_integrity(overall, seals, signed, &without, pending_word);
        lines.push(Line::from(vec![
            Span::raw("  "),
            label("Integrity"),
            Span::styled(format!("{g} {text}"), style),
        ]));
        // Store-Daten: sättigend summieren, wie `verify`.
        let gaps = reports
            .iter()
            .fold(0u64, |sum, r| sum.saturating_add(r.state.gaps));
        let events = reports
            .iter()
            .fold(0u64, |sum, r| sum.saturating_add(r.state.events));
        let mut scopes = reports.iter().filter_map(|r| r.scope.as_deref());
        let first = scopes.next();
        let scope = match first {
            Some(one) if scopes.all(|s| s == one) => one.to_string(),
            Some(_) => "differs per session".into(),
            None => "?".into(),
        };
        // Das Wort folgt dem Urteil von `verify`, nicht den Lücken des
        // Readers: Nur `verify` sieht alle Seals des Namensraums.
        let (g, style, word) = crate::view::trust::verify_coverage(overall, pending_word);
        lines.push(Line::from(vec![
            Span::raw("  "),
            label("Coverage"),
            Span::styled(
                format!("{g} {word} · boundary {scope} · {events} event(s) · {gaps} gap(s)"),
                style,
            ),
        ]));
    }

    // Artefakt — wählbar: Enter öffnet den Diff.
    let row = state.artifact_row();
    let mut spans = vec![pointer(row), label("Artifact")];
    match &state.artifact {
        Ok(counts) => {
            spans.extend(artifact_spans(counts));
            spans.push(Span::styled(
                "   (claims only, without witness observations)",
                theme::dim(),
            ));
        }
        Err(why) => spans.push(Span::styled(
            format!("· not assessed ({why})"),
            theme::dim(),
        )),
    }
    lines.push(emphasis(Line::from(spans), row == state.cursor));
    // Warum nicht 100 %: die unerklärten Zeilen nach ihrem Grund.
    if let Ok(counts) = &state.artifact
        && let Some(summary) = minds_reader::artifact::gap_summary(&counts.gaps)
    {
        lines.push(Line::from(vec![
            Span::raw("  "),
            label(""),
            Span::styled(format!("◦ {summary}"), theme::dim()),
        ]));
    }

    // Scope — wählbar: Enter öffnet die erste Datei außerhalb.
    let row = state.scope_row();
    let mut spans = vec![pointer(row), label("Scope")];
    if overall == Overall::Tampered {
        spans.push(Span::styled(
            "· not assessed (integrity violated)",
            theme::dim(),
        ));
    } else if state.pending {
        spans.push(Span::styled("… checking", theme::dim()));
    } else if let Some(err) = state.check_error() {
        spans.push(Span::styled(format!("· not checked ({err})"), theme::dim()));
    } else if let Some(cli) = state.checked() {
        match (&cli.out_of_scope, &cli.scope_note) {
            (Some(paths), _) if paths.is_empty() => spans.push(Span::styled(
                "✓ every path inside the declared scope (commit, claims, observations)",
                Style::default().fg(theme::OK),
            )),
            (Some(paths), _) => {
                spans.push(Span::styled(
                    format!("⚠ {} path(s) outside: ", paths.len()),
                    Style::default().fg(theme::REVIEW),
                ));
                let shown: Vec<&str> = paths.iter().take(3).map(String::as_str).collect();
                let more = if paths.len() > 3 { " …" } else { "" };
                spans.push(Span::raw(format!("{}{more}", shown.join(", "))));
            }
            (None, note) => spans.push(Span::styled(
                format!("· not assessed ({})", note.as_deref().unwrap_or("no scope")),
                theme::dim(),
            )),
        }
    } else {
        spans.push(Span::styled("· not available", theme::dim()));
    }
    lines.push(emphasis(Line::from(spans), row == state.cursor));

    conformance_lines(state, &mut lines);

    // Grenzen: dieselbe Kurzliste wie `minds verify`.
    if let Some((level, _)) = state.weakest() {
        let shown: Vec<&str> = minds_core::evidence::limits_at(level.level())
            .take(3)
            .map(|l| l.short)
            .collect();
        let note = if state.checked().is_none() {
            " (at the reader's level — signatures not checked)"
        } else {
            ""
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            label("Not proven"),
            Span::styled(format!("{}{note}", shown.join(" · ")), theme::dim()),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Enter: a session → its graph · Artifact → the diff · Scope → the first file outside",
        theme::dim(),
    )));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(" VERIFY ")
                .title_style(theme::title()),
        ),
        area,
    );
}

/// Die Conformance gegen das signierte Soll — je Regel *result · id ·
/// observed*, darunter gedimmt *expected*. Eine Anzeige neben dem Urteil:
/// Die Kopfzeile bleibt die von `verify` (dessen Exit-Code sie nicht ändert).
fn conformance_lines(state: &VerifyState, lines: &mut Vec<Line<'static>>) {
    use minds_reader::conformance::Outcome;
    let label = Span::styled(format!("{:<12}", "Conformance"), theme::dim());
    let report = match &state.cli {
        _ if state.pending => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                label,
                Span::styled("… checking", theme::dim()),
            ]));
            return;
        }
        Some(Ok(cli)) => cli.conformance.as_ref(),
        Some(Err(err)) => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                label,
                Span::styled(format!("· not checked ({err})"), theme::dim()),
            ]));
            return;
        }
        None => None,
    };
    let Some(report) = report else {
        lines.push(Line::from(vec![
            Span::raw("  "),
            label,
            Span::styled(
                "· no spec (.minds/spec.json, signed with minds spec sign)",
                theme::dim(),
            ),
        ]));
        return;
    };
    let style = |outcome: Outcome| match outcome {
        Outcome::Pass => ("✓", Style::default().fg(theme::OK)),
        Outcome::Drift => ("✗", Style::default().fg(theme::DELETE)),
        Outcome::Unknown => ("?", Style::default().fg(theme::REVIEW)),
    };
    let (pass, drift, unknown) = report.counts();
    let (glyph, head) = style(report.outcome());
    let source = match (&report.active, &report.signer) {
        (Some((id, _)), Some(signer)) => format!(
            "spec b3-{} signed by {}",
            &id.hex()[..12],
            minds_reader::sanitize(signer)
        ),
        (Some((id, _)), None) => format!("spec b3-{} (not signed)", &id.hex()[..12]),
        (None, _) => "no signed spec".to_owned(),
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        label,
        Span::styled(
            format!("{glyph} {}", report.outcome().label()),
            head.add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {pass} pass · {drift} drift · {unknown} unknown  "
        )),
        Span::styled(source, theme::dim()),
    ]));
    for row in report.rows() {
        let (glyph, style) = style(row.outcome);
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::raw(format!("{:<12}", "")),
            Span::styled(format!("{glyph} {:<8}", row.outcome.label()), style),
            Span::raw(format!("{}  ", row.id)),
            Span::raw(row.observed.clone()),
        ]));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::raw(format!("{:<23}", "")),
            Span::styled(format!("expected {}", row.expected), theme::dim()),
        ]));
    }
}

fn emphasis(line: Line<'static>, on: bool) -> Line<'static> {
    if on {
        line.style(theme::cursor())
    } else {
        line
    }
}

/// Wie stark eine Intent-Signatur trägt — für „die schwächste gilt".
fn rank(signature: &IntentSignature) -> u8 {
    match signature {
        IntentSignature::Invalid => 0,
        IntentSignature::Unsigned => 1,
        IntentSignature::NotChecked => 2,
        IntentSignature::Valid(_) => 3,
    }
}

/// Erste Zeile: Urteil, Stufe, Intent — das, was man zuerst wissen will.
fn headline(state: &VerifyState, spin: usize, lines: &mut Vec<Line<'static>>) {
    let (mut word, style) = crate::view::trust::verify_headline(state.overall());
    // Während der Worker prüft, dreht sich ein ASCII-Spinner (B6).
    if state.overall() == Overall::Checking {
        word.push(' ');
        word.push(crate::app::spinner(spin));
    }
    let mut spans = vec![Span::styled(
        format!(" {word} "),
        style.add_modifier(Modifier::BOLD | Modifier::REVERSED),
    )];
    if let Some((level, checked)) = state
        .weakest()
        .filter(|_| state.overall() != Overall::Tampered)
    {
        spans.push(Span::styled(format!("   {}", level.word()), theme::title()));
        if !checked {
            spans.push(Span::styled(
                if state.pending {
                    " (checking signatures…)"
                } else {
                    " (signatures not checked)"
                },
                theme::dim(),
            ));
        }
    }
    spans.push(Span::raw("   "));
    // Der schwächste Intent über alle Sessions — „signiert" nur, wenn alle
    // gebundenen es sind.
    let bound: Vec<&IntentState> = state
        .checked()
        .map(|c| {
            c.sessions
                .iter()
                .map(|s| &s.intent)
                .filter(|i| matches!(i, IntentState::Bound { .. }))
                .collect()
        })
        .unwrap_or_default();
    let weakest = bound.iter().copied().min_by_key(|i| match i {
        IntentState::Bound { signature, .. } => rank(signature),
        IntentState::Unbound => 4,
    });
    // Verschiedene Anker oder Signaturlagen — oder Sessions ohne Intent
    // neben gebundenen: sagen, statt eine zu zeigen.
    let unbound = state
        .checked()
        .map(|c| {
            c.sessions
                .iter()
                .filter(|s| matches!(s.intent, IntentState::Unbound))
                .count()
        })
        .unwrap_or(0);
    let mixed =
        bound.windows(2).any(|pair| pair[0] != pair[1]) || (unbound > 0 && !bound.is_empty());
    spans.push(match weakest {
        Some(IntentState::Bound {
            anchor_id,
            chained,
            signature,
            ..
        }) => {
            let anchor = anchor_id.to_string();
            let (text, style) = match signature {
                IntentSignature::Valid(kind) => (
                    format!("intent signed ({})", kind.word()),
                    Style::default().fg(theme::OK),
                ),
                IntentSignature::NotChecked => {
                    ("intent signature not checked".into(), theme::dim())
                }
                IntentSignature::Invalid => (
                    "intent signature invalid".into(),
                    Style::default().fg(theme::REVIEW),
                ),
                IntentSignature::Unsigned => ("intent unsigned".into(), theme::dim()),
            };
            let chain = if *chained { "chained" } else { "unchained" };
            let mixed = if mixed { " · sessions differ" } else { "" };
            Span::styled(
                format!(
                    "{text} · {}… · {chain}{mixed}",
                    anchor.get(..12).unwrap_or(&anchor)
                ),
                style,
            )
        }
        _ if state.pending => Span::styled("intent …", theme::dim()),
        _ if state.checked().is_none() => Span::styled("intent not checked", theme::dim()),
        _ => Span::styled("intent not bound", theme::dim()),
    });
    lines.push(Line::from(spans));
    if let Some(err) = state.check_error() {
        lines.push(Line::from(Span::styled(
            format!(" Signatures could not be checked: {err}"),
            Style::default().fg(theme::REVIEW),
        )));
    } else if let Some(err) = state.verdict_error() {
        lines.push(Line::from(Span::styled(
            format!(" Verdict of minds verify not available: {err}"),
            Style::default().fg(theme::REVIEW),
        )));
    }
}

fn commit_line(state: &VerifyState, lines: &mut Vec<Line<'static>>) {
    let Some(commit) = state.commit() else {
        return;
    };
    let hex = commit.to_string();
    let mut spans = vec![
        Span::styled(
            format!(" {}", hex.get(..7).unwrap_or(&hex)),
            Style::default().fg(theme::CHANGE),
        ),
        Span::raw(format!(" {}", state.subject.clone().unwrap_or_default())),
    ];
    if let Some(change) = &state.change {
        spans.push(Span::styled(format!("   {change}"), theme::dim()));
    }
    let (glyph, word, style) = theme::verdict(state.review.verdict);
    spans.push(Span::styled(format!("   {glyph} {word}"), style));
    spans.push(Span::styled(
        format!("   {}/{}", state.at + 1, state.commits.len()),
        theme::dim(),
    ));
    lines.push(Line::from(spans));
}

/// Der Balken und die Zählung je Klasse: wie viel des Commits belegt ist —
/// und wodurch.
fn artifact_spans(counts: &ClassCounts) -> Vec<Span<'static>> {
    let total = counts.total();
    if total == 0 {
        return vec![Span::styled("· no changed lines", theme::dim())];
    }
    let backed = counts.backed();
    // Abgerundet: Der Balken ist nie voll, solange eine Zeile unerklärt ist.
    let filled = (backed as usize * BAR) / total as usize;
    let percent = (backed * 100) / total;
    vec![
        Span::styled("█".repeat(filled), Style::default().fg(theme::OK)),
        Span::styled("░".repeat(BAR - filled), theme::dim()),
        Span::raw(format!("  {percent} %  {backed}/{total} lines   ")),
        Span::styled(
            format!("● {}  ", counts.explained),
            Style::default().fg(theme::OK),
        ),
        Span::styled(
            format!("◍ {}  ", counts.fs_only),
            Style::default().fg(theme::OK),
        ),
        Span::raw(format!("◇ {}  ", counts.reported)),
        Span::styled(
            format!("◦ {}", counts.unexplained),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]
}
