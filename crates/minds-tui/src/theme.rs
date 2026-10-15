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
use minds_reader::reconcile::ReconClass;
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

/// Die Farben, die einzelne Agenten in der Liste voneinander absetzen —
/// klein und fest, damit dieselbe Session in jedem Lauf gleich aussieht.
/// Index 0 ist [`AGENT`]: Wer nur einen Agenten hat, sieht keine Änderung.
/// Keine der übrigen ist eine Bedeutungsfarbe von oben — ein Agent in
/// [`OK`]-Grün neben der SEAL-Spalte sähe aus wie ein Beleg.
const AGENT_PALETTE: [Color; 6] = [
    AGENT,
    Color::Indexed(75),  // Himmelblau
    Color::Indexed(212), // Rosa
    Color::Indexed(141), // Flieder
    Color::Indexed(180), // Sand
    Color::Indexed(110), // Stahlblau
];

/// Die Farbe eines Agenten in der Liste — eine reine Funktion des Namens
/// (FNV-1a), also in jedem Lauf dieselbe. Nur eine Lesehilfe beim
/// Überfliegen: Das Wort steht immer daneben, Kollisionen in der kleinen
/// Palette sind erlaubt (Modul-Regel: Farbe trägt nie allein).
pub fn agent_color(name: &str) -> Color {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    AGENT_PALETTE[hash as usize % AGENT_PALETTE.len()]
}

/// Die Klasse eines Glyphs: **epistemisch** (was belegt, geprüft, behauptet
/// ist — Evidence-Quelle, Prüfergebnis, Seal-Zustand, Claim, Recon-Klasse,
/// Urteil) oder **strukturell** (was etwas ist — Knotenart, Effekt, Ref,
/// Badge). Epistemische Glyphen sind exklusiv: Kein strukturelles Element
/// benutzt eines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphClass {
    /// Trägt einen Beweis-, Prüf- oder Urteilszustand.
    Epistemic,
    /// Trägt eine Art, keinen Zustand.
    Structural,
}

/// Ein Eintrag des Glyph-Registers: das Zeichen, seine Klasse, seine eine
/// Bedeutung (ein Schlüssel) und das Wort, mit dem die Hilfe es erklärt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    /// Das Zeichen — genau eine Zelle breit.
    pub symbol: &'static str,
    /// Epistemisch oder strukturell.
    pub class: GlyphClass,
    /// Die eine Bedeutung.
    pub meaning: &'static str,
    /// Das Wort für die Legende.
    pub word: &'static str,
}

const fn glyph(
    symbol: &'static str,
    class: GlyphClass,
    meaning: &'static str,
    word: &'static str,
) -> Glyph {
    Glyph {
        symbol,
        class,
        meaning,
        word,
    }
}

