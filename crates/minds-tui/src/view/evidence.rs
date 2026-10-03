//! Der Evidence-Report einer Session — drei Ebenen über demselben
//! Read-Model: das **Verdikt** (ist die Evidence belastbar?), die
//! **Erklärung** (warum?) und die **Kryptographie** (welche Seals, Roots,
//! Epochen liegen zugrunde).
//!
//! Die TUI rechnet hier nichts nach: Alles kommt fertig aus
//! [`EvidenceReport`] ([`minds_reader`]) — derselben Rechnung, die auch
//! `minds verify` und das Audit-Bundle tragen. Und sie behauptet nie mehr
//! als das Modell: „vollständig" heißt vollständig **innerhalb** der
//! Beobachtungsgrenze; was außerhalb liegt, ist `— nicht erfasst`, keine
//! Lücke — und eine Lücke ist kein Beweis, dass etwas geschah.
//!
//! Die Sektion **ARTIFACT** (EA-03) zeigt den Abgleich der Commits, die die
//! Session tragen: `artifact X/Y lines explained` und je Datei die Klasse,
//! darunter die unerklärten Zeilen. Unerklärt ist nie ein Fehler — es steht
//! im neutralen Stil „not observed in the session", nie in Rot.

use minds_core::SessionId;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use minds_reader::artifact::{
    ArtifactState, CommitArtifact, NOT_OBSERVED, provenance_note, summary,
};
use minds_reader::model::{EpochLink, EpochReport, EvidenceReport, LEGACY_SENTENCE};
use minds_reader::reconcile::ReconClass;

use crate::theme;

/// Die Sektionen, in Anzeige-Reihenfolge — muss zu
/// [`crate::app::EVIDENCE_SECTIONS`] passen (testfixiert).
const SECTIONS: [&str; crate::app::EVIDENCE_SECTIONS] = [
    "INTEGRITY",
    "COVERAGE",
    "ARTIFACT",
    "EPOCHS",
    "SIGNATURE",
    "INTERPRETATION",
    "LIMITS",
];

/// Zeichnet den Report. `report: None` ist Legacy — ein ehrlicher Zustand
/// mit einem Satz, kein leerer Bildschirm.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    id: SessionId,
    report: Option<&EvidenceReport>,
    uninterpreted: usize,
    artifacts: &[CommitArtifact],
    cursor: usize,
) {
    let short: String = id.to_string().chars().take(11).collect();
    let Some(report) = report else {
        legacy(frame, area, &short, artifacts);
        return;
    };

    let verdict_h = 2 + SECTIONS.len() as u16;
    let [head, verdict_area, detail_area] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(verdict_h),
        Constraint::Min(1),
    ])
    .areas(area);

    // Kopf: die Seal-Karte — Titel trägt den Zustand als EIN Wort
    // (SEALED/INCOMPLETE/TAMPERED, dieselbe Familie wie der CLI-Block aus
    // `minds checkpoint`), darin Session, Verdikt-Wort, Leitsatz und die
    // Kennzahlen. Der Leitsatz behauptet nie mehr, als das Verdikt trägt.
    let (v_glyph, v_word, v_style) =
        theme::provenance(&minds_reader::model::Provenance::Chained(report.state));
    let state = &report.state;
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!("EVIDENCE {short}…  "),
                    theme::title().fg(theme::AGENT),
                ),
                Span::styled(format!("{v_glyph} {v_word}"), v_style.patch(theme::title())),
            ]),
            Line::from(Span::styled(report.sentence(), theme::dim())),
            Line::from(Span::raw(state.metrics_line())),
        ])
        .block(
            Block::bordered()
                .title(format!(" SESSION {} ", v_word.to_uppercase()))
                .title_style(v_style.patch(theme::title()))
                .border_style(v_style),
        ),
        head,
    );

    // Ebene 1: das Verdikt — sieben Zeilen, je Achse eine Aussage.
    let lines: Vec<Line> = rows(report, uninterpreted, artifacts)
        .into_iter()
        .enumerate()
        .map(|(i, (glyph, status, style))| {
            let mut line = Line::from(vec![
                Span::styled(format!(" {glyph} "), style),
                Span::styled(format!("{:<15}", SECTIONS[i]), style.patch(theme::title())),
                Span::raw(" "),
                Span::styled(status, style),
            ]);
            if i == cursor {
                line = line.style(theme::cursor());
            }
            line
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" VERDICT ")
                .title_style(theme::title()),
        ),
        verdict_area,
    );

    // Ebene 2 + 3: das Detail folgt dem Fokus — wie der Inspector der
    // Why-Kette, ohne dass Enter etwas verspricht.
    let (title, lines) = detail(report, uninterpreted, artifacts, cursor);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(format!(" {title} "))
                .title_style(theme::title()),
        ),
        detail_area,
    );
}

