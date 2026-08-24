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

use minds_core::{Capture, CaptureStatus, Effect, EffectKind};
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

/// Versionsstand der Claude-Code-Deutung. Bump bei jeder Änderung an
/// [`claude_effect`] oder der Turn-Bildung — damit eine gespeicherte Deutung
/// ihrem Stand zuordenbar bleibt (Interpretation ist wiederholbar, ADR-0011).
pub const CLAUDE_ADAPTER_VERSION: u32 = 1;

/// Versionsstand des generischen Fallbacks für Agents ohne eigenen Adapter.
pub const GENERIC_ADAPTER_VERSION: u32 = 1;

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

    /// Ob dieser Aufruf gedeutet wurde, und von wem (ADR-0011).
    pub capture: Capture,
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

// ---------------------------------------------------------------------------
// Der ToolAdapter-Trait (Phase 5, Plan-v0.2 A.2)
// ---------------------------------------------------------------------------

/// Deutet die Tool-Ebene **eines** Agents.
///
/// Zwei Architektur-Regeln, beide nicht verhandelbar (ADR-0011):
///
/// 1. **Adapter sitzen ÜBER der Evidence Chain.** Sie lesen Journal-Events
///    bzw. gespeicherte Aufrufe — sie verändern nie deren Bytes, Hashes oder
///    Identität. Ein neuer Adapter deutet dieselbe Evidence anders; die
///    Evidence bleibt dieselbe.
/// 2. **Deutung ist deterministisch.** Gleiche Evidence + gleiche
///    Adapter-Version + gleiche Regeln ⇒ gleiche Deutung. Ohne das wäre
///    `minds reinterpret` wertlos — testfixiert in
///    `interpretation_is_deterministic`.
pub trait ToolAdapter: Sync {
    /// Der Agent, dessen Payloads dieser Adapter deutet.
    fn agent(&self) -> &'static str;

    /// Versionsstand der Deutung — Bump bei jeder Deutungsänderung, damit
    /// eine gespeicherte Deutung ihrem Stand zuordenbar bleibt.
    fn version(&self) -> u32;

    /// Deutet ein Tool-Event vom Journal (Checkpoint-Pfad).
    fn tool_facts(&self, event: &JournalEvent) -> Option<ToolFacts>;

    /// Deutet einen **gespeicherten** Aufruf neu — aus `name` und den
    /// erhaltenen `arguments` (der Reinterpretations-Pfad, ohne Journal).
    /// `None` heißt: Dieser Adapter kann daraus keine Wirkung ableiten.
    fn interpret_stored(&self, name: &str, arguments: &str) -> Option<StoredInterpretation>;
}

/// Das Ergebnis einer (Re-)Deutung eines gespeicherten Aufrufs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredInterpretation {
    /// Die gedeutete Wirkung.
    pub effect: Effect,
    /// Gedeutet oder weiterhin nur beobachtet.
    pub status: CaptureStatus,
    /// Wer gedeutet hat, mit welchem Stand.
    pub adapter: &'static str,
    /// Der Versionsstand.
    pub adapter_version: u32,
}

/// Der Claude-Code-Adapter — die Referenz-Implementierung.
pub struct ClaudeAdapter;

impl ToolAdapter for ClaudeAdapter {
    fn agent(&self) -> &'static str {
        "claude-code"
    }

    fn version(&self) -> u32 {
        CLAUDE_ADAPTER_VERSION
    }

    fn tool_facts(&self, event: &JournalEvent) -> Option<ToolFacts> {
        parse::<Tool>(event).and_then(claude_tool)
    }

    fn interpret_stored(&self, name: &str, arguments: &str) -> Option<StoredInterpretation> {
        // `arguments` ist bei Claude-Aufrufen das verbatim erhaltene
        // `tool_input` — genau das Material, das `claude_effect` deutet.
        let raw = RawValue::from_string(arguments.to_string()).ok();
        let effect = claude_effect(name, raw.as_deref());
        let status = if claude_tool_is_interpreted(name) {
            CaptureStatus::Interpreted
        } else {
            CaptureStatus::Uninterpreted
        };
        Some(StoredInterpretation {
            effect,
            status,
            adapter: "claude-code",
            adapter_version: CLAUDE_ADAPTER_VERSION,
        })
    }
}

/// Die Registry: ein Adapter je Agent. Wer hier fehlt, bekommt den
/// generischen Fallback — beobachtet, nicht gedeutet, nie Stille.
const ADAPTERS: &[&dyn ToolAdapter] = &[&ClaudeAdapter];

/// Der Adapter für einen Agenten, falls es einen gibt.
pub fn adapter_for(agent: &str) -> Option<&'static dyn ToolAdapter> {
    ADAPTERS.iter().copied().find(|a| a.agent() == agent)
}

