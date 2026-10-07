use super::*;
use minds_store::ContextStore;

mod intent_tests;
mod observer_tests;

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "--template="]);
        for (name, value) in [
            ("user.name", "Witness Test"),
            ("user.email", "witness@example.invalid"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
            ("minds.backend", "in-repo"),
        ] {
            git(&root, &["config", name, value]);
        }
        git(&root, &["commit", "-qm", "initial", "--allow-empty"]);
        let home = dir.path().join("home");
        init(
            &home,
            &InitRequest {
                repo: root.to_str().unwrap(),
                mapping: None,
                profile: None,
                socket_group: None,
                child_repo: None,
                policy_rev: None,
            },
        )
        .unwrap();
        Self {
            _dir: dir,
            home,
            root,
        }
    }
    fn writer(&self) -> Writer {
        Writer::open(&self.home, load(&self.home).unwrap(), false).unwrap()
    }
    fn keygen(&self) -> String {
        super::super::keygen(&self.home).unwrap()
    }
}
fn key() -> SessionKey {
    SessionKey::new("claude-code", "test-session").unwrap()
}
fn at() -> (String, u64) {
    ("2026-10-04T10:00:00Z".into(), 1_791_108_000_000_000_000)
}
fn payload() -> Vec<u8> {
    br#"{"session_id":"test-session","hook_event_name":"UserPromptSubmit","prompt":"Implement sorting","cwd":"/workspaces/demo"}"#.to_vec()
}
fn append(writer: &mut Writer) {
    writer.hook("claude-code", None, payload(), at()).unwrap();
}

#[test]
fn witness_chain_equals_legacy_chain() {
    let f = Fixture::new();
    let mut writer = f.writer();
    let legacy = Journal::at(f._dir.path().join("legacy"));
    // Steigende Stempel: Die monotone Uhr (EA-08a) ließe einen gleichen
    // nicht stehen — wohl aber jeden, der schon steigt.
    for i in 0..4 {
        let nanos = at().1 + i * 1_000_000_000;
        let stamp = (minds_capture::clock::rfc3339_from_nanos(nanos), nanos);
        writer
            .hook("claude-code", None, payload(), stamp.clone())
            .unwrap();
        let mut parsed = hook_event::parse(payload(), "claude-code", None, stamp).unwrap();
        secretwall::guard(&mut parsed.event);
        legacy.append(&parsed.key, parsed.event).unwrap();
    }
    let salt = writer.epochs.salt(&key()).unwrap();
    let expected = chain::chain_salted(&salt, &legacy.read(&key()).unwrap());
    assert_eq!(writer.folders[&key()].snapshot(), expected);
    let actual = writer.journal.read(&key()).unwrap();
    assert_eq!(
        serde_json::to_vec(&actual.events).unwrap(),
        serde_json::to_vec(&legacy.read(&key()).unwrap().events).unwrap()
    );
    // Auch der Seal-Text bleibt identisch, bis auf die benannte Beobachtungsgrenze.
    f.keygen();
    writer.checkpoint_now(None).unwrap();
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    let seals = store.list_seals().unwrap();
    assert_eq!(seals.len(), 1);
    let text = store.seal_text(&seals[0]).unwrap().unwrap();
    let seal = minds_core::evidence::Seal::parse(&text).unwrap();
    assert_eq!(seal.root, expected.root);
    let legacy_fixture = Fixture::new();
    let repo = minds_git::Repo::discover(&legacy_fixture.root).unwrap();
    let legacy_store = minds_store::InRepoStore::open(&legacy_fixture.root).unwrap();
    let pipeline = crate::config::load_redaction(&legacy_fixture.root)
        .unwrap()
        .pipeline()
        .unwrap();
    let env = CheckpointEnv {
        repo: &repo,
        root: &legacy_fixture.root,
        log_dir: repo.git_dir(),
        store: &legacy_store,
        pipeline: &pipeline,
        tracked: None,
    };
    // Derselbe Salt, dieselbe Fake-Uhr und dieselben Bytes, aber der echte
    // Legacy-Checkpoint einschließlich Adapter und fail-closed Redaktion.
    let legacy_epochs = EpochState::at(legacy_fixture.home.join("evidence/state"));
    legacy_epochs.salt(&key()).unwrap();
    let salt_path = fs::read_dir(legacy_fixture.home.join("evidence/state/claude-code"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "salt"))
        .unwrap();
    fs::write(salt_path, salt).unwrap();
    let source = EvidenceSource {
        journal: &legacy,
        epochs: &legacy_epochs,
        scope: minds_core::evidence::SCOPE_AGENT_HOOKS_V1,
    };
    let outcome =
        crate::checkpoint::core::run_checkpoint(&env, &source, &SealSigner::None).unwrap();
    let legacy_text = legacy_store
        .seal_text(&outcome.sealed[0].seal_id)
        .unwrap()
        .unwrap();
    assert_eq!(legacy_text.replace("agent-hooks/v1", "witness/v1"), text);
}

#[test]
fn witness_recovers_journal_ahead_of_folder_and_resumes() {
    let f = Fixture::new();
    let mut writer = f.writer();
    append(&mut writer);
    let parsed = hook_event::parse(payload(), "claude-code", None, at()).unwrap();
    writer.journal.append(&parsed.key, parsed.event).unwrap();
    drop(writer);
    let mut writer = f.writer();
    append(&mut writer);
    let expected = chain::chain_salted(
        &writer.epochs.salt(&key()).unwrap(),
        &writer.journal.read(&key()).unwrap(),
    );
    assert_eq!(writer.folders[&key()].snapshot(), expected);
    let persisted: FolderState =
        serde_json::from_slice(&fs::read(folder_path(&f.home, &key())).unwrap()).unwrap();
    assert_eq!(ChainFolder::from_state(persisted).snapshot(), expected);
}

#[test]
fn witness_defers_tampered_folder_and_journal() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let wrong = ChainFolder::new_salted(&[99; 32]);
    atomic(
        &folder_path(&f.home, &key()),
        &serde_json::to_vec(&wrong.to_state()).unwrap(),
    )
    .unwrap();
    // Vertagt, nicht abgebrochen: Der Integritätsfehler steht im Log.
    writer.checkpoint_now(None).unwrap();
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    assert!(store.list_seals().unwrap().is_empty());
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);
    assert!(
        fs::read_to_string(f.home.join("log/witness.log"))
            .unwrap()
            .contains("integrity error")
    );
}

#[test]
fn witness_seal_is_scoped_and_signed_and_ledger_lists_every_seal() {
    let f = Fixture::new();
    let signers_line = f.keygen();
    let signers = f._dir.path().join("allowed_signers");
    fs::write(&signers, &signers_line).unwrap();
    let principal = signers_line.split_whitespace().next().unwrap();
    let mut writer = f.writer();
    for second in 0..2 {
        writer
            .hook(
                "claude-code",
                None,
                payload(),
                (format!("2026-10-04T10:00:0{second}Z"), at().1 + second),
            )
            .unwrap();
        writer.checkpoint_now(None).unwrap();
    }
    writer.checkpoint_now(None).unwrap();
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    let seals = store.list_seals().unwrap();
    assert_eq!(seals.len(), 2);
    let ledger = fs::read_to_string(f.home.join("ledger")).unwrap();
    assert_eq!(ledger.lines().count(), 2);
    for id in seals {
        let text = store.seal_text(&id).unwrap().unwrap();
        let signature = store.seal_signature(&id).unwrap().unwrap();
        assert!(
            minds_attest::ssh_verify_ns(
                &text,
                &signature,
                &signers,
                principal,
                minds_attest::NS_WITNESS
            )
            .unwrap()
        );
        assert!(
            !minds_attest::ssh_verify_ns(
                &text,
                &signature,
                &signers,
                principal,
                minds_attest::NAMESPACE
            )
            .unwrap()
        );
        assert_eq!(
            minds_core::evidence::Seal::parse(&text).unwrap().scope,
            "witness/v1"
        );
        assert_eq!(
            ledger
                .lines()
                .filter(|line| line.starts_with(id.as_str()))
                .count(),
            1
        );
    }
    assert!(git(&f.root, &["log", "-1", "--format=%B"]).contains("Minds-Session-Id:"));
    assert!(writer.journal.read(&key()).unwrap().events.is_empty());
    git(&f.root, &["fsck", "--no-dangling"]);
}

