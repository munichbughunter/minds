//! Der gemeinsame Umschlag eines Agent-Hook-Payloads — und seine Übersetzung
//! in ein [`NewEvent`].
//!
//! Dieses Modul ist die Grenze zwischen fremdem Format und unserem. Alles, was
//! wüsste, wie *ein bestimmter Agent* seine Events benennt, gehört hierher und
//! nicht in die CLI. In M5.4 wächst genau das hier zum Normalisierer heran:
//! heute ein gemeinsamer Umschlag für alle, morgen ein `match` je Agent für
//! das, was sich unterscheidet.
//!
//! # Eine reine Funktion, mit Absicht
//!
//! [`parse`] liest keine Uhr, kein Verzeichnis und keine Umgebungsvariable. Der
//! Zeitpunkt wird **hereingereicht**. Das kostet den Aufrufer eine Zeile und
//! kauft dafür Fixture-Tests: Dieselben Bytes ergeben immer dasselbe Event,
//! Byte für Byte. Wäre die Uhr hier drin, wäre jeder Testlauf ein anderer und
//! der Vertrag „gleicher Journal-Inhalt ⇒ gleiche `SessionId`" nicht prüfbar.
//!
//! Wer *wo* ist (Arbeitsverzeichnis, Repository) bleibt deshalb ebenfalls beim
//! Aufrufer. Dieses Modul sagt nur, was der Payload behauptet — siehe
//! [`ParsedEvent::cwd`].
//!
//! # Ein kaputter Payload kostet nicht das Event
//!
//! Der Payload kann unvollständig ankommen — der Regelfall ist Abschneiden an
//! der stdin-Grenze der CLI. Das trifft ausgerechnet die Events, die man am
//! wenigsten verlieren will: `PostToolUse` mit großer Tool-Ausgabe, also
//! genau die Datei-Lesungen und Kommandoausgaben, die den Record wertvoll
//! machen.
//!
//! Deshalb zwei Rettungsstufen, bevor irgendetwas verworfen wird:
//!
//! 1. Scheitert der JSON-Parser, wird der Umschlag per [`salvage`] aus den
//!    Rohbytes gefischt. Die gesuchten Felder stehen bei allen bekannten
//!    Agents weit vorn — vor dem Feld, das den Payload groß macht.
//! 2. Der Payload selbst wird als JSON-**Zeichenkette** abgelegt statt
//!    weggeworfen. Beweismittel werden nicht entsorgt, nur weil sie beschädigt
//!    sind; der Adapter sieht am Typ sofort, dass hier etwas nicht stimmte.
//!
//! Was dabei **nicht** aufgeweicht wird, ist die Prüfung des
//! [`SessionKey`]: Geratenes darf ins Journal, aber nicht in einen Pfad.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::value::RawValue;

use crate::error::{CaptureError, Result};
use crate::journal::{EventKind, NewEvent, SessionKey};

/// Ergebnis von [`parse`]: das fertige Event plus die Angaben, die der
/// Aufrufer für die Ablage braucht.
#[derive(Debug)]
pub struct ParsedEvent {
    /// Wohin das Event gehört.
    pub key: SessionKey,

    /// Das Event selbst, ohne Sequenznummer — die vergibt das Journal.
    pub event: NewEvent,

    /// Das Arbeitsverzeichnis laut Payload, falls genannt.
    ///
    /// Der Aufrufer sollte es der eigenen `current_dir` **vorziehen**: Agents
    /// starten Hooks nicht zwingend im Projektverzeichnis, und ein Hook, der
    /// das falsche Repository findet, schreibt ins falsche Journal.
    pub cwd: Option<PathBuf>,
}

/// Übersetzt einen rohen Hook-Payload in ein Journal-Event.
///
/// `agent` kommt aus der Hook-Registrierung, nicht aus dem Payload: Wer uns
/// aufruft, weiß besser, wer er ist, als das JSON, das er schickt.
///
/// `event_override` füllt die Lücke bei Agents, die den Eventnamen nicht
/// mitschicken, sondern nur über die Registrierung kennen.
///
/// `at` ist die Ablesung des Aufrufers, üblicherweise [`crate::clock::now`].
pub fn parse(
    bytes: Vec<u8>,
    agent: &str,
    event_override: Option<&str>,
    at: (String, u64),
) -> Result<ParsedEvent> {
    let envelope: Envelope = serde_json::from_slice(&bytes).unwrap_or_else(|_| salvage(&bytes));

    let local_id = envelope
        .session_id
        .clone()
        .or_else(|| transcript_stem(envelope.transcript_path.as_deref()))
        .ok_or(CaptureError::NoSessionKey)?;

    // Die Pruefung sitzt hier und nirgends sonst: Was `salvage` geraten hat,
    // muss dieselbe Huerde nehmen wie sauber geparste Werte.
    let key = SessionKey::new(agent, local_id)?;

    let raw_kind = envelope
        .hook_event_name
        .clone()
        .or_else(|| event_override.map(str::to_owned))
        .unwrap_or_else(|| "Unknown".to_string());

    let (at_text, at_nanos) = at;

    Ok(ParsedEvent {
        cwd: envelope.cwd.as_deref().map(PathBuf::from),
        key,
        event: NewEvent {
            at: at_text,
            at_nanos,
            kind: classify(&raw_kind),
            raw_kind,
            cwd: envelope.cwd,
            transcript_path: envelope.transcript_path,
            payload: payload_of(bytes),
        },
    })
}

