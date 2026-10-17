//! Der Vertrauenszustand als **eine** Komponente („Trust Strip“, A4): drei
//! feste Slots in fester Reihenfolge — Integrität · Coverage · Deutung —,
//! nie verschmolzen, nie ein Score, nie eine Ampel. Dieselbe Darstellung in
//! vier Dichten; jede Stelle der Oberfläche, die einen Seal- oder
//! Urteilszustand zeigt, holt Glyph, Wort und Satz von hier (oder aus
//! `theme`).
//!
//! Die Achsen rechnet der Reader ([`TrustAxes`]); hier wird nur gezeichnet.
//! Das Integritätswort bleibt das der CLI: `sealed`, `TAMPERED`, `legacy`.
//! „sealed“ heißt nicht „verified“ — das prüft erst `minds verify`.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use minds_reader::model::{
    CoverageAxis, EvidenceReport, IntegrityAxis, InterpretationAxis, TrustAxes,
};

use crate::theme;
use crate::verify::Overall;

/// Wie dicht die Komponente zeichnet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    /// Drei Glyphen, je Slot eines (`◈ ● ◐`) — Übersichten, die Kind-Zeile
    /// der Story.
    Glyphs,
    /// Das Integritätswort, dahinter Coverage und Deutung als Glyph
    /// (`◈ sealed ● ◐`) — die Sessions-Liste: Legacy bleibt dort als Wort
    /// sichtbar, nie ein bloßes Zeichen.
    Compact,
    /// Glyph und Wort je Slot (`◈ sealed · ● complete · ◐ 2 uninterpreted`)
    /// — Köpfe, Fußzeile.
    Short,
    /// Eine Zeile je Achse mit Satz und Grenze — Evidence.
    Full,
}

/// Ein Slot: Glyph, Wort, Satz in einfacher Sprache und Stil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// Das Achsen-Label.
    pub label: &'static str,
    /// Ein Glyph aus dem Register (epistemisch).
    pub glyph: &'static str,
    /// Das Wort.
    pub word: String,
    /// Ein Satz, was das heißt — und wo die Aussage endet.
    pub sentence: String,
    /// Der Stil; Farbe trägt nie allein.
    pub style: Style,
}

fn slot(
    label: &'static str,
    glyph: &'static str,
    word: String,
    sentence: String,
    style: Style,
) -> Slot {
    Slot {
        label,
        glyph,
        word,
        sentence,
        style,
    }
}

/// Der Integritäts-Slot.
pub fn integrity(axis: IntegrityAxis) -> Slot {
    match axis {
        IntegrityAxis::Sealed => slot(
            "Integrity",
            "◈",
            "sealed".into(),
            "Seal material matches — sealed is not verified; signatures are checked by `minds verify`."
                .into(),
            Style::default().fg(theme::OK),
        ),
        IntegrityAxis::Tampered => {
            let (glyph, word, style) = theme::tampered();
            slot(
                "Integrity",
                glyph,
                word.into(),
                "Seal material was altered — cryptographic verification fails.".into(),
                style,
            )
        }
        IntegrityAxis::Legacy => slot(
            "Integrity",
            "·",
            "legacy".into(),
            "Captured before the evidence chain — there is nothing to check.".into(),
            theme::dim(),
        ),
    }
}

/// Der Coverage-Slot.
pub fn coverage(axis: CoverageAxis) -> Slot {
    match axis {
        CoverageAxis::Complete => slot(
            "Coverage",
            "●",
            "complete".into(),
            "No gap in the observed range — complete only within the observation boundary.".into(),
            Style::default().fg(theme::OK),
        ),
        CoverageAxis::Incomplete => slot(
            "Coverage",
            "!",
            "incomplete".into(),
            "Gaps or events before the chain — not every event is attested. Incomplete is not tampered."
                .into(),
            Style::default().fg(theme::REVIEW),
        ),
        CoverageAxis::NotAssessable => slot(
            "Coverage",
            "?",
            "not assessable".into(),
            "The seal material is altered — its coverage statement cannot be trusted.".into(),
            Style::default().fg(theme::REVIEW),
        ),
        CoverageAxis::NotRecorded => slot(
            "Coverage",
            "·",
            "not recorded".into(),
            "No coverage was recorded for this session.".into(),
            theme::dim(),
        ),
    }
}

