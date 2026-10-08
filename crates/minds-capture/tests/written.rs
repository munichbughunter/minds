//! EA-01a — der Schreibzeit-Hash `Effect::written` gegen **aufgezeichnete**
//! Claude-Code-Payloads (`fixtures/claude-code/`, Claude Code 2.1.282).
//!
//! Jeder Test fährt den echten Pfad: Payload → `hook_event::parse` →
//! `secretwall::guard` → `Journal::append` → `adapter::checkpoint`. Die
//! Erwartung ist jeweils der blake3 der Datei, wie sie **nach** dem Tool-Aufruf
//! auf der Platte lag (`*.after-*`) — nicht das, was wir uns aus dem Payload
//! zusammenreimen.

use std::path::Path;

use minds_capture::{Checkpoint, Journal, adapter, clock, hook_event, secretwall};
use minds_core::{ContentHash, EffectKind, Session, WrittenUnavailable};

/// Die anonymisierte Repo-Wurzel der Aufzeichnung — Claude Code nennt Pfade
/// absolut, die Grenze braucht deshalb die Wurzel.
const ROOT: &str = "/home/anna/scratch";

/// Golden: `blake3("hello world\nsecond line\n")` in kanonischer Textform.
const WRITE_CREATE_GOLDEN: &str =
    "b3-94803d2ead501d00c5434080fc6f831289846cdfe4e33ee03e3a29576f8f22ac";

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("fixtures/claude-code/", $name))
    };
}

fn at(n: u64) -> (String, u64) {
    let nanos = 1_790_000_000_000_000_000 + n * 1_000_000_000;
    (clock::rfc3339_from_nanos(nanos), nanos)
}

/// Genau das, was `minds hook` tut — die Mauer inklusive.
fn feed(journal: &Journal, payload: &str, seq: u64) {
    let mut parsed =
        hook_event::parse(payload.as_bytes().to_vec(), "claude-code", None, at(seq)).unwrap();
    secretwall::guard(&mut parsed.event);
    journal.append(&parsed.key, parsed.event).unwrap();
}

/// Checkpoint über die einzige Session des Journals, mit `root` als Grenze
/// und der strengen Default-Policy, wie `minds checkpoint` sie ohne
/// `.minds/redact.json` verwendet.
fn checkpoint(journal: &Journal, root: Option<&Path>) -> Session {
    let policy = minds_redact::RedactionConfig::default().pipeline().unwrap();
    checkpoint_with(journal, root, Some(&policy))
}

/// Wie [`checkpoint`], mit frei gewählter (oder fehlender) Policy.
fn checkpoint_with(
    journal: &Journal,
    root: Option<&Path>,
    redaction: Option<&minds_redact::RedactionPipeline>,
) -> Session {
    let keys = journal.sessions().unwrap().keys;
    assert_eq!(keys.len(), 1, "genau eine Session erwartet");
    let events = journal.read(&keys[0]).unwrap().events;
    let ctx = Checkpoint {
        root,
        commit: None,
        tracked: None,
        redaction,
    };
    adapter::checkpoint(&keys[0], &events, &ctx)
}

