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

use minds_core::{Capture, CaptureStatus, Effect, EffectKind, WrittenUnavailable};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::exec_outcome::ExecReport;
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
/// [`claude_effect`], [`claude_written`] oder der Turn-Bildung — damit eine
/// gespeicherte Deutung ihrem Stand zuordenbar bleibt (Interpretation ist
/// wiederholbar, ADR-0011).
///
/// - v1: Tool → Wirkung und Pfad.
/// - v2: zusätzlich der Schreibzeit-Hash [`Effect::written`] aus dem
///   PostToolUse-Payload (EA-01a). Sessions aus v1 bleiben ohne ihn.
/// - v3: zusätzlich das Ergebnis bekannter Test-/Benchmark-Runner
///   ([`ToolCall::outcome`](minds_core::ToolCall::outcome)) und der Hinweis
///   `compound command not interpreted` (EA-18a). Sessions aus v1/v2 bleiben
///   ohne beides.
pub const CLAUDE_ADAPTER_VERSION: u32 = 3;

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
    ///
    /// [`Effect::written`] bleibt hier immer `None`: Er entsteht aus dem
    /// PostToolUse-Payload, und der liegt nach dem Checkpoint nicht mehr vor.
    /// Gespeichertes `written` ist damit Beweismittel des Checkpoints, nicht
    /// wiederholbare Deutung — `minds reinterpret` übernimmt es unverändert.
    fn interpret_stored(&self, name: &str, arguments: &str) -> Option<StoredInterpretation>;

    /// Die Korrelationskennung eines Tool-Events — dieselbe bei PreToolUse
    /// und PostToolUse **desselben** Aufrufs. `None`: Der Agent liefert
    /// keine, dann bleibt Pre und Post unverbunden (und `written` fehlt mit
    /// [`WrittenUnavailable::PayloadWithoutContent`]). Raten über die
    /// Reihenfolge wäre bei parallelen Tool-Aufrufen eine falsche Zuordnung.
    fn call_id(&self, _event: &JournalEvent) -> Option<String> {
        None
    }

    /// Leitet aus dem PostToolUse-Event die Bytes ab, die das Tool laut
    /// Payload geschrieben hat — die Grundlage des Schreibzeit-Hashes.
    ///
    /// `path` ist der Pfad des Effekts (aus dem Pre-Event, schon durch Mauer
    /// und Grenze). Der Adapter muss ihn gegen den Post-Payload
    /// gegenprüfen: Ein Post, der einen anderen Pfad nennt, gehört nicht zu
    /// diesem Aufruf — dann keine Bytes.
    ///
    /// **Vertrag für den Aufrufer:** Secretfile-Mauer und Repo-Grenze sind
    /// *vor* diesem Aufruf geprüft, Redaction-Scan und Hash kommen *danach*
    /// (siehe `adapter::written_hashes`) — beides Regeln des Checkpoints,
    /// nicht des Adapters. Der Adapter sieht den Payload nur für Pfade, die
    /// Mauer und Grenze passiert haben, und hasht nie selbst.
    /// Der Default: Dieser Agent trägt keinen Inhalt im Payload.
    fn written_bytes(&self, _post: &JournalEvent, _path: &str) -> WrittenOutcome {
        Err(WrittenUnavailable::PayloadWithoutContent)
    }

    /// Deutet dieser Adapter Shell-Aufrufe auf Runner-Ergebnisse
    /// ([`ToolCall::outcome`](minds_core::ToolCall::outcome), EA-18a)? Nur
    /// dann setzt der Checkpoint Ergebnis und Hinweis — ein Adapter, dessen
    /// Payload-Form nicht aufgezeichnet ist, bleibt ohne beides. Der Default:
    /// nein.
    fn exec_outcomes(&self) -> bool {
        false
    }

    /// Zieht aus dem Post-Event eines Shell-Aufrufs die sichtbare Ausgabe
    /// und den Exit-Code (falls der Payload ihn trägt).
    ///
    /// `command` ist das Kommando aus dem Pre-Event; der Post-Payload muss
    /// dasselbe nennen, sonst gehört er nicht zu diesem Aufruf (dieselbe
    /// Gegenprobe wie beim Pfad in [`written_bytes`](Self::written_bytes)).
    /// `None`, wenn die Ausgabe nicht vollständig vorliegt (abgebrochen,
    /// gekürzt) — eine halbe Ausgabe ergäbe zu kleine Zähler.
    fn exec_report(&self, _post: &JournalEvent, _command: &str) -> Option<ExecReport> {
        None
    }
}

/// Ergebnis der Schreibzeit-Deutung: die geschriebenen Bytes, oder warum es
/// keine gibt.
pub type WrittenOutcome = Result<String, WrittenUnavailable>;

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

    fn call_id(&self, event: &JournalEvent) -> Option<String> {
        parse::<CallId>(event)?.tool_use_id
    }

    fn written_bytes(&self, post: &JournalEvent, path: &str) -> WrittenOutcome {
        use WrittenUnavailable::PayloadWithoutContent;
        // `PostToolUseFailure` faellt in `hook_event::classify` bewusst mit
        // `PostToolUse` auf `ToolPost` zusammen — fuer den Record dasselbe
        // Ereignis, fuer den Schreibzeit-Hash nicht: Ein gescheiterter Write
        // hat nichts geschrieben. Nur der gelungene Aufruf traegt Bytes.
        if post.raw_kind != "PostToolUse" {
            return Err(PayloadWithoutContent);
        }
        let tool = parse::<PostTool>(post).ok_or(PayloadWithoutContent)?;
        let name = tool.tool_name.ok_or(PayloadWithoutContent)?;
        // Gegenprobe: Der Post-Payload muss denselben Pfad nennen wie der
        // Effekt. So haengt die Mauer-Aussage (geprueft am Pfad des
        // Pre-Events) nicht an einer Kennung allein.
        if !claude_post_names_path(tool.tool_input.as_ref(), tool.tool_response.as_ref(), path) {
            return Err(PayloadWithoutContent);
        }
        claude_written(&name, tool.tool_response.as_ref())
    }

    fn exec_outcomes(&self) -> bool {
        true
    }

    fn exec_report(&self, post: &JournalEvent, command: &str) -> Option<ExecReport> {
        claude_exec_report(post, command)
    }
}

/// Ab dieser Länge (Zeichen) gilt eine Ausgabe als möglicherweise gekürzt —
/// knapp unter der beobachteten Kürzungsgrenze für `stdout` (29 761
/// behaltene Zeichen, siehe Fixture-README). Gilt für den Fehlertext, dessen
/// Kürzung nicht aufgezeichnet ist, und für `stdout` zusätzlich zu den
/// `persisted*`-Markern: Eine andere Claude-Code-Version könnte ohne Marker
/// kürzen, und libtest summiert über Binaries.
const TRUNCATION_SUSPECT: usize = 29_000;

