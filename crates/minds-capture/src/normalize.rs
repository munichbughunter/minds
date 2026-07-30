//! Aus einem rohen [`JournalEvent`] die Fakten ziehen, die eine
//! [`Session`](minds_core::Session) braucht — je Agent, was sich unterscheidet.
//!
//! Dieses Modul ist die Einlösung des Versprechens aus [`crate::hook_event`]:
//! „heute ein gemeinsamer Umschlag für alle, morgen ein `match` je Agent". Der
//! Umschlag (`session_id`, `transcript_path`, `cwd`, `hook_event_name`) ist bei
//! allen Agents gleich und wird schon auf dem heißen Pfad normalisiert. Was
//! *im* Payload steht — wie der Prompt-Text heißt, wie ein Tool-Aufruf seinen
//! Pfad benennt — ist dagegen agent-spezifisch und wird erst hier, auf dem
//! kalten Pfad, gedeutet.
//!
//! # Warum getrennt von `hook_event`
//!
//! [`hook_event::parse`](crate::hook_event::parse) läuft bei *jedem* Tool-Call
//! im Prozess des Nutzers und darf deshalb nichts Teures tun. Es hält den
//! Payload nur unverändert fest. Die Deutung — JSON-Felder herausparsen,
//! Tool-Namen auf [`EffectKind`] abbilden — passiert später beim Checkpoint,
//! wo Latenz niemandem wehtut. Derselbe Schnitt wie im ganzen Crate: heißer
//! Pfad sammelt, kalter Pfad deutet.
//!
//! # Was der Hook liefert und was nicht
//!
//! Ein Hook-Payload trägt, was *im Moment des Ereignisses* bekannt ist: den
//! Prompt bei `UserPromptSubmit`, Name und Eingabe eines Tools bei
//! `PreToolUse`. Er trägt **nicht** den Antworttext des Modells — der steht nur
//! im Transkript. Deshalb liefert dieses Modul bewusst nur die Hälfte: die
//! andere Hälfte fügt der Adapter in M5.6 aus dem Transkript hinzu.
//!
//! # Robust gegen Unbekanntes
//!
//! Ein Payload, den wir nicht deuten können (fremder Agent, neues Tool,
//! beschädigtes JSON), ergibt schlicht leere [`EventFacts`] — nie einen Fehler.
//! Das Vokabular der Agents wächst schneller als unseres; ein unbekanntes Tool
//! darf den Checkpoint einer ganzen Session nicht zum Absturz bringen. Was wir
//! heute nicht deuten, liegt über den rohen Payload im Journal weiter bereit.

use minds_core::{Effect, EffectKind};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::journal::{EventKind, JournalEvent};

/// Die gedeuteten Fakten eines einzelnen Hook-Events.
///
/// Alles ist `Option`: Ein `SessionStart` trägt weder Prompt noch Tool, ein
/// unbekanntes Tool trägt kein [`Effect`]. Der Adapter fragt gezielt das ab,
/// was der jeweilige [`EventKind`] erwarten lässt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFacts {
    /// Der Prompt-Text bei einem Prompt-Event.
    pub prompt: Option<String>,

    /// Der Tool-Aufruf bei einem Tool-Event.
    pub tool: Option<ToolFacts>,
}

/// Ein normalisierter Tool-Aufruf, so weit der Hook ihn hergibt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFacts {
    /// Der Name des Tools in der Sprache des Agents (`Read`, `Bash`, …).
    pub name: String,

    /// Die rohe Tool-Eingabe als bereits serialisiertes JSON. Landet
    /// unverändert in [`ToolCall::arguments`](minds_core::ToolCall::arguments),
    /// damit der spätere Hash nicht von der Formatierung abhängt.
    pub arguments: String,

    /// Was der Aufruf in der Welt tat, normalisiert — Pfad und Art. Der
    /// Inhalts-Hash bleibt hier `None`; ihn zu bilden ist M5.7.
    pub effect: Option<Effect>,
}

