//! Intent zuerst — die Verdichtung einer Session auf eine Zeile.
//!
//! Der Reviewer soll lesen, *was verlangt wurde*, bevor er den Diff sieht. Dafür
//! braucht die Übersicht pro Session eine Überschrift, und die entsteht hier.
//!
//! # Deterministisch extrahiert, nicht generiert — null Tokens
//!
//! Es gibt kein Modell in diesem Pfad. Die Überschrift ist die erste sinnvolle
//! Zeile des ersten Prompts, an einer Wortgrenze gekürzt. Das ist weniger schön
//! als eine LLM-Zusammenfassung und dafür: kostenlos, offline, sofort — und bei
//! gleicher Session **immer identisch**. Der Plan hält den Summary-Pfad mit
//! Modell bewusst für nach v0.1 zurück (M8, mit Caching über die `SessionId`);
//! bis dahin ist das hier die ganze Wahrheit.
//!
//! Wer den vollen Prompt will, klickt die Session an — die Verdichtung ersetzt
//! ihn nicht, sie führt zu ihm hin.

use minds_core::{Session, SessionId};

/// Wie lang eine Überschrift höchstens wird, bevor gekürzt wird.
pub const HEADLINE_MAX: usize = 90;

/// Eine Session, verdichtet auf das, was in eine Übersichtszeile passt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Die Session, auf die sich das bezieht.
    pub id: SessionId,
    /// Worum es ging, in einer Zeile.
    pub headline: String,
    /// Wer es tat: `agent · modell`.
    pub actor: String,
    /// Wie viele Dateien die Session hervorgebracht hat.
    pub files: usize,
    /// Wie viele Constraints erfasst sind.
    pub constraints: usize,
    /// Wie viele verworfene Pfade erfasst sind.
    pub discarded: usize,
    /// Token ein/aus.
    pub input_tokens: u64,
    /// Token aus.
    pub output_tokens: u64,
}

impl Summary {
    /// Verdichtet eine Session.
    pub fn of(id: SessionId, session: &Session) -> Self {
        Self {
            id,
            headline: headline(&session.intent.request, HEADLINE_MAX),
            actor: format!("{} · {}", session.agent.name, session.model.id),
            files: session.produced.files.len(),
            constraints: session.intent.constraints.len(),
            discarded: session.intent.discarded.len(),
            input_tokens: session.usage.input_tokens,
            output_tokens: session.usage.output_tokens,
        }
    }
}

/// Die erste sinnvolle Zeile von `request`, auf höchstens `max` Zeichen an einer
/// Wortgrenze gekürzt.
///
/// Leerer oder nur aus Leerraum bestehender Text ergibt einen ehrlichen
/// Platzhalter statt einer leeren Überschrift — der Reader behauptet nie, es
/// gäbe einen Prompt, wo keiner erfasst wurde.
pub fn headline(request: &str, max: usize) -> String {
    let first = request
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");

    if first.is_empty() {
        return "(kein Prompt erfasst)".to_string();
    }

    // `chars().count()` statt `len()`: gekürzt wird nach Zeichen, nicht nach
    // Bytes — sonst zerschnitte ein Umlaut die Ausgabe.
    if first.chars().count() <= max {
        return first.to_string();
    }

    let cut: String = first.chars().take(max).collect();
    let trimmed = match cut.rsplit_once(' ') {
        // Nur an der Wortgrenze schneiden, wenn dabei noch etwas übrig bleibt.
        Some((head, _)) if head.chars().count() >= max / 2 => head,
        _ => cut.trim_end(),
    };
    format!("{}…", trimmed.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{Agent, Intent, Model, Produced, Usage};

    fn sid() -> SessionId {
        format!("b3-{}", "a".repeat(64)).parse().unwrap()
    }

    #[test]
    fn a_short_request_is_the_headline() {
        assert_eq!(
            headline("Fix den Retry-Test", HEADLINE_MAX),
            "Fix den Retry-Test"
        );
    }

    #[test]
    fn only_the_first_non_empty_line_counts() {
        assert_eq!(
            headline("\n\n  Erste Zeile\nZweite", HEADLINE_MAX),
            "Erste Zeile"
        );
    }

    #[test]
    fn a_long_request_is_cut_at_a_word_boundary() {
        let long = "Der Retry-Test flackert seit dem Umbau der Backoff-Logik und muss dringend \
                    repariert werden, bevor die Pipeline weiter rot bleibt";
        let out = headline(long, HEADLINE_MAX);

        assert!(out.chars().count() <= HEADLINE_MAX + 1, "{out}");
        assert!(out.ends_with('…'), "{out}");
        // Kein abgeschnittenes Wort am Ende.
        assert!(!out.contains("  "));
        assert!(long.starts_with(out.trim_end_matches('…').trim_end()));
    }

    #[test]
    fn an_empty_request_says_so_instead_of_being_blank() {
        assert_eq!(headline("", HEADLINE_MAX), "(kein Prompt erfasst)");
        assert_eq!(headline("   \n\t ", HEADLINE_MAX), "(kein Prompt erfasst)");
    }

    #[test]
    fn cutting_respects_characters_not_bytes() {
        // Zehn Umlaute sind 10 Zeichen, aber 20 Bytes — ein Byte-Schnitt
        // zerschnitte einen davon und ergäbe ungültiges UTF-8.
        let text = "ä".repeat(50);
        let out = headline(&text, 10);
        assert!(out.chars().count() <= 11, "{out}");
        assert!(out.ends_with('…'));
    }

    #[test]
    fn a_single_long_word_is_still_cut() {
        // Keine Wortgrenze in Reichweite — dann hart schneiden statt aufgeben.
        let out = headline(&"x".repeat(200), 20);
        assert!(out.chars().count() <= 21, "{out}");
        assert!(out.ends_with('…'));
    }

    #[test]
    fn summary_condenses_a_session() {
        let mut session = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.4.2".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent {
                request: "Fix den Retry-Test".into(),
                constraints: vec!["keine neuen Dependencies".into()],
                discarded: vec!["Timeout hochsetzen".into(), "Test löschen".into()],
            },
        );
        session.usage = Usage {
            input_tokens: 900,
            output_tokens: 120,
        };
        session.produced = Produced {
            commit_hint: None,
            files: vec!["src/retry.rs".into()],
        };

        let summary = Summary::of(sid(), &session);
        assert_eq!(summary.headline, "Fix den Retry-Test");
        assert_eq!(summary.actor, "claude-code · claude-opus-4");
        assert_eq!(summary.files, 1);
        assert_eq!(summary.constraints, 1);
        assert_eq!(summary.discarded, 2);
        assert_eq!(summary.input_tokens, 900);
    }

    #[test]
    fn the_same_session_always_yields_the_same_summary() {
        // Die Zusage dieses Moduls: deterministisch, weil ohne Modell.
        let session = Session::new(
            Agent {
                name: "a".into(),
                version: "1".into(),
            },
            Model {
                provider: "p".into(),
                id: "m".into(),
            },
            Intent {
                request: "mach x".into(),
                ..Intent::default()
            },
        );
        assert_eq!(Summary::of(sid(), &session), Summary::of(sid(), &session));
    }
}