#[test]
fn witness_ledger_is_idempotent_after_sealing_before_discard() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let read = writer.journal.read(&key()).unwrap();
    writer.checkpoint_now(None).unwrap();
    // Simulierter Crash: Journal lag vor dem Discard noch vollständig da.
    for event in read.events {
        writer
            .journal
            .append(
                &key(),
                NewEvent {
                    at: event.at,
                    at_nanos: event.at_nanos,
                    raw_kind: event.raw_kind,
                    kind: event.kind,
                    cwd: event.cwd,
                    transcript_path: event.transcript_path,
                    payload: event.payload,
                },
            )
            .unwrap();
    }
    let mut writer = f.writer();
    writer.checkpoint_now(None).unwrap();
    assert_eq!(
        fs::read_to_string(f.home.join("ledger"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn witness_lifecycle_is_sealed_as_its_own_stream_and_agent_cannot_forge_it() {
    use minds_core::evidence::Seal;
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    let own = SessionKey::new("witness", "123").unwrap();
    writer
        .lifecycle(
            &own,
            "witness.start",
            serde_json::json!({"previous_stop":"none"}),
        )
        .unwrap();
    // Der Agent kann nicht in den eigenen Stream schreiben.
    writer.hook("witness", None, payload(), at()).unwrap();
    assert_eq!(writer.journal.read(&own).unwrap().events.len(), 1);
    append(&mut writer);
    writer.checkpoint_now(None).unwrap();
    // Seit EA-08 wird der eigene Stream versiegelt — als eigener Bereich mit
    // Scope `witness-fs/v1`, nie als Agent-Session (`witness/v1`).
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    let mut seals: Vec<(String, String, u64)> = store
        .list_seals()
        .unwrap()
        .iter()
        .map(|id| {
            let seal = Seal::parse(&store.seal_text(id).unwrap().unwrap()).unwrap();
            (seal.scope, seal.agent, seal.events)
        })
        .collect();
    seals.sort();
    assert_eq!(
        seals,
        [
            // Nur `witness.start`: Ohne laufenden eigenen Stream (kein
            // `run`) schreibt der Checkpoint keine Grenze.
            ("witness-fs/v1".into(), "witness".into(), 1),
            ("witness/v1".into(), "claude-code".into(), 1),
        ]
    );
    assert!(writer.journal.sessions().unwrap().keys.is_empty());
}

#[test]
fn witness_follow_never_prints_payload() {
    let f = Fixture::new();
    let mut writer = f.writer();
    let stdin = br#"{"session_id":"test-session","hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"src/sort/merge.rs","content":"PAYLOAD_SENTINEL"},"tool_response":"PAYLOAD_SENTINEL"}"#.to_vec();
    writer.hook("claude-code", None, stdin, at()).unwrap();
    let events = writer.journal.read(&key()).unwrap().events;
    let line = follow_line(&key(), &events[0], &writer.folders[&key()]);
    assert!(
        line.starts_with("seq 000000  PostToolUse  Write src/sort/merge.rs   head "),
        "{line}"
    );
    assert!(!line.contains("PAYLOAD_SENTINEL"));
    append(&mut writer);
    let event = writer.journal.read(&key()).unwrap().events.pop().unwrap();
    assert!(!follow_line(&key(), &event, &writer.folders[&key()]).contains("Implement sorting"));
}

#[test]
fn witness_refuses_insecure_home_and_multiple_writers() {
    let f = Fixture::new();
    // Ein Kindprozess, den ein parallel laufender Test gerade startet, hält
    // zwischen `fork` und `exec` eine Kopie jedes Deskriptors — auch des
    // Locks, den `init` eben abgegeben hat. Der `flock` hängt an der
    // gemeinsamen Dateibeschreibung und lebt so einen Augenblick weiter.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let lock1 = loop {
        match lock(&f.home) {
            Ok(lock) => break lock,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(err) => panic!("{err}"),
        }
    };
    assert!(lock(&f.home).is_err());
    drop(lock1);
    fs::set_permissions(&f.home, fs::Permissions::from_mode(0o770)).unwrap();
    assert!(load(&f.home).is_err());
    fs::set_permissions(&f.home, fs::Permissions::from_mode(0o700)).unwrap();
    let link = f._dir.path().join("link");
    std::os::unix::fs::symlink(&f.home, &link).unwrap();
    assert!(load(&link).is_err());
    assert!(load(&link.join("")).is_err());
    let original = f.home.join("ledger");
    fs::remove_file(&original).unwrap();
    std::os::unix::fs::symlink(f.root.join("stolen"), &original).unwrap();
    assert!(load(&f.home).is_err());
    assert!(!f.root.join("stolen").exists());
}

#[test]
fn witness_path_mapping_keeps_evidence_verbatim_and_ignores_transcripts() {
    let f = Fixture::new();
    fs::write(f.root.join("file.rs"), "fn main() {}\n").unwrap();
    git(&f.root, &["add", "file.rs"]);
    let secret_transcript = f._dir.path().join("host-transcript.jsonl");
    fs::write(&secret_transcript, "HOST_TRANSCRIPT_SENTINEL").unwrap();
    let mut writer = f.writer();
    writer
        .config
        .path_map
        .push(("/workspaces/demo".into(), f.root.canonicalize().unwrap()));
    let bytes = serde_json::to_vec(&serde_json::json!({"session_id":"test-session","hook_event_name":"PreToolUse","cwd":"/workspaces/demo","transcript_path":secret_transcript,"tool_name":"Read","tool_input":{"file_path":"/workspaces/demo/file.rs"}})).unwrap();
    writer
        .hook("claude-code", None, bytes.clone(), at())
        .unwrap();
    let events = writer.journal.read(&key()).unwrap().events;
    assert_eq!(events[0].payload.get().as_bytes(), bytes);
    let pipeline = crate::config::load_redaction(&f.root)
        .unwrap()
        .pipeline()
        .unwrap();
    let root = f.root.canonicalize().unwrap();
    let tracked = crate::checkpoint::tracked_files(&root);
    let ctx = minds_capture::Checkpoint {
        root: Some(&root),
        tracked: tracked.as_ref(),
        commit: None,
        redaction: Some(&pipeline),
    };
    let session =
        minds_capture::adapter::checkpoint_witness(&key(), &events, &ctx, &writer.config.path_map);
    let effect = session
        .turns
        .iter()
        .flat_map(|turn| &turn.tool_calls)
        .find_map(|call| call.effect.as_ref())
        .unwrap();
    assert_eq!(effect.path.as_deref(), Some("/workspaces/demo/file.rs"));
    assert!(effect.content.is_some());
}

fn session_dir(home: &Path) -> PathBuf {
    let mut dirs: Vec<_> = fs::read_dir(home.join("journal/claude-code"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(dirs.len(), 1);
    dirs.pop().unwrap()
}

fn expected_chain(writer: &Writer) -> ChainResult {
    chain::chain_salted(
        &writer.epochs.salt(&key()).unwrap(),
        &writer.journal.read(&key()).unwrap(),
    )
}

fn sealed_roots(f: &Fixture) -> Vec<minds_core::ContentHash> {
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    store
        .list_seals()
        .unwrap()
        .iter()
        .map(|id| {
            minds_core::evidence::Seal::parse(&store.seal_text(id).unwrap().unwrap())
                .unwrap()
                .root
        })
        .collect()
}

#[test]
fn witness_survives_crash_between_reserve_and_rename() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    // Genau das hinterlässt ein kill -9 zwischen `create_new` und `rename`.
    private_new(&session_dir(&f.home).join("0000000001.json")).unwrap();
    drop(writer);
    let mut writer = f.writer();
    append(&mut writer);
    append(&mut writer);
    drop(writer);
    // Ein zweiter Neustart nach Live-Events hinter dem Schaden.
    let mut writer = f.writer();
    let read = writer.journal.read(&key()).unwrap();
    assert_eq!(read.damaged.len(), 1);
    assert_eq!(read.gaps, vec![1]);
    let expected = expected_chain(&writer);
    writer.checkpoint_now(None).unwrap();
    assert_eq!(sealed_roots(&f), vec![expected.root]);
    assert!(writer.journal.read(&key()).unwrap().events.is_empty());
    drop(writer);
    f.writer();
}

#[test]
fn witness_live_chain_follows_real_gaps() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    for _ in 0..3 {
        append(&mut writer);
    }
    // Ein Sprung im Startwert erzeugt eine echte Lücke 3..=4.
    fs::write(session_dir(&f.home).join(".next"), "5").unwrap();
    append(&mut writer);
    assert_eq!(writer.journal.read(&key()).unwrap().gaps, vec![3, 4]);
    let expected = expected_chain(&writer);
    assert_eq!(writer.folders[&key()].snapshot(), expected);
    writer.checkpoint_now(None).unwrap();
    assert_eq!(sealed_roots(&f), vec![expected.root]);
}

fn assert_deferred(f: &Fixture, writer: &mut Writer) {
    let status = writer.checkpoint_now(None).unwrap();
    assert_eq!(
        status,
        "witness: nothing sealed, 1 session(s) deferred — they stay open, see the witness log"
    );
    assert!(sealed_roots(f).is_empty());
    assert!(!writer.journal.read(&key()).unwrap().events.is_empty());
    assert!(
        fs::read_to_string(f.home.join("log/witness.log"))
            .unwrap()
            .contains("integrity error")
    );
}

#[test]
fn witness_refuses_a_refilled_witnessed_event() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    for _ in 0..3 {
        append(&mut writer);
    }
    // Ein bezeugtes Event verschwindet, der Startwert zeigt auf seine Nummer:
    // Das nächste ehrliche Event füllt die Lücke und darf sie nicht waschen.
    let dir = session_dir(&f.home);
    fs::remove_file(dir.join("0000000001.json")).unwrap();
    fs::write(dir.join(".next"), "1").unwrap();
    append(&mut writer);
    assert_eq!(writer.journal.read(&key()).unwrap().events[1].seq, 1);
    assert_deferred(&f, &mut writer);
    // Der Schreiber läuft weiter, die Session bleibt vertagt.
    append(&mut writer);
    assert_deferred(&f, &mut writer);
    drop(writer);
    assert!(
        Writer::open(&f.home, load(&f.home).unwrap(), false).is_err(),
        "ein Neustart übernimmt die Manipulation nicht"
    );
}

#[test]
fn witness_refuses_a_witnessed_event_turned_into_damage() {
    for victim in ["0000000001.json", "0000000002.json"] {
        let f = Fixture::new();
        f.keygen();
        let mut writer = f.writer();
        for _ in 0..3 {
            append(&mut writer);
        }
        // Der aus dem Journal übernommene Damaged-Schwanz verdeckt nichts.
        fs::write(session_dir(&f.home).join(victim), b"garbage").unwrap();
        assert_eq!(writer.journal.read(&key()).unwrap().damaged.len(), 1);
        assert_deferred(&f, &mut writer);
    }
}

#[test]
fn witness_rebuilds_chains_after_a_failed_checkpoint() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    // Scheitert erst nach Seal und Discard: beim Trailern an einen signierten
    // Commit (der angefragte Commit steht an HEAD, die Vorprüfung besteht).
    let unsigned = git(&f.root, &["rev-parse", "HEAD"]);
    let signed = signed_head(&f);
    // Versiegelt ist dann schon — der fehlende Trailer steht in der
    // Statuszeile, nicht in einem Fehler (sonst meldete die Agent-Seite
    // „bleiben offen").
    let status = writer.checkpoint_now(Some(&signed)).unwrap();
    assert!(
        status.ends_with("Trailer not attached — the commit is signed; see `minds fsck`"),
        "{status}"
    );
    git(&f.root, &["update-ref", "HEAD", unsigned.trim()]);
    assert!(writer.journal.read(&key()).unwrap().events.is_empty());
    // Der Fold der versiegelten Epoche ist aus dem Speicher verschwunden.
    assert!(!writer.folders.contains_key(&key()));
    // Ein inhaltsgleiches Event ergäbe denselben Root und damit den
    // wiederverwendeten Seal; ein anderer Zeitpunkt erzwingt einen neuen.
    writer
        .hook(
            "claude-code",
            None,
            payload(),
            ("2026-10-04T10:00:01Z".into(), at().1 + 1),
        )
        .unwrap();
    assert_eq!(writer.folders[&key()].snapshot(), expected_chain(&writer));
    writer.checkpoint_now(None).unwrap();
    assert_eq!(sealed_roots(&f).len(), 2);
    assert!(writer.journal.read(&key()).unwrap().events.is_empty());
    drop(writer);
    f.writer();
}