fn b3(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

/// Alle Schreib-Effekte der Session, in Reihenfolge.
fn writes(session: &Session) -> Vec<&minds_core::Effect> {
    session
        .turns
        .iter()
        .flat_map(|t| &t.tool_calls)
        .filter_map(|c| c.effect.as_ref())
        .filter(|e| e.kind == EffectKind::Write)
        .collect()
}

fn single_write(pre: &str, post: &str) -> minds_core::Effect {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(tmp.path());
    feed(&journal, pre, 0);
    feed(&journal, post, 1);
    let session = checkpoint(&journal, Some(Path::new(ROOT)));
    let mut writes = writes(&session);
    assert_eq!(writes.len(), 1);
    writes.remove(0).clone()
}

#[test]
fn written_hash_for_write_payload() {
    let effect = single_write(
        fixture!("write-create.pre.json"),
        fixture!("write-create.post.json"),
    );
    let expected = b3(include_bytes!(
        "fixtures/claude-code/notes.txt.after-write-create"
    ));
    assert_eq!(effect.written, Some(expected.clone()));
    assert_eq!(effect.written_unavailable, None);
    // Der Spec-Satz wörtlich: `written == blake3(content)` — und als Golden
    // eingefroren, damit „deterministisch" mehr heißt als „zweimal gleich".
    assert_eq!(expected, b3(b"hello world\nsecond line\n"));
    assert_eq!(expected.as_str(), WRITE_CREATE_GOLDEN);

    // Ein Write auf ein `.ipynb` läuft über den Write-Zweig (Inhalt im
    // Payload), nicht über NotebookEdit.
    let effect = single_write(
        fixture!("write-create-notebook.pre.json"),
        fixture!("write-create-notebook.post.json"),
    );
    let post: serde_json::Value =
        serde_json::from_str(fixture!("write-create-notebook.post.json")).unwrap();
    let content = post["tool_response"]["content"].as_str().unwrap();
    assert_eq!(effect.written, Some(b3(content.as_bytes())));

    // Überschreiben (`type:"update"`) — derselbe Weg, anderer Inhalt.
    let effect = single_write(
        fixture!("write-update.pre.json"),
        fixture!("write-update.post.json"),
    );
    assert_eq!(
        effect.written,
        Some(b3(include_bytes!(
            "fixtures/claude-code/notes.txt.after-write-update"
        )))
    );
}

#[test]
fn written_hash_for_edit_payload() {
    // Einzelne Ersetzung: Original + Ersetzung rekonstruiert die Datei, die
    // das Tool tatsächlich hinterlassen hat.
    let effect = single_write(
        fixture!("edit-single.pre.json"),
        fixture!("edit-single.post.json"),
    );
    assert_eq!(
        effect.written,
        Some(b3(include_bytes!(
            "fixtures/claude-code/notes.txt.after-edit-single"
        )))
    );
    assert_eq!(effect.written_unavailable, None);

    // `replace_all`: `beta` kam zweimal vor, beide werden ersetzt.
    let effect = single_write(
        fixture!("edit-replace-all.pre.json"),
        fixture!("edit-replace-all.post.json"),
    );
    assert_eq!(
        effect.written,
        Some(b3(include_bytes!(
            "fixtures/claude-code/seed.txt.after-edit-replace-all"
        )))
    );
    // Und nicht etwa nur das erste Vorkommen.
    assert_ne!(effect.written, Some(b3(b"alpha\ndelta\ngamma\nbeta\n")));
}

#[test]
fn written_hash_for_multiedit_payload() {
    // Claude Code 2.1.x kennt kein `MultiEdit` mehr; es gibt keine Fixture,
    // und geraten wird nicht (siehe fixtures/claude-code/README.md). Ein
    // Payload dieses Namens ergibt deshalb den dokumentierten Grund — und
    // nie einen Hash, auch wenn er inhaltsähnliche Felder trüge.
    let pre = r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"MultiEdit","tool_input":{"file_path":"/home/anna/scratch/a.rs","edits":[{"old_string":"a","new_string":"b"}]},"tool_use_id":"toolu_multi"}"#;
    let post = r#"{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"MultiEdit","tool_input":{"file_path":"/home/anna/scratch/a.rs","edits":[{"old_string":"a","new_string":"b"}]},"tool_response":{"filePath":"/home/anna/scratch/a.rs","originalFile":"a\n"},"tool_use_id":"toolu_multi"}"#;
    let effect = single_write(pre, post);
    assert_eq!(effect.written, None);
    assert_eq!(
        effect.written_unavailable,
        Some(WrittenUnavailable::PayloadWithoutContent)
    );
}

