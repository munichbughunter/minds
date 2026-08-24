//! Die visuelle Sprache: eine Farbe und ein Glyph je Bedeutung — an genau
//! einer Stelle, damit Activity, Graph und Why dasselbe sagen.
//!
//! Farbe trägt nie allein: Jede Bedeutung hat auch ein Glyph **und** ein
//! Wort, damit die Anzeige in einem monochromen Terminal dasselbe aussagt.
//! Das gilt besonders für die Evidenz — eine Vermutung, die nur grau ist,
//! sähe in `NO_COLOR` aus wie ein Beleg.
//!
//! Nur Unicode, das in gängigen Monospace-Fonts sicher ist; keine Emoji.

use minds_core::{EvidenceMark, EvidenceSource, EvidenceStatus};
use minds_reader::graph::{NodeKind, ToolKind};
use minds_reader::model::Verdict;
use minds_reader::model::{EvidenceVerdict, Provenance};
use ratatui::style::{Color, Modifier, Style};

/// Mensch und Absicht.
pub const HUMAN: Color = Color::Cyan;
/// Agent.
pub const AGENT: Color = Color::Magenta;
/// Lesen, Kontext.
pub const READ: Color = Color::Blue;
/// Schreiben, Mutation.
pub const EDIT: Color = Color::Yellow;
/// Ausführen.
pub const EXEC: Color = Color::White;
/// Löschen, Fehlschlag.
pub const DELETE: Color = Color::Red;
/// Git, Änderung.
pub const CHANGE: Color = Color::Indexed(93);
/// Review, Warnung.
pub const REVIEW: Color = Color::Indexed(214);
/// Erfolg, belegt.
pub const OK: Color = Color::Green;
/// Vermutet, sekundär, degradiert.
pub const DIM: Color = Color::DarkGray;

/// Glyph, Wort und Stil einer Evidenz-Klasse; `None` heißt „mit keinem
/// Commit verbunden".
///
/// Zwei Dimensionen seit ADR-0011: Das **Glyph** trägt die Quelle (woher die
/// Aussage stammt), der **Status-Modifikator** dahinter sagt, ob sie geprüft
/// wurde — Glyph **und** Wort, nie nur Farbe. `● ✓` ist ein nachgerechneter
/// Beleg; `● ?` ist beobachtet, aber nie geprüft — der Unterschied, den das
/// alte Alphabet nicht aussprechen konnte.
pub fn evidence(evidence: Option<EvidenceMark>) -> (String, String, Style) {
    let Some(mark) = evidence else {
        return ("·".into(), "unverknüpft".into(), Style::default().fg(DIM));
    };
    let (glyph, word, style) = match mark.source {
        EvidenceSource::Observed => ("●", "observed", Style::default().fg(OK)),
        EvidenceSource::ContentDerived => ("◆", "content", Style::default().fg(OK)),
        EvidenceSource::HumanDeclared => ("◇", "declared", Style::default().fg(EDIT)),
        EvidenceSource::Heuristic => ("○", "inferred [vermutet]", Style::default().fg(DIM)),
    };
    let (modifier, status_word, style) = match mark.status {
        EvidenceStatus::Verified => ("✓", "nachgerechnet", style),
        EvidenceStatus::Partial => ("~", "teilweise geprüft", style),
        // Ungeprüft dimmt auch eine „gute" Quelle — beobachtet heißt nicht
        // geprüft, und das darf man sehen.
        EvidenceStatus::Unknown => ("?", "ungeprüft", style.add_modifier(Modifier::DIM)),
        EvidenceStatus::Missing => ("✗", "Beleg fehlt", Style::default().fg(DELETE)),
    };
    (
        format!("{glyph} {modifier}"),
        format!("{word} [{status_word}]"),
        style,
    )
}

/// Glyph, Wort und Stil der Herkunftslage (ADR-0011): der Zustand der Seals
/// einer Session — oder `legacy`, der explizite Vor-Chain-Zustand
/// (Invariante: Legacy bleibt Legacy, kein bloßes „nichts da").
pub fn provenance(provenance: &Provenance) -> (&'static str, &'static str, Style) {
    match provenance {
        Provenance::Chained(state) => match state.verdict {
            EvidenceVerdict::Verified => ("◈", "versiegelt", Style::default().fg(OK)),
            EvidenceVerdict::Incomplete => ("!", "unvollständig", Style::default().fg(REVIEW)),
            EvidenceVerdict::Tampered => ("✗", "MANIPULIERT", Style::default().fg(DELETE)),
        },
        Provenance::Legacy => ("·", "legacy", Style::default().fg(DIM)),
    }
}

/// Glyph, Wort und Stil eines Verdicts.
pub fn verdict(verdict: Verdict) -> (&'static str, &'static str, Style) {
    match verdict {
        Verdict::Open => ("⚠", "offen", Style::default().fg(REVIEW)),
        Verdict::Approved => ("✓", "approved", Style::default().fg(OK)),
        Verdict::Rejected => ("✕", "rejected", Style::default().fg(DELETE)),
        Verdict::NeedsWork => ("↻", "needs work", Style::default().fg(REVIEW)),
    }
}

/// Glyph, Wort und Stil eines Tool-Effekts.
pub fn tool(kind: ToolKind) -> (&'static str, &'static str, Style) {
    match kind {
        ToolKind::Read => ("◇", "READ", Style::default().fg(READ)),
        ToolKind::Edit => ("✎", "EDIT", Style::default().fg(EDIT)),
        ToolKind::Exec => ("▶", "EXEC", Style::default().fg(EXEC)),
        ToolKind::Delete => ("✕", "DELETE", Style::default().fg(DELETE)),
        ToolKind::Other => ("·", "TOOL", Style::default().fg(DIM)),
        // Beobachtet, nicht gedeutet (ADR-0011): halb sichtbar — Wirkung
        // unbekannt, und das darf man sehen.
        ToolKind::Uninterpreted => ("◐", "BEOBACHTET", Style::default().fg(REVIEW)),
    }
}

/// Glyph, Wort und Stil eines Graph-Knotens.
pub fn node(kind: &NodeKind) -> (&'static str, &'static str, Style) {
    match kind {
        NodeKind::Intent => ("●", "YOU", Style::default().fg(HUMAN)),
        NodeKind::Agent => ("◉", "AGENT", Style::default().fg(AGENT)),
        NodeKind::Turn(_) => ("·", "TURN", Style::default().fg(DIM)),
        NodeKind::Tool(kind) => tool(*kind),
        NodeKind::Subagent(_) => ("◉", "SUBAGENT", Style::default().fg(AGENT)),
        NodeKind::Handover { .. } => ("⇄", "ÜBERGABE", Style::default().fg(OK)),
        NodeKind::Change(_) => ("◆", "CHANGE", Style::default().fg(CHANGE)),
        NodeKind::Commit(_) => ("◆", "COMMIT", Style::default().fg(CHANGE)),
        NodeKind::Review(v) => {
            let (glyph, _, style) = verdict(*v);
            (glyph, "REVIEW", style)
        }
    }
}

/// Der Stil einer Spur (Box-Zeichen) — menschlich bis zum Agenten, danach
/// der Agent.
pub fn lane(depth: usize) -> Style {
    if depth == 0 {
        Style::default().fg(HUMAN)
    } else {
        Style::default().fg(AGENT)
    }
}

/// Die Cursorzeile.
pub fn cursor() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// Gedimmter Nebentext.
pub fn dim() -> Style {
    Style::default().fg(DIM)
}

/// Hervorgehobener Kopf.
pub fn title() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}
