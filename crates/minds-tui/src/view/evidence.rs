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

use minds_core::SessionId;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use minds_reader::model::{EpochLink, EpochReport, EvidenceReport, LEGACY_SENTENCE};

use crate::theme;

/// Die Sektionen, in Anzeige-Reihenfolge — muss zu
/// [`crate::app::EVIDENCE_SECTIONS`] passen (testfixiert).
const SECTIONS: [&str; crate::app::EVIDENCE_SECTIONS] = [
    "INTEGRITÄT",
    "COVERAGE",
    "EPOCHEN",
    "SIGNATUR",
    "DEUTUNG",
    "GRENZEN",
];

/// Zeichnet den Report. `report: None` ist Legacy — ein ehrlicher Zustand
/// mit einem Satz, kein leerer Bildschirm.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    id: SessionId,
    report: Option<&EvidenceReport>,
    uninterpreted: usize,
    cursor: usize,
) {
    let short: String = id.to_string().chars().take(11).collect();
    let Some(report) = report else {
        legacy(frame, area, &short);
        return;
    };

    let verdict_h = 2 + SECTIONS.len() as u16;
    let [head, verdict_area, detail_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(verdict_h),
        Constraint::Min(1),
    ])
    .areas(area);

    // Kopf: Session, Verdikt-Wort — und der Leitsatz, der nie mehr
    // behauptet, als das Verdikt trägt.
    let (v_glyph, v_word, v_style) =
        theme::provenance(&minds_reader::model::Provenance::Chained(report.state));
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
        ]),
        head,
    );

    // Ebene 1: das Verdikt — sechs Zeilen, je Achse eine Aussage.
    let lines: Vec<Line> = rows(report, uninterpreted)
        .into_iter()
        .enumerate()
        .map(|(i, (glyph, status, style))| {
            let mut line = Line::from(vec![
                Span::styled(format!(" {glyph} "), style),
                Span::styled(format!("{:<12}", SECTIONS[i]), style.patch(theme::title())),
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
                .title(" VERDIKT ")
                .title_style(theme::title()),
        ),
        verdict_area,
    );

    // Ebene 2 + 3: das Detail folgt dem Fokus — wie der Inspector der
    // Why-Kette, ohne dass Enter etwas verspricht.
    let (title, lines) = detail(report, uninterpreted, cursor);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(format!(" {title} "))
                .title_style(theme::title()),
        ),
        detail_area,
    );
}

fn legacy(frame: &mut Frame, area: Rect, short: &str) {
    let lines = vec![
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
            "Diese Session wurde vor der Evidence-Chain erfasst. Sie bekommt nie nachträglich \
             eine Chain angedichtet — ihre ehrliche Auskunft ist dieser Zustand.",
            theme::dim(),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Glyph, Statustext und Stil je Sektion — die Verdikt-Zeilen.
fn rows(report: &EvidenceReport, uninterpreted: usize) -> Vec<(String, String, Style)> {
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
                "MANIPULIERT — Seal-Material verändert".into(),
                Style::default().fg(theme::DELETE),
            )
        } else {
            (
                "✓".into(),
                format!("intakt · {} Seal(s) hash-valide", state.seals),
                ok,
            )
        },
    );

    // Coverage: immer gescoped — „vollständig" nur innerhalb der Grenze.
    out.push(if state.gaps == 0 && state.pre_chain == 0 {
        (
            "✓".into(),
            format!(
                "VOLLSTÄNDIG · {} Event(s) · innerhalb {scope}",
                state.events
            ),
            ok,
        )
    } else {
        (
            "!".into(),
            format!(
                "{} Lücke(n) · {} pre-chain · {} Event(s) · innerhalb {scope}",
                state.gaps, state.pre_chain, state.events
            ),
            warn,
        )
    });

    // Epochen: schließt sich die previous-Kette?
    let mut epochs = if state.chain_closed {
        (
            "✓".into(),
            format!("Kette geschlossen · {} Epoche(n)", state.seals),
            ok,
        )
    } else {
        (
            "!".into(),
            format!("Kette offen · {} Epoche(n)", state.seals),
            warn,
        )
    };
    if state.rejected {
        epochs.1.push_str(" · Block-Seal in der Kette");
        epochs.0 = "!".into();
        epochs.2 = warn;
    }
    out.push(epochs);

    // Signatur: unsigniert ist ein Zustand, kein Fehler — und Anwesenheit
    // ist keine Prüfung.
    out.push(if state.signed == 0 {
        (
            "○".into(),
            "NICHT SIGNIERT — unsigniert ≠ ungültig".into(),
            theme::dim(),
        )
    } else {
        (
            "✓".into(),
            format!(
                "{}/{} signiert · Gültigkeit prüft `minds verify`",
                state.signed, state.seals
            ),
            ok,
        )
    });

    // Deutung: die dritte Achse, getrennt von Integrität und Coverage.
    out.push(if uninterpreted == 0 {
        ("✓".into(), "alle Tool-Aufrufe gedeutet".into(), ok)
    } else {
        (
            "◐".into(),
            format!("{uninterpreted} Aufruf(e) beobachtet, nicht gedeutet"),
            warn,
        )
    });

    // Grenzen: keine Achse, aber Teil des Reports.
    out.push((
        "·".into(),
        format!(
            "{} benannte Grenzen des Proof-Modells",
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
    cursor: usize,
) -> (&'static str, Vec<Line<'static>>) {
    match cursor {
        0 => ("INTEGRITÄT", integrity(report)),
        1 => ("COVERAGE", coverage(report)),
        2 => ("EPOCHEN", epochs(report)),
        3 => ("SIGNATUR", signature(report)),
        4 => ("DEUTUNG", interpretation(uninterpreted)),
        _ => ("GRENZEN", limitations(report)),
    }
}

fn short_hash(hash: &minds_core::ContentHash) -> String {
    hash.to_string().chars().take(14).collect()
}

fn integrity(report: &EvidenceReport) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{:<12} ", "Algorithmus"), theme::dim()),
            Span::raw("blake3 · derive_key, Kontexte minds/evidence/v1/*"),
        ]),
        Line::default(),
    ];
    for epoch in &report.epochs {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<12} ", "Seal"), theme::dim()),
            Span::raw(format!(
                "{}…  Root {}…  {} Event(s)",
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
        "✓ Extern prüfbar: Seal-Identität (seal_id = Hash des Seal-Texts) und Signatur.",
        Style::default().fg(theme::OK),
    )));
    lines.push(Line::from(Span::styled(
        "— Chain-Root: nur lokal mit Journal + Session-Salt reproduzierbar (Anti-Orakel).",
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
            Span::styled(format!("{:<12} ", "Erfasst"), theme::dim()),
            Span::raw(format!(
                "{} Event(s) · {} Lücke(n) · {} pre-chain",
                state.events, state.gaps, state.pre_chain
            )),
        ]),
        Line::default(),
        Line::from(Span::styled("Beobachtungsgrenze", theme::title())),
        Line::from(Span::styled(
            "✓ Agent-Hook-Events (scope im Seal)",
            Style::default().fg(theme::OK),
        )),
    ];
    // „Nicht erfasst" ist KEINE Lücke: Es liegt außerhalb des Scopes —
    // visuell ein anderer Zustand (— statt !).
    for outside in [
        "— Subprozesse außerhalb der Hook-Grenze  · nicht erfasst, keine Lücke",
        "— Netzwerkaktivität                      · nicht erfasst, keine Lücke",
        "— das Fenster zwischen Append und Seal   · nicht erfasst, keine Lücke",
    ] {
        lines.push(Line::from(Span::styled(outside, theme::dim())));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::raw(
        "Fehlende Evidence beweist nicht, dass nichts geschah — sie heißt: Minds kann es nicht belegen.",
    )));
    lines
}