#[test]
fn written_hash_for_notebook_edit_payload() {
    let effect = single_write(
        fixture!("notebook-edit.pre.json"),
        fixture!("notebook-edit.post.json"),
    );
    assert_eq!(
        effect.written,
        Some(b3(include_bytes!(
            "fixtures/claude-code/nb.ipynb.after-notebook-edit"
        )))
    );
}

#[test]
fn human_edit_before_checkpoint_diverges() {
    // Der Agent schreibt `out.rs` mit einem Inhalt; vor dem Checkpoint fasst
    // ein Mensch die Datei an. `content` (Platte beim Checkpoint) und
    // `written` (Payload beim Schreiben) gehen auseinander — genau das ist
    // die Aussage, auf der EA-01 aufbaut.
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let file = root.join("out.rs");
    let path = file.to_str().unwrap();

    let journal_dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(journal_dir.path());
    feed(
        &journal,
        &format!(
            r#"{{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{{"file_path":"{path}","content":"fn agent() {{}}\n"}},"tool_use_id":"toolu_1"}}"#
        ),
        0,
    );
    feed(
        &journal,
        &format!(
            r#"{{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{{"file_path":"{path}","content":"fn agent() {{}}\n"}},"tool_response":{{"type":"create","filePath":"{path}","content":"fn agent() {{}}\n","structuredPatch":[],"originalFile":null,"userModified":false}},"tool_use_id":"toolu_1"}}"#
        ),
        1,
    );
    // Der Mensch.
    std::fs::write(&file, b"fn human() {}\n").unwrap();

    let session = checkpoint(&journal, Some(root));
    let effect = writes(&session)[0];
    assert_eq!(effect.written, Some(b3(b"fn agent() {}\n")));
    assert_eq!(effect.content, Some(b3(b"fn human() {}\n")));
    assert_ne!(effect.content, effect.written, "content ≠ written");
}

#[test]
fn written_is_never_computed_for_secret_or_outside_paths() {
    let post_for = |path: &str, id: &str| {
        format!(
            r#"{{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{{"file_path":"{path}","content":"GEHEIM"}},"tool_response":{{"type":"create","filePath":"{path}","content":"GEHEIM","originalFile":null}},"tool_use_id":"{id}"}}"#
        )
    };
    let pre_for = |path: &str, id: &str| {
        format!(
            r#"{{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{{"file_path":"{path}","content":"GEHEIM"}},"tool_use_id":"{id}"}}"#
        )
    };
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(tmp.path());
    let cases = [
        (
            "/home/anna/scratch/.env",
            "t1",
            WrittenUnavailable::SecretFile,
        ),
        (
            "/home/anna/scratch/certs/server.pem",
            "t2",
            WrittenUnavailable::SecretFile,
        ),
        (".env", "t3", WrittenUnavailable::SecretFile),
        ("/etc/hosts", "t4", WrittenUnavailable::OutsideRepo),
        ("../sibling/x.rs", "t5", WrittenUnavailable::OutsideRepo),
        (
            "/home/anna/scratch/../other/x.rs",
            "t6",
            WrittenUnavailable::OutsideRepo,
        ),
        ("~/notes.txt", "t8", WrittenUnavailable::OutsideRepo),
        ("$HOME/notes.txt", "t9", WrittenUnavailable::OutsideRepo),
    ];
    let mut seq = 0;
    for (path, id, _) in &cases {
        feed(&journal, &pre_for(path, id), seq);
        feed(&journal, &post_for(path, id), seq + 1);
        seq += 2;
    }
    let session = checkpoint(&journal, Some(Path::new(ROOT)));
    let calls: Vec<_> = session.turns.iter().flat_map(|t| &t.tool_calls).collect();
    assert_eq!(calls.len(), cases.len());
    for (call, (path, _, reason)) in calls.iter().zip(&cases) {
        let effect = call.effect.as_ref().unwrap();
        assert_eq!(effect.written, None, "{path}: kein Hash");
        assert_eq!(effect.written_unavailable, Some(*reason), "{path}");
        // Bei einer Secret-Datei ist der Inhalt gar nicht bis ins Envelope
        // gekommen: Die Hot-Path-Mauer hat den Payload ersetzt, lange bevor
        // hier etwas haette gehasht werden koennen.
        if *reason == WrittenUnavailable::SecretFile {
            assert!(
                !call.arguments.contains("GEHEIM"),
                "{path}: Inhalt im Envelope: {}",
                call.arguments
            );
        }
    }

    // Ohne bekannte Wurzel ist jeder absolute Pfad draußen — fail-closed.
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(tmp.path());
    feed(&journal, &pre_for("/home/anna/scratch/a.rs", "t7"), 0);
    feed(&journal, &post_for("/home/anna/scratch/a.rs", "t7"), 1);
    let session = checkpoint(&journal, None);
    let effect = writes(&session)[0];
    assert_eq!(effect.written, None);
    assert_eq!(
        effect.written_unavailable,
        Some(WrittenUnavailable::OutsideRepo)
    );
}

