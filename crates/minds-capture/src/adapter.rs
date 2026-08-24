//! Der Checkpoint: aus Journal und Transkript werden [`Session`]s.
//!
//! Hier laufen die beiden Hälften zusammen, die der ganze Entwurf getrennt
//! hält. Das **Journal** ist die Wahrheit über *Ordnung* und *Tool-Calls* — ein
//! Beobachter mit einer Uhr hat jedes Event gesehen. Das **Transkript** ist die
//! Wahrheit über *Inhalt* — Antworttext, Token-Zähler, Modell-ID, die im
//! Hook-Payload nicht stehen. Der Adapter baut die Struktur aus dem Journal und
//! füllt den Inhalt aus dem Transkript.
//!
//! # Deterministisch, mit Absicht
//!
//! Keine Uhr, kein Zufall: Alle Zeitangaben stammen aus den Events, die
//! Token-Zähler aus dem Transkript. Derselbe Journal-Inhalt und dasselbe
//! Transkript ergeben Byte für Byte dieselbe [`Session`] und damit dieselbe
//! `SessionId`. Ohne diese Zusage wären die Fixture-Tests aus M5.9 nicht
//! schreibbar und die Content-Adressierung eine Behauptung.
//!
//! # Was der Journal-Adapter aus den Events macht
//!
//! - Ein **Prompt** öffnet einen User-Zug.
//! - Ein oder mehrere **PreToolUse** sammeln sich in *einem* Assistant-Zug; sein
//!   `at` ist der Zeitpunkt des ersten Tool-Calls.
//! - Ein **Stop** schließt den Assistant-Zug. Hatte der Zug keine Tools (das
//!   Modell hat nur geredet), entsteht trotzdem ein leerer Assistant-Zug — sonst
//!   ginge eine reine Text-Antwort verloren, die nur das Transkript kennt.
//!
//! `Turn::parent` bleibt `None`: Das Journal zeigt einen linearen Verlauf, und
//! bei einem linearen Verlauf ist der Elternindex schlicht `i-1` und damit
//! redundant (siehe [`Turn`]). Verzweigungen aus `/resume` oder Rewind sieht der
//! Hook nicht; sie kämen additiv aus dem Transkript.
//!
//! # Was hier noch *nicht* passiert
//!
//! Kanten (Sub-Agent, Übergabe, Commit) und der Inhalts-Hash am [`Effect`] sind
//! M5.7. Redaction ist der Schritt *danach*: Dieser Adapter liefert die rohe,
//! un-redigierte [`Session`]; erst `minds capture` schickt sie durch die
//! Pipeline und in den Store. Der Store nimmt nur redigierte Sessions an — das
//! ist ein Typ, kein Vorsatz.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use minds_core::{
    Agent, ContentHash, EffectKind, Intent, Lineage, Model, Produced, Redaction, Role, Session,
    ToolCall, Turn,
};

use crate::edges;
use crate::journal::{EventKind, Journal, JournalEvent, SessionKey};
use crate::normalize;
use crate::transcript::{self, Transcript};

/// Der Kontext eines Checkpoints — was über die Events hinaus bekannt ist.
///
/// Beides ist optional, weil ein Checkpoint auch ohne beides sinnvoll ist: Ohne
/// `root` bleiben die Artefakt-Hashes leer, ohne `commit` fehlt die
/// Produced-Kante. Der Adapter erzwingt nichts, was die Aufrufstelle nicht hat.
#[derive(Debug, Default, Clone, Copy)]
pub struct Checkpoint<'a> {
    /// Wurzel für die Auflösung relativer Effekt-Pfade beim Artefakt-Hash.
    /// Üblicherweise die Repo-Wurzel.
    pub root: Option<&'a Path>,

    /// Der Commit, den dieser Checkpoint begleitet (post-commit-Hook).
    pub commit: Option<&'a str>,

    /// Die von git **getrackten**, repo-relativen Pfade — die Grenze für
    /// Read-Hashes (Phase 6). Getrackter Inhalt ist für jeden Leser des
    /// Repos ohnehin sichtbar; sein Hash verrät nichts Neues. Alles andere
    /// (untracked, absolut, außerhalb des Worktrees) bekommt beim bloßen
    /// **Lesen** nie einen Hash — ein ungesalzener Inhalts-Hash über eine
    /// kurze, private Datei wäre ein Bestätigungsorakel; dieselbe
    /// Bedrohungsklasse, gegen die der Chain-Root gesalzen ist. `None`
    /// heißt: Grenze unbekannt ⇒ keine Read-Hashes (fail-closed in Richtung
    /// „weniger Fingerabdruck").
    pub tracked: Option<&'a std::collections::BTreeSet<String>>,
}

