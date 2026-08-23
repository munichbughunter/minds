//! Die visuelle Sprache: eine Farbe und ein Glyph je Bedeutung — an genau
//! einer Stelle, damit Activity, Graph und Why dasselbe sagen.
//!
//! Farbe trägt nie allein: Jede Bedeutung hat auch ein Glyph **und** ein
//! Wort, damit die Anzeige in einem monochromen Terminal dasselbe aussagt.
//! Das gilt besonders für die Evidenz — eine Vermutung, die nur grau ist,
//! sähe in `NO_COLOR` aus wie ein Beleg.
//!
//! Nur Unicode, das in gängigen Monospace-Fonts sicher ist; keine Emoji.

use minds_core::Evidence;
use minds_reader::graph::{NodeKind, ToolKind};
use minds_reader::model::Verdict;
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
pub fn evidence(evidence: Option<Evidence>) -> (&'static str, &'static str, Style) {
    match evidence {
        Some(Evidence::Observed) => ("●", "observed", Style::default().fg(OK)),
        Some(Evidence::Content) => ("◆", "content", Style::default().fg(OK)),
        Some(Evidence::Declared) => ("◇", "declared", Style::default().fg(EDIT)),
        Some(Evidence::Inferred) => ("○", "inferred [vermutet]", Style::default().fg(DIM)),
        None => ("·", "unverknüpft", Style::default().fg(DIM)),
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
