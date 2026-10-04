use super::*;
use minds_store::ContextStore;

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
        init(&home, root.to_str().unwrap(), None).unwrap();
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
    for _ in 0..4 {
        append(&mut writer);
        let mut parsed = hook_event::parse(payload(), "claude-code", None, at()).unwrap();
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
fn witness_lifecycle_is_not_sealed_and_agent_cannot_forge_it() {
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
    writer.hook("witness", None, payload(), at()).unwrap();
    append(&mut writer);
    writer.checkpoint_now(None).unwrap();
    assert_eq!(writer.journal.read(&own).unwrap().events.len(), 1);
    assert_eq!(writer.journal.sessions().unwrap().keys, vec![own]);
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
    let lock1 = lock(&f.home).unwrap();
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
    writer.checkpoint_now(None).unwrap();
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
    // Scheitert erst nach Seal und Discard: beim Parsen des Commits.
    writer.checkpoint_now(Some("not-a-commit")).unwrap_err();
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