/// API-Parität mit `adapter::checkpoint`. Der Checkpoint-Core übergibt beiden
/// absichtlich `commit: None`; die Commit-Verknüpfung entsteht dort über
/// Trailer und Index.
#[test]
fn witness_session_carries_the_commit_edge_like_legacy() {
    let f = Fixture::new();
    let mut writer = f.writer();
    append(&mut writer);
    let events = writer.journal.read(&key()).unwrap().events;
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let ctx = minds_capture::Checkpoint {
        root: Some(&f.root),
        tracked: None,
        commit: Some(head.trim()),
        redaction: None,
    };
    let witness = minds_capture::adapter::checkpoint_witness(&key(), &events, &ctx, &[]);
    let legacy = minds_capture::adapter::checkpoint(&key(), &events, &ctx);
    assert_eq!(
        serde_json::to_value(&witness.edges).unwrap(),
        serde_json::to_value(&legacy.edges).unwrap()
    );
    assert!(witness.edges.iter().any(|edge| {
        serde_json::to_value(edge).unwrap()
            == serde_json::to_value(minds_capture::edges::commit(head.trim())).unwrap()
    }));
}

#[test]
fn witness_processes_every_complete_frame_in_one_pass() {
    use minds_capture::witness_proto::{self, Frame};
    use std::os::unix::net::UnixStream;
    let f = Fixture::new();
    let mut writer = f.writer();
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    ours.set_nonblocking(true).unwrap();
    let frame = witness_proto::encode(&Frame::Hook {
        agent: "claude-code".into(),
        event_override: None,
        stdin: payload(),
    })
    .unwrap();
    theirs.write_all(&[frame.clone(), frame].concat()).unwrap();
    let mut client = socket::Client {
        stream: ours,
        bytes: Vec::new(),
        since: std::time::Instant::now(),
        eof: false,
        pending: false,
        control: false,
    };
    assert!(socket::step(&mut client, &mut writer).unwrap());
    assert!(client.bytes.is_empty());
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 2);
}

#[test]
fn witness_caps_frames_per_pass_without_dropping_any() {
    use minds_capture::witness_proto::{self, Frame};
    use std::os::unix::net::UnixStream;
    let f = Fixture::new();
    let mut writer = f.writer();
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    ours.set_nonblocking(true).unwrap();
    let frame = witness_proto::encode(&Frame::Hook {
        agent: "claude-code".into(),
        event_override: None,
        stdin: payload(),
    })
    .unwrap();
    theirs.write_all(&frame.repeat(20)).unwrap();
    let mut client = socket::Client {
        stream: ours,
        bytes: Vec::new(),
        since: std::time::Instant::now(),
        eof: false,
        pending: false,
        control: false,
    };
    assert!(socket::step(&mut client, &mut writer).unwrap());
    assert!(client.pending);
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 16);
    assert!(socket::step(&mut client, &mut writer).unwrap());
    assert!(!client.pending);
    assert!(client.bytes.is_empty());
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 20);
}

#[test]
fn witness_status_prints_one_ledger_entry_per_line() {
    let f = Fixture::new();
    f.keygen();
    private_append(&f.home.join("ledger"))
        .unwrap()
        .write_all(
            b"first witness/v1 2026-10-04T10:00:00Z\nsecond witness/v1 2026-10-04T10:00:01Z\n",
        )
        .unwrap();
    let text = status(&f.home).unwrap();
    assert!(
        text.ends_with(
            "Ledger:\nfirst witness/v1 2026-10-04T10:00:00Z\nsecond witness/v1 2026-10-04T10:00:01Z"
        ),
        "{text}"
    );
}

/// Die Statuszeile reist als `Ack` (EA-06d): eine Zeile, ohne Steuerzeichen,
/// höchstens 4 KiB — sonst lehnte schon `encode` sie ab, und die Agent-Seite
/// sähe statt der Zusammenfassung nur „witness unavailable".
#[test]
fn witness_checkpoint_status_is_one_ack_line() {
    use crate::checkpoint::delegate::LINE_SEPARATOR;
    use minds_capture::witness_proto::{self, Frame};

    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    let status = writer.checkpoint_now(Some(head.trim())).unwrap();

    let lines: Vec<_> = status.split(LINE_SEPARATOR).collect();
    assert_eq!(lines[0], "witness: 1 range(s) sealed", "{status}");
    assert!(lines[1].starts_with("SESSION SEALED b3-"), "{status}");
    assert!(lines[1].contains("· scope witness/v1 ·"), "{status}");
    assert!(lines[1].ends_with("· signed"), "{status}");
    let retrofitted = git(&f.root, &["rev-parse", "HEAD"]);
    assert_eq!(
        lines[2],
        format!("Trailer retrofitted to {}", retrofitted.trim())
    );
    assert!(
        !status.contains("Implement sorting") && !status.contains("/workspaces"),
        "kein Payload, kein Pfad: {status}"
    );
    witness_proto::encode(&Frame::Ack {
        request_id: [0; 16],
        status,
    })
    .unwrap();

    // Nichts offen: eine Zeile, kein Trailer.
    let status = writer.checkpoint_now(None).unwrap();
    assert_eq!(status, "witness: nothing to seal");

    // Viele Seals: gekappt, mit Auslassungszeile, und weiterhin ein gültiger Ack.
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    let id = store.list_seals().unwrap()[0].clone();
    let seal = minds_core::evidence::Seal::parse(&store.seal_text(&id).unwrap().unwrap()).unwrap();
    let many = vec![minds_core::evidence::SealSummary::new(id, seal, true); 100];
    let status = checkpoint_status(&many, 0, Ok(None));
    assert!(status.len() <= 4 * 1024, "{}", status.len());
    assert!(
        status.contains(" | … 83 more — see `minds witness status` | "),
        "{status}"
    );
    // Gespeichert, aber ohne Trailer: das sagt die letzte Zeile.
    assert!(
        status.ends_with("Trailer not attached — HEAD moved; see `minds fsck`"),
        "{status}"
    );
    witness_proto::encode(&Frame::Ack {
        request_id: [0; 16],
        status,
    })
    .unwrap();
}

