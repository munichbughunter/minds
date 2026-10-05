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

use crate::journal::{EventKind, NewEvent, SessionKey};

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

/// Baut die Hook-Bytes neu, nachdem [`guard`] gegriffen hat — für die
/// Weitergabe an den Witness (EA-07).
///
/// Der Witness-Frame trägt rohes stdin, und der Witness parst es mit
/// demselben [`hook_event::parse`](crate::hook_event::parse) wie der Hook. Die
/// Mauer läuft aber schon auf der Agent-Seite, damit der Inhalt einer
/// Zugangsdaten-Datei den Socket gar nicht erst überquert. Also muss aus dem
/// gewallten Event wieder ein Hook-Payload werden: der Ersatz-Payload
/// (`tool_name` plus gewalltes `tool_input`) und daneben genau die
/// Umschlagfelder, aus denen `parse` Session, Eventnamen, `cwd` und
/// `transcript_path` liest. Maßgeblich für diese Felder ist `Envelope` in
/// [`hook_event`](crate::hook_event): Wächst er, muss er hier mitwachsen —
/// `walled_bytes_reproduce_the_local_event_at_the_witness` fällt sonst um.
///
/// Was der Witness daraus macht, ist dasselbe Event wie im lokalen Journal:
/// Er findet im `tool_input` denselben Pfad, seine eigene Mauer greift erneut
/// und ersetzt den Payload durch exakt denselben Ersatz. Die Umschlagfelder
/// in diesen Bytes sind damit nur Transport, kein Beweismittel.
///
/// `None`, wenn der Payload kein JSON-Objekt ist — nach [`guard`] ist er das
/// immer; für jeden anderen Aufrufer heißt `None`: nicht weitergeben.
pub fn walled_hook_bytes(key: &SessionKey, event: &NewEvent) -> Option<Vec<u8>> {
    let Ok(Value::Object(mut body)) = serde_json::from_str::<Value>(event.payload.get()) else {
        return None;
    };
    body.insert("session_id".into(), key.local_id().into());
    body.insert("hook_event_name".into(), event.raw_kind.clone().into());
    if let Some(cwd) = &event.cwd {
        body.insert("cwd".into(), cwd.clone().into());
    }
    if let Some(path) = &event.transcript_path {
        body.insert("transcript_path".into(), path.clone().into());
    }
    serde_json::to_vec(&Value::Object(body)).ok()
}

/// Der zweite Weg zur selben Mauer: prüft die `input`-Map eines
/// `tool_use`-Blocks und liefert den Ersatz für `ToolCall::arguments`, falls
/// sie eine Zugangsdaten-Datei berührt — plus den Regelnamen für den Audit.
///
/// [`guard`] schützt den Hook-Weg (Journal-Events), diese Funktion den
/// **Import-Weg** (`minds import` liest `tool_use`-Blöcke direkt aus dem
/// Transkript, ohne Journal). Beide teilen sich [`secret_path_in`] und
/// [`walled_input`] — Pfad-Schlüssel, Heuristik und Ersatz-Form stehen damit
/// an genau einer Stelle. Eine zweite Fassung in `import.rs` wäre exakt die
/// Divergenz, die schon einmal die Fehlerquelle war.
pub fn wall_tool_input(input: &Map<String, Value>) -> Option<(String, &'static str)> {
    let (path_key, path, reason) = secret_path_in(input)?;
    Some((walled_input(path_key, path, reason).to_string(), reason))
}

/// Die **eine** Ersatz-Form für ein gewalltes `tool_input` — beide Eingangswege
/// bauen sie hier.
///
/// Marker und Grund stehen bewusst **im** `tool_input` und nicht daneben:
/// `normalize::claude_tool` übernimmt später nur `tool_input` verbatim in
/// [`ToolCall::arguments`](minds_core::ToolCall::arguments). Lägen die Marker
/// daneben (so war es), verlöre der Hook-Weg die Auskunft „hier wurde eine
/// Datei ausgelassen" im Envelope vollständig — der Reader sähe je nach
/// Eingangsweg zwei verschiedene Dinge.
///
/// Der Feldname des Grundes heißt `minds_omitted_reason` und **darf kein
/// Detektor-Stichwort enthalten**: Ein früheres `minds_secret_file_reason`
/// wurde von der Redaction-Pipeline selbst getroffen — `secret` matcht im
/// Strict-Tier ohne Wortgrenzen, und im Store stand statt des Grundes
/// `[redacted:secret]`. Gemessen, nicht vermutet.
fn walled_input(path_key: &str, path: &str, reason: &str) -> Value {
    serde_json::json!({
        path_key: path,
        "minds_omitted": SECRET_FILE_PLACEHOLDER,
        "minds_omitted_reason": reason,
    })
}