/// Der gemeinsame Umschlag, den alle unterstützten Agents schicken.
///
/// Jedes Feld ist optional, und ein unbekanntes Feld ist kein Fehler: Der
/// Payload gehört jemand anderem und darf sich ändern, ohne dass der Rekorder
/// bricht. Was wir nicht verstehen, liegt trotzdem vollständig im Journal.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Envelope {
    session_id: Option<String>,
    transcript_path: Option<String>,
    cwd: Option<String>,
    hook_event_name: Option<String>,
}

/// Rettet den Umschlag aus JSON, das sich nicht parsen lässt.
///
/// Bewusst dumm: kein Parser, keine Escape-Behandlung, erstes Vorkommen
/// gewinnt. Ein Wert mit `\"` darin liefert hier Unsinn — und dieser Unsinn
/// wird anschließend von [`SessionKey::new`] abgelehnt, wo die eigentliche
/// Prüfung sitzt. Diese Funktion darf raten; sie darf nur nichts durchlassen,
/// was gefährlich ist.
fn salvage(bytes: &[u8]) -> Envelope {
    let text = String::from_utf8_lossy(bytes);
    Envelope {
        session_id: scan(&text, "session_id"),
        transcript_path: scan(&text, "transcript_path"),
        cwd: scan(&text, "cwd"),
        hook_event_name: scan(&text, "hook_event_name"),
    }
}