/// Ersetzt HEAD durch einen Commit mit `gpgsig`-Kopf — denselben Baum, nur
/// signiert. Minds trailert so einen Commit nie (`SignedCommit`).
fn signed_head(f: &Fixture) -> String {
    let tree = git(&f.root, &["rev-parse", "HEAD^{tree}"]);
    let object = format!(
        "tree {}\n\
         author W <w@example.invalid> 1704067200 +0000\n\
         committer W <w@example.invalid> 1704067200 +0000\n\
         gpgsig -----BEGIN PGP SIGNATURE-----\n\
         \x20nicht echt\n\
         \x20-----END PGP SIGNATURE-----\n\
         \n\
         signiert\n",
        tree.trim()
    );
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&f.root)
        .args(["hash-object", "-t", "commit", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(object.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let id = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    git(&f.root, &["update-ref", "HEAD", &id]);
    id
}

/// Alle Objekte des Repos, entpackt — für die Frage „steht das irgendwo?".
fn all_objects(f: &Fixture) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(&f.root)
        .args(["cat-file", "--batch-all-objects", "--batch"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Vor dem Versiegeln geprüft (EA-06d): Ein Commit, der nicht an HEAD steht,
/// schließt keine Session — sonst entstünden Seals ohne Trailer, während die
/// Agent-Seite „bleiben offen" meldet.
#[test]
fn witness_refuses_a_commit_that_is_not_at_head_before_sealing() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let old = git(&f.root, &["rev-parse", "HEAD"]);
    git(&f.root, &["commit", "-qm", "weiter", "--allow-empty"]);

    let err = writer.checkpoint_now(Some(old.trim())).unwrap_err();
    assert!(err.to_string().contains("not at the witness HEAD"), "{err}");
    assert!(sealed_roots(&f).is_empty());
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);

    // Unparsbar ebenso — vor dem ersten Seal.
    writer.checkpoint_now(Some("not-a-commit")).unwrap_err();
    assert!(sealed_roots(&f).is_empty());

    // Der richtige Commit versiegelt und trailert.
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let status = writer.checkpoint_now(Some(head.trim())).unwrap();
    assert!(status.contains("Trailer retrofitted to"), "{status}");
    assert_eq!(sealed_roots(&f).len(), 1);
}

/// Ein `.git`, das als gitfile woandershin zeigt, lenkt Store und Trailer
/// nicht in ein fremdes Repo.
#[test]
fn witness_refuses_a_redirected_git_dir() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let elsewhere = f._dir.path().join("elsewhere.git");
    fs::rename(f.root.join(".git"), &elsewhere).unwrap();
    fs::write(
        f.root.join(".git"),
        format!("gitdir: {}\n", elsewhere.display()),
    )
    .unwrap();

    let err = writer.checkpoint_now(None).unwrap_err();
    assert!(err.to_string().contains("checkpoint deferred"), "{err}");
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);
}

/// Auch ein echtes `.git` lenkt um, wenn darin `commondir`, `alternates`
/// oder ein Symlink auf fremde Refs steht — oder wenn eine Datei, die gix
/// liest, ein FIFO ist. Jeder Fall: abgelehnt, bevor etwas gelesen oder
/// geschrieben wird.
#[test]
fn witness_refuses_a_git_dir_that_points_elsewhere() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let elsewhere = f._dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let dot_git = f.root.join(".git");
    let fifo = |path: &Path| {
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: nul-terminierter Pfad, keine weiteren Vorbedingungen.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    };

    type Plant = Box<dyn Fn()>;
    type Undo = Box<dyn Fn()>;
    let cases: Vec<(&str, Plant, Undo)> = vec![
        (
            "commondir",
            Box::new({
                let p = dot_git.join("commondir");
                let e = elsewhere.clone();
                move || fs::write(&p, format!("{}\n", e.display())).unwrap()
            }),
            Box::new({
                let p = dot_git.join("commondir");
                move || fs::remove_file(&p).unwrap()
            }),
        ),
        (
            "alternates",
            Box::new({
                let p = dot_git.join("objects/info");
                let e = elsewhere.clone();
                move || {
                    fs::create_dir_all(&p).unwrap();
                    fs::write(p.join("alternates"), format!("{}\n", e.display())).unwrap();
                }
            }),
            Box::new({
                let p = dot_git.join("objects/info/alternates");
                move || fs::remove_file(&p).unwrap()
            }),
        ),
        (
            "symlinked refs",
            Box::new({
                let d = dot_git.clone();
                let e = elsewhere.clone();
                move || {
                    fs::rename(d.join("refs"), d.join("refs.real")).unwrap();
                    std::os::unix::fs::symlink(&e, d.join("refs")).unwrap();
                }
            }),
            Box::new({
                let d = dot_git.clone();
                move || {
                    fs::remove_file(d.join("refs")).unwrap();
                    fs::rename(d.join("refs.real"), d.join("refs")).unwrap();
                }
            }),
        ),
        (
            "fifo index",
            Box::new({
                let p = dot_git.join("index");
                move || {
                    let _ = fs::remove_file(&p);
                    fifo(&p);
                }
            }),
            Box::new({
                let p = dot_git.join("index");
                move || fs::remove_file(&p).unwrap()
            }),
        ),
    ];
    let branch = dot_git.join(git(&f.root, &["symbolic-ref", "HEAD"]).trim());
    let cases = cases
        .into_iter()
        .chain([
            (
                "fifo branch ref",
                Box::new({
                    let b = branch.clone();
                    move || {
                        fs::rename(&b, b.with_extension("real")).unwrap();
                        fifo(&b);
                    }
                }) as Plant,
                Box::new({
                    let b = branch.clone();
                    move || {
                        fs::remove_file(&b).unwrap();
                        fs::rename(b.with_extension("real"), &b).unwrap();
                    }
                }) as Undo,
            ),
            (
                "symlinked reflog",
                Box::new({
                    let d = dot_git.clone();
                    let e = elsewhere.clone();
                    move || {
                        fs::create_dir_all(d.join("logs")).unwrap();
                        let _ = fs::remove_file(d.join("logs/HEAD"));
                        fs::write(e.join("victim"), "").unwrap();
                        std::os::unix::fs::symlink(e.join("victim"), d.join("logs/HEAD")).unwrap();
                    }
                }) as Plant,
                Box::new({
                    let d = dot_git.clone();
                    move || fs::remove_file(d.join("logs/HEAD")).unwrap()
                }) as Undo,
            ),
            (
                "hard-linked reflog",
                Box::new({
                    let d = dot_git.clone();
                    let e = elsewhere.clone();
                    move || {
                        fs::write(e.join("victim2"), "").unwrap();
                        fs::hard_link(e.join("victim2"), d.join("logs/HEAD")).unwrap();
                    }
                }) as Plant,
                Box::new({
                    let d = dot_git.clone();
                    move || fs::remove_file(d.join("logs/HEAD")).unwrap()
                }) as Undo,
            ),
        ])
        .collect::<Vec<_>>();
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    for (name, plant, undo) in cases {
        plant();
        let started = std::time::Instant::now();
        let err = writer.checkpoint_now(Some(head.trim())).unwrap_err();
        assert!(
            err.to_string().contains("checkpoint deferred"),
            "{name}: {err}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{name}"
        );
        assert_eq!(
            writer.journal.read(&key()).unwrap().events.len(),
            1,
            "{name}"
        );
        undo();
    }
    assert!(sealed_roots(&f).is_empty());
    assert_eq!(fs::read_to_string(elsewhere.join("victim")).unwrap(), "");
    assert_eq!(fs::read_to_string(elsewhere.join("victim2")).unwrap(), "");
    // Wieder schlicht: Der Lauf geht durch.
    writer.checkpoint_now(Some(head.trim())).unwrap();
    assert_eq!(sealed_roots(&f).len(), 1);
}