/// Dispatch der Tool-Deutung über die Registry.
///
/// Ein Agent **ohne** eigenen Adapter landet nicht mehr in der Stille: Der
/// generische Fallback baut einen beobachteten, aber ungedeuteten Aufruf —
/// „Ich habe gesehen, dass ein Tool lief" ist eine Aussage, das frühere
/// `None` war keine (ADR-0011).
fn tool_facts(agent: &str, event: &JournalEvent) -> Option<ToolFacts> {
    match adapter_for(agent) {
        Some(adapter) => adapter.tool_facts(event),
        None => Some(generic_tool(event)),
    }
}

/// Obergrenze für generisch eingefrorene Payloads. Darüber wird der Payload
/// als **Ganzes** durch einen Marker ersetzt — nie angeschnitten, damit kein
/// halbiertes Token die Formerkennung der Redaction unterläuft (dieselbe
/// Regel wie beim `hook.log`).
const GENERIC_ARGUMENTS_CAP: usize = 256 * 1024;

/// Der generische Fallback: Name so gut wie möglich, die Roh-Argumente als
/// Beweismittel, keine gedeutete Wirkung.
///
/// `arguments` trägt den **ganzen** Payload: Das Journal wird nach dem
/// Checkpoint gelöscht — was hier nicht in die Session wandert, ist weg.
/// Zwei Härtungen, weil das Format des Agents unbekannt ist:
///
/// - **Rekursive Secretfile-Mauer:** Die Hot-Path-Wall kennt nur die
///   Top-Level-Pfadschlüssel bekannter Agents. Hier wird jeder String-Wert
///   des Payloads gegen [`minds_redact::is_secret_file`] geprüft; trifft
///   einer (`/home/x/.env`, `id_rsa`, …), wird der **ganze** Payload durch
///   den Marker ersetzt — fail-closed, wie die Wall selbst.
/// - **Größendeckel:** jenseits von [`GENERIC_ARGUMENTS_CAP`] ersetzt ein
///   Marker den Payload als Ganzes (nie anschneiden).
///
/// Die Redaction scannt `arguments` danach wie jeden Text; gespeichert wird
/// also die redigierte Fassung des Beweismittels, nie Klartext-Geheimnisse.
/// Was die Detektoren dort **nicht** erkennen (verschachtelte
/// Low-Entropy-Credentials in fremden Formaten), bleibt eine benannte
/// Grenze — siehe ADR-0011.
fn generic_tool(event: &JournalEvent) -> ToolFacts {
    let name = parse::<Tool>(event)
        .and_then(|t| t.tool_name)
        .unwrap_or_else(|| event.raw_kind.clone());
    let raw = event.payload.get();
    let arguments = if raw.len() > GENERIC_ARGUMENTS_CAP {
        format!(
            "[minds: Payload nicht übernommen — {} Bytes über dem Deckel]",
            raw.len()
        )
    } else if let Some(reason) = secret_path_anywhere(raw) {
        format!("[minds: Payload nicht übernommen — Secretfile-Pfad im Inhalt ({reason})]")
    } else {
        raw.to_owned()
    };
    ToolFacts {
        name,
        arguments,
        effect: None,
        capture: Capture {
            status: CaptureStatus::Uninterpreted,
            adapter: "generic".into(),
            adapter_version: GENERIC_ADAPTER_VERSION,
        },
    }
}

/// Rekursiv über den Payload: Nennt irgendein String-Wert eine
/// Secret-Datei, gibt es den Regelnamen zurück. Unparsebarer Payload ⇒
/// `None` (dann greift nur die Text-Redaction — mehr wissen wir nicht).
fn secret_path_anywhere(raw: &str) -> Option<&'static str> {
    fn walk(value: &serde_json::Value) -> Option<&'static str> {
        match value {
            serde_json::Value::String(s) => minds_redact::secret_file_reason(s),
            serde_json::Value::Array(items) => items.iter().find_map(walk),
            serde_json::Value::Object(map) => map.values().find_map(walk),
            _ => None,
        }
    }
    walk(&serde_json::from_str(raw).ok()?)
}

