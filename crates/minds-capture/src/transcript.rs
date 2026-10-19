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

use std::path::Path;

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
    parse_counted(bytes).0
}

/// Liest nur das **Ende** eines Transkripts (höchstens `limit` Bytes) und
/// liefert die Modell-Kennung des letzten Eintrags, der eine trägt (K1).
///
/// Für den Hook-Pfad: Beim `Stop`/`SessionEnd` kennt der Hook das Transkript,
/// darf den heißen Pfad aber nicht mit einer vollständigen Lektüre belasten.
/// Die erste Zeile des Ausschnitts kann angeschnitten sein und wird
/// übersprungen. Gelesen wird nur die Kennung — kein Text, keine Token
/// (Redaction-Grenze). Jeder Fehler ist `None`: ein fehlendes Modell, kein
/// fehlgeschlagener Hook.
pub fn model_in_tail(path: &Path, limit: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let cut = len > limit;
    if cut {
        file.seek(SeekFrom::Start(len - limit)).ok()?;
    }
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut model = None;
    for line in text.lines().skip(usize::from(cut)) {
        let Ok(entry) = serde_json::from_str::<Entry>(line.trim()) else {
            continue;
        };
        if let Some(id) = entry.message.and_then(|m| m.model) {
            model = Some(id);
        }
    }
    model
}

/// Wie [`parse`], zählt zusätzlich die Zeilen, die sich als Eintrag deuten
/// ließen — `0` bei Bytes, die kein Transkript sind (K1: `ParseFailed`
/// statt `Empty`).
pub(crate) fn parse_counted(bytes: &[u8]) -> (Transcript, usize) {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Transcript::default();
    let mut entries = 0usize;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Entry>(line) else {
            continue;
        };
        entries += 1;

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

    (out, entries)
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

    #[test]
    fn the_tail_yields_the_last_model_without_reading_the_whole_file() {
        // K1: Ein langes Transkript, das Modell steht in der letzten
        // Assistant-Zeile; ein Ausschnitt, der die erste Zeile anschneidet,
        // findet es trotzdem — und eine fehlende Datei ist `None`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let filler = format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}}}}\n",
            "x".repeat(4000)
        );
        let last = r#"{"type":"assistant","message":{"model":"claude-opus-4-1","content":[]}}"#;
        let first = r#"{"type":"assistant","message":{"model":"claude-haiku-4","content":[]}}"#;
        std::fs::write(&path, format!("{first}\n{filler}{last}\n")).unwrap();

        assert_eq!(
            model_in_tail(&path, 512).as_deref(),
            Some("claude-opus-4-1"),
            "Ausschnitt kleiner als die Datei"
        );
        assert_eq!(
            model_in_tail(&path, 1 << 20).as_deref(),
            Some("claude-opus-4-1"),
            "ganze Datei: der letzte Eintrag gewinnt"
        );
        assert_eq!(model_in_tail(&path, 8), None, "zu kurz für eine Zeile");
        assert_eq!(model_in_tail(&dir.path().join("missing.jsonl"), 512), None);
    }
}
