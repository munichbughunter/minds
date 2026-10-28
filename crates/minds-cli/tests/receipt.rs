//! `minds verify --receipt` über das Binary (Spec S3-MVP §6.3, Commit 7):
//! die Quellen des Vertrauensankers (s3_28, ohne root-eigenen Baum — der
//! steht im Isolations-Job), die Aufrufregeln und s3_25 (die beschafften
//! Witness-Refs gehen nie zurück an den Server). Die Nachrechnung gegen
//! einen echten Hook steht in den Tests des Witness
//! (`witness_cmd::daemon::tests::receipt_tests`).
//!
//! **Manuell, nicht in der Pipeline** (Entscheidung 27.10.2026): alle Tests
//! hier sind `#[ignore]`, ausgewiesen als `ignored`. Ausführen mit
//! `cargo test -p minds-cli --test receipt -- --ignored`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use minds_witness_core::{
    CommitId, HashStr, KeyStr, PublicKeyEntry, ReceiptKey, ReceiptStore, Timestamp,
};
use minds_witness_git::receipt_store::GitReceiptStore;

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

fn key(n: u8) -> String {
    format!("ed25519:{}", format!("{n:02x}").repeat(32))
}

struct World {
    dir: tempfile::TempDir,
    work: PathBuf,
    server: PathBuf,
    /// Globale Git-Konfiguration der Testläufe (leer, bis ein Test sie füllt).
    global: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn world() -> World {
    assert!(
        !Path::new("/etc/minds/receipt-keys").exists(),
        "/etc/minds/receipt-keys existiert auf diesem Rechner — die Tests brauchen einen ohne Voreinstellung"
    );
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let server = dir.path().join("server.git");
    std::fs::create_dir_all(&work).unwrap();
    git(
        dir.path(),
        &["init", "-q", "--bare", server.to_str().unwrap()],
    );
    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "test@minds.invalid"]);
    git(&work, &["config", "user.name", "Minds Test"]);
    git(
        &work,
        &["remote", "add", "origin", server.to_str().unwrap()],
    );
    std::fs::write(work.join("a.txt"), "hallo\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "erster Commit"]);
    git(&work, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    let global = dir.path().join("gitconfig");
    std::fs::write(&global, "").unwrap();
    World {
        dir,
        work,
        server,
        global,
    }
}

impl World {
    fn minds(&self, args: &[&str]) -> Output {
        Command::new(MINDS)
            .current_dir(&self.work)
            .args(args)
            .env("MINDS_WITNESS", "off")
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("xdg"))
            .env("GIT_CONFIG_GLOBAL", &self.global)
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET")
            .output()
            .unwrap()
    }

    fn store(&self) -> GitReceiptStore {
        GitReceiptStore::new(
            Path::new("/usr/bin/git"),
            &self.server,
            Duration::from_secs(10),
        )
    }

    fn head(&self) -> String {
        git(&self.work, &["rev-parse", "HEAD"])
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

#[test]
#[ignore = "manuell (S3 Commit 7): cargo test -p minds-cli --test receipt -- --ignored"]
fn s3_28_receipt_anchor_sources_basis() {
    let w = world();
    // Kein Anker — auch nicht, wenn das Repository selbst eine
    // Schlüsselliste trägt.
    let head = w.head();
    git(&w.work, &["update-ref", "refs/minds/witness/keys", &head]);
    let out = w.minds(&["verify", "--receipt"]);
    assert_eq!(code(&out), 3, "{}", text(&out));
    assert!(
        text(&out).contains("NICHT PRÜFBAR: kein Vertrauensanker"),
        "{}",
        text(&out)
    );
    git(&w.work, &["update-ref", "-d", "refs/minds/witness/keys"]);

    // Die Projekt-Angaben gehen dem Anker vor: ungültige Form 4,
    // Widerspruch zu minds.project 1 — auch ohne Anker.
    let out = w.minds(&["verify", "--receipt", "--project", "project-1"]);
    assert_eq!(code(&out), 4, "{}", text(&out));
    git(&w.work, &["config", "minds.project", "git.test/project-1"]);
    let out = w.minds(&["verify", "--receipt", "--project", "git.test/project-2"]);
    assert_eq!(code(&out), 1, "{}", text(&out));
    git(&w.work, &["config", "--unset", "minds.project"]);

    // Ausdrücklich übergeben.
    let k1 = key(1);
    let out = w.minds(&["verify", "--receipt", "--receipt-key", &k1]);
    assert_eq!(code(&out), 3, "{}", text(&out));
    assert!(
        text(&out).contains("Vertrauensanker: ausdrücklich übergeben (--receipt-key)"),
        "{}",
        text(&out)
    );
    assert!(text(&out).contains("keine Quittung"), "{}", text(&out));
    // … erfüllt `--require-protected-anchor` nicht.
    let out = w.minds(&[
        "verify",
        "--receipt",
        "--receipt-key",
        &k1,
        "--require-protected-anchor",
    ]);
    assert_eq!(code(&out), 3, "{}", text(&out));
    assert!(
        text(&out).contains("verlangt einen geschützten"),
        "{}",
        text(&out)
    );

    // Benutzerbeschreibbare Datei: ungeschützt, mit Pilot-Hinweis.
    let file = w.dir.path().join("receipt-keys");
    std::fs::write(&file, format!("{k1}\n")).unwrap();
    let path = file.to_str().unwrap();
    let out = w.minds(&["verify", "--receipt", "--receipt-key-file", path]);
    assert_eq!(code(&out), 3, "{}", text(&out));
    assert!(
        text(&out).contains(&format!(
            "Vertrauensanker: ungeschützt ({path}) – nur für den Pilotbetrieb"
        )),
        "{}",
        text(&out)
    );
    let out = w.minds(&[
        "verify",
        "--receipt",
        "--receipt-key-file",
        path,
        "--require-protected-anchor",
    ]);
    assert_eq!(code(&out), 3, "{}", text(&out));

    // git config --global: ungeschützt.
    std::fs::write(
        &w.global,
        format!("[minds \"witness\"]\n\treceiptKey = {k1}\n"),
    )
    .unwrap();
    let out = w.minds(&["verify", "--receipt"]);
    assert_eq!(code(&out), 3, "{}", text(&out));
    assert!(
        text(&out).contains("Vertrauensanker: ungeschützt (git config) – nur für den Pilotbetrieb"),
        "{}",
        text(&out)
    );
    let out = w.minds(&["verify", "--receipt", "--require-protected-anchor"]);
    assert_eq!(code(&out), 3, "{}", text(&out));
    assert!(
        text(&out).contains("verlangt einen geschützten"),
        "{}",
        text(&out)
    );
    // Gleicher Schlüssel in zwei Quellen: die erste gilt.
    let out = w.minds(&["verify", "--receipt", "--receipt-key", &k1]);
    assert!(
        text(&out).contains("ausdrücklich übergeben"),
        "{}",
        text(&out)
    );
    // Widersprüchlich: Exit 1.
    let out = w.minds(&["verify", "--receipt", "--receipt-key", &key(2)]);
    assert_eq!(code(&out), 1, "{}", text(&out));
    assert!(
        text(&out).contains("Vertrauensanker widersprüchlich"),
        "{}",
        text(&out)
    );
    let out = w.minds(&["verify", "--receipt", "--receipt-key-file", path]);
    assert_eq!(code(&out), 3, "gleich: {}", text(&out));
    std::fs::write(&file, format!("{}\n", key(3))).unwrap();
    let out = w.minds(&["verify", "--receipt", "--receipt-key-file", path]);
    assert_eq!(code(&out), 1, "{}", text(&out));
}

#[test]
#[ignore = "manuell (S3 Commit 7): cargo test -p minds-cli --test receipt -- --ignored"]
fn the_receipt_flags_are_strict() {
    let w = world();
    let k = key(1);
    for args in [
        vec!["verify", "--remote", "origin"],
        vec!["verify", "--receipt-key", &k],
        vec!["verify", "--require-protected-anchor"],
        vec!["verify", "--receipt", "--sig", "x.sig"],
        vec!["verify", "--receipt", "--online"],
        vec![
            "verify",
            "--receipt",
            "--receipt-key",
            &k,
            "--receipt-key-file",
            "/x",
        ],
        vec![
            "verify",
            "--receipt",
            "--receipt-key",
            &k,
            "--require-state",
            "gut",
        ],
        vec!["verify", "--receipt", "--receipt-key", "ed25519:00"],
        vec!["verify", "--project", "git.test/project-1"],
        vec![
            "verify",
            "--receipt",
            "--receipt-key",
            &k,
            "--project",
            "project-1",
        ],
    ] {
        let out = w.minds(&args);
        assert_eq!(code(&out), 4, "{args:?}: {}", text(&out));
    }
    // Remote als URL: Exit 4.
    let url = format!("file://{}", w.server.display());
    let out = w.minds(&["verify", "--receipt", "--receipt-key", &k, "--remote", &url]);
    assert_eq!(code(&out), 4, "{}", text(&out));
    assert!(
        text(&out).contains("kein benannter Remote"),
        "{}",
        text(&out)
    );
}

/// s3_25: `verify` holt nach `refs/minds/remotes/<r>/witness/*`; ein
/// folgender `minds sync` pusht nichts unter `refs/minds/witness/` — auch
/// keinen lokal angelegten Ref dort —, und wenn der Server-Ref `keys`
/// weiterwandert, bleibt der Sync erfolgreich.
#[test]
#[ignore = "manuell (S3 Commit 7): cargo test -p minds-cli --test receipt -- --ignored"]
fn s3_25_witness_refs_never_pushed_back() {
    let w = world();
    let head = w.head();
    let mut store = w.store();
    store
        .put(
            &ReceiptKey {
                commit: CommitId::parse(&head).unwrap(),
                id: HashStr::of_bytes(b"x"),
            },
            b"not a receipt",
        )
        .unwrap();
    let entry = |n: u8| PublicKeyEntry {
        key: KeyStr::parse(&key(n)).unwrap(),
        valid_from: Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
        valid_until: None,
    };
    store.register_public_key(&entry(1)).unwrap();
    let server_witness = || git(&w.server, &["for-each-ref", "refs/minds/witness/"]);
    let before = server_witness();

    let out = w.minds(&[
        "verify",
        "--receipt",
        "--receipt-key",
        &key(1),
        "--remote",
        "origin",
    ]);
    // Die Quittung ist Unsinn: Exit 1 — die Beschaffung lief trotzdem.
    assert_eq!(code(&out), 1, "{}", text(&out));
    let local = git(
        &w.work,
        &["for-each-ref", "--format=%(refname)", "refs/minds/"],
    );
    assert!(
        local.contains("refs/minds/remotes/origin/witness/keys"),
        "{local}"
    );
    assert!(
        local.contains("refs/minds/remotes/origin/witness/receipts/"),
        "{local}"
    );
    assert!(!local.contains("refs/minds/witness/"), "{local}");

    // Lokal etwas unter refs/minds/witness/ und ein gewöhnlicher minds-Ref.
    git(&w.work, &["update-ref", "refs/minds/witness/keys", &head]);
    git(&w.work, &["update-ref", "refs/minds/context", &head]);
    let out = w.minds(&["sync", "--remote", "origin"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(server_witness(), before, "Server-Witness-Refs unverändert");
    let remote = git(
        &w.server,
        &["for-each-ref", "--format=%(refname)", "refs/minds/"],
    );
    assert!(remote.contains("refs/minds/context"), "{remote}");
    assert!(!remote.contains("refs/minds/remotes/"), "{remote}");

    // Der Server-Ref `keys` wandert weiter; verify holt ihn, sync bleibt
    // erfolgreich und pusht weiter nichts dorthin.
    store.register_public_key(&entry(2)).unwrap();
    let moved = server_witness();
    assert_ne!(moved, before);
    let _ = w.minds(&[
        "verify",
        "--receipt",
        "--receipt-key",
        &key(1),
        "--remote",
        "origin",
    ]);
    assert_eq!(
        git(
            &w.work,
            &["rev-parse", "refs/minds/remotes/origin/witness/keys"]
        ),
        git(&w.server, &["rev-parse", "refs/minds/witness/keys"])
    );
    let out = w.minds(&["sync", "--remote", "origin"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(server_witness(), moved);
}