/// Sucht `"key": "wert"` und gibt den Wert zurück.
fn scan(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let rest = &text[text.find(&needle)? + needle.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Bildet den Eventnamen des Agenten auf unser Vokabular ab.
///
/// Unbekanntes wird zu [`EventKind::Other`] und **nicht** zum Fehler — das
/// Vokabular der Agents wächst schneller als unseres, und der Originalname
/// bleibt über `raw_kind` ohnehin erhalten.
///
/// `PostToolUseFailure` fällt bewusst mit `PostToolUse` zusammen: Ein
/// fehlgeschlagener Tool-Call ist für den Record dasselbe Ereignis wie ein
/// gelungener — was ihn unterscheidet, steht im Payload und geht nicht
/// verloren.
fn classify(raw: &str) -> EventKind {
    match raw {
        "SessionStart" => EventKind::SessionStart,
        "SessionEnd" => EventKind::SessionEnd,
        "UserPromptSubmit" => EventKind::Prompt,
        "PreToolUse" => EventKind::ToolPre,
        "PostToolUse" | "PostToolUseFailure" => EventKind::ToolPost,
        "Stop" | "StopFailure" => EventKind::TurnEnd,
        "SubagentStart" => EventKind::SubagentStart,
        "SubagentStop" => EventKind::SubagentEnd,
        _ => EventKind::Other,
    }
}

/// Notnagel, wenn der Payload keine Session-Kennung nennt: Claude Code
/// benennt die Transkriptdatei nach der Session.
fn transcript_stem(path: Option<&str>) -> Option<String> {
    Path::new(path?)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
}

/// Verpackt die Rohbytes als JSON-Wert.
///
/// Ist der Payload gültiges JSON, wandert er **unverändert** ins Journal —
/// keine umsortierten Schlüssel, keine umgeschriebenen Zahlen. Ist er es nicht,
/// wird er als JSON-Zeichenkette abgelegt statt verworfen.
fn payload_of(bytes: Vec<u8>) -> Box<RawValue> {
    let text = String::from_utf8_lossy(&bytes).into_owned();

    if let Ok(raw) = RawValue::from_string(text.clone()) {
        return raw;
    }

    // `to_string` einer Zeichenkette kann nicht fehlschlagen, und das Ergebnis
    // ist per Konstruktion gueltiges JSON.
    RawValue::from_string(serde_json::to_string(&text).unwrap_or_else(|_| "\"\"".into()))
        .unwrap_or_else(|_| RawValue::from_string("null".into()).expect("null ist JSON"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> (String, u64) {
        (
            "2026-07-23T09:12:04.512Z".to_string(),
            1_784_797_924_512_000_000,
        )
    }

    #[test]
    fn classify_covers_the_five_that_matter() {
        assert_eq!(classify("PreToolUse"), EventKind::ToolPre);
        assert_eq!(classify("PostToolUse"), EventKind::ToolPost);
        assert_eq!(classify("UserPromptSubmit"), EventKind::Prompt);
        assert_eq!(classify("SessionStart"), EventKind::SessionStart);
        assert_eq!(classify("Stop"), EventKind::TurnEnd);
    }

    #[test]
    fn an_unknown_event_is_kept_not_rejected() {
        // Das Vokabular der Agents waechst schneller als unseres. Ein neues
        // Event darf niemals dazu fuehren, dass wir es wegwerfen.
        assert_eq!(classify("WorktreeCreate"), EventKind::Other);
        assert_eq!(classify(""), EventKind::Other);
    }

    #[test]
    fn parse_is_deterministic_and_keeps_the_payload_verbatim() {
        let json =
            br#"{"hook_event_name":"PreToolUse","session_id":"abc","cwd":"/tmp/x","z":1,"a":2}"#;

        let a = parse(json.to_vec(), "claude-code", None, at()).unwrap();
        let b = parse(json.to_vec(), "claude-code", None, at()).unwrap();

        assert_eq!(a.event.payload.get(), b.event.payload.get());
        assert_eq!(
            a.event.payload.get(),
            std::str::from_utf8(json).unwrap(),
            "Schluesselreihenfolge ist Beweismittel"
        );
        assert_eq!(a.key.local_id(), "abc");
        assert_eq!(a.event.kind, EventKind::ToolPre);
        assert_eq!(a.cwd.as_deref(), Some(Path::new("/tmp/x")));
    }

    #[test]
    fn the_registration_wins_over_the_payload_for_the_agent_name() {
        // Ein Payload, der `session_id` liefert, sagt nichts darueber, welcher
        // Agent uns aufgerufen hat — das weiss nur die Registrierung.
        let p = parse(
            br#"{"session_id":"abc","hook_event_name":"Stop"}"#.to_vec(),
            "codex",
            None,
            at(),
        )
        .unwrap();
        assert_eq!(p.key.agent(), "codex");
    }

    #[test]
    fn the_event_name_falls_back_to_the_registration() {
        let p = parse(
            br#"{"session_id":"abc"}"#.to_vec(),
            "gemini",
            Some("PostToolUse"),
            at(),
        )
        .unwrap();
        assert_eq!(p.event.kind, EventKind::ToolPost);
        assert_eq!(p.event.raw_kind, "PostToolUse");
    }

    #[test]
    fn a_missing_session_id_falls_back_to_the_transcript_name() {
        let p = parse(
            br#"{"transcript_path":"/home/a/.claude/projects/p/31f3f224.jsonl"}"#.to_vec(),
            "claude-code",
            None,
            at(),
        )
        .unwrap();
        assert_eq!(p.key.local_id(), "31f3f224");
        assert_eq!(p.event.raw_kind, "Unknown");
    }

    #[test]
    fn without_any_identity_there_is_no_event() {
        let err = parse(
            br#"{"hook_event_name":"Stop"}"#.to_vec(),
            "codex",
            None,
            at(),
        );
        assert!(matches!(err, Err(CaptureError::NoSessionKey)));
    }

    #[test]
    fn a_broken_payload_keeps_its_identity_and_its_bytes() {
        // Passiert real, wenn stdin an der Groessengrenze abgeschnitten wurde —
        // und zwar bei genau den Events, die man am wenigsten verlieren will.
        let truncated =
            br#"{"session_id":"abc","hook_event_name":"PostToolUse","tool_response":"aaaa"#;
        let p = parse(truncated.to_vec(), "claude-code", None, at()).unwrap();

        assert_eq!(p.key.local_id(), "abc", "Identitaet ueberlebt den Abbruch");
        assert_eq!(p.event.kind, EventKind::ToolPost);

        let payload = p.event.payload.get();
        assert!(payload.starts_with('"'), "als JSON-Zeichenkette abgelegt");
        assert!(payload.contains("tool_response"), "Inhalt bleibt lesbar");
    }

    #[test]
    fn salvage_also_recovers_cwd_and_transcript() {
        let truncated = br#"{"cwd":"/tmp/x","transcript_path":"/t/31f3.jsonl","big":"aaa"#;
        let p = parse(truncated.to_vec(), "claude-code", Some("Stop"), at()).unwrap();

        assert_eq!(p.cwd.as_deref(), Some(Path::new("/tmp/x")));
        assert_eq!(p.key.local_id(), "31f3");
        assert_eq!(p.event.kind, EventKind::TurnEnd);
    }

    #[test]
    fn salvage_does_not_weaken_the_path_check() {
        // Raten ja, durchlassen nein: Der geratene Wert muss dieselbe Huerde
        // nehmen wie ein sauber geparster.
        let hostile = br#"{"session_id":"../../hooks/pre-commit","x":"aaa"#;
        assert!(matches!(
            parse(hostile.to_vec(), "claude-code", None, at()),
            Err(CaptureError::UnsafeKey { .. })
        ));
    }

    #[test]
    fn a_hostile_session_id_is_rejected_before_it_becomes_a_path() {
        let err = parse(
            br#"{"session_id":"../../hooks/pre-commit"}"#.to_vec(),
            "claude-code",
            None,
            at(),
        );
        assert!(matches!(err, Err(CaptureError::UnsafeKey { .. })));
    }
}
