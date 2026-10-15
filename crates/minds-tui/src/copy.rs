//! Das Glossar: jedes Statuswort der Oberfläche mit **einem** Satz in
//! einfacher Sprache — an genau einer Stelle (C1). Die Fußzeile erklärt den
//! fokussierten Zustand mit diesem Satz, die Hilfe zeigt das ganze Glossar.
//!
//! Die Sätze ziehen die Grenzen, an denen Leser sonst stolpern: versiegelt
//! ist nicht verifiziert, beobachtet nicht geprüft, eine Aussage kein Beleg,
//! nicht beobachtet nicht falsch, unvollständig nicht manipuliert.

/// Ein Eintrag: das Wort und sein Satz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Term {
    /// Das Wort, wie die Oberfläche es zeigt (klein, außer wo die CLI groß
    /// schreibt).
    pub word: &'static str,
    /// Ein Satz, was es heißt — und was nicht.
    pub sentence: &'static str,
}

const fn term(word: &'static str, sentence: &'static str) -> Term {
    Term { word, sentence }
}

/// Das Glossar, in der Reihenfolge der Hilfe.
pub const GLOSSARY: &[Term] = &[
    // Integrität
    term(
        "sealed",
        "Sealed: the seal material is unchanged — sealed is not verified; `minds verify` checks the signatures.",
    ),
    term(
        "verified",
        "Verified: `minds verify` checked the signatures against the trusted signers.",
    ),
    term(
        "TAMPERED",
        "Tampered: the seal material was changed after sealing — this is an error.",
    ),
    term(
        "legacy",
        "Legacy: captured before the evidence chain, so there is nothing to check.",
    ),
    term(
        "unsealed",
        "Unsealed: at least one session has no seal, so there is nothing to check for it.",
    ),
    term(
        "not verifiable",
        "Not verifiable: there is no seal to check, as `minds verify` says too.",
    ),
    term(
        "checking",
        "Checking: the signatures are being checked — there is no verdict yet.",
    ),
    term(
        "not checked",
        "Not checked: the signatures could not be checked, and without them there is no verdict.",
    ),
    term(
        "no session",
        "No session: Minds knows no session behind this commit.",
    ),
    term(
        "hash-valid",
        "Hash-valid: the seal's hash matches its content — whether it is signed is a separate check.",
    ),
    // Coverage
    term(
        "complete",
        "Complete: no gap in the observed range — only within the observation boundary.",
    ),
    term(
        "incomplete",
        "Incomplete: some events are not attested (gaps) — incomplete is not tampered.",
    ),
    term(
        "not assessable",
        "Not assessable: the seal material is altered, so its coverage statement cannot be trusted.",
    ),
    term(
        "not recorded",
        "Not recorded: this session has no coverage record.",
    ),
    term(
        "epoch",
        "Epoch: one sealed segment of a session's events — epochs link to each other like a chain.",
    ),
    // Deutung
    term(
        "interpreted",
        "Interpreted: every tool call has a known effect (read, edit, exec, delete).",
    ),
    term(
        "uninterpreted",
        "Uninterpreted: the call was observed, but its effect is unknown — observed, not wrong.",
    ),
    // Beleg und Aussage
    term(
        "observed",
        "Observed: seen by the capture hooks — observed is not checked.",
    ),
    term(
        "checked",
        "Checked: recomputed from the content and found to match.",
    ),
    term(
        "unchecked",
        "Unchecked: nobody has recomputed this yet — it may still be right.",
    ),
    term(
        "stated",
        "Stated: declared by a human or reported by a tool, without a witness.",
    ),
    term(
        "inferred",
        "Inferred: reconstructed by Minds from file overlap and timing — a guess, not a record.",
    ),
    term(
        "claim",
        "Claim: recorded by the agent (intent, reasons, discarded approaches) — a claim is not evidence.",
    ),
    term(
        "not observed",
        "Not observed: no observation covers this line — not observed is not wrong.",
    ),
    // Urteil und Prüfung
    term(
        "open",
        "Open: no human has decided on this change yet — the normal state.",
    ),
    term(
        "witness",
        "Witness: a separate process that observes the agent's events and signs them.",
    ),
    term(
        "conformance",
        "Conformance: whether the commit matches the human-signed spec in `.minds/spec.json`.",
    ),
    term(
        "A0",
        "A0 claimed: only what the agent recorded — nothing observed it.",
    ),
    term(
        "A1",
        "A1 observed: captured by hooks with a key the agent could read.",
    ),
    term(
        "A2",
        "A2 witnessed: observed and signed by a separated, protected witness.",
    ),
    term(
        "A3",
        "A3 reproduced: the result was reproduced independently.",
    ),
];

/// Der Satz zu `word`, falls es im Glossar steht.
pub fn sentence(word: &str) -> Option<&'static str> {
    GLOSSARY.iter().find(|t| t.word == word).map(|t| t.sentence)
}

/// Das Glossar-Wort der Integritätsachse.
pub fn integrity(axis: minds_reader::model::IntegrityAxis) -> &'static str {
    use minds_reader::model::IntegrityAxis;
    match axis {
        IntegrityAxis::Sealed => "sealed",
        IntegrityAxis::Tampered => "TAMPERED",
        IntegrityAxis::Legacy => "legacy",
    }
}

/// Das Glossar-Wort der Coverage-Achse.
pub fn coverage(axis: minds_reader::model::CoverageAxis) -> &'static str {
    use minds_reader::model::CoverageAxis;
    match axis {
        CoverageAxis::Complete => "complete",
        CoverageAxis::Incomplete => "incomplete",
        CoverageAxis::NotAssessable => "not assessable",
        CoverageAxis::NotRecorded => "not recorded",
    }
}

/// Das Glossar-Wort der Deutungsachse.
pub fn interpretation(axis: minds_reader::model::InterpretationAxis) -> &'static str {
    use minds_reader::model::InterpretationAxis;
    match axis {
        InterpretationAxis::Interpreted => "interpreted",
        InterpretationAxis::Uninterpreted(_) => "uninterpreted",
    }
}

/// Das Glossar-Wort zum Urteil des Verify-Tabs.
pub fn overall(overall: Option<crate::verify::Overall>) -> &'static str {
    use crate::verify::Overall;
    match overall {
        Some(Overall::Verified) => "verified",
        Some(Overall::Incomplete) => "incomplete",
        Some(Overall::Tampered) => "TAMPERED",
        Some(Overall::NotVerifiable) => "not verifiable",
        Some(Overall::Checking) => "checking",
        Some(Overall::NotChecked) => "not checked",
        Some(Overall::NoSession) | None => "no session",
    }
}