/// Die Policy-Datei im Worktree gehört dem Agenten; seit EA-10 liest der
/// Witness sie gar nicht mehr. Ein FIFO oder `/dev/zero` an ihrer Stelle hält
/// ihn nicht an und vertagt auch nichts.
#[test]
fn witness_ignores_the_worktree_policy_file() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    fs::create_dir_all(f.root.join(".minds")).unwrap();
    let policy = f.root.join(".minds/redact.json");
    let name = std::ffi::CString::new(policy.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: nul-terminierter Pfad, keine weiteren Vorbedingungen.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    let started = std::time::Instant::now();
    let status = writer.checkpoint_now(Some(head.trim())).unwrap();
    assert!(status.starts_with("witness: 1 range(s) sealed"), "{status}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(sealed_roots(&f).len(), 1);
}

/// EA-10: Die Policy hält `init` fest. Was danach im Worktree steht — etwa
/// ein Deny-Begriff, der jeden Prompt schwärzte —, erreicht bezeugte
/// Sessions nicht; was beim Einrichten dastand, gilt.
#[test]
fn witness_policy_is_pinned_at_init() {
    let f = Fixture::new();
    f.keygen();
    fs::create_dir_all(f.root.join(".minds")).unwrap();
    fs::write(
        f.root.join(".minds/redact.json"),
        r#"{"deny_secrets":["Projekt-Kranich"]}"#,
    )
    .unwrap();
    // Festgehalten wird, was committet ist — nicht der Worktree.
    git(&f.root, &["add", ".minds/redact.json"]);
    git(&f.root, &["commit", "-qm", "team policy"]);
    let home = f._dir.path().join("pinned-home");
    init(
        &home,
        &InitRequest {
            repo: f.root.to_str().unwrap(),
            mapping: None,
            profile: None,
            socket_group: None,
            child_repo: None,
            policy_rev: None,
        },
    )
    .unwrap();
    super::super::keygen(&home).unwrap();
    // Nach dem Einrichten schreibt der Agent einen Begriff, der alles träfe.
    fs::write(
        f.root.join(".minds/redact.json"),
        r#"{"deny_secrets":["sorting"]}"#,
    )
    .unwrap();
    let mut writer = Writer::open(&home, load(&home).unwrap(), false).unwrap();
    let payload = br#"{"session_id":"test-session","hook_event_name":"UserPromptSubmit","prompt":"Implement sorting for Projekt-Kranich"}"#;
    writer
        .hook("claude-code", None, payload.to_vec(), at())
        .unwrap();
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    let status = writer.checkpoint_now(Some(head.trim())).unwrap();
    assert!(status.starts_with("witness: 1 range(s) sealed"), "{status}");
    // Der Begriff steht selbst im committeten Policy-Blob; geprüft wird die
    // Stelle in der Session.
    let objects = all_objects(&f);
    assert!(
        objects.contains("Implement sorting for [redacted:secret]"),
        "Prompt lesbar, Begriff von init geschwärzt"
    );
    assert!(!objects.contains("sorting for Projekt-Kranich"));
}

/// EA-10: Backend und Ref hält `init` fest. Ein `minds.childPath`, das der
/// Agent danach in `.git/config` schreibt, lenkt den Witness nicht um.
#[test]
fn witness_store_is_pinned_at_init() {
    let f = Fixture::new();
    f.keygen();
    let elsewhere = f._dir.path().join("elsewhere");
    git(f._dir.path(), &["init", "-q", "--bare", "elsewhere"]);
    git(&f.root, &["config", "minds.backend", "child-repo"]);
    git(
        &f.root,
        &["config", "minds.childPath", elsewhere.to_str().unwrap()],
    );
    git(&f.root, &["config", "minds.contextRef", "refs/heads/main"]);
    let mut writer = f.writer();
    append(&mut writer);
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    let status = writer.checkpoint_now(Some(head.trim())).unwrap();
    assert!(status.starts_with("witness: 1 range(s) sealed"), "{status}");
    assert_eq!(sealed_roots(&f).len(), 1, "im festgehaltenen In-Repo-Store");
    let refs = git(&elsewhere, &["for-each-ref"]);
    assert!(refs.trim().is_empty(), "nichts im fremden Repo: {refs}");
}

/// EA-10: Ein `include.path` in `.git/config` auf ein FIFO hält den
/// Checkpoint nicht an — der Witness öffnet das festgehaltene Verzeichnis
/// ohne Includes und liest den Index selbst statt über `git ls-files`.
#[test]
fn witness_ignores_config_includes() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let fifo = f._dir.path().join("include.fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: nul-terminierter Pfad, keine weiteren Vorbedingungen.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let config = f.root.join(".git/config");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!("[include]\n\tpath = {}\n", fifo.display()));
    fs::write(&config, text).unwrap();

    let started = std::time::Instant::now();
    let status = writer.checkpoint_now(Some(head.trim())).unwrap();
    assert!(status.starts_with("witness: 1 range(s) sealed"), "{status}");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

/// `init` ist idempotent und überschreibt nie eine andere Konfiguration.
#[test]
fn witness_init_is_idempotent_and_never_overwrites() {
    let f = Fixture::new();
    let request = |profile| InitRequest {
        repo: f.root.to_str().unwrap(),
        mapping: None,
        profile,
        socket_group: None,
        child_repo: None,
        policy_rev: None,
    };
    let before = fs::read(f.home.join("witness.json")).unwrap();
    // Auch „unverändert" nennt die Pins, die gelten.
    let Initialized::Unchanged(pins) = init_config(&f.home, &request(None)).unwrap() else {
        panic!("expected unchanged");
    };
    assert!(pins.store.starts_with("in-repo"), "{pins:?}");
    assert!(matches!(
        init_config(&f.home, &request(Some("user"))).unwrap(),
        Initialized::Unchanged(_)
    ));
    let err = init_config(&f.home, &request(Some("managed"))).unwrap_err();
    assert!(err.to_string().contains("refusing to overwrite"), "{err}");
    assert_eq!(fs::read(f.home.join("witness.json")).unwrap(), before);
    let config = load(&f.home).unwrap();
    assert!(config.pinned());
    assert_eq!(
        config.git_dir(),
        f.root.join(".git").canonicalize().unwrap()
    );
    // Container ohne Pfadabbildung, Pfadabbildung ohne Container: abgelehnt.
    let err = init_config(&f.home, &request(Some("container"))).unwrap_err();
    assert!(err.to_string().contains("requires --path-map"), "{err}");
}

/// Tolerant lesen: Ein `witness.json` von vor EA-10 (Schema 1, ohne Pins)
/// lädt weiter — mit dem In-Repo-Store, dem Standard-Ref und der strengen
/// Standard-Policy.
#[test]
fn witness_loads_a_schema_1_config_with_defaults() {
    let f = Fixture::new();
    let path = f.home.join("witness.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let object = value.as_object_mut().unwrap();
    object.insert("schema_version".into(), 1.into());
    for pin in ["git_dir", "store", "redaction"] {
        object.remove(pin);
    }
    atomic(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
    let config = load(&f.home).unwrap();
    assert!(!config.pinned());
    assert_eq!(config.store(), PinnedStore::default_in_repo());
    assert_eq!(
        config.git_dir(),
        f.root.canonicalize().unwrap().join(".git")
    );

    // Fail-closed: Ohne Pins versiegelt der Witness nichts — die Extras des
    // Teams fielen sonst still weg.
    f.keygen();
    let mut writer = Writer::open(&f.home, load(&f.home).unwrap(), false).unwrap();
    append(&mut writer);
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let err = writer.checkpoint_now(Some(head.trim())).unwrap_err();
    assert!(err.to_string().contains("no pins (schema 1)"), "{err}");
    assert!(sealed_roots(&f).is_empty());

    // Ein erneutes `init` mit denselben Argumenten ergänzt die Pins.
    let repinned = init_config(
        &f.home,
        &InitRequest {
            repo: f.root.to_str().unwrap(),
            mapping: None,
            profile: None,
            socket_group: None,
            child_repo: None,
            policy_rev: None,
        },
    )
    .unwrap();
    assert!(matches!(repinned, Initialized::Pinned(_)), "{repinned:?}");
    assert!(load(&f.home).unwrap().pinned());

    // Ein Pin, der aus dem Repo herausführt, lädt nicht.
    object_with(&path, "git_dir", "/etc".into());
    assert!(load(&f.home).is_err());
}

fn object_with(path: &Path, key: &str, value: serde_json::Value) {
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    config
        .as_object_mut()
        .unwrap()
        .insert(key.to_owned(), value);
    atomic(path, &serde_json::to_vec(&config).unwrap()).unwrap();
}

/// MUST_REDACT unter feindlicher Policy: Eine vom Agenten geschriebene
/// `.minds/redact.json` schaltet beim Witness nichts ab.
#[test]
fn witness_redaction_ignores_a_weakened_repo_policy() {
    const TOKEN: &str = "ghp_1234567890abcdefghijklmnopqrstuvwxyzAB";
    let f = Fixture::new();
    f.keygen();
    fs::create_dir_all(f.root.join(".minds")).unwrap();
    fs::write(
        f.root.join(".minds/redact.json"),
        format!(
            r#"{{"known_tokens":false,"email":false,"keyed_values":false,"url_credentials":false,
                "short_flags":false,"high_entropy":{{"enabled":false}},"allow":["{TOKEN}"]}}"#
        ),
    )
    .unwrap();
    let mut writer = f.writer();
    let payload = format!(
        r#"{{"session_id":"test-session","hook_event_name":"UserPromptSubmit","prompt":"export GITHUB_TOKEN={TOKEN}"}}"#
    );
    writer
        .hook("claude-code", None, payload.into_bytes(), at())
        .unwrap();

    let status = writer.checkpoint_now(None).unwrap();
    assert!(status.starts_with("witness: 1 range(s) sealed"), "{status}");
    assert!(!status.contains(TOKEN));
    assert!(
        !all_objects(&f).contains(TOKEN),
        "der Token steht unredigiert im Store"
    );
}

/// Schutz der Eventloop: Ohne Commit keine Anfrage; ohne neues Event kein
/// Lauf; Läufe je Client (Commit) — auch gescheiterte — nicht schneller als
/// `MIN_CHECKPOINT_INTERVAL` (EA-10: je Client, nicht global).
#[test]
fn witness_coalesces_and_rate_limits_checkpoint_requests() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let head = head.trim();
    let backdate = |writer: &mut Writer| writer.limits.backdate(MIN_CHECKPOINT_INTERVAL);
    let wrong = "0".repeat(40);

    assert_eq!(
        writer.checkpoint_requested(None, &|| true),
        Err("commit required")
    );

    // Ein gescheiterter Lauf sperrt alle Clients für das Intervall: Sonst
    // brächte jeder erfundene Commit einen eigenen Lauf …
    assert_eq!(
        writer.checkpoint_requested(Some(&wrong), &|| true),
        Err("checkpoint failed")
    );
    for commit in [wrong.as_str(), head, &"1".repeat(40)] {
        assert_eq!(
            writer.checkpoint_requested(Some(commit), &|| true),
            Err("rate limited")
        );
    }
    assert!(sealed_roots(&f).is_empty());
    // … danach läuft der richtige Commit.
    backdate(&mut writer);
    let first = writer.checkpoint_requested(Some(head), &|| true).unwrap();
    assert!(first.starts_with("witness: 1 range(s) sealed"), "{first}");
    let retrofitted = git(&f.root, &["rev-parse", "HEAD"]);
    let retrofitted = retrofitted.trim();
    assert_ne!(retrofitted, head, "Trailer nachgerüstet");
    // Nichts Neues: sofort, ohne Lauf, auch innerhalb der Frist.
    for _ in 0..10 {
        assert_eq!(
            writer
                .checkpoint_requested(Some(retrofitted), &|| true)
                .as_deref(),
            Ok("witness: nothing to seal")
        );
    }
    // Neues Event, aber zu früh: Der nachgerüstete Commit ist derselbe
    // Client wie der angefragte — abgelehnt, die Session bleibt offen.
    writer
        .hook(
            "claude-code",
            None,
            payload(),
            ("2026-10-04T10:00:01Z".into(), at().1 + 1),
        )
        .unwrap();
    for commit in [head, retrofitted] {
        assert_eq!(
            writer.checkpoint_requested(Some(commit), &|| true),
            Err("rate limited")
        );
    }
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);
    assert_eq!(sealed_roots(&f).len(), 1);

    backdate(&mut writer);
    let later = writer
        .checkpoint_requested(Some(retrofitted), &|| true)
        .unwrap();
    assert!(later.starts_with("witness: 1 range(s) sealed"), "{later}");
    assert_eq!(sealed_roots(&f).len(), 2);
    // Abgewiesene Anfragen füllen das eigene Log nicht.
    let log = fs::read_to_string(f.home.join("log/witness.log")).unwrap_or_default();
    assert_eq!(log.matches("checkpoint failed").count(), 1, "{log}");
}