fn legacy(frame: &mut Frame, area: Rect, short: &str, artifacts: &[CommitArtifact]) {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("EVIDENCE {short}…  "),
                theme::title().fg(theme::AGENT),
            ),
            Span::styled("· legacy", theme::dim().patch(theme::title())),
        ]),
        Line::default(),
        Line::from(Span::raw(LEGACY_SENTENCE)),
        Line::default(),
        Line::from(Span::styled(
            "This session was captured before the evidence chain. It never gets a chain \
             attributed after the fact — its honest answer is this state.",
            theme::dim(),
        )),
    ];
    // Der Abgleich hängt nicht an der Chain: Auch eine Legacy-Session kann
    // Zeilen eines Commits erklären — oder eben nicht.
    if !artifacts.is_empty() {
        lines.push(Line::default());
        lines.extend(artifact(artifacts));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(" SESSION · LEGACY ")
                .title_style(theme::dim().patch(theme::title()))
                .border_style(theme::dim()),
        ),
        area,
    );
}

/// Glyph, Statustext und Stil je Sektion — die Verdikt-Zeilen.
fn rows(
    report: &EvidenceReport,
    uninterpreted: usize,
    artifacts: &[CommitArtifact],
) -> Vec<(String, String, Style)> {
    let state = &report.state;
    let scope = report.scope.as_deref().unwrap_or("?");
    let ok = Style::default().fg(theme::OK);
    let warn = Style::default().fg(theme::REVIEW);
    let mut out = Vec::with_capacity(SECTIONS.len());

    // Integrität: wurde das Seal-Material verändert?
    out.push(
        if state.verdict == minds_reader::model::EvidenceVerdict::Tampered {
            (
                "✗".into(),
                "TAMPERED — seal material altered".into(),
                Style::default().fg(theme::DELETE),
            )
        } else {
            (
                "✓".into(),
                format!("intact · {} seal(s) hash-valid", state.seals),
                ok,
            )
        },
    );

    // Coverage: immer gescoped — „vollständig" nur innerhalb der Grenze.
    out.push(if state.gaps == 0 && state.pre_chain == 0 {
        (
            "✓".into(),
            format!("COMPLETE · {} event(s) · within {scope}", state.events),
            ok,
        )
    } else {
        (
            "!".into(),
            format!(
                "{} gap(s) · {} pre-chain · {} event(s) · within {scope}",
                state.gaps, state.pre_chain, state.events
            ),
            warn,
        )
    });

    // Artefakt: wie viel des Commits die Session erklärt.
    out.push(artifact_row(artifacts));

    // Epochen: schließt sich die previous-Kette?
    let mut epochs = if state.chain_closed {
        (
            "✓".into(),
            format!("chain closed · {} epoch(s)", state.seals),
            ok,
        )
    } else {
        (
            "!".into(),
            format!("chain open · {} epoch(s)", state.seals),
            warn,
        )
    };
    if state.rejected {
        epochs.1.push_str(" · block seal in the chain");
        epochs.0 = "!".into();
        epochs.2 = warn;
    }
    out.push(epochs);

    // Signatur: unsigniert ist ein Zustand, kein Fehler — und Anwesenheit
    // ist keine Prüfung.
    out.push(if state.signed == 0 {
        (
            "○".into(),
            "NOT SIGNED — unsigned ≠ invalid".into(),
            theme::dim(),
        )
    } else {
        (
            "✓".into(),
            format!(
                "{}/{} signed · validity is checked by `minds verify`",
                state.signed, state.seals
            ),
            ok,
        )
    });

    // Deutung: die dritte Achse, getrennt von Integrität und Coverage.
    out.push(if uninterpreted == 0 {
        ("✓".into(), "all tool calls interpreted".into(), ok)
    } else {
        (
            "◐".into(),
            format!("{uninterpreted} call(s) observed, not interpreted"),
            warn,
        )
    });

    // Grenzen: keine Achse, aber Teil des Reports.
    out.push((
        "·".into(),
        format!(
            "{} named limits of the proof model",
            report.limitations.len()
        ),
        theme::dim(),
    ));
    out
}

