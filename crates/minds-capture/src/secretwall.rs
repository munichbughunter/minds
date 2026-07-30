//! Die Mauer vor Zugangsdaten-Dateien — durchgesetzt auf dem heißen Pfad.
//!
//! [`minds_redact::secretfile`] liefert seit M2 nur das *Prädikat* („ist dieser
//! Pfad eine Zugangsdaten-Datei?") und kündigt an, dass die Durchsetzung in
//! `minds-capture` sitzt. Das ist dieses Modul.
//!
//! # Was hier geschützt wird und warum am Hook
//!
//! Liest ein Agent eine `.env`, trägt das **PostToolUse**-Event ihren gesamten
//! Inhalt im `tool_response`. Genau dieser Inhalt soll nirgends aufgehoben
//! werden — auch nicht im lokalen Journal, das zwar 0600 und außerhalb von Git
//! liegt, aber eben doch auf der Platte. `hook.rs` hat diese offene Flanke bis
//! hierher benannt; [`guard`] schließt sie, *bevor* das Byte geschrieben wird.
//!
//! Der Milestone heißt „Mauer bei PreToolUse", weil die Entscheidung
//! **pfad-basiert** ist: Der Pfad steht schon im PreToolUse fest, lange bevor
//! der Inhalt existiert. Ein Detektor über den Inhalt wäre die falsche Bauform
//! (siehe [`minds_redact::secretfile`]) — bei einer Datei, deren einziger Zweck
//! Zugangsdaten sind, ist die einzige belastbare Aussage: *alles hier drin ist
//! verdächtig.* Deshalb wird nicht geschwärzt, sondern **weggelassen**.
//!
//! # Was bleibt, was geht
//!
//! Erhalten bleiben Tool-Name und Pfad — beides kein Geheimnis (der Pfad kann
//! einen Benutzernamen enthalten, aber das ist PII und Sache der Pipeline auf
//! dem kalten Pfad, nicht der Mauer). Ersetzt wird **alles Übrige**, allen voran
//! `tool_response`. Der Reader sieht am Marker [`minds_redact::SECRET_FILE_PLACEHOLDER`],
//! dass hier eine ganze Datei ausgelassen wurde — nicht ein einzelner Wert.
//!
//! # Grenzen, ehrlich benannt
//!
//! Lässt sich der Payload nicht deuten (abgeschnitten, fremdes Format), kann die
//! Mauer den Pfad nicht sehen und greift nicht — das Event geht unverändert ins
//! Journal. Das ist bewusst fail-**open** wie der ganze Hook: Die permanente
//! Grenze liegt ohnehin auf dem kalten Pfad, wo derselbe Pfad-Test über den
//! [`Effect`](minds_core::Effect) erneut greift und zusätzlich entscheidet, dass
//! für eine solche Datei **kein Inhalts-Hash** gebildet wird (M5.6/M5.7). Das
//! Journal ist die ephemere, lokale Stufe; der Store ist die, die zählt.

use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use minds_redact::{SECRET_FILE_PLACEHOLDER, secret_file_reason};

use crate::journal::{EventKind, NewEvent};

/// Prüft ein Tool-Event und ersetzt seinen Payload, falls er eine
/// Zugangsdaten-Datei berührt. Gibt den Regelnamen zurück, wenn die Mauer
/// gegriffen hat — für Diagnose und Audit.
///
/// Nicht-Tool-Events und Tool-Events auf gewöhnliche Dateien bleiben Byte für
/// Byte unangetastet.
pub fn guard(event: &mut NewEvent) -> Option<&'static str> {
    if !matches!(event.kind, EventKind::ToolPre | EventKind::ToolPost) {
        return None;
    }

    let tool: Tool = serde_json::from_str(event.payload.get()).ok()?;
    let name = tool.tool_name?;
    let input = tool.tool_input?;
    let (path_key, path, reason) = secret_path_in(&input)?;

    event.payload = sanitized(&name, path_key, path, reason);
    Some(reason)
}

/// Baut den minimalen Ersatz-Payload: Tool-Name, der Pfad unter seinem
/// Originalschlüssel, der Auslass-Marker und der Grund.
fn sanitized(name: &str, path_key: &str, path: &str, reason: &str) -> Box<RawValue> {
    let value = serde_json::json!({
        "tool_name": name,
        "tool_input": { path_key: path },
        "minds_omitted": SECRET_FILE_PLACEHOLDER,
        "minds_secret_file_reason": reason,
    });
    // `serde_json::json!` erzeugt per Konstruktion gültiges JSON.
    RawValue::from_string(value.to_string()).expect("json! ist gültiges JSON")
}