/// EA-10: Ein Agent, der im Sekundentakt Checkpoints anfordert, lässt den
/// post-commit-Hook des Menschen für seinen frischen Commit nicht
/// `rate limited` sehen.
#[test]
fn rate_limit_is_per_client() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let agent_head = git(&f.root, &["rev-parse", "HEAD"]);
    let first = writer
        .checkpoint_requested(Some(agent_head.trim()), &|| true)
        .unwrap();
    assert!(first.starts_with("witness: 1 range(s) sealed"), "{first}");
    writer
        .hook(
            "claude-code",
            None,
            payload(),
            ("2026-10-04T10:00:01Z".into(), at().1 + 1),
        )
        .unwrap();
    assert_eq!(
        writer.checkpoint_requested(Some(agent_head.trim()), &|| true),
        Err("rate limited")
    );

    // Der Mensch committet; sein Hook fragt sofort für den neuen Commit.
    git(&f.root, &["commit", "-qm", "human", "--allow-empty"]);
    let human_head = git(&f.root, &["rev-parse", "HEAD"]);
    let status = writer
        .checkpoint_requested(Some(human_head.trim()), &|| true)
        .unwrap();
    assert!(status.starts_with("witness: 1 range(s) sealed"), "{status}");
}

/// Ist die Agent-Seite gegangen (Frist abgelaufen), wird versiegelt, aber
/// nicht mehr getrailert: Ihr `git commit` ist zurück, ein später Amend
/// schriebe Historie um, die vielleicht schon gepusht ist.
#[test]
fn witness_does_not_amend_for_a_requester_that_is_gone() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    // Schon vor dem Lauf gegangen: kein Lauf, nichts versiegelt, keine Frist.
    assert_eq!(
        writer.checkpoint_requested(Some(head.trim()), &|| false),
        Err("requester gone")
    );
    assert!(sealed_roots(&f).is_empty());
    assert!(!writer.limits.blocked(head.trim(), MIN_CHECKPOINT_INTERVAL));
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);
    let log = fs::read_to_string(f.home.join("log/witness.log")).unwrap_or_default();
    assert!(!log.contains("checkpoint"), "keine Logzeile: {log}");

    // Erst während des Laufs gegangen: versiegelt, aber nicht getrailert.
    let asked = std::cell::Cell::new(0);
    let status = writer
        .checkpoint_requested(Some(head.trim()), &|| {
            asked.set(asked.get() + 1);
            asked.get() == 1
        })
        .unwrap();

    assert!(status.starts_with("witness: 1 range(s) sealed"), "{status}");
    assert!(
        status.ends_with("Trailer not attached — the requester is gone; see `minds fsck`"),
        "{status}"
    );
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), head, "HEAD unberührt");
    assert_eq!(sealed_roots(&f).len(), 1);
}

/// Üblich und harmlos: ein Hook als Symlink, der Socket des fsmonitor-Daemons.
/// Beides liest oder schreibt der Witness nie; es darf ihn nicht blockieren.
#[test]
fn witness_accepts_hook_symlinks_and_the_fsmonitor_socket() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let dot_git = f.root.join(".git");
    fs::create_dir_all(dot_git.join("hooks")).unwrap();
    std::os::unix::fs::symlink("../../scripts/pre-commit", dot_git.join("hooks/pre-commit"))
        .unwrap();
    let _ipc =
        std::os::unix::net::UnixListener::bind(dot_git.join("fsmonitor--daemon.ipc")).unwrap();
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    let status = writer.checkpoint_now(Some(head.trim())).unwrap();
    assert!(status.starts_with("witness: 1 range(s) sealed"), "{status}");
}

/// Ein `.git` mit mehr Einträgen als die Grenze wird vertagt, nicht
/// durchlaufen — und die Meldung nennt den Grund, nicht fremden Text.
#[test]
fn witness_defers_a_git_dir_beyond_the_entry_limit() {
    let f = Fixture::new();
    let junk = f.root.join(".git/junk");
    fs::create_dir_all(&junk).unwrap();
    for i in 0..50 {
        fs::write(junk.join(format!("f{i}")), "").unwrap();
    }
    let err = plain_repo_layout_within(&f.root, 40).unwrap_err();
    assert!(err.to_string().contains("too many entries"), "{err}");
    plain_repo_layout_within(&f.root, 10_000).unwrap();

    std::os::unix::fs::symlink("/etc/passwd", junk.join("evil")).unwrap();
    let err = plain_repo_layout_within(&f.root, 10_000).unwrap_err();
    assert!(err.to_string().contains("junk/evil"), "{err}");
}

/// Ein `.minds`, das als Symlink an einen fremden Ort zeigt: Der laufende
/// Witness liest es nicht (EA-10), und `init` hält daraus keine Policy fest.
#[test]
fn witness_ignores_a_symlinked_policy_directory() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let elsewhere = f._dir.path().join("policy");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("redact.json"), "{}").unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.root.join(".minds")).unwrap();
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    let status = writer.checkpoint_now(Some(head.trim())).unwrap();
    assert!(status.starts_with("witness: 1 range(s) sealed"), "{status}");

    // `init` liest die Policy aus HEAD; der Symlink im Worktree spielt
    // keine Rolle — ohne committete Policy gilt der strenge Default.
    let initialized = init_config(
        &f._dir.path().join("second-home"),
        &InitRequest {
            repo: f.root.to_str().unwrap(),
            mapping: None,
            profile: None,
            socket_group: None,
            child_repo: None,
            policy_rev: None,
        },
    )
    .unwrap();
    let Initialized::Created(pins) = initialized else {
        panic!("expected a new home");
    };
    assert!(pins.policy.starts_with("strict default"), "{pins:?}");
}