/// Baut aus allen Sessions eines Journals ihre [`Session`]-Records.
///
/// Reihenfolge ist die von [`Journal::sessions`] (Agent, dann `local_id`) — so
/// ist auch der Rückgabewert deterministisch.
///
/// Unauflösbare Verzeichnisse ([`SessionsOutcome::unresolved`]) meldet dieser
/// Pfad nicht — wer sie sehen muss, geht über [`Journal::sessions`] selbst,
/// wie `minds checkpoint` und `minds fsck` es tun.
///
/// [`SessionsOutcome::unresolved`]: crate::SessionsOutcome
pub fn build(journal: &Journal) -> crate::Result<Vec<Session>> {
    let mut out = Vec::new();
    for key in journal.sessions()?.keys {
        let events = journal.read(&key)?.events;
        if events.is_empty() {
            continue;
        }
        // Default-Kontext: keine Repo-Wurzel, kein Commit. Damit unterbleiben
        // Artefakt-Hashes (kein I/O) und die Commit-Kante — aber die
        // event-abgeleiteten Sub-Agent-Kanten kommen mit, weil sie nichts
        // Externes brauchen. `minds capture` (M6) ruft `checkpoint` mit vollem
        // Kontext, wenn Wurzel und Commit feststehen.
        out.push(checkpoint(&key, &events, &Checkpoint::default()));
    }
    Ok(out)
}

/// Baut die [`Session`] einer einzelnen Journal-Session.
///
/// Öffentlich, damit `minds capture` (M6) und die Fixture-Tests eine Session
/// gezielt bauen können, ohne den Umweg über das ganze Journal.
pub fn build_one(key: &SessionKey, events: &[JournalEvent]) -> Session {
    let agent = key.agent();
    let transcript = read_transcript(events);

    let turns = build_turns(agent, events, &transcript);
    let intent = Intent {
        request: first_prompt(agent, events).unwrap_or_default(),
        discarded: discarded_files(&turns),
        ..Intent::default()
    };
    let produced = Produced {
        commit_hint: None,
        files: produced_files(&turns),
    };

    Session {
        schema_version: minds_core::SCHEMA_VERSION,
        agent: Agent {
            name: agent.to_string(),
            version: transcript.agent_version.clone().unwrap_or_else(unknown),
        },
        model: transcript.model.clone().unwrap_or_else(|| Model {
            provider: unknown(),
            id: unknown(),
        }),
        intent,
        turns,
        usage: transcript.usage,
        produced,
        redaction: Redaction::default(),
        lineage: Some(lineage(key, events)),
        edges: Vec::new(),
    }
}

/// Baut die [`Session`] und reichert sie mit dem an, was nur der Checkpoint
/// weiß: Artefakt-Hashes an den Schreib-Effekten und die Kanten (Sub-Agent,
/// Commit).
///
/// Der teure Teil — Dateien lesen und hashen — passiert nur hier, nicht in
/// [`build_one`]: Wer nur die Struktur will (der Reader, ein Test), zahlt den
/// I/O nicht.
pub fn checkpoint(key: &SessionKey, events: &[JournalEvent], ctx: &Checkpoint) -> Session {
    let mut session = build_one(key, events);

    hash_artifacts(&mut session, ctx.root, ctx.tracked);

    session.edges.extend(edges::subagent(key.agent(), events));
    if let Some(commit) = ctx.commit {
        session.edges.push(edges::commit(commit));
    }

    session
}