#[test]
fn a_failed_write_yields_no_written_hash() {
    // `PostToolUseFailure` traegt die Absicht, aber kein Ergebnis — der
    // ganze Hook-Weg (parse → guard → append → checkpoint) endet ohne Hash.
    let pre = r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"/home/anna/scratch/a.rs","content":"x\n"},"tool_use_id":"toolu_fail"}"#;
    let failure = r#"{"session_id":"s","hook_event_name":"PostToolUseFailure","tool_name":"Write","tool_input":{"file_path":"/home/anna/scratch/a.rs","content":"x\n"},"tool_use_id":"toolu_fail","error":"File has not been read yet"}"#;
    let effect = single_write(pre, failure);
    assert_eq!(effect.written, None);
    assert_eq!(
        effect.written_unavailable,
        Some(WrittenUnavailable::PayloadWithoutContent)
    );
}

/// Pre + Post eines Aufrufs, eine Session, Checkpoint mit Default-Policy.
fn one_call(pre: &str, post: &str, root: &Path) -> Session {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(tmp.path());
    feed(&journal, pre, 0);
    feed(&journal, post, 1);
    checkpoint(&journal, Some(root))
}

#[test]
fn a_secret_anywhere_in_the_hashed_bytes_yields_no_hash() {
    // MUST_NOT_FINGERPRINT: Das Secret steht in Bytes, die nie in den
    // `arguments` stehen — die zweite Linie in der Redaction saehe es nicht.
    // Die erste (Scan der gehashten Bytes im Checkpoint) muss es fangen.
    let path = "/home/anna/scratch/config/settings.local.yml";

    // Edit: das Secret steht im Original, nicht in der Ersetzung.
    let pre = format!(
        r#"{{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{{"file_path":"{path}","old_string":"debug: false","new_string":"debug: true"}},"tool_use_id":"toolu_e"}}"#
    );
    let post = format!(
        r#"{{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{{"file_path":"{path}","old_string":"debug: false","new_string":"debug: true"}},"tool_response":{{"filePath":"{path}","oldString":"debug: false","newString":"debug: true","originalFile":"debug: false\ngithub_token: ghp_012345678901234567890123456789012345\n","structuredPatch":[{{"oldStart":1,"oldLines":2,"newStart":1,"newLines":2,"lines":["-debug: false","+debug: true"," github_token: ghp_012345678901234567890123456789012345"]}}],"userModified":false,"replaceAll":false}},"tool_use_id":"toolu_e"}}"#
    );
    let session = one_call(&pre, &post, Path::new(ROOT));
    let call = &session.turns[0].tool_calls[0];
    let effect = call.effect.as_ref().unwrap();
    assert_eq!(effect.written, None);
    assert_eq!(
        effect.written_unavailable,
        Some(WrittenUnavailable::RedactedContent)
    );
    assert!(!call.arguments.contains("ghp_"), "Ersetzung ohne Secret");

    // NotebookEdit: das Secret steht in einer anderen Zelle.
    let nb = "/home/anna/scratch/nb.ipynb";
    let pre = format!(
        r#"{{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"NotebookEdit","tool_input":{{"notebook_path":"{nb}","new_source":"print(1)"}},"tool_use_id":"toolu_n"}}"#
    );
    let post = format!(
        r#"{{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"NotebookEdit","tool_input":{{"notebook_path":"{nb}","new_source":"print(1)"}},"tool_response":{{"notebook_path":"{nb}","updated_file":"{{\"cells\":[{{\"source\":\"print(1)\"}},{{\"source\":\"key = 'AKIAIOSFODNN7EXAMPLE'\"}}]}}"}},"tool_use_id":"toolu_n"}}"#
    );
    let session = one_call(&pre, &post, Path::new(ROOT));
    let effect = writes(&session)[0];
    assert_eq!(effect.written, None);
    assert_eq!(
        effect.written_unavailable,
        Some(WrittenUnavailable::RedactedContent)
    );

    // Write: Die Antwort (gehasht) traegt etwas anderes als die Eingabe
    // (redigiert).
    let w = "/home/anna/scratch/a.txt";
    let pre = format!(
        r#"{{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{{"file_path":"{w}","content":"x"}},"tool_use_id":"toolu_w"}}"#
    );
    let post = format!(
        r#"{{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{{"file_path":"{w}","content":"x"}},"tool_response":{{"type":"create","filePath":"{w}","content":"token=ghp_012345678901234567890123456789012345\n","structuredPatch":[],"originalFile":null,"userModified":false}},"tool_use_id":"toolu_w"}}"#
    );
    let session = one_call(&pre, &post, Path::new(ROOT));
    let effect = writes(&session)[0];
    assert_eq!(effect.written, None);
    assert_eq!(
        effect.written_unavailable,
        Some(WrittenUnavailable::RedactedContent)
    );
}