/// EA-10: Eine Policy, die ihre eigenen Platzhalter träfe, macht jede
/// redigierte Session instabil — `init` hält sie nicht fest.
#[test]
fn witness_init_refuses_an_unstable_policy() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join(".minds")).unwrap();
    fs::write(
        f.root.join(".minds/redact.json"),
        r#"{"secret_keys":["redacted"]}"#,
    )
    .unwrap();
    git(&f.root, &["add", ".minds/redact.json"]);
    git(&f.root, &["commit", "-qm", "hostile policy"]);
    let err = init_config(
        &f._dir.path().join("unstable-home"),
        &InitRequest {
            repo: f.root.to_str().unwrap(),
            mapping: None,
            profile: None,
            socket_group: None,
            child_repo: None,
            policy_rev: None,
        },
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("matches its own placeholders"),
        "{err}"
    );
    assert!(!f._dir.path().join("unstable-home").exists());
}

/// EA-10: Ein Child-Repo, das nur die `.git/config` nennt (der Agent kann
/// sie schreiben), hält `init` nicht still fest — nur ausdrücklich genannt.
#[test]
fn witness_init_pins_a_child_repo_only_when_named() {
    let f = Fixture::new();
    git(f._dir.path(), &["init", "-q", "--bare", "context"]);
    let context = f._dir.path().join("context");
    git(&f.root, &["config", "minds.backend", "child-repo"]);
    git(
        &f.root,
        &["config", "minds.childPath", context.to_str().unwrap()],
    );
    let request = |child_repo| InitRequest {
        repo: f.root.to_str().unwrap(),
        mapping: None,
        profile: None,
        socket_group: None,
        child_repo,
        policy_rev: None,
    };
    let err = init_config(&f._dir.path().join("h1"), &request(None)).unwrap_err();
    assert!(err.to_string().contains("pass --child-repo"), "{err}");

    let Initialized::Created(pins) =
        init_config(&f._dir.path().join("h2"), &request(Some(&context))).unwrap()
    else {
        panic!("expected a new home");
    };
    assert!(pins.store.starts_with("child repo"), "{pins:?}");
    // Ein Child-Repo im beobachteten Repo wird abgelehnt.
    let inside = f.root.join("nested");
    git(&f.root, &["init", "-q", "nested"]);
    let err = init_config(&f._dir.path().join("h3"), &request(Some(&inside))).unwrap_err();
    assert!(err.to_string().contains("outside the observed"), "{err}");
}

/// Derselbe Fehlschlag Sekunde um Sekunde erzeugt eine Logzeile, nicht
/// tausend — und beim nächsten anderen Ausgang die Zahl der Wiederholungen.
#[test]
fn witness_logs_a_repeated_failure_once() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let wrong = "0".repeat(40);
    for _ in 0..5 {
        writer.limits = Default::default();
        assert_eq!(
            writer.checkpoint_requested(Some(&wrong), &|| true),
            Err("checkpoint failed")
        );
    }
    let log = || fs::read_to_string(f.home.join("log/witness.log")).unwrap_or_default();
    assert_eq!(log().matches("checkpoint failed").count(), 1, "{}", log());

    writer.limits = Default::default();
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    writer
        .checkpoint_requested(Some(head.trim()), &|| true)
        .unwrap();
    assert!(
        log().contains("previous checkpoint failure repeated 4 more time(s)"),
        "{}",
        log()
    );
}

/// Ein Ref-Namespace aus der Konfiguration des Agenten verschöbe jeden
/// Schreibzugriff unter `refs/namespaces/<ns>/…`: vertagt, bevor etwas
/// versiegelt wird.
#[test]
fn witness_refuses_a_refs_namespace() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    git(&f.root, &["config", "gitoxide.core.refsNamespace", "agent"]);

    let err = writer.checkpoint_now(Some(head.trim())).unwrap_err();
    assert!(
        err.to_string()
            .contains("refs namespaces are not supported"),
        "{err}"
    );
    assert!(sealed_roots(&f).is_empty());
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);
}

/// EA-10: Jede Zeile von `witness.log` wird dedupliziert — auch
/// abwechselnde Wiederholungen; die Zahl kommt mit der nächsten Ausgabe nach
/// Ablauf des Fensters oder beim Beenden.
#[test]
fn witness_log_deduplicates_repeated_lines() {
    let home = Path::new("/witness-home");
    let mut dedup = LogDedup::default();
    let mut written = Vec::new();
    for _ in 0..5 {
        written.extend(dedup.admit(home, "accept failed"));
        written.extend(dedup.admit(home, "frame dropped"));
    }
    assert_eq!(written, ["accept failed", "frame dropped"]);
    for entry in dedup.homes.values_mut().flatten() {
        entry.since -= DEDUP_WINDOW;
    }
    assert_eq!(
        dedup.admit(home, "accept failed"),
        [
            "previous line repeated 4 more time(s): accept failed",
            "accept failed"
        ]
    );
    // Andere Homes zählen getrennt.
    assert_eq!(
        dedup.admit(Path::new("/other"), "frame dropped"),
        ["frame dropped"]
    );

    // Beim Beenden kommt der Rest nach, in die Datei des eigenen Homes.
    let f = Fixture::new();
    let mut dedup = LogDedup::default();
    for _ in 0..3 {
        for line in dedup.admit(&f.home, "flood") {
            write_log(&f.home, &line);
        }
    }
    for line in dedup.flush(&f.home) {
        write_log(&f.home, &line);
    }
    let log = fs::read_to_string(f.home.join("log/witness.log")).unwrap();
    assert_eq!(log.matches("flood").count(), 2, "{log}");
    assert!(
        log.contains("previous line repeated 2 more time(s): flood"),
        "{log}"
    );
}

/// EA-10: Der Worker übernimmt die persistierten Live-Folds, statt sie aus
/// dem Journal neu abzuleiten — ein am Witness vorbei angehängter
/// Journal-Schwanz fällt deshalb auch im Worker auf: vertagt, nichts
/// versiegelt.
#[test]
fn worker_writer_detects_a_journal_tail_appended_behind_the_witness() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let mut forged =
        hook_event::parse(payload(), "claude-code", None, (at().0, at().1 + 5)).unwrap();
    secretwall::guard(&mut forged.event);
    Journal::at(f.home.join("journal"))
        .append(&forged.key, forged.event)
        .unwrap();
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    let worker = Writer::for_worker(&f.home, load(&f.home).unwrap()).unwrap();
    let ran = worker
        .checkpoint_sessions(Some(head.trim()), &|| true)
        .unwrap();
    assert!(
        ran.status.contains("1 session(s) deferred"),
        "{}",
        ran.status
    );
    assert!(sealed_roots(&f).is_empty());
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 2);
}

/// Ohne persistierten Fold weiß der Worker nicht, was der Witness gefaltet
/// hat — die Session wird vertagt, nicht aus dem Journal gewaschen.
#[test]
fn worker_writer_defers_a_session_without_a_persisted_fold() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    fs::remove_file(folder_path(&f.home, &key())).unwrap();
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    let worker = Writer::for_worker(&f.home, load(&f.home).unwrap()).unwrap();
    let ran = worker
        .checkpoint_sessions(Some(head.trim()), &|| true)
        .unwrap();
    assert!(
        ran.status.contains("1 session(s) deferred"),
        "{}",
        ran.status
    );
    assert!(sealed_roots(&f).is_empty());
}

/// Nach einem abgebrochenen Worker-Lauf (das Kind kann schon versiegelt
/// und verworfen haben): versiegelte Sessions fallen aus den Live-Folds,
/// `dirty` bleibt gesetzt, und der nächste Append lädt sauber neu.
#[test]
fn finish_after_an_aborted_run_keeps_the_writer_consistent() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    // Wie ein Kind, das versiegelt hat und dann starb.
    let worker = Writer::for_worker(&f.home, load(&f.home).unwrap()).unwrap();
    worker
        .checkpoint_sessions(Some(head.trim()), &|| false)
        .unwrap();
    assert_eq!(sealed_roots(&f).len(), 1);

    writer.finish_checkpoint(false);
    assert!(writer.dirty, "ein gescheiterter Lauf lässt dirty stehen");
    assert!(
        !writer.folders.contains_key(&key()),
        "verworfene Session raus"
    );
    writer
        .hook(
            "claude-code",
            None,
            payload(),
            ("2026-10-04T10:00:09Z".into(), at().1 + 9),
        )
        .unwrap();
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);
}

