//! Backfill: bestehende Agent-Transkripte in [`Session`]s, für die Zeit **vor**
//! der Einrichtung.
//!
//! ADR-0003 hat die Live-Erfassung auf Hooks umgestellt; ADR-0004 ergänzt sie um
//! diesen einmaligen Import. Der Hook bleibt der Normalweg — dieses Modul erntet
//! nur nach, was schon geschah, bevor `minds enable` lief, und was die Agents
//! noch als Transkript auf der Platte halten.
//!
//! # Ehrlich zu den Formaten
//!
//! Claude Code hat einen echten Reader (`~/.claude/projects/<slug>/<id>.jsonl`,
//! Format bekannt). Für Codex, Cursor, Gemini und OpenCode kennen wir Ort und
//! Form nicht belastbar; ihre Reader sind Platzhalter, die **nichts** liefern
//! und das sagen, statt Geratenes abzulegen. Das ist dieselbe Linie wie überall:
//! lieber eine ehrliche Lücke als ein stiller Fehler.
//!
//! # Was ein importierter Record *nicht* ist
//!
//! Eine importierte Session ist deterministisch aus dem Transkript gebaut — aber
//! sie hat nie ein Journal gesehen. Die Ordnung über Agents hinweg, die der
//! Hook-Weg beobachtet, fehlt ihr; ihre Verknüpfung zu einem Commit ist später
//! eine *Vermutung* (siehe `crate::match_commits`), kein beobachteter Trailer.
//! Der Import ist eine gute Näherung, keine Aufzeichnung.

use std::path::{Path, PathBuf};

use minds_core::{Agent, EffectKind, Intent, Lineage, Model, Produced, Session, ToolCall, Turn};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::normalize;
use crate::transcript;

/// Die Agents, für die der Import es versucht — in fester Reihenfolge, damit die
/// Ausgabe von `minds enable` reproduzierbar ist.
pub const KNOWN_AGENTS: &[&str] = &["claude-code", "codex", "cursor", "gemini", "opencode"];

/// Das Ergebnis eines Import-Versuchs für **einen** Agenten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentImport {
    /// Der Agentname.
    pub agent: String,
    /// Die gebauten Sessions (un-redigiert; Redaction folgt beim Ablegen).
    pub sessions: Vec<Session>,
    /// Eine ehrliche Notiz, wenn nichts (oder nur teilweise) gelesen werden
    /// konnte — z. B. „kein Importer".
    pub note: Option<String>,
}

/// Importiert die Transkripte **aller** bekannten Agents für das Repository unter
/// `repo_root`. `home` ist das Benutzerverzeichnis (injiziert, damit testbar).
pub fn for_repo(repo_root: &Path, home: &Path) -> Vec<AgentImport> {
    KNOWN_AGENTS
        .iter()
        .map(|agent| import_agent(agent, repo_root, home))
        .collect()
}