/// Das Glyph-Register: jedes Zeichen der Oberfläche **genau einmal**, also
/// mit genau einer Bedeutung. Die Theme-Funktionen unten zeichnen nur, was
/// hier steht; die Legende der Hilfe wird daraus erzeugt.
///
/// `◇` heißt „ausgesagt, nicht bezeugt" — für eine von Hand erklärte
/// Verbindung (declared) wie für einen Tool-Claim ohne Zeugen (reported
/// only); `·` heißt „nichts belegt" (unverbunden, Legacy, unversiegelt).
pub const REGISTER: &[Glyph] = &[
    // Evidence-Quelle
    glyph("●", GlyphClass::Epistemic, "observed", "observed"),
    glyph("◆", GlyphClass::Epistemic, "content", "content-derived"),
    glyph(
        "◇",
        GlyphClass::Epistemic,
        "stated",
        "stated (declared / reported only)",
    ),
    glyph("○", GlyphClass::Epistemic, "inferred", "inferred"),
    glyph(
        "·",
        GlyphClass::Epistemic,
        "none",
        "nothing attested (unlinked / legacy / unsealed)",
    ),
    // Prüfergebnis
    glyph("✓", GlyphClass::Epistemic, "passed", "check passed"),
    glyph("~", GlyphClass::Epistemic, "partial", "partially checked"),
    glyph("?", GlyphClass::Epistemic, "unchecked", "not checked"),
    glyph(
        "✗",
        GlyphClass::Epistemic,
        "failed",
        "check failed / tampered",
    ),
    // Seal-Zustand
    glyph("◈", GlyphClass::Epistemic, "sealed", "sealed"),
    glyph("!", GlyphClass::Epistemic, "incomplete", "incomplete"),
    glyph(
        "◎",
        GlyphClass::Epistemic,
        "unsigned",
        "sealed, not every seal signed",
    ),
    // Aussage, Abgleich, Deutung
    glyph(
        "◌",
        GlyphClass::Epistemic,
        "claim",
        "claim (recorded, not evidence)",
    ),
    glyph(
        "◍",
        GlyphClass::Epistemic,
        "fs only",
        "explained by the file system only",
    ),
    glyph("◦", GlyphClass::Epistemic, "not observed", "not observed"),
    glyph(
        "◐",
        GlyphClass::Epistemic,
        "uninterpreted",
        "observed, not interpreted",
    ),
    // Urteil (Review)
    glyph("⚠", GlyphClass::Epistemic, "gap", "gap / no decision"),
    glyph("⊕", GlyphClass::Epistemic, "approved", "approved"),
    glyph("↻", GlyphClass::Epistemic, "needs work", "needs work"),
    glyph("⊘", GlyphClass::Epistemic, "rejected", "rejected"),
    // Struktur
    glyph("◉", GlyphClass::Structural, "agent", "agent"),
    glyph("›", GlyphClass::Structural, "turn", "turn"),
    glyph("→", GlyphClass::Structural, "read", "READ"),
    glyph("✎", GlyphClass::Structural, "edit", "EDIT"),
    glyph("▶", GlyphClass::Structural, "exec", "EXEC"),
    glyph("✕", GlyphClass::Structural, "delete", "DELETE"),
    glyph("+", GlyphClass::Structural, "tool", "other tool"),
    glyph("⇄", GlyphClass::Structural, "handover", "handover"),
    glyph("■", GlyphClass::Structural, "change", "change / commit"),
    glyph("▲", GlyphClass::Structural, "head", "HEAD"),
    glyph(
        "□",
        GlyphClass::Structural,
        "pending",
        "session without a commit",
    ),
];

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
        return ("·".into(), "unlinked".into(), Style::default().fg(DIM));
    };
    let (glyph, word, style) = match mark.source {
        EvidenceSource::Observed => ("●", "observed", Style::default().fg(OK)),
        EvidenceSource::ContentDerived => ("◆", "content", Style::default().fg(OK)),
        EvidenceSource::HumanDeclared => ("◇", "declared", Style::default().fg(EDIT)),
        EvidenceSource::Heuristic => ("○", "inferred", Style::default().fg(DIM)),
    };
    let (modifier, status_word, style) = match mark.status {
        EvidenceStatus::Verified => ("✓", "recomputed", style),
        EvidenceStatus::Partial => ("~", "partially checked", style),
        // Ungeprüft dimmt auch eine „gute" Quelle — beobachtet heißt nicht
        // geprüft, und das darf man sehen.
        EvidenceStatus::Unknown => ("?", "unchecked", style.add_modifier(Modifier::DIM)),
        EvidenceStatus::Missing => ("✗", "evidence missing", Style::default().fg(DELETE)),
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
            EvidenceVerdict::Verified => ("◈", "sealed", Style::default().fg(OK)),
            EvidenceVerdict::Incomplete => ("!", "incomplete", Style::default().fg(REVIEW)),
            EvidenceVerdict::Tampered => tampered(),
        },
        Provenance::Legacy => ("·", "legacy", Style::default().fg(DIM)),
    }
}

