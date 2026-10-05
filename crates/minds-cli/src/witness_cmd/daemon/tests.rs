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

/// Die Policy-Datei gehört dem Agenten: Ein FIFO oder `/dev/zero` an ihrer
/// Stelle hält den einzigen Schreiber nicht an.
#[test]
fn witness_refuses_a_policy_file_that_is_not_regular() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    fs::create_dir_all(f.root.join(".minds")).unwrap();
    let policy = f.root.join(".minds/redact.json");
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    std::os::unix::fs::symlink("/dev/zero", &policy).unwrap();
    let started = std::time::Instant::now();
    writer.checkpoint_now(Some(head.trim())).unwrap_err();
    fs::remove_file(&policy).unwrap();

    let name = std::ffi::CString::new(policy.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: nul-terminierter Pfad, keine weiteren Vorbedingungen.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    writer.checkpoint_now(Some(head.trim())).unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(sealed_roots(&f).is_empty());
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);
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
/// Lauf; Läufe — auch gescheiterte — nicht schneller als
/// `MIN_CHECKPOINT_INTERVAL`.
#[test]
fn witness_coalesces_and_rate_limits_checkpoint_requests() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let head = head.trim();
    let backdate = |writer: &mut Writer| {
        writer.last_run = Some(std::time::Instant::now() - MIN_CHECKPOINT_INTERVAL);
    };
    let pin = |writer: &mut Writer| {
        writer.last_run = Some(std::time::Instant::now());
    };

    assert_eq!(
        writer.checkpoint_requested(None, &|| true),
        Err("commit required")
    );

    // Ein gescheiterter Lauf (falscher Commit) zählt mit: Die nächste Anfrage
    // kommt zu früh, auch mit dem richtigen Commit.
    let before = std::time::Instant::now();
    assert_eq!(
        writer.checkpoint_requested(Some(&"0".repeat(40)), &|| true),
        Err("checkpoint failed")
    );
    assert!(writer.last_run.is_some_and(|at| at >= before));
    // Wie lange der Lauf selbst dauerte, darf den Test nicht entscheiden:
    // Unter Last kann er länger als die Frist brauchen.
    pin(&mut writer);
    assert_eq!(
        writer.checkpoint_requested(Some(head), &|| true),
        Err("rate limited")
    );
    assert!(sealed_roots(&f).is_empty());
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);

    backdate(&mut writer);
    let first = writer.checkpoint_requested(Some(head), &|| true).unwrap();
    assert!(first.starts_with("witness: 1 range(s) sealed"), "{first}");
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let head = head.trim();
    // Nichts Neues: sofort, ohne Lauf, auch innerhalb der Frist.
    for _ in 0..10 {
        assert_eq!(
            writer.checkpoint_requested(Some(head), &|| true).as_deref(),
            Ok("witness: nothing to seal")
        );
    }
    // Neues Event, aber zu früh: abgelehnt, die Session bleibt offen.
    writer
        .hook(
            "claude-code",
            None,
            payload(),
            ("2026-10-04T10:00:01Z".into(), at().1 + 1),
        )
        .unwrap();
    pin(&mut writer);
    assert_eq!(
        writer.checkpoint_requested(Some(head), &|| true),
        Err("rate limited")
    );
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);
    assert_eq!(sealed_roots(&f).len(), 1);

    backdate(&mut writer);
    let later = writer.checkpoint_requested(Some(head), &|| true).unwrap();
    assert!(later.starts_with("witness: 1 range(s) sealed"), "{later}");
    assert_eq!(sealed_roots(&f).len(), 2);
    // Abgewiesene Anfragen füllen das eigene Log nicht.
    let log = fs::read_to_string(f.home.join("log/witness.log")).unwrap_or_default();
    assert_eq!(log.matches("checkpoint failed").count(), 1, "{log}");
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
    assert!(writer.last_run.is_none());
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

/// Ein `.minds`, das als Symlink an einen fremden Ort zeigt, wird nicht
/// gelesen.
#[test]
fn witness_refuses_a_symlinked_policy_directory() {
    let f = Fixture::new();
    f.keygen();
    let mut writer = f.writer();
    append(&mut writer);
    let elsewhere = f._dir.path().join("policy");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("redact.json"), "{}").unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.root.join(".minds")).unwrap();
    let head = git(&f.root, &["rev-parse", "HEAD"]);

    let err = writer.checkpoint_now(Some(head.trim())).unwrap_err();
    assert!(err.to_string().contains("not a plain directory"), "{err}");
    assert!(sealed_roots(&f).is_empty());
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
        writer.last_run = None;
        assert_eq!(
            writer.checkpoint_requested(Some(&wrong), &|| true),
            Err("checkpoint failed")
        );
    }
    let log = || fs::read_to_string(f.home.join("log/witness.log")).unwrap_or_default();
    assert_eq!(log().matches("checkpoint failed").count(), 1, "{}", log());

    writer.last_run = None;
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