/// Liegt `text` in der Nähe der Kürzungsgrenze? Gemessen in **Bytes**: Ob
/// Claude Code nach Zeichen, UTF-16-Einheiten oder Bytes kürzt, ist nicht
/// belegt — Bytes sind nie weniger als Zeichen oder UTF-16-Einheiten, die
/// Schranke greift also in jedem Fall zuerst (fail-closed).
fn suspect_truncated(text: &str) -> bool {
    text.len() >= TRUNCATION_SUSPECT
}

/// Die Ausgabe eines `Bash`-Aufrufs aus Claude Codes Post-Payload — die
/// Formen sind **aufgezeichnet** (Claude Code 2.1.292, siehe
/// `tests/fixtures/claude-code/README.md`):
///
/// - `PostToolUse`: `tool_response.{stdout, stderr, interrupted, isImage,
///   noOutputExpected}`. stderr ist in `stdout` eingemischt; einen Exit-Code
///   trägt der Payload **nicht**. Ab etwa 30 000 Zeichen behält Claude Code
///   nur den **Anfang** von `stdout` und legt die volle Ausgabe in einer
///   Datei ab (`persistedOutputPath`) — die Zusammenfassung am Ende fehlt
///   dann, und Teilsummen wären falsch: kein Ergebnis.
/// - `PostToolUseFailure` (Exit-Code ≠ 0): `error` ist der Text
///   `Exit code <n>` gefolgt von der Ausgabe; der Präfix `Error: ` (so steht
///   er im Transkript) wird toleriert. `is_interrupt: true` (Abbruch durch
///   den Nutzer) und Fehler ohne Exit-Code-Zeile (Timeout, Tool-Fehler):
///   kein Ergebnis.
///
/// Die Datei hinter `persistedOutputPath` wird **nie** gelesen: Ihr Pfad
/// stammt aus dem Payload, also vom Agenten.
fn claude_exec_report(post: &JournalEvent, command: &str) -> Option<ExecReport> {
    let tool = parse::<PostTool>(post)?;
    if tool.tool_name.as_deref() != Some("Bash") {
        return None;
    }
    let named = tool
        .tool_input
        .as_ref()
        .and_then(|input| input.get("command"))
        .and_then(serde_json::Value::as_str);
    if named != Some(command) {
        return None;
    }
    let flag = |value: Option<&serde_json::Value>, key: &str| {
        value
            .and_then(|v| v.get(key))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    match post.raw_kind.as_str() {
        "PostToolUse" => {
            let response = tool.tool_response.as_ref()?;
            if flag(Some(response), "interrupted")
                || flag(Some(response), "isImage")
                || response.get("persistedOutputPath").is_some()
                || response.get("persistedOutputSize").is_some()
            {
                return None;
            }
            let stdout = response.get("stdout")?.as_str()?;
            if suspect_truncated(stdout) {
                return None;
            }
            let stderr = response
                .get("stderr")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            // Der Payload ist fremd: dieselbe Schranke auch für `stderr`.
            if suspect_truncated(stderr) {
                return None;
            }
            let output = if stderr.is_empty() {
                stdout.to_owned()
            } else {
                format!("{stdout}\n{stderr}")
            };
            Some(ExecReport {
                output,
                exit_code: None,
            })
        }
        "PostToolUseFailure" => {
            let failure = parse::<PostToolFailure>(post)?;
            if failure.is_interrupt.unwrap_or(false) {
                return None;
            }
            let error = failure.error?;
            // Ob und wie Claude Code einen langen Fehlertext kürzt, ist nicht
            // aufgezeichnet — beim Erfolg geschieht es ab etwa 30 000
            // Zeichen. Bis eine Aufnahme es zeigt: in der Nähe dieser Grenze
            // kein Ergebnis statt möglicher Teilsummen.
            if suspect_truncated(&error) {
                return None;
            }
            let error = error.strip_prefix("Error: ").unwrap_or(&error);
            let (first, output) = error.split_once('\n').unwrap_or((error, ""));
            let code = first
                .strip_prefix("Exit code ")?
                .trim()
                .parse::<i32>()
                .ok()?;
            Some(ExecReport {
                output: output.to_owned(),
                exit_code: Some(code),
            })
        }
        _ => None,
    }
}

/// Versionsstand der Codex-Deutung. Bump bei jeder Änderung an
/// [`codex_effect`] oder der Diff-Pfad-Extraktion (ADR-0011: Deutung ist
/// wiederholbar, eine gespeicherte Deutung bleibt ihrem Stand zuordenbar).
pub const CODEX_ADAPTER_VERSION: u32 = 1;

/// Der Codex-Adapter.
pub struct CodexAdapter;

impl ToolAdapter for CodexAdapter {
    fn agent(&self) -> &'static str {
        "codex"
    }

    fn version(&self) -> u32 {
        CODEX_ADAPTER_VERSION
    }

    fn tool_facts(&self, event: &JournalEvent) -> Option<ToolFacts> {
        parse::<Tool>(event).and_then(codex_tool)
    }

    fn interpret_stored(&self, name: &str, arguments: &str) -> Option<StoredInterpretation> {
        let raw = RawValue::from_string(arguments.to_string()).ok();
        let effect = codex_effect(name, raw.as_deref());
        let status = if codex_tool_is_interpreted(name) {
            CaptureStatus::Interpreted
        } else {
            CaptureStatus::Uninterpreted
        };
        Some(StoredInterpretation {
            effect,
            status,
            adapter: "codex",
            adapter_version: CODEX_ADAPTER_VERSION,
        })
    }
}

/// Die Registry: ein Adapter je Agent. Wer hier fehlt, bekommt den
/// generischen Fallback — beobachtet, nicht gedeutet, nie Stille.
const ADAPTERS: &[&dyn ToolAdapter] = &[&ClaudeAdapter, &CodexAdapter];

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
            "[minds: payload not captured — {} bytes over the cap]",
            raw.len()
        )
    } else if let Some(reason) = secret_path_anywhere(raw) {
        format!("[minds: payload not captured — secretfile path in content ({reason})]")
    } else {
        raw.to_owned()
    };
    ToolFacts {
        name,
        arguments,
        effect: None,
        capture: Capture {
            note: None,
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
            note: None,
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
        written: None,
        written_unavailable: None,
    }
}