/// Der Deutungs-Slot.
pub fn interpretation(axis: InterpretationAxis) -> Slot {
    match axis {
        InterpretationAxis::Interpreted => slot(
            "Interpretation",
            "✓",
            "interpreted".into(),
            "Every tool call has a known effect.".into(),
            Style::default().fg(theme::OK),
        ),
        InterpretationAxis::Uninterpreted(n) => slot(
            "Interpretation",
            "◐",
            format!("{n} uninterpreted"),
            format!(
                "{n} tool call(s) observed, but not interpreted — observed, not wrong: the effect is unknown."
            ),
            Style::default().fg(theme::REVIEW),
        ),
    }
}

/// Glyph, Wort und Stil der Deutungs-Achse für **einen** ungedeuteten
/// Aufruf — ohne Zahl, für die Schrittzeile der Story (C11): dasselbe
/// Vokabular wie [`interpretation`], nur ohne den Zähler.
pub fn uninterpreted_one() -> (&'static str, &'static str, Style) {
    let slot = interpretation(InterpretationAxis::Uninterpreted(1));
    (slot.glyph, "uninterpreted", slot.style)
}

/// Die drei Slots in fester Reihenfolge.
pub fn slots(axes: &TrustAxes) -> [Slot; 3] {
    [
        integrity(axes.integrity),
        coverage(axes.coverage),
        interpretation(axes.interpretation),
    ]
}

/// Die einzeiligen Dichten als Spans; `Full` steht in [`render`] als eine
/// Zeile je Achse (hier, einzeilig, wie `Short`).
pub fn spans(axes: &TrustAxes, density: Density) -> Vec<Span<'static>> {
    let [i, c, d] = slots(axes);
    match density {
        Density::Glyphs => vec![
            Span::styled(i.glyph, i.style),
            Span::raw(" "),
            Span::styled(c.glyph, c.style),
            Span::raw(" "),
            Span::styled(d.glyph, d.style),
        ],
        Density::Compact => vec![
            Span::styled(format!("{} {}", i.glyph, i.word), i.style),
            Span::raw(" "),
            Span::styled(c.glyph, c.style),
            Span::raw(" "),
            Span::styled(d.glyph, d.style),
        ],
        Density::Short | Density::Full => vec![
            Span::styled(format!("{} {}", i.glyph, i.word), i.style),
            Span::styled(" · ", theme::dim()),
            Span::styled(format!("{} {}", c.glyph, c.word), c.style),
            Span::styled(" · ", theme::dim()),
            Span::styled(format!("{} {}", d.glyph, d.word), d.style),
        ],
    }
}

/// Die Komponente als Zeilen: `Full` eine Zeile je Achse, jede andere
/// Dichte eine einzige Zeile.
pub fn render(axes: &TrustAxes, density: Density) -> Vec<Line<'static>> {
    match density {
        Density::Full => lines(axes),
        _ => vec![Line::from(spans(axes, density))],
    }
}

/// Die Dichte `Full`: eine Zeile je Achse — Label, Glyph, Wort, Satz.
pub fn lines(axes: &TrustAxes) -> Vec<Line<'static>> {
    slots(axes)
        .into_iter()
        .map(|s| {
            Line::from(vec![
                Span::styled(format!("{:<15}", s.label), theme::title()),
                Span::styled(format!("{} {}", s.glyph, s.word), s.style),
                Span::styled(format!(" — {}", s.sentence), theme::dim()),
            ])
        })
        .collect()
}

/// Die drei Achsenzeilen des Evidence-Verdikts (Glyph, Text mit den
/// Kennzahlen, Stil) — dieselben Glyphen und Wörter wie überall, ergänzt
/// um das, was die Aussage begrenzt.
pub fn evidence_rows(
    report: &EvidenceReport,
    uninterpreted: usize,
) -> [(String, String, Style); 3] {
    let state = &report.state;
    let scope = report.scope.as_deref().unwrap_or("?");
    let axes = TrustAxes::of(
        &minds_reader::model::Provenance::Chained(*state),
        uninterpreted,
    );
    let [i, c, d] = slots(&axes);
    let integrity = match axes.integrity {
        IntegrityAxis::Tampered => format!("{} — seal material altered", i.word.to_uppercase()),
        _ => format!(
            "{} · {} seal(s) hash-valid",
            i.word.to_uppercase(),
            state.seals
        ),
    };
    let coverage = match axes.coverage {
        CoverageAxis::Complete => format!(
            "{} · {} event(s) · within {scope}",
            c.word.to_uppercase(),
            state.events
        ),
        // Keine Zahlen aus verändertem Material: Die Coverage-Aussage trägt
        // nicht, also nennt die Zeile auch keine.
        CoverageAxis::NotAssessable => {
            format!("{} — the seal material is altered", c.word.to_uppercase())
        }
        _ => format!(
            "{} · {} gap(s) · {} pre-chain · {} event(s) · within {scope}",
            c.word.to_uppercase(),
            state.gaps,
            state.pre_chain,
            state.events
        ),
    };

    let interpretation = match axes.interpretation {
        InterpretationAxis::Interpreted => format!("{} — all tool calls", d.word.to_uppercase()),
        InterpretationAxis::Uninterpreted(_) => {
            format!(
                "{} — call(s) observed, not interpreted",
                d.word.to_uppercase()
            )
        }
    };
    [
        (i.glyph.to_string(), integrity, i.style),
        (c.glyph.to_string(), coverage, c.style),
        (d.glyph.to_string(), interpretation, d.style),
    ]
}