fn epoch_link_word(link: EpochLink) -> &'static str {
    match link {
        EpochLink::Start => "Kettenanfang",
        EpochLink::Chained => "verkettet (previous belegt)",
        EpochLink::RejectedBefore => "Vorgänger-Epoche zurückgewiesen",
        EpochLink::Unresolved => "previous nicht auflösbar — Kette offen",
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
            "Block-Seal · Nutzlast zurückgewiesen",
            Style::default().fg(theme::REVIEW),
        )
    };
    vec![
        Line::from(vec![
            Span::styled(format!("Epoche {}/{total}  ", i + 1), theme::title()),
            Span::raw(format!(
                "#{}–#{} · {} Event(s) · {} Lücke(n)",
                epoch.first_seq, epoch.last_seq, epoch.events, epoch.gaps
            )),
        ]),
        Line::from(vec![
            Span::raw("  "),
            seal_word,
            Span::raw(format!(
                "  {}…  {} · {}",
                short_hash(&epoch.seal_id),
                if epoch.signed {
                    "signiert"
                } else {
                    "unsigniert"
                },
                epoch_link_word(epoch.link)
            )),
        ]),
    ]
}

fn signature(report: &EvidenceReport) -> Vec<Line<'static>> {
    let state = &report.state;
    if state.signed == 0 {
        return vec![
            Line::from(Span::styled("○ NICHT SIGNIERT", theme::title())),
            Line::default(),
            Line::from(Span::raw(
                "Die Seals sind kryptographisch selbstkonsistent (content-adressiert), aber niemand steht mit einem Schlüssel dafür ein.",
            )),
            Line::from(Span::styled(
                "Unsigniert ist nicht ungültig — `minds sign --seal` rüstet die Signatur nach.",
                theme::dim(),
            )),
        ];
    }
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "✓ {}/{} Seal(s) tragen eine Signatur",
            state.signed, state.seals
        ),
        Style::default().fg(theme::OK).patch(theme::title()),
    ))];
    for (i, epoch) in report.epochs.iter().enumerate() {
        lines.push(Line::from(Span::raw(format!(
            "  Epoche {}: {}",
            i + 1,
            if epoch.signed {
                "signiert (SSH)"
            } else {
                "unsigniert"
            }
        ))));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Anwesenheit ist keine Prüfung: Die Gültigkeit prüft `minds verify` gegen eine allowed_signers-Datei aus vertrauenswürdiger Quelle.",
        theme::dim(),
    )));
    lines
}

fn interpretation(uninterpreted: usize) -> Vec<Line<'static>> {
    let mut lines = if uninterpreted == 0 {
        vec![Line::from(Span::styled(
            "✓ Alle Tool-Aufrufe sind gedeutet.",
            Style::default().fg(theme::OK),
        ))]
    } else {
        vec![
            Line::from(Span::styled(
                format!("◐ {uninterpreted} Aufruf(e) beobachtet, aber nicht gedeutet."),
                Style::default().fg(theme::REVIEW),
            )),
            Line::from(Span::raw(
                "Beobachtet heißt: Name und Roh-Argumente sind Beweismittel — die Wirkung ist nicht normalisiert.",
            )),
        ]
    };
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Deutung ist von Integrität und Coverage getrennt: gleiche Evidence + gleicher Adapter ⇒ gleiche Deutung; `minds reinterpret` zeigt gespeicherte und aktuelle Deutung nebeneinander.",
        theme::dim(),
    )));
    lines
}

fn limitations(report: &EvidenceReport) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Minds beweist NICHT:", theme::title())),
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