#[test]
fn a_written_file_with_a_secret_on_disk_gets_no_content_hash() {
    // Die geschlossene `content`-Luecke: Der Platten-Hash einer Schreibung
    // entsteht nur, wenn die Redaction in den Bytes nichts findet — sonst
    // waere er dasselbe Orakel wie `written` unter anderem Namen.
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let file = root.join("settings.yml");
    let path = file.to_str().unwrap();
    std::fs::write(&file, "token: ghp_012345678901234567890123456789012345\n").unwrap();
    let pre = format!(
        r#"{{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{{"file_path":"{path}","content":"x"}},"tool_use_id":"toolu_c"}}"#
    );
    let session = one_call(&pre, &pre.replace("PreToolUse", "PostToolUse"), root);
    assert_eq!(writes(&session)[0].content, None);

    // Als UTF-16LE findet kein Detektor das Token — also kein Hash statt
    // eines ungeprüften.
    let utf16: Vec<u8> = "\u{feff}token=ghp_012345678901234567890123456789012345\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    std::fs::write(&file, utf16).unwrap();
    let session = one_call(&pre, &pre.replace("PreToolUse", "PostToolUse"), root);
    assert_eq!(writes(&session)[0].content, None);

    // Dieselbe Datei ohne Secret bekommt ihn.
    std::fs::write(&file, "debug: true\n").unwrap();
    let session = one_call(&pre, &pre.replace("PreToolUse", "PostToolUse"), root);
    assert_eq!(writes(&session)[0].content, Some(b3(b"debug: true\n")));
}

