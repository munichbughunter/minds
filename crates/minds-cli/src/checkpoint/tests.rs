use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use minds_capture::{EventKind, NewEvent, SessionKey};
use minds_core::evidence::{SCOPE_AGENT_HOOKS_V1, Seal, SealOutcome};
use minds_redact::{RedactionConfig, RedactionPipeline};
use minds_store::InRepoStore;

use super::core::CheckpointOutcome;
use super::*;

struct Fixture {
    dir: tempfile::TempDir,
    root: PathBuf,
    repo: Repo,
    journal: Journal,
    epochs: EpochState,
    store: InRepoStore,
    tracked: BTreeSet<String>,
    log_dir: PathBuf,
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

fn key() -> SessionKey {
    SessionKey::new("claude-code", "fixed-checkpoint").unwrap()
}

impl Fixture {
    fn new(arbitrary_roots: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "--template="]);
        for (name, value) in [
            ("user.name", "Checkpoint Test"),
            ("user.email", "checkpoint@example.invalid"),
            ("user.signingkey", ""),
            ("commit.gpgsign", "false"),
            ("minds.backend", "in-repo"),
            ("minds.contextRef", "refs/minds/context"),
        ] {
            git(&root, &["config", name, value]);
        }
        git(
            &root,
            &[
                "config",
                "core.hooksPath",
                dir.path().join("no-hooks").to_str().unwrap(),
            ],
        );
        git(&root, &["commit", "-q", "--allow-empty", "-m", "fixture"]);
        let repo = Repo::discover(&root).unwrap();
        let (journal, epochs_root) = if arbitrary_roots {
            (
                Journal::at(dir.path().join("capture/inbox")),
                dir.path().join("capture/epochs"),
            )
        } else {
            (
                Journal::open(repo.git_dir()),
                repo.git_dir().join("minds/evidence/state"),
            )
        };
        let epochs = EpochState::at(&epochs_root);
        // A fixed salt makes seals comparable across independent repositories.
        epochs.salt(&key()).unwrap();
        let salt = fs::read_dir(epochs_root.join(key().agent()))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "salt"))
            .unwrap();
        fs::write(salt, [0x42; 32]).unwrap();
        let store = InRepoStore::open(&root).unwrap();
        let log_dir = dir.path().join("logs");
        Self {
            dir,
            root,
            repo,
            journal,
            epochs,
            store,
            tracked: BTreeSet::new(),
            log_dir,
        }
    }

    fn feed(&self, epoch: u8) {
        for (second, kind, raw_kind, payload) in [
            (
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                format!(r#"{{"prompt":"Implement fixed checkpoint epoch {epoch}"}}"#),
            ),
            (1, EventKind::TurnEnd, "Stop", "{}".to_string()),
        ] {
            self.journal
                .append(
                    &key(),
                    NewEvent {
                        at: format!("2026-01-01T00:00:0{second}Z"),
                        at_nanos: 1_767_225_600_000_000_000 + second * 1_000_000_000,
                        kind,
                        raw_kind: raw_kind.into(),
                        cwd: None,
                        transcript_path: None,
                        payload: serde_json::value::RawValue::from_string(payload).unwrap(),
                    },
                )
                .unwrap();
        }
    }

    fn run(
        &self,
        pipeline: &RedactionPipeline,
        scope: &'static str,
        signer: SealSigner<'_>,
    ) -> CheckpointOutcome {
        run_checkpoint(
            &CheckpointEnv {
                repo: &self.repo,
                root: &self.root,
                log_dir: &self.log_dir,
                store: &self.store,
                pipeline,
                tracked: Some(&self.tracked),
            },
            &EvidenceSource {
                journal: &self.journal,
                epochs: &self.epochs,
                scope,
            },
            &signer,
        )
        .unwrap()
    }
}