/// Füllt die Inhalts-Hashes der Effekte, deren Datei sich lesen lässt.
///
/// Drei Regeln, alle fail-closed:
/// - **Schreib**-Effekte immer: Der Hash ist der Fingerabdruck des vom
///   Agenten *erzeugten* Artefakts.
/// - **Lese**-Effekte nur innerhalb der Read-Grenze ([`Checkpoint::tracked`]):
///   getrackte, repo-relative Pfade — als Beweismittel für Content-Übergaben
///   (Phase 6). Bloßes Lesen einer privaten oder repo-fremden Datei erzeugt
///   nie einen Fingerabdruck.
/// - **Nie** für eine Zugangsdaten-Datei: Bei einer kurzen, ratbaren Datei wäre
///   ein Hash ein Orakel. Die Secretfile-Mauer gilt auch für Fingerabdrücke.
fn hash_artifacts(
    session: &mut Session,
    root: Option<&Path>,
    tracked: Option<&std::collections::BTreeSet<String>>,
) {
    for call in session.turns.iter_mut().flat_map(|t| &mut t.tool_calls) {
        let Some(effect) = call.effect.as_mut() else {
            continue;
        };
        // Seit Phase 6 (Evidence-DAG) auch Read-Effekte: Der Hash ist das
        // Beweismittel für Content-Übergaben zwischen Agents — „B las exakt
        // die Bytes, die A schrieb" braucht beide Seiten. Der Hash entsteht
        // zum Checkpoint-Zeitpunkt: Hat sich die Datei seit dem Lesen
        // geändert, entsteht schlicht kein Match (falsch-negativ, nie
        // falsch-positiv — ein Match heißt immer „dieselben Bytes").
        if effect.content.is_some() || !matches!(effect.kind, EffectKind::Write | EffectKind::Read)
        {
            continue;
        }
        let Some(path) = effect.path.as_deref() else {
            continue;
        };
        if minds_redact::is_secret_file(path) {
            continue;
        }
        // Read-Grenze (siehe [`Checkpoint::tracked`]): Nur getrackte,
        // repo-relative Pfade — bloßes Lesen einer privaten Datei erzeugt
        // keinen Fingerabdruck. Schreib-Effekte bleiben wie bisher: Was der
        // Agent erzeugt hat, ist sein Artefakt.
        if effect.kind == EffectKind::Read {
            let inside = Path::new(path).is_relative()
                && !Path::new(path)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir));
            if !inside || !tracked.is_some_and(|set| set.contains(path)) {
                continue;
            }
        }
        if let Some(bytes) = read_artifact(root, path) {
            let digest = blake3::hash(&bytes);
            effect.content = Some(ContentHash::from_bytes(*digest.as_bytes()));
        }
    }
}

/// Liest die Artefakt-Datei. Absolute Pfade wie sie sind, relative gegen `root`
/// (fehlt `root`, gegen das Arbeitsverzeichnis). Fehlt die Datei, ist das kein
/// Fehler — der Hash bleibt dann `None`.
fn read_artifact(root: Option<&Path>, path: &str) -> Option<Vec<u8>> {
    let candidate = PathBuf::from(path);
    let full = if candidate.is_absolute() {
        candidate
    } else {
        match root {
            Some(root) => root.join(candidate),
            None => candidate,
        }
    };
    fs::read(full).ok()
}

fn unknown() -> String {
    "unknown".to_string()
}

/// Liest das Transkript, auf das die Events zeigen — best effort.
///
/// Genommen wird der `transcript_path` des letzten Events, das einen trägt: Am
/// Ende der Session ist das Transkript am vollständigsten. Fehlt die Datei oder
/// lässt sie sich nicht lesen, ist das kein Fehler, sondern ein leeres
/// Transkript — das Journal allein trägt die Session.
fn read_transcript(events: &[JournalEvent]) -> Transcript {
    let Some(path) = events
        .iter()
        .rev()
        .find_map(|e| e.transcript_path.as_deref())
    else {
        return Transcript::default();
    };

    match fs::read(Path::new(path)) {
        Ok(bytes) => transcript::parse(&bytes),
        Err(_) => Transcript::default(),
    }
}