/// Kennt die Codex-Deutung dieses Tools eine Wirkung?
///
/// `Bash` steht hier trotz des Claude-Namens absichtlich: Codex' Unified-Exec
/// (`exec_command`) meldet sich in `PreToolUse`/`PostToolUse` unter
/// `tool_name: "Bash"` — dieselbe Kompatibilitätsentscheidung, die schon den
/// Hook-Umschlag identisch zu Claude Code macht (Spec §3, verifiziert gegen
/// `developers.openai.com/codex/hooks`, Stand 2026-09).
pub fn codex_tool_is_interpreted(tool_name: &str) -> bool {
    matches!(tool_name, "apply_patch" | "Bash")
}

/// Baut aus Codex' `tool_name` + `tool_input` einen [`ToolFacts`].
fn codex_tool(t: Tool) -> Option<ToolFacts> {
    let name = t.tool_name?;
    let arguments = t
        .tool_input
        .as_ref()
        .map(|r| r.get().to_owned())
        .unwrap_or_default();
    let effect = codex_effect(&name, t.tool_input.as_deref());
    let status = if codex_tool_is_interpreted(&name) {
        CaptureStatus::Interpreted
    } else {
        CaptureStatus::Uninterpreted
    };
    Some(ToolFacts {
        capture: Capture {
            note: None,
            status,
            adapter: "codex".into(),
            adapter_version: CODEX_ADAPTER_VERSION,
        },
        name,
        arguments,
        effect: Some(effect),
    })
}

/// Die agent-spezifische Abbildung Tool-Name + `tool_input` → [`Effect`] für
/// Codex.
pub fn codex_effect(tool_name: &str, tool_input: Option<&RawValue>) -> Effect {
    match tool_name {
        "apply_patch" => {
            let diff = tool_input
                .and_then(|r| serde_json::from_str::<CodexPatch>(r.get()).ok())
                .and_then(|p| p.diff);
            let path = diff.as_deref().and_then(first_diff_path);
            // Eine reine Löschung trägt kein `+++ b/…`, nur `+++ /dev/null` —
            // genau das Signal, das `first_diff_path` schon erkennt, um dann
            // auf die Quellzeile zurückzufallen. Hier wird derselbe Fund
            // zusätzlich zur Wirkungsart: `Delete` statt `Write`.
            let kind = if diff.as_deref().is_some_and(target_is_dev_null) {
                EffectKind::Delete
            } else {
                EffectKind::Write
            };
            Effect {
                kind,
                path,
                content: None,
                written: None,
                written_unavailable: None,
            }
        }
        "Bash" => Effect {
            kind: EffectKind::Exec,
            path: None,
            content: None,
            written: None,
            written_unavailable: None,
        },
        _ => Effect {
            kind: EffectKind::Other,
            path: None,
            content: None,
            written: None,
            written_unavailable: None,
        },
    }
}

/// Codex' `apply_patch`-Eingabe, nur das interessante Feld.
#[derive(Debug, Default, Deserialize)]
struct CodexPatch {
    diff: Option<String>,
}

/// Der erste Dateipfad aus einem Unified Diff (`--- a/<path>` /
/// `+++ b/<path>`-Kopfzeilen). Codex kann in einem `apply_patch`-Aufruf
/// mehrere Dateien ändern; wie bei Claudes `Effect` (ein Pfad pro Aufruf,
/// vgl. `claude_effect`) trägt der Adapter nur den ersten — eine benannte
/// Grenze, keine falsche Aussage: der Aufruf bleibt vollständig als
/// Roh-Beweismittel in `arguments` erhalten.
///
/// Bevorzugt die `+++ b/`-Zeile (Zielpfad nach der Änderung); fehlt sie,
/// die `--- a/`-Zeile. `/dev/null` (reine Löschung) wird übersprungen.
fn first_diff_path(diff: &str) -> Option<String> {
    fn strip_prefix_path(line: &str, marker: &str) -> Option<String> {
        let rest = line.strip_prefix(marker)?.trim();
        // Klassisches `diff -u` haengt an die Kopfzeile einen Tab-getrennten
        // Zeitstempel (`--- a/foo.rs\t2024-01-01 00:00:00 +0000`) - nur der
        // Teil davor ist der Pfad.
        let rest = rest.split('\t').next().unwrap_or(rest);
        if rest == "/dev/null" {
            return None;
        }
        // Unified-Diff-Pfade tragen ein a/ bzw. b/ Präfix.
        let path = rest
            .strip_prefix("a/")
            .or_else(|| rest.strip_prefix("b/"))
            .unwrap_or(rest);
        Some(path.to_string())
    }

    let plus = diff.lines().find_map(|l| strip_prefix_path(l, "+++ "));
    plus.or_else(|| diff.lines().find_map(|l| strip_prefix_path(l, "--- ")))
}

/// Trägt die `+++ `-Kopfzeile explizit `/dev/null` — der Unified-Diff-Ausdruck
/// für „diese Datei existiert danach nicht mehr", also eine Löschung?
fn target_is_dev_null(diff: &str) -> bool {
    diff.lines()
        .find_map(|l| l.strip_prefix("+++ "))
        .is_some_and(|rest| rest.trim() == "/dev/null")
}

