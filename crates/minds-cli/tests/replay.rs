//! EA-18b: `minds replay` als Binary, gegen ein echtes Repository — mit
//! einem echten `cargo test` über eine winzige Fixture-Crate.
//!
//! Die Unit-Tests in `src/replay_cmd/tests.rs` belegen die Entscheidungen
//! mit einem Fake-Spawner; hier geht es um den ganzen Weg: Policy aus dem
//! Commit, Session über den Trailer, Prozess ohne Shell, Record unter
//! `refs/minds/anchors/replay/`, Signatur unter `minds-anchor`, Exit-Codes.
//!
//! Nur unter Unix — anderswo lehnt `minds replay` ab (Unit-Test
//! `replay_is_refused_outside_unix`).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use minds_core::{ContentHash, SessionId};
use minds_store::{ContextStore, InRepoStore};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

const POLICY: &str = r#"{"schema":1,"runners":{"cargo-test":{"argv0":"cargo","sub":["test"],"allow_flags":["--offline","--quiet","-q"]}},"timeouts":{"per_command_s":600,"total_s":1800}}"#;

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Ersetzt jede volle Id (`b3-` + 64 Hex) durch `b3-<hash>`.
fn normalize(output: &str) -> String {
    let mut out = String::new();
    let mut rest = output;
    while let Some(at) = rest.find("b3-") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 3..];
        let hex = tail
            .bytes()
            .take_while(u8::is_ascii_hexdigit)
            .take(64)
            .count();
        if hex == 64 {
            out.push_str("b3-<hash>");
            rest = &tail[64..];
        } else {
            out.push_str("b3-");
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

struct Fixture {
    dir: tempfile::TempDir,
    store: InRepoStore,
}

impl Fixture {
    /// Eine Crate mit genau einem bestandenen Test; `policy` liegt als
    /// `.minds/replay.json` im Commit.
    fn new(policy: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "--template="]);
        git(root, &["config", "user.email", "ci@example.invalid"]);
        git(root, &["config", "user.name", "Replay Test"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"replayfix\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn add(a: u32, b: u32) -> u32 { a + b }\n\n#[test]\nfn adds() { assert_eq!(add(2, 2), 4); }\n",
        )
        .unwrap();
        std::fs::write(root.join(".gitignore"), "/target\nCargo.lock\ncanary\n").unwrap();
        if let Some(policy) = policy {
            std::fs::create_dir_all(root.join(".minds")).unwrap();
            std::fs::write(root.join(".minds/replay.json"), policy).unwrap();
        }
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "fixture"]);
        let store = InRepoStore::open(root).unwrap();
        Self { dir, store }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Eine Session, die `command` mit `passed` bestandenen Tests meldet,
    /// verknüpft per Trailer mit einem neuen Commit (HEAD).
    fn session(&self, command: &[&str], passed: u64) -> SessionId {
        let mut session = minds_core::Session::new(
            minds_core::Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            minds_core::Model {
                provider: "test".into(),
                id: "test".into(),
            },
            minds_core::Intent {
                request: format!("claim {passed}"),
                ..minds_core::Intent::default()
            },
        );
        session.turns.push(minds_core::Turn {
            role: minds_core::Role::Assistant,
            text: String::new(),
            tool_calls: vec![minds_core::ToolCall {
                name: "Bash".into(),
                arguments: format!(r#"{{"command":"{}"}}"#, command.join(" ")),
                capture: None,
                effect: None,
                outcome: Some(minds_core::ExecOutcome {
                    class: minds_core::ExecClass::Test,
                    runner: "cargo-test".into(),
                    command: command.iter().map(|s| s.to_string()).collect(),
                    cwd: Some(".".into()),
                    exit_code: None,
                    tests: Some(minds_core::TestCounts {
                        passed,
                        failed: 0,
                        ignored: 0,
                    }),
                    benches: Vec::new(),
                }),
            }],
            parent: None,
            at: Some("2026-10-08T10:00:00Z".into()),
        });
        let session = minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact_session(session)
            .unwrap();
        self.store.put(&session).unwrap().id()
    }

    /// Eine Session wie [`Fixture::session`], verknüpft per Trailer mit
    /// einem neuen Commit (HEAD).
    fn claim(&self, command: &[&str], passed: u64) -> SessionId {
        let id = self.session(command, passed);
        git(
            self.root(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &format!("claim\n\nMinds-Session-Id: {id}"),
            ],
        );
        id
    }

    fn replay(&self, args: &[&str], key: Option<&Path>) -> (i32, String) {
        let mut command = Command::new(MINDS);
        command
            .current_dir(self.root())
            .arg("replay")
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env_remove("GITLAB_CI")
            .env_remove("GITHUB_ACTIONS");
        // Läuft diese Suite selbst in einer CI, soll deren Lage (Merge
        // Request, geschützter Ref) die Tests nicht färben.
        for name in [
            "CI_MERGE_REQUEST_IID",
            "CI_EXTERNAL_PULL_REQUEST_IID",
            "CI_PIPELINE_SOURCE",
            "GITHUB_EVENT_NAME",
            "CI_COMMIT_REF_PROTECTED",
            "GITHUB_REF_PROTECTED",
            "CI_COMMIT_SHA",
            "GITHUB_SHA",
            "CI_SERVER_HOST",
            "CI_PROJECT_PATH",
            "GITHUB_SERVER_URL",
            "GITHUB_REPOSITORY",
        ] {
            command.env_remove(name);
        }
        match key {
            // Ein signierender Lauf simuliert den geschützten Ref, dessen
            // Commit ausgecheckt ist.
            Some(key) => command
                .env("MINDS_ANCHOR_KEY_FILE", key)
                .env("GITLAB_CI", "true")
                .env("CI_PIPELINE_SOURCE", "push")
                .env("CI_SERVER_HOST", "gitlab.example.com")
                .env("CI_PROJECT_PATH", "group/repo")
                .env("CI_COMMIT_REF_PROTECTED", "true")
                .env("CI_COMMIT_SHA", git(self.root(), &["rev-parse", "HEAD"])),
            None => command.env_remove("MINDS_ANCHOR_KEY_FILE"),
        };
        let out = command.output().unwrap();
        (out.status.code().unwrap_or(-1), normalize(&text(&out)))
    }

    fn records(&self) -> Vec<ContentHash> {
        self.store.list_replays().unwrap()
    }
}