#[test]
fn checkpoint_core_is_byte_identical_to_command_path() {
    let command = Fixture::new(false);
    let core = Fixture::new(true);
    let pipeline = RedactionConfig::default().pipeline().unwrap();
    let original_head = git(&core.root, &["rev-parse", "HEAD"]);

    // Captured from the pre-refactor command with these events and salt.
    // Comparing both paths to fixed bytes also catches shared regressions.
    let golden = [
        include_str!("../../tests/fixtures/checkpoint-core/epoch-0.seal"),
        include_str!("../../tests/fixtures/checkpoint-core/epoch-1.seal"),
    ];
    for (epoch, expected) in golden.into_iter().enumerate() {
        let epoch = epoch as u8;
        command.feed(epoch);
        core.feed(epoch);
        checkpoint_at(&command.root, None).unwrap();
        let outcome = core.run(&pipeline, SCOPE_AGENT_HOOKS_V1, SealSigner::UserConfig);
        assert_eq!(outcome.stored.len(), 1);
        assert_eq!(outcome.sealed.len(), 1);
        let summary = &outcome.sealed[0];
        let text = command.store.seal_text(&summary.seal_id).unwrap().unwrap();
        assert_eq!(text, summary.seal.to_text().unwrap());
        assert_eq!(text, expected);
        assert_eq!(summary.seal_id, Seal::id_of_text(expected));
        assert_eq!(
            summary.seal.outcome,
            SealOutcome::Stored {
                session: outcome.stored[0].to_string()
            }
        );
        assert_eq!(command.store.list().unwrap(), core.store.list().unwrap());
        assert_eq!(
            command.store.list_seals().unwrap(),
            core.store.list_seals().unwrap()
        );
        let id = outcome.stored[0];
        assert_eq!(
            command.store.get_bytes(id).unwrap(),
            core.store.get_bytes(id).unwrap()
        );
        assert_eq!(
            command.epochs.last_seal(&key()),
            core.epochs.last_seal(&key())
        );
        assert!(command.journal.sessions().unwrap().keys.is_empty());
        assert!(core.journal.sessions().unwrap().keys.is_empty());
        assert!(
            git(&command.root, &["log", "-1", "--format=%B"])
                .contains(&format!("Minds-Session-Id: {id}"))
        );
        let head = git(&command.root, &["rev-parse", "HEAD"]);
        assert!(
            command
                .store
                .index()
                .unwrap()
                .links_of(head.trim())
                .iter()
                .any(|link| link.session == id)
        );
        assert_eq!(git(&core.root, &["rev-parse", "HEAD"]), original_head);
        assert!(core.store.index().unwrap().is_empty());
    }
}

#[test]
fn core_reports_rejected_seals_and_reuses_them_without_discard() {
    let fixture = Fixture::new(true);
    fixture.feed(0);
    // An empty pipeline is rejected fail-closed by redact_session.
    let pipeline = RedactionPipeline::new();
    let first = fixture.run(&pipeline, SCOPE_AGENT_HOOKS_V1, SealSigner::None);
    assert!(first.stored.is_empty());
    assert_eq!(first.sealed.len(), 1);
    assert_eq!(first.sealed[0].seal.outcome, SealOutcome::Rejected);
    assert!(!first.sealed[0].signed);
    assert_eq!(fixture.journal.read(&key()).unwrap().events.len(), 2);
    let again = fixture.run(&pipeline, SCOPE_AGENT_HOOKS_V1, SealSigner::None);
    assert_eq!(again.sealed, first.sealed);
    assert_eq!(fixture.store.list_seals().unwrap().len(), 1);
    assert!(fixture.store.list().unwrap().is_empty());
    assert!(fixture.log_dir.join("minds/hook.log").exists());
    assert!(!fixture.repo.git_dir().join("minds/hook.log").exists());

    // The scope is part of the claim: a different scope must not reuse it.
    let changed = fixture.run(&pipeline, "test-observation-v1", SealSigner::None);
    assert_eq!(changed.sealed[0].seal.scope, "test-observation-v1");
    assert_ne!(changed.sealed[0].seal_id, first.sealed[0].seal_id);
    assert_eq!(
        changed.sealed[0].seal.previous,
        Some(first.sealed[0].seal_id.clone())
    );
}

#[test]
fn core_selects_explicit_signer_and_namespace_or_no_signing() {
    if !minds_attest::ssh_keygen_available() {
        return;
    }
    let fixture = Fixture::new(true);
    let key_path = fixture.dir.path().join("signing-key");
    assert!(
        Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-q", "-f"])
            .arg(&key_path)
            .status()
            .unwrap()
            .success()
    );
    let pipeline = RedactionConfig::default().pipeline().unwrap();
    fixture.feed(0);
    let outcome = fixture.run(
        &pipeline,
        "test-observation-v1",
        SealSigner::Key {
            path: &key_path,
            namespace: "test-checkpoint",
        },
    );
    assert_eq!(outcome.stored.len(), 1);
    let summary = &outcome.sealed[0];
    assert!(summary.signed);
    assert_eq!(summary.seal.scope, "test-observation-v1");
    let text = fixture.store.seal_text(&summary.seal_id).unwrap().unwrap();
    let signature = fixture
        .store
        .seal_signature(&summary.seal_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        signature,
        minds_attest::ssh_sign_ns(&text, &key_path, "test-checkpoint").unwrap()
    );

    git(
        &fixture.root,
        &["config", "user.signingkey", key_path.to_str().unwrap()],
    );
    fixture.feed(1);
    let unsigned = fixture.run(&pipeline, SCOPE_AGENT_HOOKS_V1, SealSigner::None);
    assert_eq!(unsigned.stored.len(), 1);
    assert!(!unsigned.sealed[0].signed);
    assert!(
        fixture
            .store
            .seal_signature(&unsigned.sealed[0].seal_id)
            .unwrap()
            .is_none()
    );

    fixture.feed(2);
    let failed = fixture.run(
        &pipeline,
        SCOPE_AGENT_HOOKS_V1,
        SealSigner::Key {
            path: &fixture.dir.path().join("missing-key"),
            namespace: "test-checkpoint",
        },
    );
    assert_eq!(failed.stored.len(), 1, "signing remains best-effort");
    assert!(!failed.sealed[0].signed);
    assert!(fixture.journal.sessions().unwrap().keys.is_empty());
}

