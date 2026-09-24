//! `minds seals` — Evidence-Chain-Seals auffindbar machen, ohne eine Id schon
//! zu kennen (ADR-0011, docs/specs/seals-list-command.md).
//!
//! Gegen das echte Binary und ein echtes Git-Repo, wie die übrigen
//! Evidence-Tests in `end_to_end.rs`/`audit.rs`.

use std::path::Path;
use std::process::{Command, Output};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

/// Ein leeres Zuhause für `minds enable`s Hintergrund-Import — sonst durchsucht
/// er das reale `$HOME/.claude/projects` des Testläufers (siehe
/// `end_to_end.rs`).
static HOME: std::sync::LazyLock<tempfile::TempDir> =
    std::sync::LazyLock::new(|| tempfile::tempdir().expect("ein leeres Home"));

fn scratch_repo() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    let ok = git(dir.path(), &["init", "-q"]).status.success();
    if !ok {
        return None;
    }
    git(dir.path(), &["config", "user.email", "test@minds.invalid"]);
    git(dir.path(), &["config", "user.name", "Minds Test"]);
    Some(dir)
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.env("PATH", path_with_minds());
    without_user_config(&mut cmd).output().expect("git läuft")
}

fn without_user_config(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
}

fn path_with_minds() -> std::ffi::OsString {
    let bin_dir = Path::new(MINDS)
        .parent()
        .expect("Binary hat ein Verzeichnis");
    let mut dirs = vec![bin_dir.to_path_buf()];
    dirs.extend(
        std::env::var_os("PATH")
            .iter()
            .flat_map(std::env::split_paths),
    );
    std::env::join_paths(dirs).expect("PATH lässt sich zusammensetzen")
}

fn minds(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    use std::io::Write;
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir).args(args).env("HOME", HOME.path());
    without_user_config(&mut cmd);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("minds startet");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().expect("minds endet")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Ein Hook-Event, wie es der Agent auf stdin schickt.
fn event(dir: &Path, body: &str) {
    let payload = format!(
        r#"{{"session_id":"sess-seals","cwd":"{}",{body}}}"#,
        dir.display()
    );
    let out = minds(dir, &["hook", "--agent", "claude-code"], Some(&payload));
    assert!(out.status.success(), "hook endet immer mit 0");
}

/// Die Session-Id aus dem Trailer des letzten Commits.
fn last_session_id(dir: &Path) -> String {
    let body = stdout(&git(dir, &["log", "-1", "--format=%B"]));
    body.lines()
        .find_map(|l| l.strip_prefix("Minds-Session-Id: "))
        .unwrap_or_else(|| panic!("kein Session-Trailer:\n{body}"))
        .trim()
        .to_string()
}

fn seal_refs(dir: &Path) -> Vec<String> {
    stdout(&git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/evidence/",
        ],
    ))
    .lines()
    .map(str::to_owned)
    .collect()
}

/// `refs/minds/evidence/<hex>` → `b3-<hex>`, die Form, die `minds seals`
/// erwartet.
fn seal_id_of_ref(reference: &str) -> String {
    format!(
        "b3-{}",
        reference
            .strip_prefix("refs/minds/evidence/")
            .unwrap_or_else(|| panic!("kein Seal-Ref: {reference}"))
    )
}

fn seal_text(dir: &Path, reference: &str) -> String {
    stdout(&git(dir, &["show", &format!("{reference}:seal")]))
}

fn git_stdin(dir: &Path, args: &[&str], input: &str) -> Output {
    use std::io::Write;
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.env("PATH", path_with_minds());
    without_user_config(&mut cmd);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("git startet");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().expect("git endet")
}