/// Der Detail-Block der fokussierten Sektion.
fn detail(
    report: &EvidenceReport,
    uninterpreted: usize,
    artifacts: &[CommitArtifact],
    cursor: usize,
) -> (&'static str, Vec<Line<'static>>) {
    match cursor {
        0 => ("INTEGRITY", integrity(report)),
        1 => ("COVERAGE", coverage(report)),
        2 => ("ARTIFACT", artifact(artifacts)),
        3 => ("EPOCHS", epochs(report)),
        4 => ("SIGNATURE", signature(report)),
        5 => ("INTERPRETATION", interpretation(uninterpreted)),
        _ => ("LIMITS", limitations(report)),
    }
}

/// Ob ein abgeglichener Commit irgendetwas Unerklärtes trägt — Zeilen,
/// ganze Dateien (Löschung, binär) oder strukturelle Änderungen.
fn any_unexplained(assessed: &minds_reader::artifact::Assessed) -> bool {
    !assessed.structural.is_empty()
        || assessed
            .recon
            .files
            .iter()
            .any(|f| f.class == ReconClass::Unexplained)
}

/// Ob ein abgeglichener Commit Zeilen trägt, die nur berichtet sind — ein
/// Tool-Claim ohne Zeugen.
fn any_reported_only(assessed: &minds_reader::artifact::Assessed) -> bool {
    assessed.recon.files.iter().any(|f| {
        f.class == ReconClass::ReportedOnly
            || matches!(&f.line_level, minds_reader::reconcile::LineLevel::Available(lines)
                if lines.iter().any(|l| l.class == ReconClass::ReportedOnly))
    })
}

/// Glyph und Stil der Artefakt-Zeile: unerklärt → neutral `◦`; nur durch
/// Claims belegt → `◇` (belegt, aber nicht bezeugt, also nicht grün); alles
/// bezeugt → `✓`.
fn artifact_mark(unexplained: bool, reported_only: bool) -> (String, Style) {
    let (neutral_glyph, _, neutral) = theme::not_observed();
    if unexplained {
        (neutral_glyph.into(), neutral)
    } else if reported_only {
        let (glyph, _, style) = theme::recon(ReconClass::ReportedOnly);
        (glyph.into(), style)
    } else {
        ("✓".into(), Style::default().fg(theme::OK))
    }
}

