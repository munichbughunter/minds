//! `minds inspect` in der Pipe: Ist stdout kein Terminal, kommen die Zeilen
//! tab-separiert und ohne ANSI — aus demselben Lese-Modell wie die
//! Oberfläche. Geprüft wird der Weg über das echte Binary und ein echtes
//! Repository, wie in `end_to_end.rs`.

use std::path::Path;
use std::process::{Command, Output};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

static HOME: std::sync::LazyLock<tempfile::TempDir> =
    std::sync::LazyLock::new(|| tempfile::tempdir().expect("ein leeres Home"));

fn scratch_repo() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    if !git(dir.path(), &["init", "-q"]).status.success() {
        return None;
    }
    git(dir.path(), &["config", "user.email", "test@minds.invalid"]);
    git(dir.path(), &["config", "user.name", "Minds Test"]);
    Some(dir)
}

fn path_with_minds() -> std::ffi::OsString {
    let bin_dir = Path::new(MINDS)
        .parent()
        .expect("Binary hat ein Verzeichnis");
    let mut dirs = vec![bin_dir.to_path_buf()];
    dirs.extend(
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    std::env::join_paths(dirs).expect("PATH lässt sich zusammensetzen")
}

fn without_user_config(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("HOME", HOME.path())
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("PATH", path_with_minds());
    without_user_config(&mut cmd).output().expect("git läuft")
}

fn minds(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    use std::io::Write;
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir).args(args);
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

fn event(dir: &Path, body: &str) {
    let payload = format!(
        r#"{{"session_id":"sess-inspect","cwd":"{}",{body}}}"#,
        dir.display()
    );
    let out = minds(dir, &["hook", "--agent", "claude-code"], Some(&payload));
    assert!(out.status.success(), "hook endet immer mit 0");
}

/// Eine Session durch den Kern-Loop: Hook-Events, Commit, Checkpoint.
fn one_session(dir: &Path) -> String {
    let enable = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(enable.status.success(), "{}", stdout(&enable));
    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Schreibe eine Grußfunktion""#,
    );
    event(
        dir,
        r#""hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"greet.rs"}"#,
    );
    event(dir, r#""hook_event_name":"Stop""#);
    std::fs::write(
        dir.join("greet.rs"),
        "fn greet() {\n    println!(\"hallo\");\n}\n",
    )
    .unwrap();
    git(dir, &["add", "greet.rs"]);
    assert!(
        git(dir, &["commit", "-q", "-m", "feat: Grußfunktion"])
            .status
            .success()
    );
    let head = stdout(&git(dir, &["rev-parse", "HEAD"])).trim().to_owned();
    let checkpoint = minds(dir, &["checkpoint", "--commit", &head], None);
    assert!(checkpoint.status.success(), "{}", stdout(&checkpoint));
    let message = stdout(&git(dir, &["log", "-1", "--format=%B"]));
    message
        .lines()
        .find_map(|line| line.strip_prefix("Minds-Session-Id: "))
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| panic!("kein Trailer:\n{message}"))
}

#[test]
fn piped_inspect_prints_one_tab_separated_line_per_session_without_ansi() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    let session_id = one_session(dir);

    let out = minds(dir, &["inspect"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(!text.contains('\u{1b}'), "ANSI in der Pipe:\n{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    let cols: Vec<&str> = lines[0].split('\t').collect();
    assert_eq!(cols.len(), 10, "{cols:?}");
    assert_eq!(cols[1], session_id);
    assert!(cols[2].starts_with("claude-code"), "{cols:?}");
    assert_eq!(
        cols[6], "observed",
        "der Trailer belegt die Kante: {cols:?}"
    );
    assert_eq!(cols[7], "offen");
    assert!(
        cols[8].starts_with('I'),
        "Change-Id aus dem Trailer: {cols:?}"
    );
    assert_eq!(cols[9], "Schreibe eine Grußfunktion");

    // Die Suche filtert dieselbe Liste.
    let hit = minds(dir, &["inspect", "grußfunktion"], None);
    assert_eq!(stdout(&hit).lines().count(), 1);
    let miss = minds(dir, &["inspect", "nirgends"], None);
    assert!(miss.status.success());
    assert_eq!(stdout(&miss), "");
}

#[test]
fn piped_inspect_of_a_line_prints_the_chain_down_to_the_intent() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    let session_id = one_session(dir);

    let out = minds(dir, &["inspect", "greet.rs:2"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(text.starts_with("line\tgreet.rs:2\n"), "{text}");
    assert!(text.contains("\ncommit\t"), "{text}");
    assert!(text.contains("\nchange\tI"), "{text}");
    assert!(
        text.contains(&format!("\nsession\t{session_id}\t")),
        "{text}"
    );
    assert!(
        text.contains("\nintent\tSchreibe eine Grußfunktion\n"),
        "{text}"
    );
    assert!(text.contains("\tobserved\n"), "{text}");
    assert!(text.contains("\nreview\toffen\n"), "{text}");
    // Die Lücken zuletzt: hier genau eine — niemand hat bewertet.
    assert!(text.contains("\ngap\tNoReview\t"), "{text}");
    assert!(!text.contains("gap\tInferred"), "{text}");
}

#[test]
fn an_empty_repo_prints_nothing_and_a_forgotten_session_stays_a_line() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    assert!(
        git(dir, &["commit", "-q", "--allow-empty", "-m", "leer"])
            .status
            .success()
    );
    let out = minds(dir, &["inspect"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout(&out), "");

    let session_id = one_session(dir);
    let forget = minds(dir, &["forget", &session_id, "--reason", "Testdaten"], None);
    assert!(
        forget.status.success(),
        "{}",
        String::from_utf8_lossy(&forget.stderr)
    );
    let out = minds(dir, &["inspect"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains(&session_id), "{text}");
    assert!(text.contains("vergessen: Testdaten"), "{text}");
    assert!(
        !text.contains("Grußfunktion"),
        "Nutzlast nach forget sichtbar:\n{text}"
    );
}