#[test]
fn replay_reproduces_a_real_passing_cargo_test_and_detects_a_false_claim() {
    let fixture = Fixture::new(Some(POLICY));

    // Die Session meldet den einen Test als bestanden — er besteht.
    let honest = fixture.claim(&["cargo", "test", "--offline"], 1);
    let (code, out) = fixture.replay(&["--unsigned"], None);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        out,
        "replay   1/1 decisive test runs reproduced (cargo test --offline)\n\
         replay   record b3-<hash> (unsigned)\n"
    );
    let records = fixture.records();
    assert_eq!(records.len(), 1);
    let (record, _) = fixture.store.get_replay(&records[0]).unwrap().unwrap();
    assert_eq!(record.session, honest.to_string());
    assert_eq!(record.commit, git(fixture.root(), &["rev-parse", "HEAD"]));
    assert_eq!(fixture.store.replay_signature(&records[0]).unwrap(), None);

    // Manipulierter Bericht: zwei bestandene Tests behauptet, einer echt.
    fixture.claim(&["cargo", "test", "--offline"], 2);
    let (code, out) = fixture.replay(&["--unsigned"], None);
    assert_eq!(code, 2, "{out}");
    assert_eq!(
        out,
        "replay   0/1 decisive test runs reproduced (cargo test --offline)\n\
         replay   claim not reproduced (tests: recorded 2 passed, observed 1 passed): cargo test --offline\n\
         replay   record b3-<hash> (unsigned)\n"
    );
    assert_eq!(fixture.records().len(), 2);

    // Unsichtbar für Git-Nutzer: nur `refs/minds/`, `fsck` sauber.
    let refs = git(fixture.root(), &["for-each-ref", "--format=%(refname)"]);
    assert!(
        refs.lines()
            .all(|r| r.starts_with("refs/minds/") || r.starts_with("refs/heads/")),
        "{refs}"
    );
    assert!(refs.contains("refs/minds/anchors/replay/"), "{refs}");
    git(fixture.root(), &["fsck", "--strict", "--no-progress"]);
}

#[test]
fn an_unlisted_command_is_never_executed() {
    // Die Session behauptet einen „Testlauf" mit `touch canary`; die Policy
    // erlaubt nur `cargo test`. Belegt am Dateisystem: Es entsteht nichts.
    let fixture = Fixture::new(Some(POLICY));
    fixture.claim(&["touch", "canary"], 1);
    let (code, out) = fixture.replay(&["--unsigned"], None);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        out,
        "replay   no decisive run executed\n\
         replay   1 skipped (not allowlisted): touch canary\n\
         replay   record b3-<hash> (unsigned)\n"
    );
    assert!(!fixture.root().join("canary").exists());

    // Ohne Policy im Commit: auch `cargo test` nicht.
    let fixture = Fixture::new(None);
    fixture.claim(&["cargo", "test"], 1);
    let (code, out) = fixture.replay(&["--unsigned"], None);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.starts_with("replay   no .minds/replay.json in this commit — nothing is allowlisted\n"),
        "{out}"
    );
    assert!(
        out.contains("replay   1 skipped (not allowlisted): cargo test\n"),
        "{out}"
    );
    assert!(!fixture.root().join("target").exists(), "cargo ran");
}