/// Eine gültige Session-Id aus einem wiederholten Hex-Zeichen.
fn session(hex: char) -> SessionId {
    format!("b3-{}", hex.to_string().repeat(64))
        .parse()
        .unwrap()
}

#[test]
fn trailer_attach_is_idempotent_across_writers() {
    // EA-06d: Witness und lokaler Pfad trailern an denselben Commit, in dieser
    // Reihenfolge, und jeder darf erneut laufen. Am Ende steht jede Session
    // genau einmal in der Message, und kein Wiederholungslauf schreibt HEAD um.
    let f = Fixture::new(false);
    let guard = f.repo.head().unwrap().commit().unwrap();
    let (witnessed, local) = (session('a'), session('b'));

    // Der Witness rüstet nach; HEAD wandert dabei.
    let first = attach_trailers(&f.repo, Some(&guard.to_string()), &[witnessed], &|_| {})
        .unwrap()
        .unwrap();
    assert!(first.rewrote_head());

    // Der lokale Pfad hält noch den alten Wächter. HEAD ist aber nur derselbe
    // Commit mit mehr Trailern — also folgt er und trailert dorthin.
    assert!(f.repo.is_trailer_retrofit(guard, first.commit()).unwrap());
    let second = attach_trailers(&f.repo, Some(&guard.to_string()), &[local], &|_| {})
        .unwrap()
        .unwrap();
    assert!(second.rewrote_head());

    // Ein echter neuer Commit obendrauf: Dorthin folgt der alte Wächter nicht.
    let after = second.commit();
    git(
        &f.root,
        &["commit", "-q", "--allow-empty", "-m", "dazwischen"],
    );
    assert!(
        attach_trailers(&f.repo, Some(&after.to_string()), &[local], &|_| {})
            .unwrap()
            .is_none()
    );
    git(&f.root, &["reset", "-q", "--soft", &after.to_string()]);

    // Beide Schreiber noch einmal, mit altem wie neuem Wächter: nichts zu tun.
    let head = second.commit();
    for (expected, sessions) in [(guard, [witnessed]), (head, [local]), (guard, [local])] {
        let again = attach_trailers(&f.repo, Some(&expected.to_string()), &sessions, &|_| {})
            .unwrap()
            .unwrap();
        assert_eq!(again, TrailerUpdate::Unchanged(head));
    }
    assert_eq!(f.repo.head().unwrap().commit(), Some(head));
    assert_eq!(
        f.repo.session_ids_of(head).unwrap(),
        vec![witnessed, local],
        "jede Session genau einmal, in Schreib-Reihenfolge"
    );
    assert!(f.repo.is_trailer_retrofit(guard, head).unwrap());
}

/// Der andere Schreiber rüstet genau zwischen Prüfung und Amend nach: Der
/// Compare-and-Swap merkt es (`RefRaced`), der zweite Versuch folgt dem
/// Nachtrag. Kommt er zweimal dazwischen, endet es im benannten Fehler.
#[test]
fn trailer_attach_retries_once_after_a_race() {
    let f = Fixture::new(false);
    let guard = f.repo.head().unwrap().commit().unwrap();
    let (local, other, third) = (session('a'), session('b'), session('c'));
    let raced = std::cell::Cell::new(0);

    // Einmal überholt: folgt und trailert.
    let update = attach_with(
        &f.repo,
        Some(&guard.to_string()),
        &[local],
        &|_| {},
        &|| {
            if raced.get() == 0 {
                raced.set(1);
                f.repo.amend_head_with_sessions(&[other]).unwrap();
            }
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(raced.get(), 1);
    assert_eq!(
        f.repo.session_ids_of(update.commit()).unwrap(),
        vec![other, local]
    );

    // Jedes Mal überholt: nach dem zweiten Versuch der getypte Fehler.
    let guard = update.commit();
    let next = std::cell::Cell::new(0u8);
    let err = attach_with(
        &f.repo,
        Some(&guard.to_string()),
        &[third],
        &|_| {},
        &|| {
            next.set(next.get() + 1);
            let id = format!(
                "b3-{}",
                char::from(b'0' + next.get()).to_string().repeat(64)
            )
            .parse()
            .unwrap();
            f.repo.amend_head_with_sessions(&[id]).unwrap();
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<minds_git::GitError>(),
            Some(minds_git::GitError::RefRaced { .. })
        ),
        "{err}"
    );
    assert!(
        !f.repo
            .session_ids_of(f.repo.head().unwrap().commit().unwrap())
            .unwrap()
            .contains(&third)
    );
}