fn import_agent(agent: &str, repo_root: &Path, home: &Path) -> AgentImport {
    match agent {
        "claude-code" => import_claude_code(repo_root, home),
        other => AgentImport {
            agent: other.to_string(),
            sessions: Vec::new(),
            note: Some("kein Importer (Format nicht verifiziert)".to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

/// Sucht die Claude-Code-Transkripte für `repo_root` und baut je Datei eine
/// Session.
///
/// Claude Code legt Transkripte unter `~/.claude/projects/<slug>/<id>.jsonl` ab,
/// wobei `<slug>` das Arbeitsverzeichnis mit `/` und `.` zu `-` ist. Existiert
/// das Verzeichnis nicht, hat der Nutzer mit Claude Code in diesem Repo nie
/// gearbeitet — kein Fehler, nur nichts zu tun.
fn import_claude_code(repo_root: &Path, home: &Path) -> AgentImport {
    let dir = home
        .join(".claude")
        .join("projects")
        .join(claude_slug(repo_root));

    let mut sessions = Vec::new();
    let mut note = None;

    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
                .collect();
            // Sortiert, damit die Reihenfolge nicht von der Platte abhängt.
            files.sort();
            for path in files {
                let fallback = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unbekannt");
                match std::fs::read(&path) {
                    Ok(bytes) => sessions.extend(parse_claude_code(&bytes, fallback)),
                    Err(err) => note = Some(format!("{} nicht lesbar: {err}", path.display())),
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => note = Some(format!("{} nicht lesbar: {err}", dir.display())),
    }

    // Keine stillen Ausfälle — auch keine stillen Auslassungen: Wenn die
    // secretfile-Mauer gegriffen hat, soll der Nutzer das beim Import sehen,
    // nicht erst beim Lesen des Records.
    let walled = sessions
        .iter()
        .flat_map(|s| &s.turns)
        .flat_map(|t| &t.tool_calls)
        .filter(|c| c.arguments.contains(minds_redact::SECRET_FILE_PLACEHOLDER))
        .count();
    if walled > 0 {
        let hint = format!("{walled} Tool-Call(s) hinter der secretfile-Mauer ausgelassen");
        note = Some(match note {
            Some(existing) => format!("{existing}; {hint}"),
            None => hint,
        });
    }

    AgentImport {
        agent: "claude-code".to_string(),
        sessions,
        note,
    }
}

/// Claude Codes Verzeichnisname für ein Arbeitsverzeichnis: jeder `/` und `.`
/// wird zu `-`.
fn claude_slug(repo_root: &Path) -> String {
    repo_root
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// Baut aus einem Claude-Code-Transkript (JSONL) **eine Session je `git
/// commit`** — segmentiert, statt den ganzen Verlauf zu einem Klumpen zu machen.
///
/// # Warum segmentieren
///
/// Ein Claude-Code-Transkript ist *eine* Sitzung, die oft viele Commits
/// hervorbringt. Als eine einzige Session gebaut, hinge sie an jedem dieser
/// Commits — `minds show <commit>` zeigte für jeden dasselbe. Deshalb wird der
/// Verlauf an den Stellen geschnitten, an denen der Agent `git commit` ausführt:
/// Jedes Segment ist die Arbeit *bis zu und einschließlich* eines Commits, wird
/// eine eigene Session und matcht über seine Dateien und sein (kleines)
/// Zeitfenster genau den Commit, den es erzeugt hat.
///
/// Deterministisch: keine Uhr, kein Zufall. Dieselben Bytes ergeben dieselben
/// Sessions und damit dieselben `SessionId`s.
pub fn parse_claude_code(bytes: &[u8], fallback_local_id: &str) -> Vec<Session> {
    let text = String::from_utf8_lossy(bytes);

    // Über den ganzen Verlauf konstant: Modell, Version, Kennung, cwd.
    let mut model: Option<String> = None;
    let mut version: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut cwd: Option<String> = None;

    let mut segments: Vec<Segment> = Vec::new();
    let mut cur = Segment::default();
    // Der zuletzt gesehene Prompt. Ein Prompt kann zu mehreren Commits führen;
    // die Folge-Segmente haben dann keinen *eigenen* Prompt und erben diesen —
    // sonst stünde dort „(kein Prompt erfasst)", obwohl die Absicht bekannt ist.
    let mut carried: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Line>(line) else {
            continue;
        };

        session_id = session_id.or(entry.session_id);
        version = version.or(entry.version);
        cwd = cwd.or(entry.cwd.clone());

        let Some(message) = entry.message else {
            continue;
        };

        match entry.kind.as_deref() {
            Some("user") => {
                // Ein User-*String* ist ein Prompt; eine Liste sind Tool-Ergebnisse,
                // deren Inhalt wir nicht aufheben (der Effekt steht am Tool-Call).
                if let Some(serde_json::Value::String(prompt)) = message.content {
                    if !prompt.trim().is_empty() {
                        cur.note_time(&entry.timestamp);
                        cur.request.get_or_insert_with(|| prompt.clone());
                        carried = Some(prompt.clone());
                        cur.turns.push(user_turn(prompt, entry.timestamp.clone()));
                    }
                }
            }
            Some("assistant") => {
                model = model.or(message.model);
                if let Some(usage) = message.usage {
                    cur.input = cur.input.saturating_add(usage.input_tokens);
                    cur.output = cur.output.saturating_add(usage.output_tokens);
                }
                cur.note_time(&entry.timestamp);
                let (text, tool_calls) = assistant_blocks(message.content.as_ref());
                let commits = tool_calls.iter().any(is_git_commit);
                cur.turns.push(Turn {
                    role: minds_core::Role::Assistant,
                    text,
                    tool_calls,
                    parent: None,
                    at: entry.timestamp.clone(),
                });
                // Der Commit schließt das Segment ab — die nächste Arbeit gehört
                // zu einem anderen Commit.
                if commits {
                    finalize(&mut segments, std::mem::take(&mut cur), &carried);
                }
            }
            _ => {}
        }
    }
    finalize(&mut segments, cur, &carried);

    let model = model
        .map(|id| Model {
            provider: transcript::provider_of(&id),
            id,
        })
        .unwrap_or_else(|| Model {
            provider: unknown(),
            id: unknown(),
        });
    let version = version.unwrap_or_else(unknown);
    let local_id = session_id.unwrap_or_else(|| fallback_local_id.to_string());

    segments
        .into_iter()
        .map(|seg| seg.into_session(&version, &model, &local_id, cwd.as_deref()))
        .collect()
}

/// Schließt ein Segment ab: Fehlt ihm ein eigener Prompt, erbt es den zuletzt
/// gesehenen (`carried`). Ohne jede Absicht — nur die Segmente vor dem ersten
/// Prompt — wird es verworfen, denn ein Record ohne Absicht ist keiner.
fn finalize(segments: &mut Vec<Segment>, mut seg: Segment, carried: &Option<String>) {
    if seg.turns.is_empty() {
        return;
    }
    if seg.request.is_none() {
        seg.request = carried.clone();
    }
    if seg.request.is_some() {
        segments.push(seg);
    }
}

/// Ein Segment des Transkripts — die Arbeit bis zu einem `git commit`.
#[derive(Default)]
struct Segment {
    turns: Vec<Turn>,
    input: u64,
    output: u64,
    first_ts: Option<String>,
    last_ts: Option<String>,
    request: Option<String>,
}

impl Segment {
    /// Merkt sich den ersten und letzten Zeitpunkt des Segments.
    fn note_time(&mut self, ts: &Option<String>) {
        if let Some(ts) = ts {
            if self.first_ts.is_none() {
                self.first_ts = Some(ts.clone());
            }
            self.last_ts = Some(ts.clone());
        }
    }

    /// Baut die Session dieses Segments; die konstanten Metadaten kommen von
    /// außen.
    fn into_session(
        self,
        version: &str,
        model: &Model,
        local_id: &str,
        cwd: Option<&str>,
    ) -> Session {
        let files = written_files(&self.turns, cwd);
        Session {
            schema_version: minds_core::SCHEMA_VERSION,
            agent: Agent {
                name: "claude-code".to_string(),
                version: version.to_string(),
            },
            model: model.clone(),
            intent: Intent {
                request: self.request.unwrap_or_default(),
                ..Intent::default()
            },
            turns: self.turns,
            usage: minds_core::Usage {
                input_tokens: self.input,
                output_tokens: self.output,
            },
            produced: Produced {
                commit_hint: None,
                files,
            },
            redaction: minds_core::Redaction::default(),
            lineage: Some(Lineage {
                local_id: local_id.to_string(),
                started_at: self.first_ts,
                ended_at: self.last_ts,
                cwd: cwd.map(str::to_string),
            }),
            edges: Vec::new(),
        }
    }
}

/// Ob ein Tool-Call ein `git commit` ist — die Segmentgrenze. Geprüft am rohen
/// `arguments` (der Kommandozeile im `tool_input`); `git commit-tree` u. Ä.
/// sind so selten im Agent-Alltag, dass die grobe Prüfung genügt.
fn is_git_commit(call: &ToolCall) -> bool {
    call.name == "Bash" && call.arguments.contains("git commit")
}

fn unknown() -> String {
    "unknown".to_string()
}

fn user_turn(text: String, at: Option<String>) -> Turn {
    Turn {
        role: minds_core::Role::User,
        text,
        tool_calls: Vec::new(),
        parent: None,
        at,
    }
}

/// Zerlegt den Assistant-Inhalt in Antworttext und Tool-Calls.
fn assistant_blocks(content: Option<&serde_json::Value>) -> (String, Vec<ToolCall>) {
    let Some(serde_json::Value::Array(blocks)) = content else {
        // Ein schlichter String ist selten, aber möglich.
        if let Some(serde_json::Value::String(s)) = content {
            return (s.clone(), Vec::new());
        }
        return (String::new(), Vec::new());
    };

    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in blocks {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(t.to_string());
                }
            }
            Some("tool_use") => {
                let name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = block.get("input");
                // Doppelt serialisiert: In einem defekten oder fremden
                // Transkript kann `input` ein JSON-**String** sein statt eines
                // Objekts. Ohne das Unwrap würde die Mauer nie gefragt — die
                // Verbatim-Kopie unten nähme den Inhalt aber trotzdem mit.
                // Sicherheitsentscheidung und Datenübernahme müssen auf
                // **derselben** Form arbeiten, sonst ist die Prüfung strenger
                // als die Kopie. Eine Ebene genügt; Transkripte sind untrusted,
                // aber nicht adversarial rekursiv — und jede weitere Ebene wäre
                // wieder ein String, den die Mauer sehen kann.
                let unwrapped: Option<serde_json::Value> = match input {
                    Some(serde_json::Value::String(s)) => serde_json::from_str(s).ok(),
                    _ => None,
                };
                let effective = unwrapped.as_ref().or(input);
                // Die Mauer gilt auch auf diesem Eingangsweg (#93): Ein `Write`
                // auf eine Zugangsdaten-Datei trägt den vollen Inhalt im
                // `input`. Die Prüfung sitzt in `secretwall`, damit
                // Pfad-Schlüssel und Heuristik nicht doppelt existieren.
                let walled = effective
                    .and_then(|i| i.as_object())
                    .and_then(crate::secretwall::wall_tool_input);
                let arguments = match &walled {
                    Some((replacement, _reason)) => replacement.clone(),
                    None => effective.map(|i| i.to_string()).unwrap_or_default(),
                };
                // Der Effekt entsteht aus dem **Original**: Er extrahiert nur
                // den Pfad, nie den Inhalt, und einen Artefakt-Hash bildet der
                // Import ohnehin nicht (`claude_effect` setzt `content: None`).
                let raw = effective.and_then(|i| RawValue::from_string(i.to_string()).ok());
                let effect = normalize::claude_effect(&name, raw.as_deref());
                tool_calls.push(ToolCall {
                    name,
                    arguments,
                    effect: Some(effect),
                });
            }
            _ => {}
        }
    }

    (text_parts.join("\n"), tool_calls)
}

/// Die von der Session geschriebenen Dateien, repo-relativ (gegen `cwd`
/// gekürzt), sortiert und ohne Duplikate.
fn written_files(turns: &[Turn], cwd: Option<&str>) -> Vec<String> {
    let mut files: Vec<String> = turns
        .iter()
        .flat_map(|t| &t.tool_calls)
        .filter_map(|c| c.effect.as_ref())
        .filter(|e| matches!(e.kind, EffectKind::Write | EffectKind::Delete))
        .filter_map(|e| e.path.as_deref())
        .map(|path| relativize(path, cwd))
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Kürzt einen absoluten Pfad gegen `cwd`, damit er repo-relativ wird (wie Git
/// ihn führt). Passt `cwd` nicht, bleibt der Pfad, wie er ist.
fn relativize(path: &str, cwd: Option<&str>) -> String {
    if let Some(cwd) = cwd {
        let prefix = format!("{}/", cwd.trim_end_matches('/'));
        if let Some(rest) = path.strip_prefix(&prefix) {
            return rest.to_string();
        }
    }
    path.to_string()
}

// ---------------------------------------------------------------------------
// Drahtformat, so viel wie nötig
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    cwd: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    version: Option<String>,
    message: Option<Msg>,
}

#[derive(Debug, Deserialize)]
struct Msg {
    model: Option<String>,
    usage: Option<UsageWire>,
    content: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct UsageWire {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein knappes, aber echtes Claude-Code-Transkript.
    const SAMPLE: &str = concat!(
        r#"{"type":"user","cwd":"/home/anna/projekt","timestamp":"2026-07-23T09:00:00.000Z","sessionId":"s-1","version":"1.4.2","message":{"role":"user","content":"Fix den Retry-Test"}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-07-23T09:00:05.000Z","sessionId":"s-1","message":{"role":"assistant","model":"claude-opus-4","content":[{"type":"text","text":"Ich schaue nach."},{"type":"tool_use","name":"Read","input":{"file_path":"/home/anna/projekt/src/retry.rs"}}],"usage":{"input_tokens":900,"output_tokens":40}}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-07-23T09:00:20.000Z","sessionId":"s-1","message":{"role":"assistant","model":"claude-opus-4","content":[{"type":"tool_use","name":"Write","input":{"file_path":"/home/anna/projekt/src/retry.rs"}},{"type":"text","text":"Fertig."}],"usage":{"input_tokens":950,"output_tokens":15}}}"#,
    );

    #[test]
    fn builds_a_full_session_from_a_transcript() {
        // Ohne git commit: ein Segment, eine Session.
        let sessions = parse_claude_code(SAMPLE.as_bytes(), "fallback");
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];

        assert_eq!(s.agent.name, "claude-code");
        assert_eq!(s.agent.version, "1.4.2");
        assert_eq!(s.model.id, "claude-opus-4");
        assert_eq!(s.model.provider, "anthropic");
        assert_eq!(s.intent.request, "Fix den Retry-Test");
        assert_eq!(s.usage.input_tokens, 1850);
        assert_eq!(s.usage.output_tokens, 55);

        // Ein User-Zug, zwei Assistant-Züge.
        assert_eq!(s.turns.len(), 3);
        assert_eq!(s.turns[0].role, minds_core::Role::User);
        assert_eq!(s.turns[1].text, "Ich schaue nach.");
        assert_eq!(s.turns[2].text, "Fertig.");

        // Pfade repo-relativ gegen cwd; nur die Schreibung zählt als produziert.
        assert_eq!(s.produced.files, vec!["src/retry.rs"]);

        let lineage = s.lineage.as_ref().unwrap();
        assert_eq!(lineage.local_id, "s-1");
        assert_eq!(
            lineage.started_at.as_deref(),
            Some("2026-07-23T09:00:00.000Z")
        );
        assert_eq!(
            lineage.ended_at.as_deref(),
            Some("2026-07-23T09:00:20.000Z")
        );
        assert_eq!(lineage.cwd.as_deref(), Some("/home/anna/projekt"));
    }

    #[test]
    fn a_git_commit_splits_the_transcript_into_segments() {
        // Zwei Prompts, dazwischen ein `git commit` — zwei Sessions, jede mit
        // ihrer eigenen Absicht, ihren Dateien und ihrem Zeitfenster.
        let jsonl = concat!(
            r#"{"type":"user","cwd":"/p","timestamp":"2026-07-23T09:00:00Z","sessionId":"s","version":"1","message":{"content":"schreib a"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-07-23T09:00:05Z","message":{"model":"claude-opus-4","content":[{"type":"tool_use","name":"Write","input":{"file_path":"/p/a.rs"}}],"usage":{"input_tokens":100,"output_tokens":10}}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-07-23T09:00:10Z","message":{"model":"claude-opus-4","content":[{"type":"tool_use","name":"Bash","input":{"command":"git commit -m a"}}],"usage":{"input_tokens":100,"output_tokens":5}}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-07-23T09:05:00Z","message":{"content":"schreib b"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-07-23T09:05:05Z","message":{"model":"claude-opus-4","content":[{"type":"tool_use","name":"Write","input":{"file_path":"/p/b.rs"}}],"usage":{"input_tokens":120,"output_tokens":8}}}"#,
        );

        let sessions = parse_claude_code(jsonl.as_bytes(), "f");
        assert_eq!(sessions.len(), 2, "je Commit-Grenze eine Session");

        // Segment 1: bis zum Commit — Absicht „a", Datei a.rs, Fenster 09:00.
        assert_eq!(sessions[0].intent.request, "schreib a");
        assert_eq!(sessions[0].produced.files, vec!["a.rs"]);
        assert_eq!(
            sessions[0].lineage.as_ref().unwrap().ended_at.as_deref(),
            Some("2026-07-23T09:00:10Z")
        );

        // Segment 2: die Arbeit danach — Absicht „b", Datei b.rs, Fenster 09:05.
        assert_eq!(sessions[1].intent.request, "schreib b");
        assert_eq!(sessions[1].produced.files, vec!["b.rs"]);
        assert_eq!(
            sessions[1].lineage.as_ref().unwrap().started_at.as_deref(),
            Some("2026-07-23T09:05:00Z")
        );
    }

    #[test]
    fn a_follow_up_commit_inherits_the_driving_prompt() {
        // Ein Prompt, zwei Commits: das zweite Segment hat keinen eigenen Prompt
        // und darf trotzdem nicht „(kein Prompt erfasst)" heißen.
        let jsonl = concat!(
            r#"{"type":"user","cwd":"/p","timestamp":"2026-07-23T09:00:00Z","message":{"content":"schreib a und b, committe einzeln"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-07-23T09:00:05Z","message":{"model":"m","content":[{"type":"tool_use","name":"Write","input":{"file_path":"/p/a.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"git commit -m a"}}]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-07-23T09:01:00Z","message":{"model":"m","content":[{"type":"tool_use","name":"Write","input":{"file_path":"/p/b.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"git commit -m b"}}]}}"#,
        );
        let sessions = parse_claude_code(jsonl.as_bytes(), "f");
        assert_eq!(sessions.len(), 2);
        assert_eq!(
            sessions[0].intent.request,
            "schreib a und b, committe einzeln"
        );
        assert_eq!(
            sessions[1].intent.request,
            "schreib a und b, committe einzeln"
        );
        assert_eq!(sessions[0].produced.files, vec!["a.rs"]);
        assert_eq!(sessions[1].produced.files, vec!["b.rs"]);
    }

    #[test]
    fn a_segment_before_any_prompt_is_dropped() {
        // Reine Tool-Arbeit ohne je einen Prompt → kein Record.
        let jsonl = r#"{"type":"assistant","timestamp":"t","message":{"model":"m","content":[{"type":"tool_use","name":"Bash","input":{"command":"git commit -m x"}}]}}"#;
        assert!(parse_claude_code(jsonl.as_bytes(), "f").is_empty());
    }

    #[test]
    fn import_is_deterministic() {
        let a = parse_claude_code(SAMPLE.as_bytes(), "f");
        let b = parse_claude_code(SAMPLE.as_bytes(), "f");
        assert_eq!(a, b);
    }

    #[test]
    fn an_empty_transcript_yields_no_session() {
        assert!(parse_claude_code(b"", "f").is_empty());
        assert!(parse_claude_code(b"kein json\n", "f").is_empty());
    }

    #[test]
    fn the_fallback_id_is_used_when_none_is_named() {
        let line = r#"{"type":"user","message":{"role":"user","content":"hi"}}"#;
        let sessions = parse_claude_code(line.as_bytes(), "datei-name");
        assert_eq!(sessions[0].lineage.as_ref().unwrap().local_id, "datei-name");
    }

    #[test]
    fn for_repo_reads_claude_and_stubs_the_rest() {
        let home = tempfile::tempdir().unwrap();
        let repo_root = Path::new("/home/anna/projekt");
        let dir = home
            .path()
            .join(".claude/projects")
            .join(claude_slug(repo_root));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s-1.jsonl"), SAMPLE).unwrap();

        let reports = for_repo(repo_root, home.path());
        assert_eq!(reports.len(), KNOWN_AGENTS.len());

        let claude = reports.iter().find(|r| r.agent == "claude-code").unwrap();
        assert_eq!(claude.sessions.len(), 1);
        assert!(claude.note.is_none());

        let codex = reports.iter().find(|r| r.agent == "codex").unwrap();
        assert!(codex.sessions.is_empty());
        assert!(codex.note.as_deref().unwrap().contains("kein Importer"));
    }

    #[test]
    fn a_repo_without_claude_transcripts_imports_nothing_quietly() {
        let home = tempfile::tempdir().unwrap();
        let claude = import_claude_code(Path::new("/nie/benutzt"), home.path());
        assert!(claude.sessions.is_empty());
        assert!(claude.note.is_none(), "kein Verzeichnis ist kein Fehler");
    }

    #[test]
    fn the_slug_matches_claude_codes_encoding() {
        assert_eq!(
            claude_slug(Path::new("/Users/anna/dev/minds")),
            "-Users-anna-dev-minds"
        );
        assert_eq!(claude_slug(Path::new("/home/a.b/proj")), "-home-a-b-proj");
    }

    // --- Die Mauer gilt auch auf dem Import-Weg (#93) -------------------------

    #[test]
    fn a_write_to_a_credential_file_loses_its_content_on_import() {
        // Der zweite Eingangsweg in den Store: Ein Transkript trägt den vollen
        // Dateiinhalt im `input` des `tool_use`-Blocks. Ohne Mauer stünde er
        // wörtlich in `ToolCall::arguments` — und für einen GCP-Key rettet
        // danach zwar die PEM-Regel den Schlüssel, aber nicht die Nachbarfelder.
        let jsonl = concat!(
            r#"{"type":"user","cwd":"/home/p/projekt","timestamp":"2026-07-23T09:00:00Z","sessionId":"s-1","message":{"role":"user","content":"leg den Key ab"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-07-23T09:00:05Z","sessionId":"s-1","message":{"role":"assistant","model":"m","content":[{"type":"tool_use","name":"Write","input":{"file_path":"/home/p/projekt/credentials.json","content":"{\"private_key\":\"-----BEGIN PRIVATE KEY-----\\nGEHEIMWERT123\\n-----END PRIVATE KEY-----\","#,
            r#"\"client_email\":\"svc@beispiel.iam.gserviceaccount.com\"}"}}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
        );

        let sessions = parse_claude_code(jsonl.as_bytes(), "fallback");
        assert_eq!(sessions.len(), 1);
        let call = &sessions[0].turns[1].tool_calls[0];

        for leak in ["GEHEIMWERT123", "PRIVATE KEY", "gserviceaccount"] {
            assert!(
                !call.arguments.contains(leak),
                "{leak:?} steht in den Import-Arguments:\n{}",
                call.arguments
            );
        }
        // Wie beim Hook-Pfad: Pfad, Marker und Grund bleiben — der Reader soll
        // sehen, *dass* und *warum* hier eine Datei ausgelassen wurde.
        assert!(call.arguments.contains("credentials.json"), "Pfad weg");
        assert!(call.arguments.contains("[omitted:secret-file]"));
        assert!(call.arguments.contains("credentials-file"), "Grund fehlt");
        // Der Effekt behält seinen Pfad; einen Inhalts-Hash bildet der Import
        // ohnehin nie (`claude_effect` setzt `content: None`).
        let effect = call.effect.as_ref().unwrap();
        assert_eq!(
            effect.path.as_deref(),
            Some("/home/p/projekt/credentials.json")
        );
        assert!(effect.content.is_none());
    }

    #[test]
    fn a_double_serialized_input_cannot_slip_past_the_wall() {
        // Der Fund aus dem Security-Review: `input` als JSON-**String** statt
        // Objekt. Die Mauer parste strikt (nur Objekt), die Verbatim-Kopie
        // nahm alles — die Prüfung war strenger als die Übernahme, und genau
        // die Dateiklasse, für die die Mauer die einzige Schicht ist
        // (patternfreies Vault-Passwort), stand wörtlich im Store.
        let jsonl = concat!(
            r#"{"type":"user","timestamp":"2026-07-23T09:00:00Z","sessionId":"s-1","message":{"role":"user","content":"x"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-07-23T09:00:05Z","sessionId":"s-1","message":{"role":"assistant","model":"m","content":[{"type":"tool_use","name":"Write","input":"{\"file_path\":\"/home/p/.vault_pass\",\"content\":\"doppelt-serialisiert-geheim\"}"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
        );

        let sessions = parse_claude_code(jsonl.as_bytes(), "fallback");
        let call = &sessions[0].turns[1].tool_calls[0];
        assert!(
            !call.arguments.contains("doppelt-serialisiert-geheim"),
            "Inhalt überlebt das Unwrap:\n{}",
            call.arguments
        );
        assert!(call.arguments.contains("[omitted:secret-file]"));
        assert!(call.arguments.contains("ansible-vault-password"));
        // Auch der Effekt sieht den Pfad jetzt — vorher scheiterte die
        // Extraktion am String-Literal.
        assert_eq!(
            call.effect.as_ref().unwrap().path.as_deref(),
            Some("/home/p/.vault_pass")
        );
    }

    #[test]
    fn an_ordinary_write_keeps_its_arguments_on_import() {
        // Die Gegenprobe: Gewöhnliche Dateien bleiben Byte für Byte erhalten —
        // die Arguments sind für `why`/`show` echter Kontext.
        let jsonl = concat!(
            r#"{"type":"user","timestamp":"2026-07-23T09:00:00Z","sessionId":"s-1","message":{"role":"user","content":"fix"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-07-23T09:00:05Z","sessionId":"s-1","message":{"role":"assistant","model":"m","content":[{"type":"tool_use","name":"Write","input":{"file_path":"src/retry.rs","content":"fn main(){}"}}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
        );

        let sessions = parse_claude_code(jsonl.as_bytes(), "fallback");
        let call = &sessions[0].turns[1].tool_calls[0];
        assert!(call.arguments.contains("fn main(){}"), "Kontext verloren");
        assert!(!call.arguments.contains("minds_omitted"));
    }
}