/// Tool-Name und die rohe Eingabe als generische Map — der Schlüssel des
/// Pfad-Felds ist agent-spezifisch und wird erst in [`secret_path_in`] gedeutet.
#[derive(Debug, Deserialize)]
struct Tool {
    tool_name: Option<String>,
    tool_input: Option<Map<String, Value>>,
}

/// Feldnamen, die über Agents hinweg einen Datei-Pfad tragen — mit Priorität
/// geprüft. Claude nutzt `file_path`/`notebook_path`; andere Agents (Codex,
/// Gemini, …) `path`, `absolute_path`, `filename` u. a.
const KNOWN_PATH_KEYS: &[&str] = &[
    "file_path",
    "notebook_path",
    "path",
    "absolute_path",
    "abs_path",
    "filepath",
    "filename",
    "file",
    "target_file",
    "target_path",
    "source_file",
    "src_path",
];

/// Sucht in `tool_input` einen Pfad, der eine Zugangsdaten-Datei ist — fail-closed
/// und **agent-agnostisch**.
///
/// Zuerst die bekannten Pfad-Schlüssel in fester Priorität, dann **jedes** weitere
/// Feld, dessen Name auf einen Pfad hindeutet (enthält `path` oder `file`).
/// Bewusst **nicht** gescannt werden Felder wie `command`, `query`, `content`:
/// sonst würde ein `cat .env` in einem Bash-Aufruf fälschlich als Datei-Lesung
/// gewertet (`secret_file_reason` matcht das `.env`-Suffix auch mitten im Text).
///
/// Die Iterationsreihenfolge ist deterministisch: `serde_json::Map` ist nach
/// Schlüsseln sortiert.
fn secret_path_in(input: &Map<String, Value>) -> Option<(&str, &str, &'static str)> {
    for key in KNOWN_PATH_KEYS {
        if let Some(Value::String(value)) = input.get(*key) {
            if let Some(reason) = secret_file_reason(value) {
                return Some((*key, value, reason));
            }
        }
    }
    for (key, value) in input {
        if KNOWN_PATH_KEYS.contains(&key.as_str()) {
            continue;
        }
        if let Value::String(text) = value {
            if looks_like_path_key(key) {
                if let Some(reason) = secret_file_reason(text) {
                    return Some((key.as_str(), text, reason));
                }
            }
        }
    }
    None
}