/// Die HEAD-Pille der Übersicht — eine Ref, kein Zustand.
pub const HEAD: &str = "▲";

/// Eine Session ohne Commit im Graphen der Übersicht — ein Platzhalter-
/// Knoten, kein Claim.
pub const PENDING: &str = "□";

/// Glyph, Wort und Stil einer Seal-Pille der Übersicht — dieselben Glyphen
/// wie [`provenance`], wo die Bedeutung dieselbe ist. „Versiegelt, aber
/// nicht jedes Seal signiert" ist **nicht** `◈ sealed`: Wer `refs/minds/`
/// schreibt, kann unsigniertes Material erzeugen.
pub fn seal(mark: minds_reader::overview::SealMark) -> (&'static str, &'static str, Style) {
    use minds_reader::overview::SealMark;
    match mark {
        SealMark::Sealed => ("◈", "sealed", Style::default().fg(OK)),
        SealMark::Unsigned => ("◎", "sealed · unsigned", Style::default().fg(Color::Gray)),
        SealMark::Partial => (
            "◎",
            "sealed · first 64 checked",
            Style::default().fg(Color::Gray),
        ),
        SealMark::Incomplete => ("!", "incomplete", Style::default().fg(REVIEW)),
        SealMark::Unsealed => ("·", "unsealed", Style::default().fg(Color::Gray)),
        SealMark::Tampered => tampered(),
    }
}

/// Glyph, Wort und Stil der Manipulation — der eine Zustand, der immer rot
/// und immer laut ist (Badge, Karte, Banner sagen dasselbe).
pub fn tampered() -> (&'static str, &'static str, Style) {
    ("✗", "TAMPERED", Style::default().fg(DELETE))
}

/// Glyph, Wort und Stil einer **Aussage** aus dem Session-Record (Intent,
/// Begründung): aufgezeichnet, nie beobachtet, nie geprüft. Ein eigenes Wort
/// neben [`evidence`], damit eine Aussage nie wie ein Beleg liest — die
/// Trennung, an der Evidence-Systeme sonst leise scheitern.
pub fn claim() -> (&'static str, &'static str, Style) {
    (
        "◌",
        "CLAIM",
        Style::default().fg(HUMAN).add_modifier(Modifier::DIM),
    )
}

/// Glyph, Wort und Stil einer Reconciliation-Klasse (EA-03). Unerklärt ist
/// **nie** ein Fehler: kein Rot, sondern der neutrale Stil von
/// [`not_observed`] — eine Stelle ohne Beleg ist nicht beobachtet, nicht
/// falsch.
pub fn recon(class: ReconClass) -> (&'static str, &'static str, Style) {
    match class {
        ReconClass::Explained => ("●", class.word(), Style::default().fg(OK)),
        ReconClass::ExplainedFsOnly => ("◍", class.word(), Style::default().fg(OK)),
        // Ein Tool-Claim ohne Zeugen: belegt, aber nur berichtet.
        ReconClass::ReportedOnly => ("◇", class.word(), Style::default()),
        ReconClass::Unexplained => {
            let (glyph, _, style) = not_observed();
            (glyph, class.word(), style)
        }
    }
}

/// Der neutrale Zustand „in der Session nicht beobachtet" — Glyph, Wort und
/// Stil. Gedimmt wie alles Sekundäre, nie in einer Warn- oder Fehlerfarbe.
pub fn not_observed() -> (&'static str, &'static str, Style) {
    ("◦", "not observed", Style::default().fg(DIM))
}

/// Glyph, Wort und Stil eines Verdicts.
pub fn verdict(verdict: Verdict) -> (&'static str, &'static str, Style) {
    match verdict {
        Verdict::Open => ("⚠", "open", Style::default().fg(REVIEW)),
        Verdict::Approved => ("⊕", "approved", Style::default().fg(OK)),
        Verdict::Rejected => ("⊘", "rejected", Style::default().fg(DELETE)),
        Verdict::NeedsWork => ("↻", "needs work", Style::default().fg(REVIEW)),
    }
}