/// Der Text des ersten Prompts — die deterministisch extrahierte Absicht.
fn first_prompt(agent: &str, events: &[JournalEvent]) -> Option<String> {
    events
        .iter()
        .filter(|e| e.kind == EventKind::Prompt)
        .find_map(|e| normalize::facts(agent, e).prompt)
}

/// Baut die Züge aus den Events und färbt die Assistant-Züge der Reihe nach mit
/// den Texten aus dem Transkript ein.
fn build_turns(agent: &str, events: &[JournalEvent], transcript: &Transcript) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut open: Option<Turn> = None;

    for event in events {
        let facts = normalize::facts(agent, event);
        match event.kind {
            EventKind::Prompt => {
                flush(&mut open, &mut turns);
                turns.push(Turn {
                    role: Role::User,
                    text: facts.prompt.unwrap_or_default(),
                    tool_calls: Vec::new(),
                    parent: None,
                    at: Some(event.at.clone()),
                });
            }
            EventKind::ToolPre => {
                let turn = open.get_or_insert_with(|| assistant_turn(&event.at));
                if let Some(tool) = facts.tool {
                    turn.tool_calls.push(ToolCall {
                        name: tool.name,
                        arguments: tool.arguments,
                        effect: tool.effect,
                        capture: Some(tool.capture),
                    });
                }
            }
            EventKind::TurnEnd => {
                if open.is_some() {
                    flush(&mut open, &mut turns);
                } else {
                    // Reine Text-Antwort ohne Tools: trotzdem ein Zug, sonst
                    // verschwindet, was nur das Transkript kennt.
                    turns.push(assistant_turn(&event.at));
                }
            }
            _ => {}
        }
    }
    flush(&mut open, &mut turns);

    paint_assistant_text(&mut turns, &transcript.assistant_texts);
    turns
}

fn assistant_turn(at: &str) -> Turn {
    Turn {
        role: Role::Assistant,
        text: String::new(),
        tool_calls: Vec::new(),
        parent: None,
        at: Some(at.to_string()),
    }
}

fn flush(open: &mut Option<Turn>, turns: &mut Vec<Turn>) {
    if let Some(turn) = open.take() {
        turns.push(turn);
    }
}

/// Weist die Transkript-Texte den Assistant-Zügen der Reihe nach zu. Gibt es
/// mehr Züge als Texte (oder umgekehrt), bleibt der Rest, wie er war — best
/// effort, nie ein Absturz.
fn paint_assistant_text(turns: &mut [Turn], texts: &[String]) {
    let mut texts = texts.iter();
    for turn in turns.iter_mut().filter(|t| t.role == Role::Assistant) {
        if let Some(text) = texts.next() {
            turn.text = text.clone();
        }
    }
}