/// Die Verdikt-Zeile der Artefakt-Sektion. Unerklärtes ist neutral
/// (`◦`, gedimmt) — nie die Warn- oder Fehlerfarbe.
fn artifact_row(artifacts: &[CommitArtifact]) -> (String, String, Style) {
    match artifacts {
        [] => (
            "·".into(),
            "not assessed (no linked commit)".into(),
            theme::dim(),
        ),
        [one] => match &one.state {
            ArtifactState::Assessed(assessed) => {
                let mut text = format!("{} · {}", summary(&assessed.recon), short_commit(one));
                let note = provenance_note(one);
                if !note.is_empty() {
                    text.push_str(&format!(" · {note}"));
                }
                let (glyph, style) =
                    artifact_mark(any_unexplained(assessed), any_reported_only(assessed));
                (glyph, text, style)
            }
            ArtifactState::Unavailable(why) => {
                ("·".into(), format!("not assessed ({why})"), theme::dim())
            }
            ArtifactState::Failed(_) => ("·".into(), "not assessed (error)".into(), theme::dim()),
        },
        many => {
            let assessed: Vec<_> = many
                .iter()
                .filter_map(|a| match &a.state {
                    ArtifactState::Assessed(assessed) => Some(assessed),
                    _ => None,
                })
                .collect();
            let backed: u64 = assessed.iter().map(|a| a.recon.backed_lines()).sum();
            let total: u64 = assessed.iter().map(|a| a.recon.total_changed_lines).sum();
            let mut text = format!(
                "artifact {backed}/{total} lines explained · {} commits",
                many.len()
            );
            let skipped = many.len() - assessed.len();
            if skipped > 0 {
                text.push_str(&format!(" · {skipped} not assessed"));
            }
            if many.iter().any(|a| a.inferred) {
                text.push_str(" · partly inferred links");
            }
            let (glyph, style) = artifact_mark(
                skipped > 0 || assessed.iter().any(|a| any_unexplained(a)),
                assessed.iter().any(|a| any_reported_only(a)),
            );
            (glyph, text, style)
        }
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

/// Das Detail der Artefakt-Sektion: je Commit die Zusammenfassung, dann je
/// Datei Klasse, Pfad und was unerklärt ist. Pfade sind fremdbestimmt und
/// laufen durch die Terminal-Härtung des Readers.
fn artifact(artifacts: &[CommitArtifact]) -> Vec<Line<'static>> {
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
    let mut lines = Vec::new();
    for (i, artifact) in artifacts.iter().enumerate() {
        if i > 0 {
            lines.push(Line::default());
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
        lines.push(Line::from(head));
        let assessed = match &artifact.state {
            ArtifactState::Assessed(assessed) => assessed,
            ArtifactState::Unavailable(why) => {
                lines.push(Line::from(Span::styled(
                    format!("{:<12} not assessed ({why})", ""),
                    theme::dim(),
                )));
                continue;
            }
            ArtifactState::Failed(err) => {
                lines.push(Line::from(Span::styled(
                    format!("{:<12} not assessed (error: {err})", ""),
                    theme::dim(),
                )));
                continue;
            }
        };
        lines.push(Line::from(vec![
            Span::raw(format!("{:<12} ", "")),
            Span::styled(summary(&assessed.recon), theme::title()),
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
            lines.push(Line::from(spans));
        }
        for structural in &assessed.structural {
            let (glyph, word, style) = theme::recon(ReconClass::Unexplained);
            lines.push(Line::from(vec![
                Span::styled(format!("  {glyph} "), style),
                Span::styled(format!("{word:<20}"), style),
                Span::raw(format!("{:<width$}", shown_path(&structural.path))),
                Span::styled(format!("  {} {NOT_OBSERVED}", structural.what), neutral),
            ]));
        }
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(format!("{neutral_glyph} "), neutral),
        Span::styled(NOT_OBSERVED, neutral),
        Span::styled(
            "  ·  ◇ reported only: a tool claim, no witness",
            theme::dim(),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "\"explained\" counts every line with evidence, reported only included.",
        theme::dim(),
    )));
    lines
}

fn short_hash(hash: &minds_core::ContentHash) -> String {
    hash.to_string().chars().take(14).collect()
}

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
            Span::raw(report.scope.clone().unwrap_or_else(|| "?".into())),
        ]),
        Line::from(vec![
            Span::styled(format!("{:<12} ", "Captured"), theme::dim()),
            Span::raw(format!(
                "{} event(s) · {} gap(s) · {} pre-chain",
                state.events, state.gaps, state.pre_chain
            )),
        ]),
        Line::default(),
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
    lines.push(Line::default());
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
    let mut lines = if uninterpreted == 0 {
        vec![Line::from(Span::styled(
            "✓ All tool calls are interpreted.",
            Style::default().fg(theme::OK),
        ))]
    } else {
        vec![
            Line::from(Span::styled(
                format!("◐ {uninterpreted} call(s) observed, but not interpreted."),
                Style::default().fg(theme::REVIEW),
            )),
            Line::from(Span::raw(
                "Observed means: name and raw arguments are evidence — the effect is not normalized.",
            )),
        ]
    };
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Interpretation is separate from integrity and coverage: same evidence + same adapter ⇒ same interpretation; `minds reinterpret` shows the stored and the current interpretation side by side.",
        theme::dim(),
    )));
    lines
}

fn limitations(report: &EvidenceReport) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Minds does NOT prove:", theme::title())),
        Line::default(),
    ];
    for limit in report.limitations {
        lines.push(Line::from(vec![
            Span::styled("• ", theme::dim()),
            Span::raw(*limit),
        ]));
    }
    lines
}