/// Kennt die Claude-Deutung dieses Tools eine Wirkung? Geteilt mit dem
/// Transkript-Import, damit Journal- und Import-Pfad denselben Stand melden.
///
/// Die Liste ist dieselbe wie in [`claude_effect`] — hier steht nur die
/// Frage „gedeutet oder bloß beobachtet?". Glob/Grep/WebFetch/Task sind
/// bewusst **nicht** gedeutet: Sie sind Teil der Erzählung, aber ihre Wirkung
/// wird nicht normalisiert, und genau das sagt der Capture-Status jetzt.
pub fn claude_tool_is_interpreted(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "Bash"
    )
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
    let status = if claude_tool_is_interpreted(&name) {
        CaptureStatus::Interpreted
    } else {
        CaptureStatus::Uninterpreted
    };

    Some(ToolFacts {
        capture: Capture {
            status,
            adapter: "claude-code".into(),
            adapter_version: CLAUDE_ADAPTER_VERSION,
        },
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
            payload_hash: None,
            event_hash: None,
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
    fn an_unknown_agent_still_gets_the_prompt_and_an_uninterpreted_tool() {
        // Der Prompt ist agent-uebergreifend und darf nie verloren gehen.
        let prompt = event(EventKind::Prompt, "UserPromptSubmit", r#"{"prompt":"hi"}"#);
        assert_eq!(
            facts("some-future-agent", &prompt).prompt.as_deref(),
            Some("hi")
        );

        // Seit ADR-0011 verschwindet auch der Tool-Aufruf nicht mehr in der
        // Stille: Er kommt als beobachtet-aber-ungedeutet, mit dem ganzen
        // Payload als Beweismittel (das Journal wird nach dem Checkpoint
        // geloescht — was hier fehlt, ist weg).
        let payload = r#"{"tool_name":"apply_patch","tool_input":{"diff":"…"}}"#;
        let tool = event(EventKind::ToolPre, "PreToolUse", payload);
        let got = facts("some-future-agent", &tool).tool.expect("Fallback");
        assert_eq!(got.name, "apply_patch");
        assert_eq!(got.arguments, payload);
        assert!(got.effect.is_none());
        assert_eq!(got.capture.status, CaptureStatus::Uninterpreted);
        assert_eq!(got.capture.adapter, "generic");

        // Ohne parsbares tool_name-Feld traegt der rohe Event-Name den Namen.
        let opaque = event(EventKind::ToolPre, "WeirdHook", r#"{"x":1}"#);
        let got = facts("some-future-agent", &opaque).tool.expect("Fallback");
        assert_eq!(got.name, "WeirdHook");
    }

    #[test]
    fn the_recursive_wall_also_sees_arrays() {
        // Der Array-Zweig von `secret_path_anywhere`: ein Secretfile-Pfad in
        // einer Liste laesst den GANZEN Payload zum Marker werden.
        let payload = r#"{"tool_name":"batch_read","tool_input":{"files":["src/main.rs","/home/anna/.aws/credentials"]}}"#;
        let ev = event(EventKind::ToolPre, "PreToolUse", payload);
        let got = facts("some-future-agent", &ev).tool.expect("Fallback");
        assert!(
            got.arguments
                .starts_with("[minds: Payload nicht übernommen"),
            "{}",
            got.arguments
        );
        assert!(!got.arguments.contains(".aws"), "{}", got.arguments);
    }

    #[test]
    fn interpretation_is_deterministic() {
        // Architektur-Regel (ADR-0011): gleiche Evidence + gleiche
        // Adapter-Version ⇒ gleiche Deutung. Ohne das waere `minds
        // reinterpret` wertlos.
        let ev = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"Edit","tool_input":{"file_path":"a.rs"}}"#,
        );
        assert_eq!(facts("claude-code", &ev), facts("claude-code", &ev));

        let a = ClaudeAdapter.interpret_stored("Edit", r#"{"file_path":"a.rs"}"#);
        let b = ClaudeAdapter.interpret_stored("Edit", r#"{"file_path":"a.rs"}"#);
        assert_eq!(a, b);
        let got = a.unwrap();
        assert_eq!(got.status, CaptureStatus::Interpreted);
        assert_eq!(got.effect.kind, EffectKind::Write);
        assert_eq!(got.effect.path.as_deref(), Some("a.rs"));
        assert_eq!(got.adapter_version, CLAUDE_ADAPTER_VERSION);
    }

    #[test]
    fn the_registry_resolves_known_agents_and_only_those() {
        assert!(adapter_for("claude-code").is_some());
        assert!(adapter_for("codex").is_none());
        assert_eq!(
            adapter_for("claude-code").unwrap().version(),
            CLAUDE_ADAPTER_VERSION
        );
    }

    #[test]
    fn a_known_claude_tool_is_interpreted_an_unknown_one_is_not() {
        let read = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#,
        );
        let got = facts("claude-code", &read).tool.unwrap();
        assert_eq!(got.capture.status, CaptureStatus::Interpreted);
        assert_eq!(got.capture.adapter, "claude-code");
        assert_eq!(got.capture.adapter_version, CLAUDE_ADAPTER_VERSION);

        // Glob ist Teil der Erzaehlung, aber seine Wirkung ist nicht
        // normalisiert — genau das sagt der Status jetzt.
        let glob = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"Glob","tool_input":{"pattern":"*.rs"}}"#,
        );
        let got = facts("claude-code", &glob).tool.unwrap();
        assert_eq!(got.capture.status, CaptureStatus::Uninterpreted);
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