/// Die von der Session geänderten Dateien: die Pfade der Schreib-Effekte,
/// sortiert und ohne Duplikate. Lesungen zählen nicht als „produziert".
fn produced_files(turns: &[Turn]) -> Vec<String> {
    let mut files: Vec<String> = turns
        .iter()
        .flat_map(|t| &t.tool_calls)
        .filter_map(|c| c.effect.as_ref())
        .filter(|e| {
            matches!(
                e.kind,
                minds_core::EffectKind::Write | minds_core::EffectKind::Delete
            )
        })
        .filter_map(|e| e.path.clone())
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Verworfene Ansätze, deterministisch aus den Effekten: Dateien, die in
/// derselben Session **angelegt und wieder entfernt** wurden.
///
/// Das ist der eine Sackgassen-Beleg, der hart im Effekt-Muster steht — anders
/// als eine Korrektur im Freitext, die nur eine Heuristik wäre. Weil der
/// Claude-Adapter kein `Delete`-Effekt kennt (Löschen läuft über `Bash rm`),
/// wird die Entfernung auch aus `rm`/`git rm`-Kommandos gelesen; ein künftiger
/// Adapter mit echtem `Delete`-Effekt fällt automatisch mit hinein.
///
/// `constraints` bleibt bewusst leer: dafür gibt es kein verlässliches
/// deterministisches Signal (das bräuchte ein Modell — Summary-Pfad M8), und ein
/// geratener Constraint wäre schlechter als keiner.
fn discarded_files(turns: &[Turn]) -> Vec<String> {
    let mut written: BTreeSet<String> = BTreeSet::new();
    let mut removed: BTreeSet<String> = BTreeSet::new();

    for call in turns.iter().flat_map(|t| &t.tool_calls) {
        let Some(effect) = &call.effect else { continue };
        match effect.kind {
            EffectKind::Write => {
                if let Some(path) = &effect.path {
                    written.insert(path.clone());
                }
            }
            EffectKind::Delete => {
                if let Some(path) = &effect.path {
                    removed.insert(path.clone());
                }
            }
            EffectKind::Exec => {
                if let Some(command) = command_str(&call.arguments) {
                    for target in rm_targets(&command) {
                        removed.insert(target);
                    }
                }
            }
            _ => {}
        }
    }

    let mut out: Vec<String> = written
        .iter()
        .filter(|w| removed.iter().any(|r| same_file(w, r)))
        .map(|w| format!("{w} — angelegt und wieder entfernt"))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Der Kommando-String aus dem rohen Bash-`arguments`-JSON, falls vorhanden.
fn command_str(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    value
        .get("command")
        .or_else(|| value.get("cmd"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
}

/// Die von einem `rm`/`git rm`-Kommando entfernten Pfade (Nicht-Flag-Tokens).
/// Leer, wenn das Kommando gar nichts löscht.
fn rm_targets(command: &str) -> Vec<String> {
    let cmd = command.trim();
    let rest = cmd
        .strip_prefix("rm ")
        .or_else(|| cmd.strip_prefix("git rm "));
    match rest {
        Some(rest) => rest
            .split_whitespace()
            .filter(|token| !token.starts_with('-'))
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    }
}

/// Ob zwei Pfade dieselbe Datei meinen — exakt oder über den Basename, damit
/// `rm scratch.rs` auch die geschriebene `src/scratch.rs` trifft.
fn same_file(a: &str, b: &str) -> bool {
    a == b || basename(a) == basename(b)
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Die Herkunft: Kennung, Zeitfenster aus dem ersten und letzten Event, `cwd`
/// aus dem ersten Event, das eines nennt.
fn lineage(key: &SessionKey, events: &[JournalEvent]) -> Lineage {
    Lineage {
        local_id: key.local_id().to_string(),
        started_at: events.first().map(|e| e.at.clone()),
        ended_at: events.last().map(|e| e.at.clone()),
        cwd: events.iter().find_map(|e| e.cwd.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::EffectKind;
    use serde_json::value::RawValue;

    fn ev(seq: u64, kind: EventKind, raw_kind: &str, at: &str, payload: &str) -> JournalEvent {
        JournalEvent {
            seq,
            at: at.into(),
            at_nanos: seq,
            kind,
            raw_kind: raw_kind.into(),
            cwd: Some("/home/anna/projects/minds".into()),
            transcript_path: None,
            payload: RawValue::from_string(payload.to_string()).unwrap(),
            payload_hash: None,
            event_hash: None,
        }
    }

    fn key() -> SessionKey {
        SessionKey::new("claude-code", "31f3f224").unwrap()
    }

    #[test]
    fn a_prompt_and_a_tool_burst_become_two_turns() {
        let events = vec![
            ev(
                0,
                EventKind::SessionStart,
                "SessionStart",
                "t0",
                r#"{"session_id":"x"}"#,
            ),
            ev(
                1,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t1",
                r#"{"prompt":"fix retry"}"#,
            ),
            ev(
                2,
                EventKind::ToolPre,
                "PreToolUse",
                "t2",
                r#"{"tool_name":"Read","tool_input":{"file_path":"src/retry.rs"}}"#,
            ),
            ev(
                3,
                EventKind::ToolPre,
                "PreToolUse",
                "t3",
                r#"{"tool_name":"Write","tool_input":{"file_path":"src/retry.rs"}}"#,
            ),
            ev(4, EventKind::TurnEnd, "Stop", "t4", r#"{}"#),
        ];

        let s = build_one(&key(), &events);

        assert_eq!(s.intent.request, "fix retry");
        assert_eq!(s.turns.len(), 2);
        assert_eq!(s.turns[0].role, Role::User);
        assert_eq!(s.turns[0].at.as_deref(), Some("t1"));
        assert_eq!(s.turns[1].role, Role::Assistant);
        assert_eq!(s.turns[1].at.as_deref(), Some("t2"), "erster Tool-Call");
        assert_eq!(s.turns[1].tool_calls.len(), 2);
        assert_eq!(
            s.turns[1].tool_calls[0].effect.as_ref().unwrap().kind,
            EffectKind::Read
        );
        // Nur die Schreibung zählt als produziert.
        assert_eq!(s.produced.files, vec!["src/retry.rs"]);
        // Herkunft aus den Events, ohne Uhr.
        let lin = s.lineage.unwrap();
        assert_eq!(lin.local_id, "31f3f224");
        assert_eq!(lin.started_at.as_deref(), Some("t0"));
        assert_eq!(lin.ended_at.as_deref(), Some("t4"));
        assert_eq!(lin.cwd.as_deref(), Some("/home/anna/projects/minds"));
    }

    #[test]
    fn a_stop_without_tools_still_makes_an_assistant_turn() {
        let events = vec![
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"hi"}"#,
            ),
            ev(1, EventKind::TurnEnd, "Stop", "t1", r#"{}"#),
        ];
        let s = build_one(&key(), &events);
        assert_eq!(s.turns.len(), 2);
        assert_eq!(s.turns[1].role, Role::Assistant);
        assert!(s.turns[1].tool_calls.is_empty());
    }

    #[test]
    fn build_is_deterministic() {
        let events = vec![
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"x"}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
            ),
        ];
        let a = build_one(&key(), &events);
        let b = build_one(&key(), &events);
        assert_eq!(
            minds_core::to_canonical_string(&a).unwrap(),
            minds_core::to_canonical_string(&b).unwrap()
        );
    }

    #[test]
    fn a_trailing_tool_burst_without_stop_is_still_flushed() {
        let events = vec![
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"x"}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#,
            ),
        ];
        let s = build_one(&key(), &events);
        assert_eq!(
            s.turns.len(),
            2,
            "der offene Assistant-Zug wird am Ende geschlossen"
        );
        assert_eq!(s.turns[1].tool_calls.len(), 1);
    }

    #[test]
    fn checkpoint_hashes_written_artifacts_but_not_secrets() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("out.rs"), b"fn main() {}").unwrap();
        fs::write(dir.path().join(".env"), b"DB_PASSWORD=hunter2").unwrap();

        let events = vec![
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"schreib"}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Write","tool_input":{"file_path":"out.rs"}}"#,
            ),
            ev(
                2,
                EventKind::ToolPre,
                "PreToolUse",
                "t2",
                r#"{"tool_name":"Write","tool_input":{"file_path":".env"}}"#,
            ),
        ];

        let ctx = Checkpoint {
            root: Some(dir.path()),
            commit: None,
            tracked: None,
        };
        let s = checkpoint(&key(), &events, &ctx);

        let calls = &s.turns[1].tool_calls;
        let out = calls[0].effect.as_ref().unwrap();
        let env = calls[1].effect.as_ref().unwrap();
        assert!(out.content.is_some(), "das Artefakt wird gehasht");
        assert!(
            env.content.is_none(),
            "eine Zugangsdaten-Datei wird nie gehasht — sonst ein Orakel"
        );
    }

    #[test]
    fn checkpoint_adds_a_commit_edge() {
        let events = vec![ev(
            0,
            EventKind::Prompt,
            "UserPromptSubmit",
            "t0",
            r#"{"prompt":"x"}"#,
        )];
        let ctx = Checkpoint {
            root: None,
            commit: Some("deadbeefcafe"),
            tracked: None,
        };
        let s = checkpoint(&key(), &events, &ctx);
        assert_eq!(s.edges.len(), 1);
        assert_eq!(
            s.edges[0].to,
            minds_core::Endpoint::Commit {
                id: "deadbeefcafe".into()
            }
        );
    }

    #[test]
    fn a_file_written_then_removed_via_bash_is_a_discarded_approach() {
        let events = vec![
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"probier einen Ansatz"}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Write","tool_input":{"file_path":"src/scratch.rs"}}"#,
            ),
            ev(
                2,
                EventKind::ToolPre,
                "PreToolUse",
                "t2",
                r#"{"tool_name":"Bash","tool_input":{"command":"rm src/scratch.rs"}}"#,
            ),
        ];
        let s = build_one(&key(), &events);
        assert_eq!(
            s.intent.discarded,
            vec!["src/scratch.rs — angelegt und wieder entfernt"]
        );
        // Kein geratener Constraint.
        assert!(s.intent.constraints.is_empty());
    }

    #[test]
    fn rm_by_basename_still_matches_a_written_path() {
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                r#"{"tool_name":"Write","tool_input":{"file_path":"src/scratch.rs"}}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Bash","tool_input":{"command":"rm -f scratch.rs"}}"#,
            ),
        ];
        assert_eq!(build_one(&key(), &events).intent.discarded.len(), 1);
    }

    #[test]
    fn a_written_file_that_survives_is_not_discarded() {
        let events = vec![ev(
            0,
            EventKind::ToolPre,
            "PreToolUse",
            "t0",
            r#"{"tool_name":"Write","tool_input":{"file_path":"keep.rs"}}"#,
        )];
        assert!(build_one(&key(), &events).intent.discarded.is_empty());
    }

    #[test]
    fn a_read_effect_is_hashed_but_a_secret_read_never() {
        // Seit Phase 6 traegt auch ein Read-Effekt den Inhalts-Hash — das
        // Beweismittel fuer Content-Uebergaben. Die Orakel-Regel gilt
        // unveraendert: Secret-Dateien bekommen nie einen Hash.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("read.rs"), b"content").unwrap();
        fs::write(dir.path().join(".env"), b"SECRET=x").unwrap();
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                r#"{"tool_name":"Read","tool_input":{"file_path":"read.rs"}}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Read","tool_input":{"file_path":".env"}}"#,
            ),
        ];
        // Die Read-Grenze: nur getrackte Pfade bekommen einen Lese-Hash.
        let tracked: std::collections::BTreeSet<String> = ["read.rs".to_string()].into();
        let ctx = Checkpoint {
            root: Some(dir.path()),
            commit: None,
            tracked: Some(&tracked),
        };
        let s = checkpoint(&key(), &events, &ctx);
        let calls: Vec<_> = s.turns.iter().flat_map(|t| t.tool_calls.iter()).collect();
        let read = calls
            .iter()
            .find(|c| c.effect.as_ref().and_then(|e| e.path.as_deref()) == Some("read.rs"))
            .unwrap();
        let expected = ContentHash::from_bytes(*blake3::hash(b"content").as_bytes());
        assert_eq!(
            read.effect.as_ref().unwrap().content.as_ref(),
            Some(&expected)
        );
        // Ohne tracked-Set (Grenze unbekannt) entsteht fuer Reads NIE ein
        // Hash — fail-closed in Richtung „weniger Fingerabdruck".
        let ctx_unbounded = Checkpoint {
            root: Some(dir.path()),
            commit: None,
            tracked: None,
        };
        let s2 = checkpoint(&key(), &events, &ctx_unbounded);
        let read2 = s2
            .turns
            .iter()
            .flat_map(|t| t.tool_calls.iter())
            .find(|c| c.effect.as_ref().and_then(|e| e.path.as_deref()) == Some("read.rs"))
            .unwrap();
        assert!(read2.effect.as_ref().unwrap().content.is_none());

        // Die Secretwall hat den .env-Aufruf schon auf dem heissen Pfad
        // gewallt — hier darf so oder so nie ein Hash entstehen.
        for call in &calls {
            if let Some(effect) = &call.effect {
                if effect.path.as_deref() == Some(".env") {
                    assert!(effect.content.is_none(), "Hash-Orakel ueber Secret-Datei");
                }
            }
        }
    }
}