/// Deutet ein Journal-Event für den benannten Agenten.
///
/// `agent` kommt aus dem [`SessionKey`](crate::SessionKey), nicht aus dem
/// Payload — die Registrierung weiß besser, wer geschrieben hat, als das JSON.
/// Ein unbekannter Agent ergibt leere Fakten; sein roher Payload bleibt im
/// Journal erhalten.
pub fn facts(agent: &str, event: &JournalEvent) -> EventFacts {
    match event.kind {
        // Der Prompt ist das eine wirklich agent-übergreifende Feld: Claude,
        // Codex, Cursor und Gemini nennen ihn alle `prompt`. Ihn zu extrahieren
        // braucht deshalb keinen agent-spezifischen Normalisierer — sonst
        // verlöre ein noch nicht normalisierter Agent seine Prompts, und das
        // Journal wäre umsonst „ein Beobachter für alle".
        EventKind::Prompt => EventFacts {
            prompt: parse::<Prompt>(event).and_then(|p| p.prompt),
            tool: None,
        },
        // Die Tool→Effect-Abbildung ist dagegen agent-spezifisch (welcher
        // Tool-Name schreibt, welches Feld trägt den Pfad) und dispatcht je
        // Agent. Ein Agent ohne Normalisierer liefert hier `None`.
        EventKind::ToolPre | EventKind::ToolPost => EventFacts {
            prompt: None,
            tool: tool_facts(agent, event),
        },
        _ => EventFacts::default(),
    }
}

// ---------------------------------------------------------------------------
// Tool-Normalisierung je Agent
// ---------------------------------------------------------------------------

/// Dispatch der Tool-Deutung. Weitere Agents kommen als eigene Zweige hinzu;
/// jeder kennt nur die Felder *seines* Payloads.
fn tool_facts(agent: &str, event: &JournalEvent) -> Option<ToolFacts> {
    match agent {
        "claude-code" => parse::<Tool>(event).and_then(claude_tool),
        _ => None,
    }
}

/// Baut aus Claude Codes `tool_name` + `tool_input` einen [`ToolFacts`].
///
/// Die Abbildung Tool-Name → [`EffectKind`] ist das agent-spezifische Wissen:
/// Nur hier steht, dass `Edit` schreibt und `Bash` ausführt. Ein unbekanntes
/// Tool ist [`EffectKind::Other`] ohne Pfad — kein Fehler, nur weniger Detail.
fn claude_tool(t: Tool) -> Option<ToolFacts> {
    let name = t.tool_name?;

    // `tool_input` bleibt verbatim für `arguments`; den Effekt deutet die
    // gemeinsame Abbildung.
    let arguments = t
        .tool_input
        .as_ref()
        .map(|r| r.get().to_owned())
        .unwrap_or_default();
    let effect = claude_effect(&name, t.tool_input.as_deref());

    Some(ToolFacts {
        name,
        arguments,
        effect: Some(effect),
    })
}

/// Die agent-spezifische Abbildung Tool-Name + `tool_input` → [`Effect`] für
/// Claude Code — geteilt vom Journal-Pfad ([`facts`]) und vom Transkript-Import
/// ([`crate::import`]), damit „`Edit` schreibt, `Bash` führt aus" an genau einer
/// Stelle steht.
///
/// `content` bleibt `None`; der Artefakt-Hash wird erst beim Checkpoint gebildet.
/// Ein unbekanntes Tool ist [`EffectKind::Other`] ohne Pfad — kein Fehler, nur
/// weniger Detail.
pub fn claude_effect(tool_name: &str, tool_input: Option<&RawValue>) -> Effect {
    let paths = tool_input
        .and_then(|r| serde_json::from_str::<ToolPaths>(r.get()).ok())
        .unwrap_or_default();

    let (kind, path) = match tool_name {
        "Read" => (EffectKind::Read, paths.file_path),
        "Write" => (EffectKind::Write, paths.file_path),
        "Edit" | "MultiEdit" => (EffectKind::Write, paths.file_path),
        "NotebookEdit" => (EffectKind::Write, paths.notebook_path),
        "Bash" => (EffectKind::Exec, None),
        // Glob/Grep/WebFetch/Task/… greifen auf keinen einzelnen Pfad zu, den
        // sich ein Artefakt-Hash merken könnte. Sie sind Teil der Erzählung,
        // aber kein Datei-Effekt.
        _ => (EffectKind::Other, None),
    };

    Effect {
        kind,
        path,
        content: None,
    }
}

/// Liest den Payload eines Events in `T`; misslingt das, ergibt es `None`
/// statt eines Fehlers (siehe Modul-Doku).
fn parse<T: for<'de> Deserialize<'de>>(event: &JournalEvent) -> Option<T> {
    serde_json::from_str(event.payload.get()).ok()
}

/// Claude Codes `UserPromptSubmit`-Payload, nur das interessante Feld.
#[derive(Debug, Deserialize)]
struct Prompt {
    prompt: Option<String>,
}

/// Claude Codes Tool-Payload. `tool_input` bleibt als [`RawValue`] verbatim
/// erhalten, damit `arguments` nicht von einem serde-Roundtrip umformatiert
/// wird.
#[derive(Debug, Deserialize)]
struct Tool {
    tool_name: Option<String>,
    tool_input: Option<Box<RawValue>>,
}