/// Glyph, Wort und Stil eines Tool-Effekts.
pub fn tool(kind: ToolKind) -> (&'static str, &'static str, Style) {
    match kind {
        ToolKind::Read => ("→", "READ", Style::default().fg(READ)),
        ToolKind::Edit => ("✎", "EDIT", Style::default().fg(EDIT)),
        ToolKind::Exec => ("▶", "EXEC", Style::default().fg(EXEC)),
        ToolKind::Delete => ("✕", "DELETE", Style::default().fg(DELETE)),
        ToolKind::Other => ("+", "TOOL", Style::default().fg(DIM)),
        // Beobachtet, nicht gedeutet (ADR-0011): halb sichtbar — Wirkung
        // unbekannt, und das darf man sehen.
        ToolKind::Uninterpreted => ("◐", "OBSERVED", Style::default().fg(REVIEW)),
    }
}

/// Glyph, Wort und Stil eines Graph-Knotens.
pub fn node(kind: &NodeKind) -> (&'static str, &'static str, Style) {
    match kind {
        // Der Auftrag *ist* eine Aussage: dasselbe Glyph wie `claim`.
        NodeKind::Intent => ("◌", "YOU", Style::default().fg(HUMAN)),
        NodeKind::Agent => ("◉", "AGENT", Style::default().fg(AGENT)),
        NodeKind::Turn(_) => ("›", "TURN", Style::default().fg(DIM)),
        NodeKind::Tool(kind) => tool(*kind),
        NodeKind::Subagent(_) => ("◉", "SUBAGENT", Style::default().fg(AGENT)),
        NodeKind::Handover { .. } => ("⇄", "HANDOVER", Style::default().fg(OK)),
        NodeKind::Change(_) => ("■", "CHANGE", Style::default().fg(CHANGE)),
        NodeKind::Commit(_) => ("■", "COMMIT", Style::default().fg(CHANGE)),
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

/// Die Cursorzeile — der eine Auswahlstil der Oberfläche (B7).
pub fn cursor() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// Rahmenstil und Titel-Marke eines Bereichs (B7): der fokussierte mit
/// hervorgehobenem Rahmen und `▸` im Titel, ein unfokussierter gedimmt — die
/// Marke trägt den Fokus auch ohne Farbe.
pub fn pane(focused: bool) -> (Style, &'static str) {
    if focused {
        (Style::default().add_modifier(Modifier::BOLD), "▸ ")
    } else {
        (dim(), "")
    }
}

/// Gedimmter Nebentext.
pub fn dim() -> Style {
    Style::default().fg(DIM)
}

/// Hervorgehobener Kopf.
pub fn title() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Die Agentenfarbe ist eine reine Funktion des Namens — über Läufe
    /// **und Versionen** hinweg, deshalb feste Sollwerte statt `f(x) ==
    /// f(x)`. Total: Ein leerer Akteur darf die Oberfläche nicht panicken.
    #[test]
    fn agent_color_is_deterministic_and_total() {
        assert_eq!(agent_color("claude-code · opus"), AGENT_PALETTE[3]);
        assert_eq!(agent_color("codex · gpt-5"), AGENT_PALETTE[4]);
        assert_eq!(agent_color(""), AGENT_PALETTE[1]);
        // Der erste Eintrag ist bewusst `AGENT`: Die Palette verschiebt
        // niemanden, sie unterscheidet nur — und keine ihrer Farben trägt
        // anderswo eine Bedeutung.
        assert_eq!(AGENT_PALETTE[0], AGENT);
        for meaning in [HUMAN, READ, EDIT, EXEC, DELETE, CHANGE, REVIEW, OK, DIM] {
            assert!(!AGENT_PALETTE[1..].contains(&meaning), "{meaning:?}");
        }
    }
}
