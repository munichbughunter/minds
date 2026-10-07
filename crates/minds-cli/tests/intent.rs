//! EA-15: `minds intent bind` / `sign` — welche Anforderung, freigegeben
//! von wem. Gebunden wird eine Datei-Fassung, signiert unter
//! `minds-intent`, aktiviert über den Steuer-Socket des Witness oder — ohne
//! Witness — über die A1-Datei. `minds verify` nennt die Intent-Lage als
//! Assurance-Fakt; eine fehlende oder ungültige Signatur ist nie
//! `TAMPERED`.

#![cfg(unix)]

#[path = "support/hook_witness.rs"]
mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use minds_core::intent_anchor::{IntentAnchor, IntentSource, content_hash};
use support::*;

const REQUIREMENT: &str = "fachliche-anforderung.md";
const REQUIREMENT_TEXT: &str =
    "Die Sortierung ist stabil.\nGleiche Schlüssel behalten ihre Reihenfolge.\n";
const HUMAN: &str = "patrick@doering-it";

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn git_command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Human")
        .env("GIT_AUTHOR_EMAIL", "human@example.invalid")
        .env("GIT_COMMITTER_NAME", "Human")
        .env("GIT_COMMITTER_EMAIL", "human@example.invalid")
        .env_remove(SOCKET_ENV);
    cmd
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = git_command(root).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// `minds` im Repo — ohne einen Witness aus der Umgebung des Testlaufs:
/// Der XDG-Pfad zeigt in das Testverzeichnis.
fn intent(f: &Fixture, args: &[&str]) -> Output {
    minds()
        .current_dir(&f.root)
        .env("XDG_STATE_HOME", f.dir.path().join("state"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("MINDS_WITNESS_HOME")
        .args(args)
        .output()
        .unwrap()
}

/// Ein Repo ohne Witness, mit Identität für die Store-Commits unter
/// `refs/minds/` (auf dem Host aus der Nutzerkonfiguration).
fn repo() -> Fixture {
    let f = Fixture::repo_only();
    git(&f.root, &["config", "user.name", "Human"]);
    git(&f.root, &["config", "user.email", "human@example.invalid"]);
    f
}

/// Legt die Anforderung an und committet sie.
fn commit_requirement(f: &Fixture) {
    fs::write(f.root.join(REQUIREMENT), REQUIREMENT_TEXT).unwrap();
    git(&f.root, &["add", REQUIREMENT]);
    git(&f.root, &["commit", "-q", "-m", "docs: requirement"]);
}

/// Ein Software-Schlüssel mit Kommentar `HUMAN`.
fn human_key(f: &Fixture) -> PathBuf {
    let key = f.dir.path().join("human_ed25519");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", HUMAN, "-f"])
            .arg(&key)
            .status()
            .unwrap()
            .success()
    );
    key
}

fn public(key: &Path) -> String {
    fs::read_to_string(key.with_extension("pub"))
        .unwrap()
        .trim()
        .to_owned()
}

/// Die erwarteten Werte des Ankers über `REQUIREMENT_TEXT`.
struct Expected {
    blob: String,
    content: String,
    id: String,
}

fn expected(f: &Fixture, scope: &[&str]) -> Expected {
    let blob = git(&f.root, &["hash-object", REQUIREMENT]);
    let anchor = IntentAnchor {
        source: IntentSource::File {
            path: REQUIREMENT.into(),
            blob: blob.clone(),
        },
        content: content_hash(REQUIREMENT_TEXT.as_bytes()),
        scope: scope.iter().map(|s| (*s).to_owned()).collect(),
    };
    let text = anchor.to_text().unwrap();
    Expected {
        blob,
        content: anchor.content.to_string(),
        id: IntentAnchor::id_of_text(&text).to_string(),
    }
}

fn bind(f: &Fixture) -> Expected {
    let out = intent(
        f,
        &[
            "intent",
            "bind",
            "--file",
            REQUIREMENT,
            "--scope",
            "src/sort/**,tests/**",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    expected(f, &["src/sort/**", "tests/**"])
}

#[test]
fn intent_bind_file_golden() {
    let f = repo();
    commit_requirement(&f);
    let out = intent(
        &f,
        &[
            "intent",
            "bind",
            "--file",
            REQUIREMENT,
            "--scope",
            "src/sort/**, tests/**",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let e = expected(&f, &["src/sort/**", "tests/**"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "intent  file:{REQUIREMENT}@{}  content {}…\n\
             scope   src/sort/**, tests/**\n\
             anchor  {}\n",
            &e.blob[..8],
            &e.content[..7],
            e.id
        )
    );
    // Committet: keine Warnung.
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");

    // Abgelegt unter refs/minds/intents/, idempotent, fsck sauber.
    let refs = git(
        &f.root,
        &["for-each-ref", "--format=%(refname)", "refs/minds/"],
    );
    assert_eq!(
        refs,
        format!("refs/minds/intents/{}", e.id.trim_start_matches("b3-"))
    );
    let again = intent(
        &f,
        &[
            "intent",
            "bind",
            "--file",
            REQUIREMENT,
            "--scope",
            "src/sort/**,tests/**",
        ],
    );
    assert!(again.status.success(), "{}", text(&again));
    assert_eq!(again.stdout, out.stdout);
    assert_eq!(
        git(
            &f.root,
            &["for-each-ref", "--format=%(refname)", "refs/minds/"]
        ),
        refs
    );
    let fsck = git_command(&f.root)
        .args(["fsck", "--strict"])
        .output()
        .unwrap();
    assert!(fsck.status.success(), "{}", text(&fsck));

    // Ohne Scope: `none`.
    let out = intent(&f, &["intent", "bind", "--file", REQUIREMENT]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\nscope   none\n"),
        "{}",
        text(&out)
    );
}

#[test]
fn intent_bind_warns_uncommitted() {
    let f = repo();
    // Gar nicht committet (ungeborenes HEAD) …
    fs::write(f.root.join(REQUIREMENT), REQUIREMENT_TEXT).unwrap();
    let out = intent(&f, &["intent", "bind", "--file", REQUIREMENT]);
    assert!(out.status.success(), "{}", text(&out));
    let warning = "warning: requirement not committed — anchor refers to a working-tree version\n";
    assert_eq!(String::from_utf8_lossy(&out.stderr), warning);

    // … und committet, dann im Worktree geändert: Der Anker nennt die
    // Worktree-Fassung, die Blob-Id ist ihr `git hash-object`.
    commit_requirement(&f);
    fs::write(f.root.join(REQUIREMENT), "Neue Fassung.\n").unwrap();
    let out = intent(&f, &["intent", "bind", "--file", REQUIREMENT]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stderr), warning);
    let blob = git(&f.root, &["hash-object", REQUIREMENT]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&format!("@{}  content", &blob[..8])),
        "{}",
        text(&out)
    );
}

#[test]
fn intent_bind_refuses_what_is_no_requirement() {
    let f = repo();
    commit_requirement(&f);
    // Eine Datei mit Secret: Die Blob-Id wäre ein Orakel — kein Anker, kein
    // Ref, und das Secret erscheint in keiner Ausgabe. Zur Laufzeit
    // zusammengesetzt, damit kein Secret-Scanner das Literal meldet.
    let token = format!("ghp_{}", "R4nd0mT0k3nV4lu3F0rT3st1ngPurp0s3s00");
    fs::write(f.root.join("secret.md"), format!("Deploy mit {token}\n")).unwrap();
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let secret_scope = format!("src/**,{aws}/**");
    let outside = f.dir.path().join("outside.md");
    fs::write(&outside, "Außerhalb.\n").unwrap();
    for (args, expected) in [
        (vec!["--file", "secret.md"], "clean the file first"),
        (
            vec!["--file", outside.to_str().unwrap()],
            "inside this repository's worktree",
        ),
        (vec!["--file", ".git/config"], "inside .git"),
        (vec!["--file", "missing.md"], "cannot read"),
        (
            vec!["--file", REQUIREMENT, "--scope", "a,,b"],
            "non-empty globs",
        ),
        // Ein Secret im Scope wird abgelehnt, nicht umgeschrieben.
        (
            vec!["--file", REQUIREMENT, "--scope", secret_scope.as_str()],
            "intent anchor refused",
        ),
    ] {
        let mut full = vec!["intent", "bind"];
        full.extend(args);
        let out = intent(&f, &full);
        assert_eq!(out.status.code(), Some(1), "{}", text(&out));
        assert!(text(&out).contains(expected), "{expected}: {}", text(&out));
        assert!(!text(&out).contains(&token), "{}", text(&out));
        assert!(!text(&out).contains(&aws), "{}", text(&out));
    }
    assert!(!f.root.join(".git/minds/intent/last-bound").exists());
    assert_eq!(
        git(
            &f.root,
            &["for-each-ref", "--format=%(refname)", "refs/minds/"]
        ),
        ""
    );
    // Flags, die nicht zum Unterkommando gehören, sind ein Fehler.
    let out = intent(&f, &["intent", "sign", "--scope", "x"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).contains("--scope does not apply to `minds intent sign`"));
}

#[test]
fn intent_sign_without_witness_writes_active_file() {
    let f = repo();
    commit_requirement(&f);
    let e = bind(&f);
    let key = human_key(&f);
    // Ohne Anker-Id: der zuletzt gebundene.
    let out = intent(&f, &["intent", "sign", "--key", key.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "intent  file:{REQUIREMENT}@{}  content {}…\n\
             scope   src/sort/**, tests/**\n\
             signed  {HUMAN} (ssh-ed25519, software key)\n\
             active  local file .git/minds/intent/active (A1, unchained — no witness)\n",
            &e.blob[..8],
            &e.content[..7],
        )
    );
    assert_eq!(
        fs::read_to_string(f.root.join(".git/minds/intent/active")).unwrap(),
        format!("{}\n", e.id)
    );
    // Vor der Berührung, auf stderr: die vollen Werte zum Vergleich.
    let review = String::from_utf8_lossy(&out.stderr);
    assert!(
        review.contains(&format!("review  anchor {}\n", e.id)),
        "{review}"
    );
    assert!(
        review.contains(&format!("review  content {}\n", e.content)),
        "{review}"
    );
    assert!(
        review.contains(&format!("review  file {REQUIREMENT} blob {}\n", e.blob)),
        "{review}"
    );
    // Die Signatur liegt neben dem Anker und gilt unter `minds-intent`.
    let id_hex = e.id.trim_start_matches("b3-");
    let signature = git(
        &f.root,
        &["show", &format!("refs/minds/intents/{id_hex}:anchor.sig")],
    );
    let anchor = git(
        &f.root,
        &["show", &format!("refs/minds/intents/{id_hex}:anchor")],
    );
    let signers = f.dir.path().join("allowed_signers");
    fs::write(
        &signers,
        format!("{HUMAN} namespaces=\"minds-intent\" {}\n", public(&key)),
    )
    .unwrap();
    assert!(
        minds_attest::ssh_verify_ns(
            &format!("{anchor}\n"),
            &format!("{signature}\n"),
            &signers,
            HUMAN,
            minds_attest::NS_INTENT,
        )
        .unwrap()
    );

    // `show` und `list` nennen Signatur und lokale Aktivierung.
    let show = intent(&f, &["intent", "show"]);
    assert!(show.status.success(), "{}", text(&show));
    let shown = String::from_utf8_lossy(&show.stdout);
    assert!(shown.starts_with(&format!("anchor  {}\n", e.id)), "{shown}");
    assert!(
        shown.contains(&format!("content {}\n", e.content)),
        "{shown}"
    );
    assert!(shown.contains("version the version in HEAD\n"), "{shown}");
    assert!(shown.contains("proof   ok\n"), "{shown}");
    assert!(
        shown.contains(
            "signed  signature present (claims ssh-ed25519) — not verified here; minds verify checks it\n"
        ),
        "{shown}"
    );
    assert!(shown.contains("active  local file (A1)"), "{shown}");
    assert!(
        shown.contains("  Gleiche Schlüssel behalten ihre Reihenfolge.\n"),
        "{shown}"
    );
    let list = intent(&f, &["intent", "list"]);
    assert_eq!(
        String::from_utf8_lossy(&list.stdout),
        format!(
            "{}  has sig   file:{REQUIREMENT}@{}  (active, local file)\n",
            e.id,
            &e.blob[..8]
        )
    );
}

#[test]
fn intent_sign_refuses_a_witness_it_cannot_reach() {
    let f = repo();
    commit_requirement(&f);
    let e = bind(&f);
    let key = human_key(&f);
    // Die Agent-Seite: Hooks melden an einen Witness, dessen Steuer-Socket
    // hier nicht liegt — kein stiller Rückfall auf die Datei, die der
    // Witness nie liest, und keine Berührung für eine Freigabe, die nicht
    // aktiviert werden kann.
    let out = minds()
        .current_dir(&f.root)
        .env("XDG_STATE_HOME", f.dir.path().join("state"))
        .env_remove("MINDS_WITNESS_HOME")
        .env(SOCKET_ENV, "/run/minds-witness/witness.sock")
        .args(["intent", "sign", &e.id, "--key", key.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(!text(&out).contains("signed  "), "{}", text(&out));
    assert!(
        text(&out).contains("activate on the host"),
        "{}",
        text(&out)
    );
    assert!(!f.root.join(".git/minds/intent/active").exists());

    // Ein ausdrücklich genanntes Home ohne laufenden Witness ist ein Fehler,
    // kein Rückfall.
    let out = intent(
        &f,
        &[
            "intent",
            "sign",
            &e.id,
            "--key",
            key.to_str().unwrap(),
            "--witness-home",
            f.dir.path().join("nowhere").to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("did not answer — is the witness running"),
        "{}",
        text(&out)
    );
    assert!(!f.root.join(".git/minds/intent/active").exists());
    let hex = e.id.trim_start_matches("b3-");
    assert!(
        !git_command(&f.root)
            .args([
                "cat-file",
                "-e",
                &format!("refs/minds/intents/{hex}:anchor.sig")
            ])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

/// Der post-commit-Hook ruft den Checkpoint — wie `minds enable` ihn schreibt.
fn install_post_commit(f: &Fixture) {
    let hooks = f.dir.path().join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    let script = hooks.join("post-commit");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nexec \"{}\" checkpoint --commit \"$(git rev-parse HEAD)\"\n",
            env!("CARGO_BIN_EXE_minds")
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.root,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    git(&f.root, &["config", "user.name", "Witness"]);
    git(
        &f.root,
        &["config", "user.email", "witness@example.invalid"],
    );
}

/// Committet eine Datei; `socket` landet beim post-commit-Hook.
fn commit_work(f: &Fixture, file: &str, socket: Option<&Path>) {
    fs::write(f.root.join(file), format!("// {file}\n")).unwrap();
    git(&f.root, &["add", file]);
    let mut cmd = git_command(&f.root);
    cmd.args(["commit", "-q", "-m", &format!("feat: {file}")]);
    if let Some(socket) = socket {
        cmd.env(SOCKET_ENV, socket);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
}

fn verify(f: &Fixture, signers: &Path) -> Output {
    minds()
        .current_dir(&f.root)
        .env("HOME", f.dir.path())
        .args(["verify", "HEAD", "--signers", signers.to_str().unwrap()])
        .output()
        .unwrap()
}

/// Eine lokal (A1) erfasste Session, gebunden an die aktive Datei — der
/// Schlüssel `key` hat den Anker signiert.
fn local_session_bound_by(f: &Fixture, key: &Path) -> Expected {
    commit_requirement(f);
    let e = bind(f);
    let out = intent(f, &["intent", "sign", "--key", key.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out));
    install_post_commit(f);
    for event in ["UserPromptSubmit", "Stop"] {
        let stdin = f.payload("run", event, r#","prompt":"sortiere stabil""#);
        assert_silent_success(&f.hook(None, stdin.as_bytes()).0);
    }
    commit_work(f, "sort.rs", None);
    e
}

#[test]
fn intent_wrong_namespace_is_reported() {
    let f = repo();
    let key = human_key(&f);
    let e = local_session_bound_by(&f, &key);
    let short = &e.id[..11];

    // Der Principal darf nur `minds` signieren (Reviews), nicht
    // `minds-intent`: eine Assurance-Aussage, keine Manipulation.
    let signers = f.dir.path().join("allowed_signers");
    fs::write(
        &signers,
        format!("{HUMAN} namespaces=\"minds\" {}\n", public(&key)),
    )
    .unwrap();
    let out = verify(&f, &signers);
    let report = text(&out);
    assert!(
        report.contains(&format!(
            "Intent         intent signature invalid for minds-intent ({short}…, unchained)\n"
        )),
        "{report}"
    );
    assert!(report.contains("Integrity      intact"), "{report}");
    assert!(!report.contains("TAMPERED"), "{report}");
    assert_ne!(out.status.code(), Some(1), "{report}");

    // Derselbe Schlüssel, für `minds-intent` zugelassen: gültig.
    fs::write(
        &signers,
        format!("{HUMAN} namespaces=\"minds-intent\" {}\n", public(&key)),
    )
    .unwrap();
    let report = text(&verify(&f, &signers));
    assert!(
        report.contains(&format!(
            "Intent         intent signed, software key ({short}…, unchained)\n"
        )),
        "{report}"
    );

    // Ohne vertrauenswürdige Signer: nicht geprüft — nie „signiert".
    let report = text(&verify(&f, &f.dir.path().join("missing")));
    assert!(
        report.contains(&format!(
            "Intent         intent signature not checked ({short}…, unchained)\n"
        )),
        "{report}"
    );
}

#[test]
fn verify_reports_unsigned_and_unbound_intents() {
    let f = repo();
    let key = human_key(&f);
    let signers = f.dir.path().join("allowed_signers");
    fs::write(
        &signers,
        format!("{HUMAN} namespaces=\"minds-intent\" {}\n", public(&key)),
    )
    .unwrap();
    install_post_commit(&f);
    commit_requirement(&f);

    // Ohne Anker: nicht gebunden.
    let stdin = f.payload("first", "UserPromptSubmit", r#","prompt":"a""#);
    assert_silent_success(&f.hook(None, stdin.as_bytes()).0);
    commit_work(&f, "a.rs", None);
    let report = text(&verify(&f, &signers));
    assert!(
        report.contains("Intent         intent not bound\n"),
        "{report}"
    );

    // Gebunden und aktiv, aber nie signiert.
    let e = bind(&f);
    fs::create_dir_all(f.root.join(".git/minds/intent")).unwrap();
    fs::write(
        f.root.join(".git/minds/intent/active"),
        format!("{}\n", e.id),
    )
    .unwrap();
    let stdin = f.payload("second", "UserPromptSubmit", r#","prompt":"b""#);
    assert_silent_success(&f.hook(None, stdin.as_bytes()).0);
    commit_work(&f, "b.rs", None);
    let report = text(&verify(&f, &signers));
    assert!(
        report.contains(&format!(
            "Intent         intent unsigned ({}…, unchained)\n",
            &e.id[..11]
        )),
        "{report}"
    );
}

#[test]
fn intent_sign_activates_witness() {
    let f = Fixture::new();
    install_post_commit(&f);
    commit_requirement(&f);
    let e = bind(&f);
    let key = human_key(&f);
    let _daemon = f.start();
    let socket = f.socket();

    let out = intent(
        &f,
        &[
            "intent",
            "sign",
            &e.id,
            "--key",
            key.to_str().unwrap(),
            "--witness-home",
            f.home.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "intent  file:{REQUIREMENT}@{}  content {}…\n\
             scope   src/sort/**, tests/**\n\
             signed  {HUMAN} (ssh-ed25519, software key)\n\
             active  witness (user)\n",
            &e.blob[..8],
            &e.content[..7],
        )
    );
    // Der Witness hält den Anker; die A1-Datei bleibt unberührt.
    let active = fs::read_to_string(f.home.join("evidence/intent.json")).unwrap();
    assert!(active.contains(&e.id), "{active}");
    assert!(!f.root.join(".git/minds/intent/active").exists());

    // Ohne `--witness-home`: das Home aus `MINDS_WITNESS_HOME`, wenn dort
    // ein Steuer-Socket liegt — dieselbe Aktivierung (unverändert).
    let out = minds()
        .current_dir(&f.root)
        .env("MINDS_WITNESS_HOME", &f.home)
        .args(["intent", "sign", &e.id, "--key", key.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).ends_with("active  witness (user)\n"),
        "{}",
        text(&out)
    );

    // Auf dem Host darf `MINDS_WITNESS_SOCKET` gesetzt sein: Mit
    // erreichbarem Steuer-Socket gilt der Witness-Weg.
    let out = minds()
        .current_dir(&f.root)
        .env(SOCKET_ENV, &socket)
        .args([
            "intent",
            "sign",
            &e.id,
            "--key",
            key.to_str().unwrap(),
            "--witness-home",
            f.home.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).ends_with("active  witness (user)\n"),
        "{}",
        text(&out)
    );

    // Ende zu Ende: Die neue Session beginnt beim Intent, der Witness
    // versiegelt sie, `verify` liest den Anker verkettet und die Signatur
    // gültig unter `minds-intent`.
    for (event, extra) in [
        ("SessionStart", r#","source":"startup""#),
        ("UserPromptSubmit", r#","prompt":"sortiere stabil""#),
        ("Stop", ""),
    ] {
        let stdin = f.payload("run", event, extra);
        assert_silent_success(&f.hook(Some(&socket), stdin.as_bytes()).0);
    }
    wait_for_events(&f.witness_journal(), "run", 4);
    commit_work(&f, "sort.rs", Some(&socket));

    let witness_pub = fs::read_to_string(f.home.join("key/witness_ed25519.pub")).unwrap();
    let signers = f.dir.path().join("allowed_signers");
    fs::write(
        &signers,
        format!(
            "minds-witness@test namespaces=\"minds-witness\" {}\n\
             {HUMAN} namespaces=\"minds-intent\" {}\n",
            witness_pub.trim(),
            public(&key)
        ),
    )
    .unwrap();
    let out = verify(&f, &signers);
    let report = text(&out);
    assert!(
        report.contains(&format!(
            "Intent         intent signed, software key ({}…, chained)\n",
            &e.id[..11]
        )),
        "{report}"
    );
    assert!(!report.contains("TAMPERED"), "{report}");
}

/// Code-/Security-Review EA-15: Die Vorgabe „zuletzt gebunden" kann der
/// Agent gesetzt haben — ohne ausdrückliche Id wird nur die Fassung in HEAD
/// freigegeben; mit Id wird gewarnt.
#[test]
fn intent_sign_refuses_a_working_tree_version_without_an_explicit_id() {
    let f = repo();
    commit_requirement(&f);
    let key = human_key(&f);
    // Der Agent ändert die Datei im Worktree und bindet seine Fassung.
    fs::write(f.root.join(REQUIREMENT), "Sortierung egal.\n").unwrap();
    let out = intent(
        &f,
        &["intent", "bind", "--file", REQUIREMENT, "--scope", "**"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("anchor  ").map(str::to_owned))
        .unwrap();

    let out = intent(&f, &["intent", "sign", "--key", key.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("is not a file version in HEAD"),
        "{}",
        text(&out)
    );
    assert!(text(&out).contains("pass the anchor id explicitly"));
    // Nichts signiert, nichts aktiviert.
    let hex = id.trim_start_matches("b3-");
    assert!(
        !git_command(&f.root)
            .args([
                "cat-file",
                "-e",
                &format!("refs/minds/intents/{hex}:anchor.sig")
            ])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    assert!(!f.root.join(".git/minds/intent/active").exists());

    // Ausdrücklich genannt: signiert, mit Warnung.
    let out = intent(&f, &["intent", "sign", &id, "--key", key.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(
            "warning: this anchor is not the version in HEAD — it refers to a working-tree version"
        ),
        "{}",
        text(&out)
    );
}

/// Security-Review EA-15: Ein im Store getauschter Snapshot wird in `show`
/// gemeldet und nicht signiert.
#[test]
fn intent_show_and_sign_detect_a_swapped_snapshot() {
    let f = repo();
    commit_requirement(&f);
    let e = bind(&f);
    let key = human_key(&f);
    let hex = e.id.trim_start_matches("b3-");
    let reference = format!("refs/minds/intents/{hex}");
    let anchor = git(&f.root, &["rev-parse", &format!("{reference}:anchor")]);
    let planted = {
        use std::io::Write;
        let mut child = git_command(&f.root)
            .args(["hash-object", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"Harmlos.\n")
            .unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned()
    };
    let tree = {
        use std::io::Write;
        let mut child = git_command(&f.root)
            .args(["mktree"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                format!("100644 blob {anchor}\tanchor\n100644 blob {planted}\tsnapshot\n")
                    .as_bytes(),
            )
            .unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned()
    };
    let commit = git(&f.root, &["commit-tree", &tree, "-m", "planted"]);
    git(&f.root, &["update-ref", &reference, &commit]);

    let show = intent(&f, &["intent", "show", &e.id]);
    assert!(show.status.success(), "{}", text(&show));
    assert!(
        String::from_utf8_lossy(&show.stdout)
            .contains("proof   NOT PROVEN — intent snapshot does not match its anchor\n"),
        "{}",
        text(&show)
    );
    let out = intent(
        &f,
        &["intent", "sign", &e.id, "--key", key.to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("cannot be signed"), "{}", text(&out));
}

/// Code-Review EA-15: Ein Steuer-Socket im vorgefundenen Home, der nicht
/// antwortet, bricht **vor** der Berührung ab — kein stiller Rückfall auf
/// die Datei, keine Signatur.
#[test]
fn intent_sign_stops_before_signing_when_the_witness_is_down() {
    let f = repo();
    commit_requirement(&f);
    let e = bind(&f);
    let key = human_key(&f);
    let home = f.dir.path().join("whome");
    let socket = home.join("control/control.sock");
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    // Ein Socket ohne Gegenseite.
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    let out = minds()
        .current_dir(&f.root)
        .env("MINDS_WITNESS_HOME", &home)
        .args(["intent", "sign", &e.id, "--key", key.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("did not answer — is the witness running"),
        "{}",
        text(&out)
    );
    assert!(!text(&out).contains("signed  "), "{}", text(&out));
    let hex = e.id.trim_start_matches("b3-");
    assert!(
        !git_command(&f.root)
            .args([
                "cat-file",
                "-e",
                &format!("refs/minds/intents/{hex}:anchor.sig")
            ])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    assert!(!f.root.join(".git/minds/intent/active").exists());
}

/// Code-Review EA-15: Lehnt der Witness ab, ist das ein Fehler mit seinem
/// Grund — die Datei wird nicht geschrieben.
#[test]
fn intent_sign_reports_a_witness_refusal() {
    use minds_capture::witness_proto::{self, Frame};
    use std::io::{Read, Write};

    let f = repo();
    commit_requirement(&f);
    let e = bind(&f);
    let key = human_key(&f);
    let home = f.dir.path().join("whome");
    let socket = home.join("control/control.sock");
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    // Ein Stand-in: beantwortet den Ping, lehnt die Aktivierung ab.
    let stand_in = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            let frame = loop {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0, "client hung up");
                bytes.extend_from_slice(&chunk[..n]);
                if let Ok((frame, _)) = witness_proto::decode(&bytes) {
                    break frame;
                }
            };
            let answer = match frame {
                Frame::Ping => Frame::Ack {
                    request_id: [0; 16],
                    status: "pong".into(),
                },
                Frame::IntentActivate { request_id, .. } => Frame::Nack {
                    request_id,
                    reason: "intent anchor is not in the store".into(),
                },
                other => panic!("unexpected frame {other:?}"),
            };
            stream
                .write_all(&witness_proto::encode(&answer).unwrap())
                .unwrap();
        }
    });
    let out = intent(
        &f,
        &[
            "intent",
            "sign",
            &e.id,
            "--key",
            key.to_str().unwrap(),
            "--witness-home",
            home.to_str().unwrap(),
        ],
    );
    stand_in.join().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains(
            "signature stored, but the intent is not active: the witness refused it: intent anchor is not in the store"
        ),
        "{}",
        text(&out)
    );
    assert!(!f.root.join(".git/minds/intent/active").exists());
}

/// Code-/Security-Review EA-15: Was nicht unter einem auf `minds-intent`
/// beschränkten Principal gilt, ist `invalid` — eine Signatur unter dem
/// falschen Namespace, ein fremder Schlüssel, ein unbeschränkter Schlüssel.
#[test]
fn intent_signatures_outside_minds_intent_are_invalid() {
    use minds_store::ContextStore;

    let f = repo();
    let key = human_key(&f);
    let e = local_session_bound_by(&f, &key);
    let short = &e.id[..11];
    let id: minds_core::ContentHash = e.id.parse().unwrap();
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    let anchor = store.get_intent(&id).unwrap().unwrap().text;
    let invalid =
        format!("Intent         intent signature invalid for minds-intent ({short}…, unchained)\n");
    let restricted = f.dir.path().join("restricted");
    fs::write(
        &restricted,
        format!("{HUMAN} namespaces=\"minds-intent\" {}\n", public(&key)),
    )
    .unwrap();

    // (a) Derselbe Schlüssel, aber unter `minds` signiert.
    let wrong_ns = minds_attest::ssh_sign_ns(&anchor, &key, minds_attest::NS_DEFAULT).unwrap();
    store.put_intent_signature(&id, &wrong_ns).unwrap();
    let report = text(&verify(&f, &restricted));
    assert!(report.contains(&invalid), "{report}");

    // (b) Ein Schlüssel, den die Signer-Datei nicht kennt.
    let stranger = f.dir.path().join("stranger");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&stranger)
            .status()
            .unwrap()
            .success()
    );
    let foreign = minds_attest::ssh_sign_ns(&anchor, &stranger, minds_attest::NS_INTENT).unwrap();
    store.put_intent_signature(&id, &foreign).unwrap();
    let report = text(&verify(&f, &restricted));
    assert!(report.contains(&invalid), "{report}");

    // (c) Richtig signiert, aber der Principal ist unbeschränkt — etwa der
    // Commit-Signing-Key im ssh-agent, den ein Agent mitbenutzen kann.
    let right = minds_attest::ssh_sign_ns(&anchor, &key, minds_attest::NS_INTENT).unwrap();
    store.put_intent_signature(&id, &right).unwrap();
    let unrestricted = f.dir.path().join("unrestricted");
    fs::write(&unrestricted, format!("{HUMAN} {}\n", public(&key))).unwrap();
    let out = verify(&f, &unrestricted);
    let report = text(&out);
    assert!(report.contains(&invalid), "{report}");
    assert!(!report.contains("TAMPERED"), "{report}");
    assert_ne!(out.status.code(), Some(1), "{report}");
    // Gegenprobe: beschränkt gilt dieselbe Signatur.
    let report = text(&verify(&f, &restricted));
    assert!(
        report.contains(&format!(
            "Intent         intent signed, software key ({short}…, unchained)\n"
        )),
        "{report}"
    );
}

/// Security-Review EA-15 (Blocker): Zugangsdaten-Dateien und von
/// `.gitignore` ausgeschlossene Dateien kommen nie in einen gesyncten Ref —
/// auch wenn kein Detektor ihren Inhalt erkennt.
#[test]
fn intent_bind_refuses_secret_files() {
    let f = repo();
    commit_requirement(&f);
    let password = "Winter2024orders";
    fs::write(f.root.join(".gitignore"), "local-notes.md\n").unwrap();
    fs::create_dir_all(f.root.join("config")).unwrap();
    for (file, content) in [
        (".env", format!("DB_PASS={password}\n")),
        (
            ".pgpass",
            format!("db.internal:5432:orders:svc_orders:{password}\n"),
        ),
        (
            "config/credentials.json",
            format!("{{\"password\":\"{password}\"}}\n"),
        ),
    ] {
        fs::write(f.root.join(file), content).unwrap();
        let out = intent(&f, &["intent", "bind", "--file", file]);
        assert_eq!(out.status.code(), Some(1), "{file}: {}", text(&out));
        assert!(
            text(&out).contains("is a credential file"),
            "{file}: {}",
            text(&out)
        );
        assert!(!text(&out).contains(password), "{}", text(&out));
    }
    fs::write(f.root.join("local-notes.md"), format!("pw {password}\n")).unwrap();
    let out = intent(&f, &["intent", "bind", "--file", "local-notes.md"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("ignored by .gitignore"),
        "{}",
        text(&out)
    );
    assert_eq!(
        git(
            &f.root,
            &["for-each-ref", "--format=%(refname)", "refs/minds/"]
        ),
        ""
    );
    assert!(!f.root.join(".git/minds/intent/last-bound").exists());

    // Eine Anforderung, die nur über Passwörter spricht, in einer Datei, die
    // nach `env` klingt, bleibt bindbar.
    fs::create_dir_all(f.root.join("docs")).unwrap();
    fs::write(
        f.root.join("docs/env-variables.md"),
        "Passwörter werden mit Argon2 gehasht.\n",
    )
    .unwrap();
    let out = intent(&f, &["intent", "bind", "--file", "docs/env-variables.md"]);
    assert!(out.status.success(), "{}", text(&out));
}

/// Code-/Security-Review EA-15: Auch ein Prompt-Anker, den der Agent in den
/// Store gelegt und als „zuletzt gebunden" eingetragen hat, wird ohne
/// ausdrückliche Id nicht signiert.
#[test]
fn intent_sign_without_id_refuses_a_prompt_anchor() {
    use minds_store::ContextStore;

    let f = repo();
    commit_requirement(&f);
    let key = human_key(&f);
    let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let planted = pipeline
        .redact_intent(
            IntentSource::Prompt,
            vec!["**".into()],
            b"Alles umbauen.\n".to_vec(),
        )
        .unwrap();
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    let id = store.put_intent(&planted).unwrap();
    fs::create_dir_all(f.root.join(".git/minds/intent")).unwrap();
    fs::write(
        f.root.join(".git/minds/intent/last-bound"),
        format!("{id}\n"),
    )
    .unwrap();

    let out = intent(&f, &["intent", "sign", "--key", key.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("is not a file version in HEAD"),
        "{}",
        text(&out)
    );
    assert_eq!(store.intent_signature(&id).unwrap(), None);
    assert!(!f.root.join(".git/minds/intent/active").exists());

    // Mit ausdrücklicher Id: signiert — nachdem der ganze Text zu sehen war.
    let out = intent(
        &f,
        &[
            "intent",
            "sign",
            id.as_str(),
            "--key",
            key.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("review  | Alles umbauen.\n"),
        "{}",
        text(&out)
    );
}

/// Security-Review EA-15: Eine kaputte `anchor.sig` ist ungültig, nicht
/// „nicht geprüft".
#[test]
fn a_malformed_intent_signature_is_invalid() {
    let f = repo();
    let key = human_key(&f);
    let e = local_session_bound_by(&f, &key);
    let hex = e.id.trim_start_matches("b3-");
    let reference = format!("refs/minds/intents/{hex}");
    let entries: String = git(&f.root, &["ls-tree", &reference])
        .lines()
        .filter(|line| !line.ends_with("\tanchor.sig"))
        .map(|line| format!("{line}\n"))
        .collect();
    let pipe = |args: &[&str], input: &str| {
        use std::io::Write;
        let mut child = git_command(&f.root)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned()
    };
    let garbage = pipe(&["hash-object", "-w", "--stdin"], "not a signature\n");
    let tree = pipe(
        &["mktree"],
        &format!("{entries}100644 blob {garbage}\tanchor.sig\n"),
    );
    let commit = git(&f.root, &["commit-tree", &tree, "-m", "planted"]);
    git(&f.root, &["update-ref", &reference, &commit]);

    let signers = f.dir.path().join("allowed_signers");
    fs::write(
        &signers,
        format!("{HUMAN} namespaces=\"minds-intent\" {}\n", public(&key)),
    )
    .unwrap();
    let out = verify(&f, &signers);
    let report = text(&out);
    assert!(
        report.contains(&format!(
            "Intent         intent signature invalid for minds-intent ({}…, unchained)\n",
            &e.id[..11]
        )),
        "{report}"
    );
    assert_ne!(out.status.code(), Some(1), "{report}");
}

/// Code-/Security-Review EA-15: `bind` startet keinen `git`-Prozess, der den
/// Index liest — ein vom Agenten gesetztes `core.fsmonitor` liefe sonst als
/// der Mensch auf dem Host.
#[test]
fn intent_bind_never_runs_the_repositorys_fsmonitor() {
    let f = repo();
    commit_requirement(&f);
    let marker = f.dir.path().join("PWNED");
    let script = f.dir.path().join("fsmonitor.sh");
    fs::write(
        &script,
        format!("#!/bin/sh\ntouch \"{}\"\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.root,
        &["config", "core.fsmonitor", script.to_str().unwrap()],
    );
    let out = intent(&f, &["intent", "bind", "--file", REQUIREMENT]);
    assert!(out.status.success(), "{}", text(&out));
    // Gegenprobe: `git status` startet das Skript tatsächlich.
    assert!(!marker.exists(), "minds started core.fsmonitor");
    git(&f.root, &["status", "--porcelain"]);
    assert!(marker.exists(), "the probe script never runs");
}

/// Code-Review EA-15: Ein Symlink auf eine Zugangsdaten-Datei und eine zu
/// große Datei werden abgelehnt; nichts wird abgelegt.
#[test]
fn intent_bind_refuses_links_to_secrets_and_oversized_files() {
    let f = repo();
    commit_requirement(&f);
    fs::write(f.root.join(".env"), "DB_PASS=Winter2024orders\n").unwrap();
    std::os::unix::fs::symlink(".env", f.root.join("notes.md")).unwrap();
    let out = intent(&f, &["intent", "bind", "--file", "notes.md"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("is a credential file"),
        "{}",
        text(&out)
    );

    fs::write(
        f.root.join("huge.md"),
        vec![b'a'; minds_core::intent_anchor::MAX_SNAPSHOT + 1],
    )
    .unwrap();
    let out = intent(&f, &["intent", "bind", "--file", "huge.md"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("larger than 4 MiB"), "{}", text(&out));
    assert_eq!(
        git(
            &f.root,
            &["for-each-ref", "--format=%(refname)", "refs/minds/"]
        ),
        ""
    );
    assert!(!f.root.join(".git/minds/intent/last-bound").exists());
}

/// Code-Review EA-15: Hardlink, FIFO und Symlink nach außen werden
/// abgelehnt — ohne Hängen, ohne Ref.
#[test]
fn intent_bind_refuses_hard_links_fifos_and_links_out() {
    let f = repo();
    commit_requirement(&f);
    let outside = f.dir.path().join("outside.md");
    fs::write(&outside, "Außerhalb.\n").unwrap();
    fs::hard_link(&outside, f.root.join("hard.md")).unwrap();
    std::os::unix::fs::symlink(&outside, f.root.join("out.md")).unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(f.root.join("pipe.md"))
            .status()
            .unwrap()
            .success()
    );
    for (file, expected) in [
        ("hard.md", "other hard links"),
        ("out.md", "inside this repository's worktree"),
        ("pipe.md", "regular file"),
    ] {
        let out = intent(&f, &["intent", "bind", "--file", file]);
        assert_eq!(out.status.code(), Some(1), "{file}: {}", text(&out));
        assert!(text(&out).contains(expected), "{file}: {}", text(&out));
    }
    assert_eq!(
        git(
            &f.root,
            &["for-each-ref", "--format=%(refname)", "refs/minds/"]
        ),
        ""
    );
}