#[test]
fn without_a_redaction_policy_no_write_is_fingerprinted() {
    // Fail-closed: Nichts pruefbar ⇒ weder `written` noch `content` einer
    // Schreibung, und der Grund steht dabei.
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let file = root.join("notes.txt");
    std::fs::write(&file, "hello world\nsecond line\n").unwrap();
    let payload = fixture!("write-create.post.json").replace(ROOT, root.to_str().unwrap());
    let pre = fixture!("write-create.pre.json").replace(ROOT, root.to_str().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(tmp.path());
    feed(&journal, &pre, 0);
    feed(&journal, &payload, 1);
    let session = checkpoint_with(&journal, Some(root), None);
    let effect = writes(&session)[0];
    assert_eq!(effect.written, None);
    assert_eq!(effect.content, None);
    assert_eq!(
        effect.written_unavailable,
        Some(WrittenUnavailable::Unscanned)
    );
    // Mit Policy: beide da, und gleich — Agent und Platte stimmen ueberein.
    let session = checkpoint(&journal, Some(root));
    let effect = writes(&session)[0];
    assert_eq!(effect.written, Some(b3(b"hello world\nsecond line\n")));
    assert_eq!(effect.content, effect.written);
}

#[test]
fn written_is_deterministic_per_adapter_version() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(tmp.path());
    let pairs = [
        (
            fixture!("write-create.pre.json"),
            fixture!("write-create.post.json"),
        ),
        (
            fixture!("edit-single.pre.json"),
            fixture!("edit-single.post.json"),
        ),
        (
            fixture!("edit-replace-all.pre.json"),
            fixture!("edit-replace-all.post.json"),
        ),
        (
            fixture!("notebook-edit.pre.json"),
            fixture!("notebook-edit.post.json"),
        ),
    ];
    let mut seq = 0;
    for (pre, post) in pairs {
        feed(&journal, pre, seq);
        feed(&journal, post, seq + 1);
        seq += 2;
    }
    let a = checkpoint(&journal, Some(Path::new(ROOT)));
    let b = checkpoint(&journal, Some(Path::new(ROOT)));
    assert_eq!(
        minds_core::to_canonical_string(&a).unwrap(),
        minds_core::to_canonical_string(&b).unwrap()
    );
    // Und gegen die eingefrorenen Werte — nicht nur gegen sich selbst. Alle
    // vier: Write, Edit, Edit mit replace_all, NotebookEdit.
    let golden = [
        WRITE_CREATE_GOLDEN,
        "b3-8ce033e8c48d2ffd8e0b1a664dce8904e3329215d7db3d8840ebb8acf59c14b7",
        "b3-2c823fdeed921c16452a8f33b7bdca90533ccd1bd7c59b9f427064f649ca0cee",
        "b3-4ce040209a9a3a0046f4689bb873b03ad900d98575d1a13bc9d17bc1fc1cb57e",
    ];
    let got: Vec<_> = writes(&a)
        .iter()
        .map(|e| e.written.as_ref().map(ContentHash::as_str))
        .collect();
    assert_eq!(got, golden.map(Some));
    // Jede Schreibung trägt den Hash und den Adapter-Stand, der ihn kennt.
    let calls: Vec<_> = a.turns.iter().flat_map(|t| &t.tool_calls).collect();
    assert_eq!(calls.len(), 4);
    for call in &calls {
        assert!(
            call.effect.as_ref().unwrap().written.is_some(),
            "{}",
            call.name
        );
        assert_eq!(
            call.capture.as_ref().unwrap().adapter_version,
            minds_capture::normalize::CLAUDE_ADAPTER_VERSION
        );
    }
    // `written` kennt die Deutung seit v2 (EA-01a); den aktuellen Stand
    // pinnt `tests/exec_outcome.rs` (EA-18a: v3).
    const { assert!(minds_capture::normalize::CLAUDE_ADAPTER_VERSION >= 2) };
}

#[test]
fn a_session_without_written_serializes_as_before() {
    // Legacy bleibt Legacy: Ein Effekt ohne `written` und ohne Grund zeigt
    // keinen der beiden Schlüssel — die SessionIds bestehender Sessions
    // bleiben, was sie sind.
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(tmp.path());
    feed(
        &journal,
        r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#,
        0,
    );
    let session = checkpoint(&journal, Some(Path::new(ROOT)));
    let json = minds_core::to_canonical_string(&session).unwrap();
    assert!(!json.contains("\"written\""), "{json}");
    assert!(!json.contains("\"written_unavailable\""), "{json}");
}
