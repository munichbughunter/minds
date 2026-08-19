//! End-to-End-Fixtures: roher Hook-Payload rein, [`Session`] raus.
//!
//! Diese Tests fahren den *ganzen* heißen und kalten Pfad, den `minds` in echt
//! fährt — nur die Uhr wird hereingereicht, damit die Ergebnisse Byte für Byte
//! reproduzierbar sind:
//!
//! ```text
//!   payload ──► hook_event::parse ──► secretwall::guard ──► Journal::append
//!                                                               │
//!               adapter::build ◄──────────────────────────────┘
//! ```
//!
//! Drei Szenarien aus der Anforderung: eine einzelne Session, ein Sub-Agent, und
//! zwei Agents, die parallel in *dasselbe* Journal schreiben — der Fall, den ein
//! Transkript-Parser prinzipiell nicht sehen könnte.

use std::path::Path;

use minds_capture::{Journal, adapter, clock, hook_event, secretwall};
use minds_core::{EdgeKind, Effect, EffectKind, Endpoint, Evidence, Role, Session};

/// Ein fester Zeitstempel, abgeleitet aus einer Sequenz — so ist jeder Testlauf
/// derselbe, und `at`/`at_nanos` bleiben konsistent.
fn at(n: u64) -> (String, u64) {
    let nanos = 1_784_797_924_000_000_000 + n * 1_000_000_000;
    (clock::rfc3339_from_nanos(nanos), nanos)
}

/// Genau das, was `minds hook` tut: parsen, die Secretfile-Mauer anwenden,
/// anhängen. Der Agentname kommt aus der Registrierung, nie aus dem Payload.
fn feed(journal: &Journal, agent: &str, event: Option<&str>, payload: &str, seq: u64) {
    let mut parsed = hook_event::parse(payload.as_bytes().to_vec(), agent, event, at(seq)).unwrap();
    secretwall::guard(&mut parsed.event);
    journal.append(&parsed.key, parsed.event).unwrap();
}

fn journal_in(dir: &Path) -> Journal {
    Journal::open(dir)
}

// ---------------------------------------------------------------------------
// 1) Eine einzelne Session — mit Transkript-Anreicherung
// ---------------------------------------------------------------------------

#[test]
fn single_session_with_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = journal_in(tmp.path());

    // Ein echtes Transkript auf der Platte, auf das die Events zeigen.
    let transcript = tmp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"assistant","version":"1.4.2","message":{"model":"claude-opus-4","content":[{"type":"text","text":"Ich schaue mir die Retry-Logik an."}],"usage":{"input_tokens":900,"output_tokens":120}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4","content":[{"type":"text","text":"Fertig, Backoff korrigiert."}],"usage":{"input_tokens":950,"output_tokens":30}}}"#,
        ),
    )
    .unwrap();
    let tp = transcript.to_str().unwrap();

    // Der gemeinsame Umschlag jeder Zeile; `extra` trägt Eventname und Nutzlast.
    let ev = |extra: &str| {
        format!(
            r#"{{"session_id":"s-single","transcript_path":"{tp}","cwd":"/home/anna/minds"{extra}}}"#
        )
    };

    feed(
        &journal,
        "claude-code",
        None,
        &ev(r#","hook_event_name":"SessionStart""#),
        0,
    );
    feed(
        &journal,
        "claude-code",
        None,
        &ev(
            r#","hook_event_name":"UserPromptSubmit","prompt":"Der Retry-Test flackert, bitte fixen.""#,
        ),
        1,
    );
    feed(
        &journal,
        "claude-code",
        None,
        &ev(
            r#","hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{"file_path":"src/retry.rs"}"#,
        ),
        2,
    );
    feed(
        &journal,
        "claude-code",
        None,
        &ev(
            r#","hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"src/retry.rs"}"#,
        ),
        3,
    );
    feed(
        &journal,
        "claude-code",
        None,
        &ev(r#","hook_event_name":"Stop""#),
        4,
    );

    let sessions = adapter::build(&journal).unwrap();
    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];

    // Intent, deterministisch aus dem ersten Prompt.
    assert_eq!(s.intent.request, "Der Retry-Test flackert, bitte fixen.");

    // Struktur aus dem Journal: ein User-Zug, ein Assistant-Zug mit zwei Tools.
    assert_eq!(s.turns.len(), 2);
    assert_eq!(s.turns[0].role, Role::User);
    assert_eq!(s.turns[1].role, Role::Assistant);
    assert_eq!(s.turns[1].tool_calls.len(), 2);
    assert!(matches!(
        s.turns[1].tool_calls[0].effect,
        Some(Effect {
            kind: EffectKind::Read,
            ..
        })
    ));

    // Inhalt aus dem Transkript: Modell, Token, Assistant-Text.
    assert_eq!(s.model.id, "claude-opus-4");
    assert_eq!(s.model.provider, "anthropic");
    assert_eq!(s.usage.input_tokens, 1850);
    assert_eq!(s.usage.output_tokens, 150);
    assert_eq!(s.agent.version, "1.4.2");
    assert_eq!(s.turns[1].text, "Ich schaue mir die Retry-Logik an.");

    // Herkunft und Produziertes.
    let lineage = s.lineage.as_ref().unwrap();
    assert_eq!(lineage.local_id, "s-single");
    assert_eq!(lineage.cwd.as_deref(), Some("/home/anna/minds"));
    assert_eq!(s.produced.files, vec!["src/retry.rs"]);

    // Vollständig reproduzierbar: derselbe Journal-Inhalt, dieselbe SessionId.
    let again = adapter::build(&journal).unwrap();
    assert_eq!(
        minds_core::to_canonical_string(s).unwrap(),
        minds_core::to_canonical_string(&again[0]).unwrap()
    );
}