/// Baut den minimalen Ersatz-Payload: Tool-Name plus das gewallte `tool_input`
/// aus [`walled_input`].
fn sanitized(name: &str, path_key: &str, path: &str, reason: &str) -> Box<RawValue> {
    let value = serde_json::json!({
        "tool_name": name,
        "tool_input": walled_input(path_key, path, reason),
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
    use crate::hook_event::ParsedEvent;

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
    fn both_ingestion_paths_agree_on_the_same_fixtures() {
        // Akzeptanzkriterium aus #93: Hook-Weg (`guard`) und Import-Weg
        // (`wall_tool_input`) gegen **dieselben** Eingaben — driften die beiden
        // je auseinander, wird genau dieser Test rot. Die Fälle decken beide
        // Richtungen ab: Mauer greift / Mauer greift nicht.
        let cases: &[(&str, Option<&'static str>)] = &[
            (".env", Some("dotenv")),
            ("config/credentials.json", Some("credentials-file")),
            ("/home/p/service_account.json", Some("gcp-service-account")),
            ("/home/p/.aws/credentials.bak", Some("aws-credentials")),
            ("certs/server.pem", Some("private-key")),
            // Die Dateiklassen, für die die Mauer die **einzige** Schicht ist —
            // patternfreie Inhalte, die kein Detektor fangen kann.
            ("/home/p/.dockercfg", Some("docker-config")),
            (
                "/home/p/ansible/vault_pass.txt",
                Some("ansible-vault-password"),
            ),
            (".vault_pass", Some("ansible-vault-password")),
            ("src/retry.rs", None),
            (".env.example", None),
        ];

        for (path, expected) in cases {
            // Hook-Weg.
            let payload = format!(
                r#"{{"tool_name":"Write","tool_input":{{"file_path":"{path}","content":"GEHEIM"}}}}"#
            );
            let mut event = tool_event(EventKind::ToolPre, &payload);
            let hook_reason = guard(&mut event);
            assert_eq!(hook_reason, *expected, "Hook-Weg bei {path:?}");

            // Import-Weg, dieselbe Eingabe als `tool_use`-Input.
            let input: Map<String, Value> =
                serde_json::from_str(&format!(r#"{{"file_path":"{path}","content":"GEHEIM"}}"#))
                    .unwrap();
            let import_reason = wall_tool_input(&input).map(|(_, reason)| reason);
            assert_eq!(import_reason, *expected, "Import-Weg bei {path:?}");

            // Greift die Mauer, verlieren beide den Inhalt und behalten den Pfad.
            if expected.is_some() {
                assert!(!event.payload.get().contains("GEHEIM"));
                let (replacement, _) = wall_tool_input(&input).unwrap();
                assert!(!replacement.contains("GEHEIM"));
                assert!(replacement.contains(path));
                assert!(replacement.contains("[omitted:secret-file]"));

                // **Ergebnis-Parität, nicht nur Regel-Parität:** Der Hook-Weg
                // wird bis zur Envelope-Sicht zu Ende gegangen — `claude_tool`
                // übernimmt `tool_input` verbatim in `ToolCall::arguments`.
                // Was dort ankommt, muss byte-gleich dem Import-Ersatz sein.
                // Genau diese Divergenz gab es schon einmal: Die Marker lagen
                // *neben* `tool_input`, und der Hook-Weg verlor sie im
                // Envelope vollständig.
                let hook_tool: Tool = serde_json::from_str(event.payload.get()).unwrap();
                let hook_arguments = serde_json::to_string(&hook_tool.tool_input.unwrap()).unwrap();
                assert_eq!(
                    hook_arguments, replacement,
                    "Envelope-Sicht der beiden Wege driftet bei {path:?}"
                );
            }
        }
    }

    #[test]
    fn the_walled_replacement_is_a_fixpoint_of_the_redaction_pipeline() {
        // Der Ersatz läuft später durch `redact_session`. Frisst ein künftiger
        // Detektor den Marker oder den Grund an, verfälscht das die Zähler und
        // nimmt dem Reader die Auskunft — genau so ist der Grund schon einmal
        // verschwunden, als sein Feldname noch `secret` enthielt.
        let input: Map<String, Value> =
            serde_json::from_str(r#"{"file_path":".env","content":"DB_PASSWORD=hunter2"}"#)
                .unwrap();
        let (replacement, _) = wall_tool_input(&input).unwrap();

        let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
        let out = pipeline.redact(&replacement);
        assert_eq!(out.text, replacement, "der Ersatz ist kein Fixpunkt mehr");
        assert_eq!(out.counts, minds_core::RedactionCounts::default());
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

    // --- Weitergabe an den Witness (EA-07) ------------------------------------

    /// Der Hook-Weg bis zum Witness: Agent-Seite parst und wallt, baut die
    /// Bytes neu; der Witness parst und wallt dieselben Bytes erneut.
    fn through_witness(stdin: &str, event_override: Option<&str>) -> (ParsedEvent, ParsedEvent) {
        let at = ("2026-07-23T09:12:04.512Z".to_string(), 7);
        let mut local = crate::hook_event::parse(
            stdin.as_bytes().to_vec(),
            "claude-code",
            event_override,
            at.clone(),
        )
        .unwrap();
        guard(&mut local.event).expect("die Mauer greift");
        let bytes = walled_hook_bytes(&local.key, &local.event).unwrap();
        let mut remote =
            crate::hook_event::parse(bytes, "claude-code", event_override, at).unwrap();
        assert!(
            guard(&mut remote.event).is_some(),
            "die Mauer greift beim Witness erneut"
        );
        (local, remote)
    }

    fn assert_same_event(local: &ParsedEvent, remote: &ParsedEvent) {
        assert_eq!(remote.key, local.key);
        assert_eq!(remote.cwd, local.cwd);
        assert_eq!(remote.event.kind, local.event.kind);
        assert_eq!(remote.event.raw_kind, local.event.raw_kind);
        assert_eq!(remote.event.cwd, local.event.cwd);
        assert_eq!(remote.event.transcript_path, local.event.transcript_path);
        assert_eq!(remote.event.payload.get(), local.event.payload.get());
    }

    #[test]
    fn walled_bytes_reproduce_the_local_event_at_the_witness() {
        let stdin = r#"{"session_id":"s-1","transcript_path":"/t/s-1.jsonl","cwd":"/w/demo","hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{"file_path":".env"},"tool_response":"DB_PASSWORD=hunter2"}"#;
        let (local, remote) = through_witness(stdin, None);
        assert_same_event(&local, &remote);

        let bytes = walled_hook_bytes(&local.key, &local.event).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("hunter2"), "kein Geheimnis auf dem Socket");
        assert!(!text.contains("tool_response"));
        assert!(text.contains(SECRET_FILE_PLACEHOLDER));
    }

    #[test]
    fn walled_bytes_keep_a_session_key_taken_from_the_transcript() {
        // Ohne `session_id` kommt der Schlüssel aus dem Transkript-Namen, ohne
        // `hook_event_name` aus der Registrierung — beides muss beim Witness
        // zum selben Ergebnis führen.
        let stdin = r#"{"transcript_path":"/t/from-stem.jsonl","tool_name":"Read","tool_input":{"path":"certs/server.pem"}}"#;
        let (local, remote) = through_witness(stdin, Some("PreToolUse"));
        assert_eq!(local.key.local_id(), "from-stem");
        assert_same_event(&local, &remote);
    }

    #[test]
    fn walled_bytes_refuse_a_non_object_payload() {
        let e = tool_event(EventKind::ToolPost, r#""kaputt""#);
        let key = SessionKey::new("claude-code", "s").unwrap();
        assert_eq!(walled_hook_bytes(&key, &e), None);
    }
}
