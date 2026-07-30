//! `minds audit --export` — die Provenienz-Kette als Bündel (Schicht 3, R6).
//!
//! Geprüft wird, was ein Auditor damit anfangen können muss: Die Kette hängt
//! zusammen (Change → Commit → Session → Verdict), die kanonischen Payloads sind
//! byte-genau da, und die **Grenzen** stehen im Artefakt selbst — nicht nur in
//! der Doku, die beim Weiterreichen zurückbleibt.

use std::path::Path;
use std::process::{Command, Output};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("PATH", path_with_minds())
        .output()
        .expect("git läuft")
}

/// Der `PATH` für Git-Aufrufe: vorneweg das Verzeichnis des Test-Binaries.
///
/// Die von `minds enable` installierten Hooks rufen `minds` **ohne Pfad** auf.
/// Ohne diesen Eintrag greift der Aufruf ins Leere, `|| true` schluckt ihn, und
/// der Commit bekäme weder Change-Id noch Checkpoint — der Test wäre rot aus
/// einem Grund, der nichts mit der Zusage zu tun hat. Das Verzeichnis steht
/// **vorn**, damit auch eine global installierte `minds` den Lauf nicht
/// verfälscht.
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

fn minds(dir: &Path, args: &[&str]) -> Output {
    Command::new(MINDS)
        .current_dir(dir)
        .args(args)
        .output()
        .expect("minds läuft")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Ein Repo mit einer erfassten Session an HEAD.
fn repo_with_session() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    if !git(dir.path(), &["init", "-q", "-b", "main"])
        .status
        .success()
    {
        return None;
    }
    git(dir.path(), &["config", "user.email", "anna@example.org"]);
    git(dir.path(), &["config", "user.name", "Anna"]);
    minds(dir.path(), &["enable"]);

    std::fs::write(dir.path().join("a.txt"), "eins\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "Basis"]);

    // Ein Hook-Event, damit eine echte Session entsteht.
    let payload = format!(
        r#"{{"session_id":"sess-audit","cwd":"{}","hook_event_name":"UserPromptSubmit","prompt":"Retry-Test reparieren"}}"#,
        dir.path().display()
    );
    let mut child = Command::new(MINDS)
        .current_dir(dir.path())
        .args(["hook", "--agent", "claude-code"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
    }
    child.wait().unwrap();

    std::fs::write(dir.path().join("a.txt"), "zwei\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "fix: retry"]);
    Some(dir)
}

fn change_id_of_head(dir: &Path) -> String {
    let body = stdout(&git(dir, &["show", "-s", "--format=%B", "HEAD"]));
    body.lines()
        .find_map(|line| line.strip_prefix("Minds-Change-Id: "))
        .map(|id| id.trim().to_owned())
        .unwrap_or_else(|| panic!("kein Change-Id-Trailer:\n{body}"))
}

#[test]
fn the_bundle_carries_the_whole_chain_and_its_limits() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();

    let change = change_id_of_head(dir);
    minds(
        dir,
        &["review", &change, "--approve", "--summary", "geprüft"],
    );
    minds(dir, &["comment", &change, "--on", "a.txt:1", "hier ok"]);

    let out = minds(dir, &["audit", "--export"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let bundle: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("gültiges JSON");

    assert_eq!(bundle["schema_version"], 1);

    // Die Grenzen stehen im Artefakt, nicht nur in der Doku.
    let limits = bundle["does_not_prove"].as_array().expect("does_not_prove");
    assert!(!limits.is_empty());
    assert!(
        limits
            .iter()
            .any(|line| line.as_str().is_some_and(|text| text.contains("fail-open"))),
        "die fail-open-Lücke muss benannt sein: {limits:?}"
    );

    // Die Kette: Change → Commit → Session → Verdict.
    let changes = bundle["changes"].as_array().expect("changes");
    let entry = changes
        .iter()
        .find(|entry| entry["change_id"] == serde_json::json!(change))
        .unwrap_or_else(|| panic!("Change {change} fehlt im Bündel: {changes:?}"));

    assert!(!entry["commits"].as_array().unwrap().is_empty());

    let session = &entry["sessions"][0];
    assert!(
        session["id"].as_str().unwrap().starts_with("b3-"),
        "{session:?}"
    );
    assert_eq!(session["payload"], "present");
    assert_eq!(session["intent"], "Retry-Test reparieren");
    // Byte-genau der Text, über den `minds sign` signiert.
    let attestation = session["attestation_payload"].as_str().unwrap();
    assert!(
        attestation.starts_with("minds-attestation-v1\n"),
        "{attestation}"
    );
    assert!(attestation.contains(session["id"].as_str().unwrap()));

    let verdict = &entry["verdicts"][0];
    assert_eq!(verdict["decision"], "approve");
    assert_eq!(verdict["reviewer"], "anna@example.org");
    let payload = verdict["review_payload"].as_str().unwrap();
    assert!(payload.starts_with("minds-review-v1\n"), "{payload}");
    assert!(payload.contains(verdict["hash"].as_str().unwrap()));

    assert_eq!(entry["comments"][0]["anchor"], "a.txt:1");
}

#[test]
fn a_forgotten_session_stays_visible_in_the_chain() {
    // Der Punkt an einer redigierbaren Nutzlast: Die Löschung ist nachweisbar,
    // nicht spurlos. Ein Bündel, in dem die Session einfach fehlte, sähe aus wie
    // eines, in dem sie nie erfasst wurde.
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();

    let bundle: serde_json::Value =
        serde_json::from_str(&stdout(&minds(dir, &["audit", "--export"]))).unwrap();
    let id = bundle["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|entry| entry["sessions"][0]["id"].as_str())
        .expect("eine Session")
        .to_owned();

    let forgotten = minds(dir, &["forget", &id, "--reason", "DSGVO-Auskunft"]);
    assert!(forgotten.status.success(), "{}", stdout(&forgotten));

    let after: serde_json::Value =
        serde_json::from_str(&stdout(&minds(dir, &["audit", "--export"]))).unwrap();
    let session = after["changes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|entry| entry["sessions"].as_array().unwrap())
        .find(|session| session["id"] == serde_json::json!(id))
        .expect("die Referenz muss in der Kette bleiben");

    assert_eq!(session["payload"], "forgotten");
    assert!(
        session["intent"].as_str().unwrap_or_default().is_empty(),
        "der Inhalt darf nicht mehr im Bündel stehen: {session:?}"
    );
}
