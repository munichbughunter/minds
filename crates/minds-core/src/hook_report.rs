//! Was der Hook beim `SessionEnd` aus dem Transkript **meldet** — eine
//! Angabe des Agenten-Prozesses, keine Beobachtung des Witness.
//!
//! Der Witness liest kein Transkript aus dem Namensraum des Agenten (K1).
//! Damit eine Session trotzdem Version, Token-Zähler und Antworttexte
//! trägt, liest der `SessionEnd`-Hook — er läuft als Entwickler und bekommt
//! den `transcript_path` — die Datei einmal, redigiert die Texte fail-closed
//! und schickt das Ergebnis als eigene Frames hinter dem Event. Der Witness
//! hängt es an das Event, und der Checkpoint baut daraus die Session mit
//! `lineage.transcript = reported_by_hook`: Jeder Leser sieht, dass diese
//! Werte vom Hook stammen (`◌`, eine Aussage), nicht vom Witness.
//!
//! Alles hier ist **additiv**: Ein Event ohne Report serialisiert wie
//! zuvor und behält seinen `event_hash`; mit Report geht der Report in den
//! Hash ein — als Angabe des Hooks tamper-evident, nicht als Tatsache.

use serde::{Deserialize, Serialize};

use crate::session::Usage;

/// Die Angaben des Hooks zu einer Session, gelesen aus dem Transkript beim
/// `SessionEnd`. Jedes Feld fehlt, wenn das Transkript es nicht hergab.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HookReport {
    /// Die Version des Agenten (Claude Codes `version`-Feld), wie zuletzt
    /// im Transkript genannt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,

    /// Die Summe der `usage`-Felder aller Assistant-Nachrichten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,

    /// Die Antworttexte des Agenten, **je Zug** (zwischen zwei Prompts des
    /// Menschen) zusammengefügt, in Reihenfolge — bereits durch die
    /// Redaction-Policy des Repos gegangen; ein Text, der sich nicht sauber
    /// redigieren ließ, fehlt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub said: Vec<String>,
}

impl HookReport {
    /// `true`, wenn der Report nichts trägt — dann wird er nicht geschrieben.
    pub fn is_empty(&self) -> bool {
        self.agent_version.is_none() && self.usage.is_none() && self.said.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_report_serializes_to_nothing_and_a_full_one_round_trips() {
        assert!(HookReport::default().is_empty());
        assert_eq!(serde_json::to_string(&HookReport::default()).unwrap(), "{}");
        let full = HookReport {
            agent_version: Some("2.0.76".into()),
            usage: Some(Usage {
                input_tokens: 14_200,
                output_tokens: 3_900,
            }),
            said: vec!["Ich schaue nach.".into(), "Fertig.".into()],
        };
        assert!(!full.is_empty());
        let text = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<HookReport>(&text).unwrap(), full);
    }
}