/// Ein ausdrücklich genannter Store, der nicht der festgehaltene ist, geht
/// nicht still als „unverändert" durch.
#[test]
fn witness_init_names_a_differing_store() {
    let f = Fixture::new();
    git(f._dir.path(), &["init", "-q", "--bare", "context"]);
    let context = f._dir.path().join("context");
    let err = init_config(
        &f.home,
        &InitRequest {
            repo: f.root.to_str().unwrap(),
            mapping: None,
            profile: None,
            socket_group: None,
            child_repo: Some(&context),
            policy_rev: None,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("(store differ)"), "{err}");
}

/// Die Policy kommt aus dem Commit, den der Mensch nennt — etwa dem
/// geprüften Stand —, auch wenn HEAD inzwischen etwas anderes trägt. Die
/// Quelle nennt Commit und Blob.
#[test]
fn witness_init_reads_the_policy_from_the_named_revision() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join(".minds")).unwrap();
    fs::write(
        f.root.join(".minds/redact.json"),
        r#"{"deny_secrets":["Projekt-Kranich","Projekt-Reiher"]}"#,
    )
    .unwrap();
    git(&f.root, &["add", ".minds/redact.json"]);
    git(&f.root, &["commit", "-qm", "reviewed policy"]);
    git(&f.root, &["tag", "reviewed"]);
    fs::write(f.root.join(".minds/redact.json"), "{}").unwrap();
    git(&f.root, &["commit", "-qam", "agent weakens the policy"]);
    let request = |policy_rev| InitRequest {
        repo: f.root.to_str().unwrap(),
        mapping: None,
        profile: None,
        socket_group: None,
        child_repo: None,
        policy_rev,
    };

    let Initialized::Created(pins) =
        init_config(&f._dir.path().join("h1"), &request(Some("reviewed"))).unwrap()
    else {
        panic!("expected a new home");
    };
    assert!(pins.policy.contains("2 custom term(s)"), "{pins:?}");
    assert!(pins.policy.contains("at commit "), "{pins:?}");
    assert!(pins.policy.contains(", blob "), "{pins:?}");

    let Initialized::Created(head) =
        init_config(&f._dir.path().join("h2"), &request(None)).unwrap()
    else {
        panic!("expected a new home");
    };
    assert!(head.policy.contains("0 custom term(s)"), "{head:?}");

    let err = init_config(&f._dir.path().join("h3"), &request(Some("no-such-rev"))).unwrap_err();
    assert!(err.to_string().contains("names no commit"), "{err}");

    // Eine volle Commit-Id gilt als Commit — auch wenn der Agent einen Ref
    // gleichen Namens auf seinen Commit gelegt hat. Die Quelle nennt die
    // volle Id.
    let reviewed = git(&f.root, &["rev-parse", "reviewed"]);
    let reviewed = reviewed.trim();
    git(&f.root, &["branch", reviewed, "HEAD"]);
    let Initialized::Created(by_id) =
        init_config(&f._dir.path().join("h4"), &request(Some(reviewed))).unwrap()
    else {
        panic!("expected a new home");
    };
    assert!(by_id.policy.contains("2 custom term(s)"), "{by_id:?}");
    assert!(by_id.policy.contains(reviewed), "{by_id:?}");

    // Ein schon gepinntes Home übernimmt eine andere Policy-Revision nicht
    // still: Der Fehler nennt "policy".
    let err = init_config(&f._dir.path().join("h2"), &request(Some(reviewed))).unwrap_err();
    assert!(err.to_string().contains("(policy differ)"), "{err}");
    // Dieselbe Policy ist dagegen „unverändert".
    assert!(matches!(
        init_config(&f._dir.path().join("h4"), &request(Some(reviewed))).unwrap(),
        Initialized::Unchanged(_)
    ));
}

/// Ein `refs/replace/…` hinter Füllzeilen über der Lesegrenze bleibt nicht
/// unsichtbar: Was ungelesen bleibt, gilt als vorhanden.
#[test]
fn packed_refs_beyond_the_limit_count_as_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let mut text = String::new();
    for i in 0..200 {
        text.push_str(&format!("{:040x} refs/heads/filler-{i}\n", i));
    }
    text.push_str(&format!("{:040x} refs/replace/{:040x}\n", 1, 2));
    fs::write(dir.path().join("packed-refs"), &text).unwrap();
    // Die Grenze endet vor der Replace-Zeile: Ein abschneidendes Lesen sähe
    // sie gar nicht — nur „über der Grenze = vorhanden" findet sie.
    let head_len = text.find("refs/replace").unwrap() - 41;
    let limit = head_len as u64;
    assert!(packed_refs_mention_within(
        dir.path(),
        "refs/replace/",
        limit
    ));
    // Genau an der Grenze und ohne Replace-Zeile: nicht vorhanden.
    fs::write(dir.path().join("packed-refs"), &text[..head_len]).unwrap();
    assert!(!packed_refs_mention_within(
        dir.path(),
        "refs/replace/",
        limit
    ));
    // Keine gewöhnliche Datei (hier ein Verzeichnis): gilt als vorhanden.
    fs::remove_file(dir.path().join("packed-refs")).unwrap();
    fs::create_dir(dir.path().join("packed-refs")).unwrap();
    assert!(packed_refs_mention_within(
        dir.path(),
        "refs/replace/",
        limit
    ));
}

/// Ein erneutes `init` nennt die gespeicherte Quelle — volle Commit- und
/// Blob-Id —, damit der Mensch sie auch später abgleichen kann.
#[test]
fn witness_init_unchanged_names_the_stored_policy_source() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join(".minds")).unwrap();
    fs::write(
        f.root.join(".minds/redact.json"),
        r#"{"deny_secrets":["Projekt-Kranich"]}"#,
    )
    .unwrap();
    git(&f.root, &["add", ".minds/redact.json"]);
    git(&f.root, &["commit", "-qm", "policy"]);
    let blob = git(&f.root, &["rev-parse", "HEAD:.minds/redact.json"]);
    let home = f._dir.path().join("source-home");
    let request = InitRequest {
        repo: f.root.to_str().unwrap(),
        mapping: None,
        profile: None,
        socket_group: None,
        child_repo: None,
        policy_rev: None,
    };
    assert!(matches!(
        init_config(&home, &request).unwrap(),
        Initialized::Created(_)
    ));
    let Initialized::Unchanged(pins) = init_config(&home, &request).unwrap() else {
        panic!("expected unchanged");
    };
    assert!(pins.policy.contains(blob.trim()), "{pins:?}");
    let commit = git(&f.root, &["rev-parse", "HEAD"]);
    assert!(pins.policy.contains(commit.trim()), "{pins:?}");
}

/// `refs/replace` liest minds nicht, das `git` des Menschen schon: Er sähe
/// bei der Kontrolle anderen Inhalt als den festgehaltenen. `init` bricht ab.
#[test]
fn witness_init_refuses_replace_refs() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join(".minds")).unwrap();
    fs::write(f.root.join(".minds/redact.json"), "{}").unwrap();
    git(&f.root, &["add", ".minds/redact.json"]);
    git(&f.root, &["commit", "-qm", "policy"]);
    let weak = git(&f.root, &["rev-parse", "HEAD:.minds/redact.json"]);
    fs::write(
        f.root.join("strong.json"),
        r#"{"deny_secrets":["Projekt-Kranich"]}"#,
    )
    .unwrap();
    let strong = git(&f.root, &["hash-object", "-w", "strong.json"]);
    git(&f.root, &["replace", weak.trim(), strong.trim()]);
    let err = init_config(
        &f._dir.path().join("replaced-home"),
        &InitRequest {
            repo: f.root.to_str().unwrap(),
            mapping: None,
            profile: None,
            socket_group: None,
            child_repo: None,
            policy_rev: None,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("refs/replace"), "{err}");
}

/// Ein Repository ohne Commit pinnt den strengen Default — und sagt es.
#[test]
fn witness_init_on_an_unborn_head_pins_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "--template="]);
    let Initialized::Created(pins) = init_config(
        &dir.path().join("home"),
        &InitRequest {
            repo: root.to_str().unwrap(),
            mapping: None,
            profile: None,
            socket_group: None,
            child_repo: None,
            policy_rev: None,
        },
    )
    .unwrap() else {
        panic!("expected a new home");
    };
    assert!(
        pins.policy.starts_with("strict default (no commit yet)"),
        "{pins:?}"
    );
}

/// Liegengebliebene Lock-Dateien: höchstens acht beim Namen, dann eine
/// Zeile mit der Zahl — der Agent bestimmt, wie viele dort liegen.
#[test]
fn stale_lock_reports_are_bounded() {
    let f = Fixture::new();
    let refs = f.root.join(".git/refs/heads");
    for i in 0..20 {
        fs::write(refs.join(format!("x{i}.lock")), "").unwrap();
    }
    assert_eq!(stale_locks(&f.root.join(".git")).len(), 20);
    let writer = f.writer();
    writer.report_stale_locks();
    let log = fs::read_to_string(f.home.join("log/witness.log")).unwrap();
    assert_eq!(
        log.matches("lock file left behind").count(),
        MAX_REPORTED_LOCKS
    );
    assert!(log.contains("and 12 more lock file(s)"), "{log}");
}