/// Eine Session + Commit, mit `prompt` als Intent — liefert die Session-Id.
fn record_session(dir: &Path, prompt: &str, file: &str) -> String {
    event(
        dir,
        &format!(r#""hook_event_name":"UserPromptSubmit","prompt":"{prompt}""#),
    );
    event(dir, r#""hook_event_name":"Stop""#);
    std::fs::write(dir.join(file), "x\n").unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", &format!("feat: {file}")]);
    last_session_id(dir)
}

#[test]
fn an_empty_repo_says_so_and_exits_zero() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    let out = minds(dir, &["seals"], None);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("no seals yet"), "{}", stdout(&out));
}

/// Akzeptanzkriterium: ein reiner Lesepfad. Kein `put`, keine Signatur, kein
/// neuer Ref — `refs/minds/evidence/*` sieht vor und nach `minds seals`
/// identisch aus.
#[test]
fn listing_never_touches_the_evidence_refs() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);
    record_session(dir, "unangetastet", "a.txt");

    let before = seal_refs(dir);
    assert_eq!(before.len(), 1);
    let before_text = seal_text(dir, &before[0]);

    for args in [
        vec!["seals"],
        vec!["seals", "--limit", "1"],
        vec!["seals", "--session", &last_session_id(dir)],
    ] {
        let out = minds(dir, &args, None);
        assert!(out.status.success(), "{}", stdout(&out));
    }

    let after = seal_refs(dir);
    assert_eq!(before, after, "die Seal-Refs haben sich verändert");
    assert_eq!(
        before_text,
        seal_text(dir, &after[0]),
        "der Seal-Inhalt hat sich verändert"
    );
}

#[test]
fn a_stored_unsigned_seal_lists_its_id_session_and_time() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);
    let session = record_session(dir, "eine Session", "a.txt");

    let seals = seal_refs(dir);
    assert_eq!(seals.len(), 1, "{seals:?}");
    let short = &seal_id_of_ref(&seals[0])[..15];

    let out = minds(dir, &["seals"], None);
    assert!(out.status.success(), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(text.starts_with("1 seal(s):\n\n"), "{text}");
    assert!(text.contains(short), "{text}");
    assert!(text.contains("stored — unsigned"), "{text}");
    assert!(text.contains(&format!("session  {session}")), "{text}");
    assert!(text.contains("time     "), "{text}");
}

#[test]
fn a_rejected_seal_shows_a_dash_session_and_no_leak() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    std::fs::create_dir_all(dir.join(".minds")).unwrap();
    std::fs::write(
        dir.join(".minds/redact.json"),
        r#"{"deny_pii":["redacted"]}"#,
    )
    .unwrap();
    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"das wort redacted steht hier""#,
    );
    event(dir, r#""hook_event_name":"Stop""#);
    std::fs::write(dir.join("c.txt"), "c\n").unwrap();
    git(dir, &["add", "c.txt"]);
    git(dir, &["commit", "-q", "-m", "feat: geblockt"]);

    let out = minds(dir, &["seals"], None);
    assert!(out.status.success(), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(text.starts_with("1 seal(s):\n\n"), "{text}");
    assert!(text.contains("rejected (payload) — unsigned"), "{text}");
    assert!(text.contains("session  -\n"), "{text}");
    assert!(!text.contains("redacted"), "{text}");
}

#[test]
fn signing_a_seal_flips_it_to_signed_in_the_listing() {
    if !minds_attest::ssh_keygen_available() {
        eprintln!("kein ssh-keygen — Test übersprungen");
        return;
    }
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);
    record_session(dir, "zu signieren", "a.txt");

    let before = stdout(&minds(dir, &["seals"], None));
    assert!(before.contains("stored — unsigned"), "{before}");

    let seal_id = seal_id_of_ref(&seal_refs(dir)[0]);
    let key = dir.join("id_ed25519");
    let generated = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status();
    if !generated.map(|s| s.success()).unwrap_or(false) {
        eprintln!("ssh-keygen kann keinen Schlüssel erzeugen — Test übersprungen");
        return;
    }
    let signed = minds(
        dir,
        &["sign", "--seal", &seal_id, "--key", key.to_str().unwrap()],
        None,
    );
    assert!(signed.status.success(), "{}", stderr(&signed));

    let after = stdout(&minds(dir, &["seals"], None));
    assert!(after.contains("stored — signed"), "{after}");
}