/// Ob der Feldname auf einen Datei-Pfad hindeutet — die Naht, die unbekannte
/// Agents mitnimmt, ohne `command`/`query` zu treffen.
fn looks_like_path_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("path") || lower.contains("file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_event(kind: EventKind, payload: &str) -> NewEvent {
        NewEvent {
            at: "2026-07-23T09:12:04.512Z".into(),
            at_nanos: 0,
            kind,
            raw_kind: "PostToolUse".into(),
            cwd: None,
            transcript_path: None,
            payload: RawValue::from_string(payload.to_string()).unwrap(),
        }
    }

    #[test]
    fn a_dotenv_read_loses_its_response() {
        let mut e = tool_event(
            EventKind::ToolPost,
            r#"{"tool_name":"Read","tool_input":{"file_path":".env"},"tool_response":"DB_PASSWORD=hunter2"}"#,
        );
        let reason = guard(&mut e);

        assert_eq!(reason, Some("dotenv"));
        let payload = e.payload.get();
        assert!(
            !payload.contains("hunter2"),
            "der Inhalt darf nicht bleiben"
        );
        assert!(!payload.contains("tool_response"));
        assert!(payload.contains("[omitted:secret-file]"));
        assert!(payload.contains(".env"), "Pfad bleibt");
        assert!(payload.contains("dotenv"), "Grund bleibt");
    }

    #[test]
    fn an_ordinary_file_is_untouched() {
        let original = r#"{"tool_name":"Read","tool_input":{"file_path":"src/retry.rs"},"tool_response":"fn main(){}"}"#;
        let mut e = tool_event(EventKind::ToolPost, original);
        assert_eq!(guard(&mut e), None);
        assert_eq!(e.payload.get(), original, "kein Byte geändert");
    }

    #[test]
    fn a_pem_key_is_walled_at_pretooluse() {
        // Der Pfad steht schon beim PreToolUse fest — daher der Milestone-Name.
        let mut e = tool_event(
            EventKind::ToolPre,
            r#"{"tool_name":"Read","tool_input":{"file_path":"certs/server.pem"}}"#,
        );
        assert_eq!(guard(&mut e), Some("private-key"));
        assert!(e.payload.get().contains("[omitted:secret-file]"));
    }

    #[test]
    fn a_template_file_is_not_walled() {
        // .env.example trägt keine echten Werte — die Pipeline reicht.
        let original = r#"{"tool_name":"Read","tool_input":{"file_path":".env.example"},"tool_response":"DB_PASSWORD="}"#;
        let mut e = tool_event(EventKind::ToolPost, original);
        assert_eq!(guard(&mut e), None);
        assert_eq!(e.payload.get(), original);
    }

    #[test]
    fn notebook_path_is_recognized() {
        let mut e = tool_event(
            EventKind::ToolPre,
            r#"{"tool_name":"NotebookEdit","tool_input":{"notebook_path":"secrets/.env"}}"#,
        );
        assert_eq!(guard(&mut e), Some("dotenv"));
        assert!(e.payload.get().contains("notebook_path"));
    }

    #[test]
    fn a_prompt_event_is_never_touched() {
        let original = r#"{"prompt":"lies die .env"}"#;
        let mut e = tool_event(EventKind::Prompt, original);
        assert_eq!(guard(&mut e), None);
        assert_eq!(e.payload.get(), original);
    }

    #[test]
    fn bash_without_a_path_is_untouched() {
        let original = r#"{"tool_name":"Bash","tool_input":{"command":"cat .env"}}"#;
        let mut e = tool_event(EventKind::ToolPre, original);
        // `command` ist kein Pfad-Feld — die Mauer greift pfad-basiert nicht, obwohl
        // `secret_file_reason("cat .env")` das `.env`-Suffix matchen würde. Der
        // `cat .env`-Fall wird über den PostToolUse-Inhalt der *Read*-Tools und die
        // Redaction-Pipeline abgedeckt, nicht hier.
        assert_eq!(guard(&mut e), None);
        assert_eq!(e.payload.get(), original);
    }

    // --- Schicht 0: agent-agnostisch, fail-closed für alle Agents ------------

    #[test]
    fn a_non_claude_path_field_is_walled() {
        // Gemini/Codex nennen den Pfad nicht `file_path`. Vorher rutschte das
        // durch — genau das Sicherheitsloch.
        for payload in [
            r#"{"tool_name":"read_file","tool_input":{"path":"/home/x/.env"}}"#,
            r#"{"tool_name":"read","tool_input":{"absolute_path":"certs/server.pem"}}"#,
            r#"{"tool_name":"open","tool_input":{"filename":".git-credentials"}}"#,
        ] {
            let mut e = tool_event(EventKind::ToolPre, payload);
            assert!(
                guard(&mut e).is_some(),
                "nicht-Claude-Pfadfeld nicht geschützt: {payload}"
            );
            assert!(e.payload.get().contains("[omitted:secret-file]"));
        }
    }

    #[test]
    fn an_unknown_path_named_field_is_walled_by_heuristic() {
        // Ein Agent, den wir noch nicht kennen, mit einem selbst benannten
        // Pfad-Feld: greift über die Namens-Heuristik (enthält „file"/„path").
        let mut e = tool_event(
            EventKind::ToolPost,
            r#"{"tool_name":"X","tool_input":{"input_file":".env"},"tool_response":"DB_PASSWORD=hunter2"}"#,
        );
        assert_eq!(guard(&mut e), Some("dotenv"));
        assert!(!e.payload.get().contains("hunter2"));
    }

    #[test]
    fn a_non_path_field_with_a_dotenv_value_is_not_walled() {
        // `query`/`content` sind keine Pfade — ein `.env` darin darf die Mauer
        // nicht auslösen (sonst Fehlalarm auf jeden Text, der „.env" erwähnt).
        for payload in [
            r#"{"tool_name":"grep","tool_input":{"query":"grep .env"}}"#,
            r#"{"tool_name":"edit","tool_input":{"content":"lies die .env"}}"#,
        ] {
            let mut e = tool_event(EventKind::ToolPre, payload);
            assert_eq!(guard(&mut e), None, "Fehlalarm auf {payload}");
            assert_eq!(e.payload.get(), payload);
        }
    }
}