// ---------------------------------------------------------------------------
// 2) Ein Sub-Agent — Kanten in beide Richtungen, beide beobachtet
// ---------------------------------------------------------------------------

#[test]
fn subagent_edges_are_observed_both_ways() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = journal_in(tmp.path());

    // Elternteil: sieht seinen SubagentEnd, der das Kind nennt.
    feed(
        &journal,
        "claude-code",
        None,
        r#"{"session_id":"s-parent","hook_event_name":"UserPromptSubmit","prompt":"delegiere das Review"}"#,
        0,
    );
    feed(
        &journal,
        "claude-code",
        None,
        r#"{"session_id":"s-parent","hook_event_name":"SubagentStop","subagent_session_id":"s-child","subagent_agent":"claude-code"}"#,
        1,
    );

    // Kind: sieht seinen eigenen Start als Sub-Agent, nennt den Elternteil.
    feed(
        &journal,
        "claude-code",
        None,
        r#"{"session_id":"s-child","hook_event_name":"SessionStart","source":"subagent","parent_session_id":"s-parent"}"#,
        2,
    );
    feed(
        &journal,
        "claude-code",
        None,
        r#"{"session_id":"s-child","hook_event_name":"Stop"}"#,
        3,
    );

    let sessions = adapter::build(&journal).unwrap();
    assert_eq!(sessions.len(), 2);

    let parent = find(&sessions, "s-parent");
    let child = find(&sessions, "s-child");

    // Elternteil → Kind, Spawned, beobachtet.
    assert_eq!(parent.edges.len(), 1);
    assert_eq!(parent.edges[0].kind, EdgeKind::Spawned);
    assert_eq!(parent.edges[0].evidence, Evidence::Observed);
    assert_eq!(
        parent.edges[0].to,
        Endpoint::Session {
            agent: "claude-code".into(),
            local_id: "s-child".into()
        }
    );

    // Kind → Elternteil, SpawnedBy, beobachtet.
    assert_eq!(child.edges.len(), 1);
    assert_eq!(child.edges[0].kind, EdgeKind::SpawnedBy);
    assert_eq!(child.edges[0].evidence, Evidence::Observed);
    assert_eq!(
        child.edges[0].to,
        Endpoint::Session {
            agent: "claude-code".into(),
            local_id: "s-parent".into()
        }
    );
}

// ---------------------------------------------------------------------------
// 3) Zwei Agents parallel im selben Journal
// ---------------------------------------------------------------------------

#[test]
fn two_agents_in_one_journal() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = journal_in(tmp.path());

    // Verschränkt angehängt — genau der Fall, den ein einzelnes Transkript nie
    // sähe: Claude und Codex, aufgezeichnet von einem Beobachter mit einer Uhr.
    feed(
        &journal,
        "claude-code",
        None,
        r#"{"session_id":"c1","hook_event_name":"UserPromptSubmit","prompt":"plane den Umbau"}"#,
        0,
    );
    feed(
        &journal,
        "codex",
        None,
        r#"{"session_id":"x1","hook_event_name":"UserPromptSubmit","prompt":"review den Plan"}"#,
        1,
    );
    feed(
        &journal,
        "claude-code",
        None,
        r#"{"session_id":"c1","hook_event_name":"Stop"}"#,
        2,
    );
    feed(
        &journal,
        "codex",
        None,
        r#"{"session_id":"x1","hook_event_name":"Stop"}"#,
        3,
    );

    let sessions = adapter::build(&journal).unwrap();
    assert_eq!(sessions.len(), 2, "beide Agents sind erfasst");

    // Sortiert nach (Agent, local_id): claude-code vor codex.
    assert_eq!(sessions[0].agent.name, "claude-code");
    assert_eq!(sessions[0].intent.request, "plane den Umbau");
    assert_eq!(sessions[1].agent.name, "codex");
    assert_eq!(sessions[1].intent.request, "review den Plan");

    // codex hat (noch) keinen Normalisierer — der Prompt wird trotzdem erfasst,
    // weil das Feld agent-unabhängig heißt; das Journal verliert nichts.
    assert_eq!(sessions[1].lineage.as_ref().unwrap().local_id, "x1");
}

// ---------------------------------------------------------------------------
// 4) Die Secretfile-Mauer greift auf dem heißen Pfad
// ---------------------------------------------------------------------------

#[test]
fn a_dotenv_read_never_reaches_the_journal_content() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = journal_in(tmp.path());

    feed(
        &journal,
        "claude-code",
        None,
        r#"{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{"file_path":".env"},"tool_response":"DB_PASSWORD=hunter2"}"#,
        0,
    );

    // Schon im Journal — vor jedem Checkpoint — ist der Inhalt weg.
    let key = journal.sessions().unwrap().keys.into_iter().next().unwrap();
    let events = journal.read(&key).unwrap().events;
    let raw = events[0].payload.get();
    assert!(
        !raw.contains("hunter2"),
        "der .env-Inhalt darf nie ins Journal"
    );
    assert!(raw.contains("[omitted:secret-file]"));
}

fn find<'a>(sessions: &'a [Session], local_id: &str) -> &'a Session {
    sessions
        .iter()
        .find(|s| s.lineage.as_ref().is_some_and(|l| l.local_id == local_id))
        .unwrap_or_else(|| panic!("Session {local_id} nicht gefunden"))
}
