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

use minds_core::{Session, SessionId, TranscriptStatus};

use crate::text::sanitize;

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
            actor: actor_line(session),
            files: session.produced.files.len(),
            constraints: session.intent.constraints.len(),
            discarded: session.intent.discarded.len(),
            input_tokens: session.usage.input_tokens,
            output_tokens: session.usage.output_tokens,
        }
    }
}

/// Der Platzhalter, den `minds-capture` schreibt, wenn ein Wert nicht
/// gelesen wurde (`adapter.rs::unknown()`, `transcript.rs::provider_of`).
/// Er ist kein Wert: Die Oberfläche druckt ihn nie, sie lässt das Feld weg
/// oder sagt „not recorded" (C3.6). Die Prüfung liegt hier, nicht in einer
/// View — eine Stelle, nicht ein String-Vergleich je Widget.
pub const PLACEHOLDER: &str = "unknown";

/// Ein erfasster Wert — `None` für den Platzhalter und für Leerraum.
pub fn recorded(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value != PLACEHOLDER).then_some(value)
}

/// Wer eine Session gefahren hat, für die Anzeige (C3.6): Agent, Version,
/// Modell und Anbieter — jedes Feld nur, wenn es erfasst wurde. Entschärft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// Der Agent (`claude-code`, `codex`).
    pub agent: Option<String>,
    /// Seine Version.
    pub version: Option<String>,
    /// Die Modell-Id (`claude-opus-4-1`).
    pub model: Option<String>,
    /// Der Anbieter in seiner Schreibweise (`Anthropic`), nur wenn bekannt.
    pub provider: Option<String>,
    /// Ob und warum das Transkript gelesen wurde (K1) — der Grund hinter
    /// einem fehlenden Modell. `None`: von einem Binary ohne das Feld gebaut.
    pub transcript: Option<TranscriptStatus>,
}

/// Was in der Oberfläche steht, wo ein Modell stünde, das nicht erfasst ist.
pub const MODEL_NOT_RECORDED: &str = "model not recorded";

impl Actor {
    /// Liest Agent und Modell einer Session; Platzhalter werden `None`.
    pub fn of(session: &Session) -> Self {
        let field = |value: &str| recorded(value).map(sanitize);
        Self {
            agent: field(&session.agent.name),
            version: field(&session.agent.version),
            model: field(&session.model.id),
            provider: recorded(&session.model.provider).map(provider_name),
            transcript: session.lineage.as_ref().and_then(|l| l.transcript),
        }
    }

    /// Das Modell samt Anbieter (`claude-opus-4-1 (Anthropic)`) — oder,
    /// wenn es nicht erfasst ist, `model not recorded — <Grund>`; ohne
    /// bekannten Grund nur `model not recorded`. Nie der Platzhalter.
    pub fn model_label(&self) -> String {
        match &self.model {
            Some(model) => match &self.provider {
                Some(provider) => format!("{model} ({provider})"),
                None => model.clone(),
            },
            None => self.model_missing(),
        }
    }

    /// Das Modell ohne Anbieter (`claude-opus-4-1`) für die knappe Form
    /// `agent · modell`; nicht erfasst wie bei [`Self::model_label`].
    pub fn model_word(&self) -> String {
        match &self.model {
            Some(model) => model.clone(),
            None => self.model_missing(),
        }
    }

    fn model_missing(&self) -> String {
        match self.transcript {
            // `Read` ohne Modell: das Transkript trug keines — ohne weiteren
            // Grund, den der Record kennt.
            Some(TranscriptStatus::Read) | None => MODEL_NOT_RECORDED.to_string(),
            Some(status) => format!("{MODEL_NOT_RECORDED} — {}", transcript_reason(status)),
        }
    }
}

/// Der Glossar-Satz zu einem Transkript-Status (K1): warum das Modell fehlt,
/// in den Worten der Oberfläche — eine Stelle, ein Satz je Status.
pub fn transcript_reason(status: TranscriptStatus) -> &'static str {
    match status {
        TranscriptStatus::Read => "the transcript was read",
        TranscriptStatus::NoPathInEvents => "the hook events carried no transcript path",
        TranscriptStatus::NotFound => "the transcript file was not found at checkpoint time",
        TranscriptStatus::NotReadable => "the transcript file could not be read",
        TranscriptStatus::Empty => "the transcript held no assistant message",
        TranscriptStatus::ParseFailed => "the transcript could not be parsed",
        TranscriptStatus::NotReadByWitness => {
            "the witness does not read transcripts from agent paths"
        }
    }
}

/// `agent · modell` für Listen, Graph-Knoten und die Why-Box — der Agent
/// wie erfasst (oder `agent not recorded`), das Modell wie
/// [`Actor::model_word`]. Entschärft; nie der Platzhalter `unknown`.
pub fn actor_line(session: &Session) -> String {
    let actor = Actor::of(session);
    format!(
        "{} · {}",
        actor.agent.as_deref().unwrap_or("agent not recorded"),
        actor.model_word()
    )
}