#[test]
fn session_filter_shows_only_that_sessions_seal() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);
    let first = record_session(dir, "erste Session", "a.txt");
    let second = record_session(dir, "zweite Session", "b.txt");
    assert_ne!(first, second);
    assert_eq!(seal_refs(dir).len(), 2);

    let out = minds(dir, &["seals", "--session", &first], None);
    assert!(out.status.success(), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(text.starts_with("1 seal(s):\n\n"), "{text}");
    assert!(text.contains(&format!("session  {first}")), "{text}");
    assert!(!text.contains(&second), "{text}");
}

#[test]
fn an_unfiltered_session_with_no_seals_says_so() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);
    record_session(dir, "eine Session", "a.txt");
    let other = format!("b3-{}", "0".repeat(64));

    let out = minds(dir, &["seals", "--session", &other], None);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains(&format!("no seals for session {other}")),
        "{}",
        stdout(&out)
    );
}

#[test]
fn an_invalid_session_id_is_a_clear_error() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    let out = minds(dir, &["seals", "--session", "not-an-id"], None);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("not a valid session id"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn an_invalid_limit_is_a_clear_error() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);
    record_session(dir, "eine Session", "a.txt");

    let out = minds(dir, &["seals", "--limit", "not-a-number"], None);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("not a valid --limit"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn limit_caps_the_list_to_the_most_recent_entries() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);
    let first = record_session(dir, "erste Session", "a.txt");
    let second = record_session(dir, "zweite Session", "b.txt");
    assert_eq!(seal_refs(dir).len(), 2);

    let out = minds(dir, &["seals", "--limit", "1"], None);
    assert!(out.status.success(), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(text.starts_with("1 seal(s):\n\n"), "{text}");
    // Jüngstes zuerst: die zweite Session verdrängt die erste.
    assert!(text.contains(&format!("session  {second}")), "{text}");
    assert!(!text.contains(&first), "{text}");
}

#[test]
fn a_tampered_seal_is_reported_but_the_healthy_one_still_lists() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);
    record_session(dir, "gesund", "a.txt");
    record_session(dir, "manipuliert", "b.txt");

    let seals = seal_refs(dir);
    assert_eq!(seals.len(), 2);
    let victim = &seals[0];
    let healthy_short = &seal_id_of_ref(&seals[1])[..15];

    // Plumbing wie ein Angreifer mit Repo-Zugriff — dieselbe Fälschung wie in
    // `end_to_end.rs::verify_says_tampered_for_a_forged_seal`.
    let forged = seal_text(dir, victim).replacen("events=2", "events=9", 1);
    let tmp = dir.join("forged");
    std::fs::write(&tmp, &forged).unwrap();
    let blob = stdout(&git(dir, &["hash-object", "-w", tmp.to_str().unwrap()]));
    let tree = stdout(&git_stdin(
        dir,
        &["mktree"],
        &format!("100644 blob {}\tseal\n", blob.trim()),
    ));
    let commit = stdout(&git(dir, &["commit-tree", tree.trim(), "-m", "forged"]));
    git(dir, &["update-ref", victim, commit.trim()]);

    let out = minds(dir, &["seals"], None);
    assert!(out.status.success(), "does not abort: {}", stdout(&out));
    let text = stdout(&out);
    assert!(text.contains("TAMPERED"), "{text}");
    // Die TAMPERED-Zeile steht vor der Zusammenfassung — sie entsteht beim
    // Einsammeln, der Kopf erst danach (dieselbe Reihenfolge wie `verify`s
    // Diagnosen vor seinem Verdikt).
    assert!(text.contains("1 seal(s):\n\n"), "{text}");
    assert!(text.contains(healthy_short), "{text}");
    assert!(text.contains("stored — unsigned"), "{text}");
}
