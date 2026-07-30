//! Der leniente Leser für Claude Codes Transkript — die reiche Zweitquelle.
//!
//! Der Hook liefert Zeitpunkt, Reihenfolge und Tool-Calls, aber nicht den
//! Antworttext des Modells, nicht die Token-Zähler und nicht die Modell-ID. Die
//! stehen nur im Transkript, einer JSONL-Datei (eine Zeile ein Ereignis). Dieses
//! Modul liest genau diese drei Dinge heraus — mehr nicht.
//!
//! # Warum leniant und nicht streng
//!
//! Das Transkript gehört jemand anderem, und sein Format ändert sich zwischen
//! Claude-Code-Versionen. Ein strenger Parser wäre eine Wette darauf, dass sich
//! nichts ändert — eine Wette, die man verliert. Deshalb: Jede Zeile, die sich
//! nicht deuten lässt, wird **übersprungen**, nie zum Fehler. Was wir verstehen,
//! nehmen wir; der Rest ist Sache des Hooks, der ohnehin die verlässlichere
//! Quelle für Struktur ist.
//!
//! # Was hier bewusst *nicht* passiert
//!
//! Kein Abgleich von Transkript-Einträgen mit Journal-Events über UUIDs. Der
//! Adapter (siehe [`crate::adapter`]) ordnet die Assistant-Texte den
//! Assistant-Zügen der Reihe nach zu — best effort. Ein UUID-genauer Abgleich
//! wäre genauer, aber auch zerbrechlicher; er kann später additiv dazukommen,
//! ohne dass sich der Vertrag dieses Moduls ändert.

use minds_core::{Model, Usage};
use serde::Deserialize;

/// Was sich aus einem Transkript herausziehen lässt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    /// Das zuletzt gesehene Modell. `None`, wenn keine Assistant-Nachricht eine
    /// Modell-ID trug.
    pub model: Option<Model>,

    /// Die aufsummierten Token-Zähler über alle Assistant-Nachrichten.
    ///
    /// `input_tokens` ist die Summe der Pro-Anfrage-Eingaben; weil jede Anfrage
    /// den Kontext erneut sendet, ist das eine Obergrenze, kein Zähler
    /// verschiedener Token. `output_tokens` summiert das tatsächlich Erzeugte.
    pub usage: Usage,

    /// Die zuletzt gesehene Agent-Version (Claude Codes `version`-Feld).
    pub agent_version: Option<String>,

    /// Die Assistant-Texte in Reihenfolge — je Assistant-Nachricht ein Eintrag,
    /// leere übersprungen.
    pub assistant_texts: Vec<String>,
}

/// Liest ein Transkript aus rohen JSONL-Bytes. Schlägt nie fehl: Was sich nicht
/// deuten lässt, wird übersprungen.
pub fn parse(bytes: &[u8]) -> Transcript {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Transcript::default();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Entry>(line) else {
            continue;
        };

        if let Some(version) = entry.version {
            out.agent_version = Some(version);
        }

        let Some(message) = entry.message else {
            continue;
        };

        if let Some(model_id) = message.model {
            out.model = Some(Model {
                provider: provider_of(&model_id),
                id: model_id,
            });
        }

        if let Some(usage) = message.usage {
            out.usage.input_tokens = out.usage.input_tokens.saturating_add(usage.input_tokens);
            out.usage.output_tokens = out.usage.output_tokens.saturating_add(usage.output_tokens);
        }

        if entry.kind.as_deref() == Some("assistant") {
            let text = message
                .content
                .as_ref()
                .map(content_text)
                .unwrap_or_default();
            if !text.is_empty() {
                out.assistant_texts.push(text);
            }
        }
    }

    out
}

/// Rät den Anbieter aus der Modell-ID. Bewusst grob — die ID ist die Wahrheit,
/// der Anbieter nur eine Bequemlichkeit für den Reader.
pub(crate) fn provider_of(model_id: &str) -> String {
    let id = model_id.to_ascii_lowercase();
    if id.starts_with("claude") {
        "anthropic"
    } else if id.starts_with("gpt") || is_openai_o_series(&id) {
        "openai"
    } else if id.starts_with("gemini") {
        "google"
    } else {
        "unknown"
    }
    .to_string()
}

/// OpenAIs o-Serie: `o1`, `o3`, `o4-mini`, … — ein `o`, gefolgt von einer
/// Ziffer. Bewusst eng, damit `opus` nicht fälschlich als OpenAI gilt.
fn is_openai_o_series(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next() == Some('o') && chars.next().is_some_and(|c| c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Das Drahtformat, so viel wie nötig
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: Option<String>,
    version: Option<String>,
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    model: Option<String>,
    usage: Option<UsageWire>,
    /// Roh als [`serde_json::Value`] gehalten statt in ein untagged Enum
    /// gezwängt: Der Inhalt ist mal ein String (User), mal eine Block-Liste
    /// (Assistant), und ein unerwarteter dritter Fall darf die ganze Zeile nicht
    /// verwerfen. Die Deutung übernimmt [`content_text`].
    content: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct UsageWire {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

/// Der Antworttext eines Nachrichteninhalts.
///
/// Ein String ist der User-Fall. Eine Liste ist der Assistant-Fall: nur die
/// `text`-Blöcke tragen Antworttext; `thinking` und `tool_use` werden
/// übergangen. Alles Übrige ergibt leeren Text — nie einen Fehler.
fn content_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulls_model_usage_and_text() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"role":"user","content":"fix den test"}}"#,
            "\n",
            r#"{"type":"assistant","version":"1.2.3","message":{"role":"assistant","model":"claude-opus-4","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"Ich schaue nach."}],"usage":{"input_tokens":100,"output_tokens":40}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-4","content":[{"type":"tool_use","name":"Read","input":{}},{"type":"text","text":"Fertig."}],"usage":{"input_tokens":120,"output_tokens":10}}}"#,
        );

        let t = parse(jsonl.as_bytes());
        assert_eq!(t.model.as_ref().unwrap().id, "claude-opus-4");
        assert_eq!(t.model.as_ref().unwrap().provider, "anthropic");
        assert_eq!(t.usage.input_tokens, 220);
        assert_eq!(t.usage.output_tokens, 50);
        assert_eq!(t.agent_version.as_deref(), Some("1.2.3"));
        assert_eq!(t.assistant_texts, vec!["Ich schaue nach.", "Fertig."]);
    }

    #[test]
    fn broken_lines_are_skipped_not_fatal() {
        let jsonl = concat!(
            "das ist kein json\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"a"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"tr"#, // abgeschnitten
        );
        let t = parse(jsonl.as_bytes());
        assert_eq!(t.assistant_texts, vec!["a"]);
    }

    #[test]
    fn an_empty_transcript_is_empty_not_an_error() {
        let t = parse(b"");
        assert!(t.model.is_none());
        assert_eq!(t.usage, Usage::default());
        assert!(t.assistant_texts.is_empty());
    }

    #[test]
    fn user_string_content_does_not_count_as_assistant_text() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"nur user"}}"#;
        let t = parse(jsonl.as_bytes());
        assert!(t.assistant_texts.is_empty());
    }

    #[test]
    fn provider_inference() {
        assert_eq!(provider_of("claude-opus-4"), "anthropic");
        assert_eq!(provider_of("gpt-4o"), "openai");
        assert_eq!(provider_of("o1-preview"), "openai");
        assert_eq!(provider_of("gemini-2.0"), "google");
        assert_eq!(provider_of("llama-3"), "unknown");
    }
}