/// Die Schreibweise eines Anbieters, den `minds-capture` rät; Unbekanntes
/// bleibt, wie es ist (entschärft).
fn provider_name(provider: &str) -> String {
    match provider.to_ascii_lowercase().as_str() {
        "anthropic" => "Anthropic".into(),
        "openai" => "OpenAI".into(),
        "google" => "Google".into(),
        _ => sanitize(provider),
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
        return "(no prompt captured)".to_string();
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

    fn session_of(agent: &str, version: &str, provider: &str, model: &str) -> Session {
        Session::new(
            Agent {
                name: agent.into(),
                version: version.into(),
            },
            Model {
                provider: provider.into(),
                id: model.into(),
            },
            Intent::default(),
        )
    }

    /// C3.6: `unknown` ist der Platzhalter von `minds-capture`, kein Wert —
    /// der Reader meldet ihn als „nicht erfasst", die Oberfläche druckt ihn nie.
    #[test]
    fn the_capture_placeholder_is_not_a_recorded_value() {
        assert_eq!(recorded("unknown"), None);
        assert_eq!(recorded("  "), None);
        assert_eq!(recorded(""), None);
        assert_eq!(recorded("claude-opus-4-1"), Some("claude-opus-4-1"));
        assert_eq!(recorded(" opus "), Some("opus"));
    }

    /// C3.6: Agent und Modell für die Anzeige — der Anbieter nur, wenn er
    /// bekannt ist, mit Schreibweise; Platzhalter werden zu `None`, nie zu
    /// einem Wort, das wie ein Modellname aussieht.
    #[test]
    fn the_actor_names_the_provider_only_when_known() {
        let known = Actor::of(&session_of(
            "claude-code",
            "2.0",
            "anthropic",
            "claude-opus-4-1",
        ));
        assert_eq!(known.agent.as_deref(), Some("claude-code"));
        assert_eq!(known.version.as_deref(), Some("2.0"));
        assert_eq!(known.model.as_deref(), Some("claude-opus-4-1"));
        assert_eq!(known.provider.as_deref(), Some("Anthropic"));

        let bare = Actor::of(&session_of("claude-code", "unknown", "unknown", "unknown"));
        assert_eq!(bare.agent.as_deref(), Some("claude-code"));
        assert_eq!(bare.version, None);
        assert_eq!(bare.model, None);
        assert_eq!(bare.provider, None);

        let other = Actor::of(&session_of("codex", "1", "acme", "acme-1"));
        assert_eq!(other.provider.as_deref(), Some("acme"));
        assert_eq!(
            Actor::of(&session_of("x", "1", "openai", "gpt-5"))
                .provider
                .as_deref(),
            Some("OpenAI")
        );
        // Entschärft wie alles im Sichtmodell.
        let hostile = Actor::of(&session_of("a\u{1b}[31mb", "1", "anthropic", "m\nn"));
        assert!(!hostile.agent.unwrap().contains('\u{1b}'));
        assert!(!hostile.model.unwrap().contains('\n'));
    }

    /// K1: Ein fehlendes Modell nennt seinen Grund aus der Lineage — als
    /// Satz, je Status einer; ohne Status (altes Binary) nur „not recorded".
    /// Ein erfasstes Modell steht vollständig, mit Anbieter. Das Wort
    /// `unknown` kommt nirgends vor.
    #[test]
    fn a_missing_model_names_why_the_transcript_was_not_read() {
        let mut bare = session_of("claude-code", "unknown", "unknown", "unknown");
        let mut lineage = minds_core::Lineage::new("l");
        assert_eq!(Actor::of(&bare).model_label(), "model not recorded");
        assert_eq!(actor_line(&bare), "claude-code · model not recorded");

        for (status, reason) in [
            (
                TranscriptStatus::NoPathInEvents,
                "the hook events carried no transcript path",
            ),
            (
                TranscriptStatus::NotFound,
                "the transcript file was not found at checkpoint time",
            ),
            (
                TranscriptStatus::NotReadable,
                "the transcript file could not be read",
            ),
            (
                TranscriptStatus::Empty,
                "the transcript held no assistant message",
            ),
            (
                TranscriptStatus::ParseFailed,
                "the transcript could not be parsed",
            ),
            (
                TranscriptStatus::NotReadByWitness,
                "the witness does not read transcripts from agent paths",
            ),
        ] {
            lineage.transcript = Some(status);
            bare.lineage = Some(lineage.clone());
            let actor = Actor::of(&bare);
            assert_eq!(actor.transcript, Some(status));
            assert_eq!(
                actor.model_label(),
                format!("model not recorded — {reason}")
            );
            assert_eq!(
                actor_line(&bare),
                format!("claude-code · model not recorded — {reason}")
            );
            assert!(!actor.model_label().contains("unknown"));
        }

        let mut known = session_of("claude-code", "2.0", "anthropic", "claude-opus-4-1");
        lineage.transcript = Some(TranscriptStatus::Read);
        known.lineage = Some(lineage);
        let actor = Actor::of(&known);
        assert_eq!(actor.model_label(), "claude-opus-4-1 (Anthropic)");
        assert_eq!(actor.model_word(), "claude-opus-4-1");
        assert_eq!(actor_line(&known), "claude-code · claude-opus-4-1");
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
        assert_eq!(headline("", HEADLINE_MAX), "(no prompt captured)");
        assert_eq!(headline("   \n\t ", HEADLINE_MAX), "(no prompt captured)");
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