/// Die zwei Pfadfelder, die bei den datei-berührenden Tools vorkommen — eine
/// tolerante Zweitdeutung des `tool_input`-Blocks. Alles Übrige wird ignoriert.
#[derive(Debug, Default, Deserialize)]
struct ToolPaths {
    file_path: Option<String>,
    notebook_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::value::RawValue;

    fn event(kind: EventKind, raw_kind: &str, payload: &str) -> JournalEvent {
        JournalEvent {
            seq: 0,
            at: "2026-07-23T09:12:04.512Z".into(),
            at_nanos: 0,
            kind,
            raw_kind: raw_kind.into(),
            cwd: None,
            transcript_path: None,
            payload: RawValue::from_string(payload.to_string()).unwrap(),
        }
    }

    #[test]
    fn a_prompt_yields_its_text() {
        let e = event(
            EventKind::Prompt,
            "UserPromptSubmit",
            r#"{"prompt":"Fix den Retry-Test","session_id":"x"}"#,
        );
        let f = facts("claude-code", &e);
        assert_eq!(f.prompt.as_deref(), Some("Fix den Retry-Test"));
        assert!(f.tool.is_none());
    }

    #[test]
    fn a_read_becomes_a_read_effect_with_path() {
        let e = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"Read","tool_input":{"file_path":"src/retry.rs"}}"#,
        );
        let t = facts("claude-code", &e).tool.unwrap();
        assert_eq!(t.name, "Read");
        let effect = t.effect.unwrap();
        assert_eq!(effect.kind, EffectKind::Read);
        assert_eq!(effect.path.as_deref(), Some("src/retry.rs"));
        assert!(effect.content.is_none(), "Inhalts-Hash ist M5.7");
    }

    #[test]
    fn edit_and_multiedit_write() {
        for name in ["Edit", "MultiEdit"] {
            let e = event(
                EventKind::ToolPost,
                "PostToolUse",
                &format!(r#"{{"tool_name":"{name}","tool_input":{{"file_path":"a.rs"}}}}"#),
            );
            let effect = facts("claude-code", &e).tool.unwrap().effect.unwrap();
            assert_eq!(effect.kind, EffectKind::Write, "{name}");
            assert_eq!(effect.path.as_deref(), Some("a.rs"));
        }
    }

    #[test]
    fn bash_is_exec_without_a_path() {
        let e = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
        );
        let effect = facts("claude-code", &e).tool.unwrap().effect.unwrap();
        assert_eq!(effect.kind, EffectKind::Exec);
        assert!(effect.path.is_none());
    }

    #[test]
    fn an_unknown_tool_is_other_not_an_error() {
        let e = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"WebFetch","tool_input":{"url":"https://example.com"}}"#,
        );
        let effect = facts("claude-code", &e).tool.unwrap().effect.unwrap();
        assert_eq!(effect.kind, EffectKind::Other);
        assert!(effect.path.is_none());
    }

    #[test]
    fn arguments_keep_the_raw_tool_input() {
        let e = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"Bash","tool_input":{"command":"echo hi","z":1}}"#,
        );
        let t = facts("claude-code", &e).tool.unwrap();
        assert!(t.arguments.contains("echo hi"));
        assert!(t.arguments.contains("\"z\""));
    }

    #[test]
    fn an_unknown_agent_still_gets_the_prompt_but_no_tool() {
        // Der Prompt ist agent-uebergreifend und darf nie verloren gehen; die
        // Tool-Deutung braucht dagegen einen Normalisierer.
        let prompt = event(EventKind::Prompt, "UserPromptSubmit", r#"{"prompt":"hi"}"#);
        assert_eq!(
            facts("some-future-agent", &prompt).prompt.as_deref(),
            Some("hi")
        );

        let tool = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#,
        );
        assert!(facts("some-future-agent", &tool).tool.is_none());
    }

    #[test]
    fn a_broken_payload_yields_empty_facts_not_a_panic() {
        // So legt hook_event ein an der stdin-Grenze abgeschnittenes Event ab:
        // als gültigen JSON-*String*, nicht als Objekt. Als Tool gedeutet ergibt
        // das None statt eines Fehlers.
        let wrapped = serde_json::to_string(r#"{"tool_name":"Read","tool_res"#).unwrap();
        let e = event(EventKind::ToolPre, "PreToolUse", &wrapped);
        assert_eq!(facts("claude-code", &e), EventFacts::default());
    }
}