/// Die geschriebenen Bytes aus Claude Codes PostToolUse-Payload — die
/// Payload-Formen sind **aufgezeichnet, nicht geraten** (Fixtures unter
/// `tests/fixtures/claude-code/`, Claude Code 2.1.282):
///
/// - `Write`: `tool_response.content` ist der geschriebene Inhalt — der
///   Beleg des **gelungenen** Schreibens. `tool_input.content` ist nur die
///   Absicht und wird bewusst nicht herangezogen: Ein `PostToolUseFailure`
///   trägt sie ebenfalls, geschrieben wurde aber nichts.
/// - `Edit`: `tool_response.originalFile` ist der Inhalt **vor** der
///   Änderung, `oldString`/`newString`/`replaceAll` die **tatsächlich
///   angewandte** Ersetzung (Claude Code normalisiert z. B. Anführungszeichen
///   gegenüber `tool_input`, deshalb nie der Rückfall auf die Eingabe).
///   Einen Nachher-Inhalt trägt der Payload nicht; er wird so rekonstruiert,
///   wie das Tool ersetzt (erstes Vorkommen bzw. alle), und dann gegen den
///   `structuredPatch` des Tools geprüft ([`agrees_with_patch`]). Die
///   Fixture-Tests prüfen das Ergebnis gegen die tatsächlich entstandene Datei.
/// - `NotebookEdit`: `tool_response.updated_file` ist der Nachher-Inhalt.
/// - `MultiEdit`: In Claude Code 2.1.x nicht mehr vorhanden, keine Fixture —
///   deshalb bewusst [`WrittenUnavailable::PayloadWithoutContent`] statt
///   einer geratenen Form.
///
/// `userModified: true` (Write und Edit): Der Nutzer hat den Vorschlag vor
/// dem Übernehmen geändert; welche Bytes geschrieben wurden, sagt der Payload
/// dann nicht verlässlich ([`WrittenUnavailable::UserModified`]).
///
/// Fail-closed an jeder Stelle, an der wir nicht *sicher* wissen, welche
/// Bytes das Tool geschrieben hat ([`WrittenUnavailable::ReconstructionFailed`]):
/// Suchtext nicht im Original, leerer Suchtext, CRLF-Zeilenenden im Original
/// (Claude Code passt `newString` dann an die Zeilenenden an — ohne Fixture
/// dafür behaupten wir lieber nichts), ein Ergebnis über
/// [`WRITTEN_RECONSTRUCTION_CAP`], oder eine Rekonstruktion, die dem
/// `structuredPatch` widerspricht (etwa weil das Tool beim Löschen einer
/// Zeile den Zeilenumbruch mitnimmt — ein Verhalten, das wir nicht
/// nachbauen, sondern am Patch erkennen).
pub(crate) fn claude_written(
    tool_name: &str,
    response: Option<&serde_json::Value>,
) -> WrittenOutcome {
    use WrittenUnavailable::{PayloadWithoutContent, ReconstructionFailed, UserModified};

    let field = |key: &str| -> Option<String> { response?.get(key)?.as_str().map(str::to_owned) };
    let user_modified = response
        .and_then(|r| r.get("userModified"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    match tool_name {
        "Write" => {
            let content = field("content").ok_or(PayloadWithoutContent)?;
            if user_modified {
                return Err(UserModified);
            }
            Ok(content)
        }
        "Edit" => {
            let original = field("originalFile").ok_or(PayloadWithoutContent)?;
            let old = field("oldString").ok_or(PayloadWithoutContent)?;
            let new = field("newString").ok_or(PayloadWithoutContent)?;
            if user_modified {
                return Err(UserModified);
            }
            let replace_all = response
                .and_then(|r| r.get("replaceAll"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if old.is_empty() || original.contains("\r\n") || !original.contains(&old) {
                return Err(ReconstructionFailed);
            }
            // Schranke vor der Allokation: Der Payload ist fremd, und ein
            // kurzer Suchtext mit langem Ersatz und `replace_all` waechst
            // multiplikativ. Das Tool hat dieselbe Datei geschrieben, also
            // ist alles jenseits der Hook-Obergrenze ohnehin nicht plausibel.
            let matches = if replace_all {
                original.matches(&old).count()
            } else {
                1
            };
            let growth = matches.saturating_mul(new.len().saturating_sub(old.len()));
            if original.len().saturating_add(growth) > WRITTEN_RECONSTRUCTION_CAP {
                return Err(ReconstructionFailed);
            }
            let result = if replace_all {
                original.replace(&old, &new)
            } else {
                original.replacen(&old, &new, 1)
            };
            let patch = response.and_then(|r| r.get("structuredPatch"));
            if !agrees_with_patch(&original, &result, patch) {
                return Err(ReconstructionFailed);
            }
            Ok(result)
        }
        "NotebookEdit" => field("updated_file").ok_or(PayloadWithoutContent),
        _ => Err(PayloadWithoutContent),
    }
}

/// Stimmt die Rekonstruktion mit dem `structuredPatch` überein, den Claude
/// Code zum `Edit` mitliefert? Der Patch wird vollständig auf das Original
/// angewandt ([`apply_patch`]), und das Ergebnis muss mit der
/// Rekonstruktion **byte-gleich** sein — inklusive der Zeilen hinter dem
/// letzten Hunk und des Zeilenumbruchs am Dateiende.
///
/// Das fängt jedes Tool-Verhalten, das [`claude_written`] nicht nachbaut,
/// ohne es kennen zu müssen (etwa den mitgelöschten Zeilenumbruch beim
/// Entfernen der letzten Zeile). Kein Patch, ein leerer Patch (ein `Edit`
/// ändert immer etwas) oder eine unbekannte Hunk-Form: keine
/// Übereinstimmung — fail-closed.
fn agrees_with_patch(original: &str, result: &str, patch: Option<&serde_json::Value>) -> bool {
    patch
        .and_then(serde_json::Value::as_array)
        .filter(|hunks| !hunks.is_empty())
        .and_then(|hunks| apply_patch(original, hunks))
        .is_some_and(|patched| patched == result)
}

/// Wendet jsdiff-Hunks (Claude Codes `structuredPatch`) auf `original` an.
///
/// Streng: Die Hunks müssen aufsteigend und überlappungsfrei sein, ihre
/// alte Seite muss im Original genau an `oldStart` stehen, und die
/// Zeilenzahlen müssen stimmen. `\ No newline at end of file` nach einer
/// Zeile der neuen Seite heißt: Die neue Datei endet ohne Umbruch; nach einer
/// der alten Seite muss das Original so enden. Ein Hunk mit leerer alter
/// Seite kommt nur bei leerem Original vor — das rekonstruieren wir ohnehin
/// nicht, also `None`. Jede Abweichung: `None`.
fn apply_patch(original: &str, hunks: &[serde_json::Value]) -> Option<String> {
    let before: Vec<&str> = original.lines().collect();
    let mut after: Vec<&str> = Vec::with_capacity(before.len());
    let mut cursor = 0usize;
    // Endet die neue Datei mit Umbruch? Ohne Hunk am Dateiende wie das
    // Original; mit Hunk dort ja, außer ein Marker sagt nein.
    let mut trailing_newline = original.ends_with('\n');

    for hunk in hunks {
        let number = |key: &str| {
            hunk.get(key)
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
        };
        let (old_start, old_lines, new_lines) = (
            number("oldStart")?,
            number("oldLines")?,
            number("newLines")?,
        );
        let from = old_start.checked_sub(1)?;
        let to = from.checked_add(old_lines)?;
        if old_lines == 0 || from < cursor || to > before.len() {
            return None;
        }
        after.extend_from_slice(&before[cursor..from]);

        let (mut old_side, mut new_side) = (Vec::new(), Vec::new());
        let (mut old_no_eol, mut new_no_eol) = (false, false);
        let mut last: Option<char> = None;
        for line in hunk.get("lines")?.as_array()? {
            let line = line.as_str()?;
            let (tag, text) = line.split_at_checked(1)?;
            match tag {
                " " => {
                    old_side.push(text);
                    new_side.push(text);
                }
                "-" => old_side.push(text),
                "+" => new_side.push(text),
                "\\" => match last? {
                    ' ' => (old_no_eol, new_no_eol) = (true, true),
                    '-' => old_no_eol = true,
                    _ => new_no_eol = true,
                },
                _ => return None,
            }
            last = tag.chars().next().filter(|c| *c != '\\').or(last);
        }
        if old_side.len() != old_lines
            || new_side.len() != new_lines
            || before[from..to] != old_side[..]
        {
            return None;
        }
        let at_end = to == before.len();
        if at_end && old_no_eol == original.ends_with('\n') {
            return None;
        }
        if at_end {
            trailing_newline = !new_no_eol;
        } else if old_no_eol || new_no_eol {
            return None;
        }
        after.extend(new_side);
        cursor = to;
    }
    after.extend_from_slice(&before[cursor..]);

    let mut patched = after.join("\n");
    if trailing_newline && !after.is_empty() {
        patched.push('\n');
    }
    Some(patched)
}

/// Obergrenze für ein rekonstruiertes Edit-Ergebnis — dieselbe Größe wie die
/// stdin-Grenze des Hooks: Was das Tool nicht durch den Hook bekommen hätte,
/// rekonstruieren wir auch nicht.
const WRITTEN_RECONSTRUCTION_CAP: usize = 32 * 1024 * 1024;

/// Nennt der Post-Payload den Pfad des Effekts? Geprüft werden die Felder,
/// die Claude Code bei den datei-berührenden Tools führt: `tool_input`
/// (`file_path`, `notebook_path`) und `tool_response` (`filePath`,
/// `notebook_path`). Nichts davon: kein Aufruf zu diesem Effekt.
fn claude_post_names_path(
    input: Option<&serde_json::Value>,
    response: Option<&serde_json::Value>,
    path: &str,
) -> bool {
    let names = |value: Option<&serde_json::Value>, key: &str| -> bool {
        value
            .and_then(|v| v.get(key))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|named| named == path)
    };
    names(input, "file_path")
        || names(input, "notebook_path")
        || names(response, "filePath")
        || names(response, "notebook_path")
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

/// Claude Codes Korrelationskennung: `tool_use_id` steht in PreToolUse und
/// PostToolUse desselben Aufrufs.
#[derive(Debug, Deserialize)]
struct CallId {
    tool_use_id: Option<String>,
}

/// Claude Codes PostToolUse-Payload, so weit die Schreibzeit-Deutung ihn
/// braucht: `tool_input` nur für die Pfad-Gegenprobe, `tool_response` als
/// Beleg des Ergebnisses. Beide als [`serde_json::Value`]: Die Felder werden
/// gezielt abgefragt, nichts davon wandert verbatim weiter.
#[derive(Debug, Deserialize)]
struct PostTool {
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    tool_response: Option<serde_json::Value>,
}

/// Claude Codes `PostToolUseFailure`-Payload, so weit die Ergebnis-Deutung
/// ihn braucht: der Fehlertext (`Exit code <n>` + Ausgabe) und ob der Nutzer
/// abgebrochen hat.
#[derive(Debug, Deserialize)]
struct PostToolFailure {
    error: Option<String>,
    is_interrupt: Option<bool>,
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
            got.arguments.starts_with("[minds: payload not captured"),
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
        assert!(adapter_for("codex").is_some());
        assert!(adapter_for("some-future-agent").is_none());
        assert_eq!(
            adapter_for("claude-code").unwrap().version(),
            CLAUDE_ADAPTER_VERSION
        );
        assert_eq!(
            adapter_for("codex").unwrap().version(),
            CODEX_ADAPTER_VERSION
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
    fn the_call_id_is_claudes_tool_use_id() {
        let e = event(
            EventKind::ToolPost,
            "PostToolUse",
            r#"{"tool_name":"Write","tool_use_id":"toolu_01","tool_input":{}}"#,
        );
        assert_eq!(ClaudeAdapter.call_id(&e).as_deref(), Some("toolu_01"));
        let without = event(EventKind::ToolPre, "PreToolUse", r#"{"tool_name":"Write"}"#);
        assert_eq!(ClaudeAdapter.call_id(&without), None);
    }

    /// Deutet einen Post-Payload; die Pfad-Gegenprobe wird hier mit dem Pfad
    /// aus dem Payload selbst bedient (`file_path`, sonst `notebook_path`,
    /// sonst `filePath`), damit die Tests die Deutung isoliert sehen.
    fn written_of(payload: &str) -> WrittenOutcome {
        let value: serde_json::Value = serde_json::from_str(payload).unwrap_or_default();
        let path = ["file_path", "notebook_path"]
            .iter()
            .find_map(|k| value.get("tool_input")?.get(k)?.as_str())
            .or_else(|| value.get("tool_response")?.get("filePath")?.as_str())
            .unwrap_or("a.rs")
            .to_owned();
        ClaudeAdapter.written_bytes(&event(EventKind::ToolPost, "PostToolUse", payload), &path)
    }

    #[test]
    fn a_failed_write_never_yields_a_written_hash() {
        // `PostToolUseFailure` ist ebenfalls `ToolPost` und traegt die
        // Absicht (`tool_input.content`), aber kein `tool_response`: Es
        // wurde nichts geschrieben — also auch kein Hash ueber „nichts".
        let failure = event(
            EventKind::ToolPost,
            "PostToolUseFailure",
            r#"{"tool_name":"Write","tool_input":{"file_path":"a.rs","content":"x\n"},"tool_use_id":"t","error":"File has not been read yet"}"#,
        );
        assert_eq!(
            ClaudeAdapter.written_bytes(&failure, "a.rs"),
            Err(WrittenUnavailable::PayloadWithoutContent)
        );
        // Und selbst ein `PostToolUse` ohne `tool_response` haengt nicht an
        // der Absicht: `tool_input.content` zaehlt nie.
        let got = written_of(
            r#"{"tool_name":"Write","tool_input":{"file_path":"a.rs","content":"x\n"},"tool_use_id":"t"}"#,
        );
        assert_eq!(got, Err(WrittenUnavailable::PayloadWithoutContent));
        // Auch nicht, wenn ein Failure-Event zufaellig ein tool_response traegt.
        let failure = event(
            EventKind::ToolPost,
            "PostToolUseFailure",
            r#"{"tool_name":"Write","tool_input":{"file_path":"a.rs"},"tool_response":{"content":"x\n"},"tool_use_id":"t"}"#,
        );
        assert_eq!(
            ClaudeAdapter.written_bytes(&failure, "a.rs"),
            Err(WrittenUnavailable::PayloadWithoutContent)
        );
    }

    #[test]
    fn a_post_naming_another_path_does_not_count() {
        // Die Gegenprobe: Der Post muss den Pfad des Effekts nennen —
        // ueber tool_input.file_path, tool_input.notebook_path oder
        // tool_response.filePath. Sonst gehoert er nicht zu diesem Aufruf.
        let post = event(
            EventKind::ToolPost,
            "PostToolUse",
            r#"{"tool_name":"Write","tool_input":{"file_path":"a.rs"},"tool_response":{"type":"create","filePath":"a.rs","content":"x"}}"#,
        );
        assert!(ClaudeAdapter.written_bytes(&post, "a.rs").is_ok());
        assert_eq!(
            ClaudeAdapter.written_bytes(&post, "b.rs"),
            Err(WrittenUnavailable::PayloadWithoutContent)
        );
        let only_response = event(
            EventKind::ToolPost,
            "PostToolUse",
            r#"{"tool_name":"Write","tool_response":{"filePath":"a.rs","content":"x"}}"#,
        );
        assert!(ClaudeAdapter.written_bytes(&only_response, "a.rs").is_ok());
        let notebook = event(
            EventKind::ToolPost,
            "PostToolUse",
            r#"{"tool_name":"NotebookEdit","tool_input":{"notebook_path":"n.ipynb"},"tool_response":{"updated_file":"{}"}}"#,
        );
        assert!(ClaudeAdapter.written_bytes(&notebook, "n.ipynb").is_ok());
        assert!(ClaudeAdapter.written_bytes(&notebook, "m.ipynb").is_err());
    }

    #[test]
    fn an_edit_result_over_the_cap_is_not_reconstructed() {
        // Kurzer Suchtext, langer Ersatz, replace_all: Das Ergebnis wuerde
        // multiplikativ wachsen — die Schranke greift vor der Allokation.
        let original = "a".repeat(64 * 1024);
        let new = "b".repeat(1024);
        let payload = format!(
            r#"{{"tool_name":"Edit","tool_input":{{"file_path":"a"}},"tool_response":{{"originalFile":"{original}","oldString":"a","newString":"{new}","replaceAll":true}}}}"#
        );
        assert_eq!(
            written_of(&payload),
            Err(WrittenUnavailable::ReconstructionFailed)
        );
        // Ohne replace_all waechst es nur um einen Ersatz — das geht.
        let result = format!("{new}{}", &original[1..]);
        let payload = format!(
            r#"{{"tool_name":"Edit","tool_input":{{"file_path":"a"}},"tool_response":{{"originalFile":"{original}","oldString":"a","newString":"{new}","replaceAll":false,"structuredPatch":[{{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-{original}","\\ No newline at end of file","+{result}","\\ No newline at end of file"]}}]}}}}"#
        );
        assert_eq!(written_of(&payload), bytes(&result));
    }

    fn bytes(text: &str) -> WrittenOutcome {
        Ok(text.to_owned())
    }

    #[test]
    fn written_for_write_is_the_hash_of_the_content() {
        let got = written_of(
            r#"{"tool_name":"Write","tool_input":{"file_path":"a.rs","content":"x\n"},"tool_response":{"type":"create","content":"x\n"}}"#,
        );
        assert_eq!(got, bytes("x\n"));
        // Ohne Inhalt im Payload: der Grund, kein Hash über nichts.
        let got = written_of(r#"{"tool_name":"Write","tool_input":{"file_path":"a.rs"}}"#);
        assert_eq!(got, Err(WrittenUnavailable::PayloadWithoutContent));
    }

    #[test]
    fn written_for_edit_reconstructs_exactly_like_the_tool() {
        // Erstes Vorkommen.
        let got = written_of(
            r#"{"tool_name":"Edit","tool_input":{"file_path":"a","old_string":"b","new_string":"X","replace_all":false},"tool_response":{"originalFile":"a b b\n","oldString":"b","newString":"X","replaceAll":false,"structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-a b b","+a X b"]}]}}"#,
        );
        assert_eq!(got, bytes("a X b\n"));
        // Alle Vorkommen.
        let got = written_of(
            r#"{"tool_name":"Edit","tool_input":{"file_path":"a","old_string":"b","new_string":"X","replace_all":true},"tool_response":{"originalFile":"a b b\n","oldString":"b","newString":"X","replaceAll":true,"structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-a b b","+a X X"]}]}}"#,
        );
        assert_eq!(got, bytes("a X X\n"));
        // Nur `tool_response` zaehlt (die tatsaechlich angewandte Ersetzung);
        // `tool_input` ist die Eingabe vor Claude Codes Normalisierung und
        // wird nie als Rueckfall genommen — eine nicht aufgezeichnete
        // Payload-Form ergibt den Grund, keinen geratenen Hash.
        let got = written_of(
            r#"{"tool_name":"Edit","tool_input":{"file_path":"a","old_string":"b","new_string":"X"},"tool_response":{"originalFile":"a b\n"}}"#,
        );
        assert_eq!(got, Err(WrittenUnavailable::PayloadWithoutContent));
    }

    #[test]
    fn an_edit_that_contradicts_its_structured_patch_is_not_reconstructed() {
        use WrittenUnavailable::ReconstructionFailed;
        // Eine Zeile loeschen: Claude Code nimmt den Zeilenumbruch mit
        // (`foo\n` faellt weg), die blosse Ersetzung liesse eine Leerzeile
        // stehen. Der Patch des Tools zeigt, was wirklich geschah — die
        // Rekonstruktion widerspricht ihm, also kein Hash statt eines
        // falschen (der sonst „ein Mensch hat editiert" vortaeuschte).
        let delete = r#"{"tool_name":"Edit","tool_input":{"file_path":"a"},"tool_response":{"originalFile":"x\nfoo\ny\n","oldString":"foo","newString":"","replaceAll":false,"structuredPatch":[{"oldStart":1,"oldLines":3,"newStart":1,"newLines":2,"lines":[" x","-foo"," y"]}]}}"#;
        assert_eq!(written_of(delete), Err(ReconstructionFailed));

        // Die letzte Zeile loeschen: Hinter dem Hunk steht nichts mehr, das
        // den Unterschied zeigen koennte — der Vergleich des ganzen
        // Ergebnisses muss ihn finden (Tool: `x\n`, blosse Ersetzung: `x\n\n`).
        let last = r#"{"tool_name":"Edit","tool_input":{"file_path":"a"},"tool_response":{"originalFile":"x\nfoo\n","oldString":"foo","newString":"","structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":1,"lines":[" x","-foo"]}]}}"#;
        assert_eq!(written_of(last), Err(ReconstructionFailed));
        // Die einzige Zeile loeschen (Tool: leere Datei, Ersetzung: `\n`).
        let only = r#"{"tool_name":"Edit","tool_input":{"file_path":"a"},"tool_response":{"originalFile":"foo\n","oldString":"foo","newString":"","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":0,"newLines":0,"lines":["-foo"]}]}}"#;
        assert_eq!(written_of(only), Err(ReconstructionFailed));
        // Nur der Umbruch am Ende unterscheidet sich (Tool: `x` ohne Umbruch,
        // Ersetzung: `x\n`) — der Marker sagt es.
        let eol = r#"{"tool_name":"Edit","tool_input":{"file_path":"a"},"tool_response":{"originalFile":"x\nfoo\n","oldString":"\nfoo","newString":"","structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":1,"lines":["-x","-foo","+x","\\ No newline at end of file"]}]}}"#;
        assert_eq!(written_of(eol), Err(ReconstructionFailed));
        // Ein Patch, der das Dateiende nicht erreicht, laesst den Rest stehen.
        let tail = r#"{"tool_name":"Edit","tool_input":{"file_path":"a"},"tool_response":{"originalFile":"a\nb\nc\nd\ne\n","oldString":"a","newString":"A","structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":2,"lines":["-a","+A"," b"]}]}}"#;
        assert_eq!(written_of(tail), bytes("A\nb\nc\nd\ne\n"));

        // Derselbe Edit mit passendem Patch geht durch — der Check ist keine
        // Pauschalabsage an leere Ersetzungen.
        let blank = r#"{"tool_name":"Edit","tool_input":{"file_path":"a"},"tool_response":{"originalFile":"x\nfoo\ny\n","oldString":"foo","newString":"","replaceAll":false,"structuredPatch":[{"oldStart":1,"oldLines":3,"newStart":1,"newLines":3,"lines":[" x","-foo","+"," y"]}]}}"#;
        assert_eq!(written_of(blank), bytes("x\n\ny\n"));

        // Kein Patch, leerer Patch, ein Hunk an falscher Stelle, eine
        // unbekannte Zeilenform: fail-closed.
        for patch in [
            "",
            r#","structuredPatch":[]"#,
            r#","structuredPatch":[{"oldStart":2,"oldLines":1,"newStart":2,"newLines":1,"lines":["-a b b","+a X b"]}]"#,
            r#","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["?a b b","+a X b"]}]"#,
            r#","structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":1,"lines":["-a b b","+a X b"]}]"#,
        ] {
            let payload = format!(
                r#"{{"tool_name":"Edit","tool_input":{{"file_path":"a"}},"tool_response":{{"originalFile":"a b b\n","oldString":"b","newString":"X"{patch}}}}}"#
            );
            assert_eq!(written_of(&payload), Err(ReconstructionFailed), "{patch}");
        }
    }

    #[test]
    fn a_user_modified_proposal_yields_no_bytes() {
        // `userModified: true`: Der Nutzer hat den Vorschlag im Diff
        // geaendert — welche Bytes auf der Platte landeten, sagt der Payload
        // dann nicht verlaesslich.
        for payload in [
            r#"{"tool_name":"Write","tool_input":{"file_path":"a"},"tool_response":{"type":"update","content":"x\n","userModified":true}}"#,
            r#"{"tool_name":"Edit","tool_input":{"file_path":"a"},"tool_response":{"originalFile":"a b\n","oldString":"b","newString":"X","userModified":true,"structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-a b","+a X"]}]}}"#,
        ] {
            assert_eq!(
                written_of(payload),
                Err(WrittenUnavailable::UserModified),
                "{payload}"
            );
        }
        // `false` (wie in allen Aufzeichnungen) aendert nichts.
        let got = written_of(
            r#"{"tool_name":"Write","tool_input":{"file_path":"a"},"tool_response":{"type":"update","content":"x\n","userModified":false}}"#,
        );
        assert_eq!(got, bytes("x\n"));
    }

    #[test]
    fn written_for_edit_fails_closed_where_the_result_is_uncertain() {
        use WrittenUnavailable::{PayloadWithoutContent, ReconstructionFailed};
        // Kein Original: nichts zu rekonstruieren.
        let got = written_of(
            r#"{"tool_name":"Edit","tool_input":{"old_string":"b","new_string":"X"},"tool_response":{"oldString":"b","newString":"X"}}"#,
        );
        assert_eq!(got, Err(PayloadWithoutContent));
        // Suchtext nicht im Original.
        let got = written_of(
            r#"{"tool_name":"Edit","tool_input":{"file_path":"a"},"tool_response":{"originalFile":"a\n","oldString":"zzz","newString":"X"}}"#,
        );
        assert_eq!(got, Err(ReconstructionFailed));
        // Leerer Suchtext (Datei anlegen ueber Edit): nicht aufgezeichnet.
        let got = written_of(
            r#"{"tool_name":"Edit","tool_input":{"file_path":"a"},"tool_response":{"originalFile":"","oldString":"","newString":"X"}}"#,
        );
        assert_eq!(got, Err(ReconstructionFailed));
        // CRLF: Claude Code passt newString an — ohne Fixture keine Behauptung.
        let got = written_of(
            r#"{"tool_name":"Edit","tool_input":{"file_path":"a"},"tool_response":{"originalFile":"a\r\nb\r\n","oldString":"a","newString":"X"}}"#,
        );
        assert_eq!(got, Err(ReconstructionFailed));
    }

    #[test]
    fn written_for_notebook_edit_is_the_updated_file() {
        let got = written_of(
            r#"{"tool_name":"NotebookEdit","tool_input":{"notebook_path":"n.ipynb"},"tool_response":{"original_file":"{}","updated_file":"{\"cells\":[]}"}}"#,
        );
        assert_eq!(got, bytes("{\"cells\":[]}"));
        let got = written_of(r#"{"tool_name":"NotebookEdit","tool_response":{"error":"x"}}"#);
        assert_eq!(got, Err(WrittenUnavailable::PayloadWithoutContent));
    }

    #[test]
    fn written_for_unknown_or_multiedit_is_documented_as_unavailable() {
        for payload in [
            r#"{"tool_name":"MultiEdit","tool_response":{"originalFile":"a"}}"#,
            r#"{"tool_name":"Bash","tool_response":"ok"}"#,
            r#"{"tool_name":"Read","tool_response":{"file":{"content":"x"}}}"#,
            r#"{"nope":1}"#,
        ] {
            assert_eq!(
                written_of(payload),
                Err(WrittenUnavailable::PayloadWithoutContent),
                "{payload}"
            );
        }
        // Ein abgeschnittener Payload (als JSON-String abgelegt) ebenso.
        let wrapped = serde_json::to_string(r#"{"tool_name":"Write","tool_inp"#).unwrap();
        assert_eq!(
            written_of(&wrapped),
            Err(WrittenUnavailable::PayloadWithoutContent)
        );
    }

    #[test]
    fn interpret_stored_never_invents_a_written_hash() {
        // Die gespeicherten `arguments` sind redigiert und tragen kein
        // Post-Event: Ein daraus gerechneter Hash waere falsch. `written`
        // bleibt beim Reinterpretieren leer, ohne Grund — der Grund ist
        // Sache des Checkpoints.
        let got = ClaudeAdapter
            .interpret_stored("Write", r#"{"file_path":"a.rs","content":"x"}"#)
            .unwrap();
        assert_eq!(got.effect.written, None);
        assert_eq!(got.effect.written_unavailable, None);
        assert_eq!(got.adapter_version, 3);
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

    #[test]
    fn apply_patch_becomes_a_write_effect_with_path() {
        let e = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"apply_patch","tool_input":{"diff":"--- a/src/retry.rs\n+++ b/src/retry.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n"}}"#,
        );
        let t = facts("codex", &e).tool.unwrap();
        assert_eq!(t.name, "apply_patch");
        assert_eq!(t.capture.status, CaptureStatus::Interpreted);
        assert_eq!(t.capture.adapter, "codex");
        assert_eq!(t.capture.adapter_version, CODEX_ADAPTER_VERSION);
        let effect = t.effect.unwrap();
        assert_eq!(effect.kind, EffectKind::Write);
        assert_eq!(effect.path.as_deref(), Some("src/retry.rs"));
        assert!(effect.content.is_none());
    }

    #[test]
    fn apply_patch_with_a_dev_null_target_becomes_a_delete_effect_with_the_source_path() {
        let e = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"apply_patch","tool_input":{"diff":"--- a/deleted.rs\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-x\n"}}"#,
        );
        let t = facts("codex", &e).tool.unwrap();
        assert_eq!(t.capture.status, CaptureStatus::Interpreted);
        let effect = t.effect.unwrap();
        assert_eq!(effect.kind, EffectKind::Delete);
        assert_eq!(effect.path.as_deref(), Some("deleted.rs"));
    }

    #[test]
    fn codex_bash_is_exec_without_a_path() {
        let e = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
        );
        let t = facts("codex", &e).tool.unwrap();
        assert_eq!(t.capture.status, CaptureStatus::Interpreted);
        let effect = t.effect.unwrap();
        assert_eq!(effect.kind, EffectKind::Exec);
        assert!(effect.path.is_none());
    }

    #[test]
    fn an_unknown_codex_tool_is_other_not_an_error() {
        let e = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"unknown_tool","tool_input":{"x":1}}"#,
        );
        let t = facts("codex", &e).tool.unwrap();
        assert_eq!(t.capture.status, CaptureStatus::Uninterpreted);
        let effect = t.effect.unwrap();
        assert_eq!(effect.kind, EffectKind::Other);
        assert!(effect.path.is_none());
    }

    #[test]
    fn codex_arguments_keep_the_raw_tool_input() {
        let e = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"apply_patch","tool_input":{"diff":"--- a/x\n+++ b/x\n","z":1}}"#,
        );
        let t = facts("codex", &e).tool.unwrap();
        assert!(t.arguments.contains("--- a/x"));
        assert!(t.arguments.contains("\"z\""));
    }

    #[test]
    fn codex_interpretation_is_deterministic() {
        let ev = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"apply_patch","tool_input":{"diff":"--- a/x\n+++ b/x\n"}}"#,
        );
        assert_eq!(facts("codex", &ev), facts("codex", &ev));

        let a = CodexAdapter.interpret_stored("apply_patch", r#"{"diff":"--- a/x\n+++ b/x\n"}"#);
        let b = CodexAdapter.interpret_stored("apply_patch", r#"{"diff":"--- a/x\n+++ b/x\n"}"#);
        assert_eq!(a, b);
        let got = a.unwrap();
        assert_eq!(got.status, CaptureStatus::Interpreted);
        assert_eq!(got.effect.kind, EffectKind::Write);
        assert_eq!(got.effect.path.as_deref(), Some("x"));
        assert_eq!(got.adapter_version, CODEX_ADAPTER_VERSION);
    }

    #[test]
    fn a_known_codex_tool_is_interpreted_an_unknown_one_is_not() {
        let patch = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"apply_patch","tool_input":{"diff":"--- a/x\n+++ b/x\n"}}"#,
        );
        let got = facts("codex", &patch).tool.unwrap();
        assert_eq!(got.capture.status, CaptureStatus::Interpreted);
        assert_eq!(got.capture.adapter, "codex");
        assert_eq!(got.capture.adapter_version, CODEX_ADAPTER_VERSION);

        let other = event(
            EventKind::ToolPre,
            "PreToolUse",
            r#"{"tool_name":"web_search","tool_input":{"query":"x"}}"#,
        );
        let got = facts("codex", &other).tool.unwrap();
        assert_eq!(got.capture.status, CaptureStatus::Uninterpreted);
    }

    #[test]
    fn first_diff_path_prefers_the_target_of_the_first_file() {
        let diff = "--- a/one.rs\n+++ b/one.rs\n@@ ...\n--- a/two.rs\n+++ b/two.rs\n@@ ...\n";
        assert_eq!(first_diff_path(diff).as_deref(), Some("one.rs"));
    }

    #[test]
    fn first_diff_path_falls_back_to_the_source_when_the_target_is_dev_null() {
        let diff = "--- a/deleted.rs\n+++ /dev/null\n@@ ...\n";
        assert_eq!(first_diff_path(diff).as_deref(), Some("deleted.rs"));
    }

    #[test]
    fn first_diff_path_is_none_for_an_empty_diff() {
        assert_eq!(first_diff_path(""), None);
    }

    #[test]
    fn first_diff_path_reads_the_source_line_when_no_target_line_exists() {
        assert_eq!(first_diff_path("--- a/x\n").as_deref(), Some("x"));
    }

    #[test]
    fn first_diff_path_drops_a_tab_separated_timestamp() {
        let diff = "--- a/foo.rs\t2024-01-01 00:00:00.000000000 +0000\n+++ b/foo.rs\t2024-01-01 00:00:01.000000000 +0000\n";
        assert_eq!(first_diff_path(diff).as_deref(), Some("foo.rs"));
    }
}