#[test]
fn a_signed_record_verifies_under_minds_anchor() {
    assert!(minds_attest::ssh_keygen_available());
    let fixture = Fixture::new(None);
    fixture.claim(&["cargo", "test"], 1);
    let keys = tempfile::tempdir().unwrap();
    let key: PathBuf = keys.path().join("ci");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status()
            .unwrap()
            .success()
    );
    let (code, out) = fixture.replay(&[], Some(&key));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.ends_with("replay   record b3-<hash> (signed, minds-anchor)\n"),
        "{out}"
    );
    let id = &fixture.records()[0];
    let (_, bytes) = fixture.store.get_replay(id).unwrap().unwrap();
    let signature = fixture.store.replay_signature(id).unwrap().unwrap();
    let signers = keys.path().join("allowed_signers");
    let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    std::fs::write(
        &signers,
        format!("ci@pipeline namespaces=\"minds-anchor\" {}", public.trim()),
    )
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let verify = |namespace| {
        minds_attest::ssh_verify_ns(&text, &signature, &signers, "ci@pipeline", namespace).unwrap()
    };
    assert!(verify(minds_attest::NS_ANCHOR));
    assert!(!verify(minds_attest::NS_WITNESS));
}

#[test]
fn operational_failures_exit_4_before_anything_runs() {
    let fixture = Fixture::new(Some(POLICY));
    fixture.claim(&["cargo", "test", "--offline"], 1);

    // Ohne Schlüssel und ohne --unsigned: nichts läuft, nichts liegt ab.
    let (code, out) = fixture.replay(&[], None);
    assert_eq!(code, 4, "{out}");
    assert!(out.contains("MINDS_ANCHOR_KEY_FILE is not set"), "{out}");
    assert!(fixture.records().is_empty());
    assert!(!fixture.root().join("target").exists(), "cargo ran");

    // Ein anderer Commit als HEAD: Der Checkout passt nicht.
    let (code, out) = fixture.replay(&["--unsigned", "--commit", "HEAD~1"], None);
    assert_eq!(code, 4, "{out}");
    assert!(
        out.contains("replay runs on a checkout of the commit"),
        "{out}"
    );
    assert!(fixture.records().is_empty());

    // Eine kaputte Policy ist ein Abbruch, nie „keine Policy".
    let broken = Fixture::new(Some(r#"{"schema":1,"tolerance":{"default_pc":5}}"#));
    broken.claim(&["cargo", "test"], 1);
    let (code, out) = broken.replay(&["--unsigned"], None);
    assert_eq!(code, 4, "{out}");
    assert!(out.contains("invalid replay policy"), "{out}");
    assert!(broken.records().is_empty());
}

#[test]
fn a_signing_run_never_executes_sessions_linked_only_via_the_index() {
    // Ein Commit ohne Trailer; die Session hängt nur über den Store-Index
    // an ihm — eine Verknüpfung, die jeder mit Push-Recht auf `refs/minds/*`
    // anlegen kann. Signiert wird sie nie ausgeführt, unsigniert schon.
    assert!(minds_attest::ssh_keygen_available());
    let fixture = Fixture::new(Some(POLICY));
    let id = fixture.session(&["cargo", "test", "--offline"], 1);
    let head = git(fixture.root(), &["rev-parse", "HEAD"]);
    fixture
        .store
        .link(
            id,
            &head,
            minds_core::EvidenceMark::of(minds_core::EvidenceSource::Observed),
        )
        .unwrap();
    let keys = tempfile::tempdir().unwrap();
    let key: PathBuf = keys.path().join("ci");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status()
            .unwrap()
            .success()
    );

    let (code, out) = fixture.replay(&[], Some(&key));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("is linked only via the store index — not replayed in a signing run"),
        "{out}"
    );
    assert!(fixture.records().is_empty(), "{out}");
    assert!(!fixture.root().join("target").exists(), "cargo ran");

    // Unsigniert wird dieselbe Session wiederholt.
    let (code, out) = fixture.replay(&["--unsigned"], None);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("replay   1/1 decisive test runs reproduced (cargo test --offline)"),
        "{out}"
    );
    assert_eq!(fixture.records().len(), 1);
}