/// Das Kopf-Urteil des Verify-Tabs — **die Wörter von `minds verify`**,
/// wörtlich (VERIFIED, VERIFIED, INCOMPLETE, TAMPERED, NOT VERIFIABLE).
pub fn verify_headline(overall: Overall) -> (String, Style) {
    match overall {
        Overall::Verified => ("✓ VERIFIED".to_string(), Style::default().fg(theme::OK)),
        Overall::Incomplete => (
            "! VERIFIED, INCOMPLETE".to_string(),
            Style::default().fg(theme::REVIEW),
        ),
        Overall::NotVerifiable => (
            "? NOT VERIFIABLE".to_string(),
            Style::default().fg(theme::REVIEW),
        ),
        Overall::Tampered => {
            let (glyph, word, style) = theme::tampered();
            (format!("{glyph} {word}"), style)
        }
        Overall::Checking => ("… CHECKING".to_string(), theme::dim()),
        Overall::NotChecked => (
            "? NOT CHECKED — run minds verify, or press v".to_string(),
            Style::default().fg(theme::REVIEW),
        ),
        Overall::NoSession => ("· NO SESSION".to_string(), theme::dim()),
    }
}

/// Die Seal-Marke einer Session-Zeile im Verify-Tab: kein eigenes Verdikt,
/// nur Fakten — ✗ nur bei Manipulation.
pub fn verify_session_mark(tampered: bool) -> (&'static str, &'static str, Style) {
    if tampered {
        theme::tampered()
    } else {
        ("·", "hash-valid", theme::dim())
    }
}

/// Die Integritätszeile des Verify-Tabs, wörtlich wie `minds verify` sie
/// sagt — ohne geprüftes Urteil weder ✓ noch „intact".
pub fn verify_integrity(
    overall: Overall,
    seals: usize,
    signed: usize,
    without: &str,
    pending_word: &str,
) -> (&'static str, Style, String) {
    let unchecked = matches!(overall, Overall::Checking | Overall::NotChecked);
    if overall == Overall::Tampered {
        let (glyph, word, style) = theme::tampered();
        (
            glyph,
            style,
            format!("{word} — seal material altered or a witness signature invalid"),
        )
    } else if unchecked {
        (
            "·",
            theme::dim(),
            format!("{seals} seal(s) hash-valid · {pending_word}{without}"),
        )
    } else if overall == Overall::NotVerifiable {
        // Kein ✓ neben „nichts prüfbar".
        (
            "·",
            theme::dim(),
            format!("{seals} seal(s) hash-valid{without}"),
        )
    } else {
        // „trägt eine Signatur", nicht „gültig signiert": Gültigkeit steht in
        // der Stufe (gegen die vertrauenswürdigen Signer).
        (
            "✓",
            Style::default().fg(theme::OK),
            format!(
                "intact · {seals} seal(s) hash-valid · {signed}/{seals} carry a signature{without}"
            ),
        )
    }
}

/// Die Coverage-Zeile des Verify-Tabs: Das Wort folgt dem Urteil von
/// `verify`, nicht den Lücken des Readers — nur `verify` sieht alle Seals
/// des Namensraums.
pub fn verify_coverage(overall: Overall, pending_word: &str) -> (&'static str, Style, &str) {
    match overall {
        Overall::Tampered => ("·", theme::dim(), "not assessable"),
        Overall::Verified => ("✓", Style::default().fg(theme::OK), "complete"),
        Overall::Incomplete => ("!", Style::default().fg(theme::REVIEW), "incomplete"),
        Overall::NotVerifiable => ("·", theme::dim(), "not verifiable (no seal)"),
        _ => ("·", theme::dim(), pending_word),
    }
}
