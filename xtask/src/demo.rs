//! `cargo xtask demo-fixture <dir>` und `cargo xtask demo-tamper <dir>`: ein
//! reproduzierbares Demo-Repository für `minds inspect` (Phase D der
//! TUI-Überarbeitung) — die echte TUI läuft dagegen, kein Demo-Modus.
//!
//! Drei Fälle:
//! 1. **sauber** — eine Session mit klarem Auftrag, drei verworfenen
//!    Ansätzen (angelegt und wieder entfernt) und einem Commit mit Trailer;
//! 2. **Lücke** — ein Commit, der zusätzlich eine von Hand geschriebene,
//!    von keiner Session beobachtete Datei trägt (unerklärte Zeilen);
//! 3. **Manipulation** — `demo-tamper` ersetzt danach das Seal-Material von
//!    Fall 1 per Git-Plumbing: Banner in der TUI, `TAMPERED` in Verify.
//!
//! Nur über das gebaute `minds`-Binary und `git` — wie die End-to-End-Tests
//! der CLI; keine weitere Abhängigkeit. Witness aus, Nutzer-Konfiguration
//! von Git ausgeblendet, `HOME` im Demo-Verzeichnis: reproduzierbar.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Der Betreff des sauberen Commits — `tamper` findet ihn daran wieder.
const CLEAN_SUBJECT: &str = "fix(http): retry only idempotent requests";

/// Was die Fixture angelegt hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demo {
    /// Der Commit des sauberen Falls.
    pub clean_commit: String,
    /// Der Commit mit der Lücke.
    pub gap_commit: String,
    /// So viele verworfene Ansätze zeigt `minds why --full` am sauberen
    /// Commit — gelesen, nicht angenommen.
    pub discarded: usize,
}

/// Ein `minds`-Aufruf im Demo-Repo — isoliert wie in den CLI-Tests.
pub fn command(minds: &Path, dir: &Path) -> Command {
    let mut cmd = Command::new(minds);
    cmd.current_dir(dir)
        .env("MINDS_WITNESS", "off")
        .env_remove("MINDS_WITNESS_HOME")
        .env_remove("MINDS_WITNESS_SOCKET")
        .env("HOME", home(dir))
        .env("PATH", path_with(minds))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    cmd
}

/// Baut die Fixture in `dir` (leer oder neu).
pub fn create(dir: &Path, minds: &Path) -> Result<Demo, String> {
    std::fs::create_dir_all(home(dir)).map_err(|e| e.to_string())?;
    // Das Demo-HOME liegt im Repo — Git soll es nicht sehen.
    git(dir, minds, &["init", "-q"])?;
    std::fs::write(dir.join(".git/info/exclude"), ".demo-home/\n").map_err(|e| e.to_string())?;
    git(dir, minds, &["config", "user.email", "demo@minds.invalid"])?;
    git(dir, minds, &["config", "user.name", "Minds Demo"])?;
    std::fs::create_dir_all(dir.join("src")).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("README.md"), "# demo\n").map_err(|e| e.to_string())?;
    git(dir, minds, &["add", "README.md"])?;
    git(dir, minds, &["commit", "-q", "-m", "init: demo repository"])?;
    run(
        command(minds, dir).args(["enable", "--agent", "claude-code"]),
        None,
    )?;

    // 1. Sauber: klarer Auftrag, drei verworfene Ansätze, ein Commit.
    let session = "demo-clean";
    event(
        dir,
        minds,
        session,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Retry only idempotent requests, at most three times""#,
    )?;
    for scratch in ["retry_in_client.rs", "circuit_breaker.rs", "fixed_sleep.rs"] {
        let path = format!("src/{scratch}");
        event(
            dir,
            minds,
            session,
            &tool("Write", &format!(r#"{{"file_path":"{path}"}}"#)),
        )?;
        event(
            dir,
            minds,
            session,
            &tool("Bash", &format!(r#"{{"command":"rm {path}"}}"#)),
        )?;
    }
    event(
        dir,
        minds,
        session,
        &tool("Edit", r#"{"file_path":"src/retry.rs"}"#),
    )?;
    event(dir, minds, session, r#""hook_event_name":"Stop""#)?;
    std::fs::write(
        dir.join("src/retry.rs"),
        "pub fn retry_idempotent(attempts: u8) -> bool {\n    attempts < 3\n}\n",
    )
    .map_err(|e| e.to_string())?;
    git(dir, minds, &["add", "src/retry.rs"])?;
    git(dir, minds, &["commit", "-q", "-m", CLEAN_SUBJECT])?;
    let clean_commit = head(dir, minds)?;

    // 2. Lücke: die Session bearbeitet eine Datei — der Commit trägt eine
    // zweite, die niemand beobachtet hat.
    let session = "demo-gap";
    event(
        dir,
        minds,
        session,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Add exponential backoff""#,
    )?;
    event(
        dir,
        minds,
        session,
        &tool("Edit", r#"{"file_path":"src/backoff.rs"}"#),
    )?;
    event(dir, minds, session, r#""hook_event_name":"Stop""#)?;
    std::fs::write(
        dir.join("src/backoff.rs"),
        "pub fn backoff(attempt: u32) -> u64 {\n    100 << attempt\n}\n",
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("src/manual.rs"),
        "// Von Hand ergänzt — keine Session hat das gesehen.\npub const JITTER_MS: u64 = 25;\n",
    )
    .map_err(|e| e.to_string())?;
    git(dir, minds, &["add", "src/backoff.rs", "src/manual.rs"])?;
    git(
        dir,
        minds,
        &["commit", "-q", "-m", "feat(http): exponential backoff"],
    )?;
    let gap_commit = head(dir, minds)?;

    let why = run(
        command(minds, dir).args(["why", "src/retry.rs:1", "--full"]),
        None,
    )?;
    let discarded = String::from_utf8_lossy(&why.stdout)
        .lines()
        .filter(|l| l.contains("Discarded:"))
        .count();
    Ok(Demo {
        clean_commit,
        gap_commit,
        discarded,
    })
}

/// Fall 3: ersetzt den Text des ersten Seals (des sauberen Falls) — wie ein
/// Angreifer mit Repo-Zugriff, per Git-Plumbing.
pub fn tamper(dir: &Path) -> Result<(), String> {
    // Der Seal der Session hinter dem sauberen Commit — gewählt über den
    // Trailer, nicht über die (zufällige) Reihenfolge der Refs.
    let log = plain_git(dir, &["log", "--format=%B%x00"])?;
    let session = log
        .split('\0')
        .find(|body| body.contains(CLEAN_SUBJECT))
        .and_then(|body| {
            body.lines()
                .find_map(|l| l.strip_prefix("Minds-Session-Id: "))
                .map(|id| id.trim().to_string())
        })
        .ok_or("no clean demo commit with a session — run demo-fixture first")?;
    let refs = plain_git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/evidence/",
        ],
    )?;
    let mut target = None;
    for seal in refs.lines() {
        let text = plain_git(dir, &["cat-file", "-p", &format!("{seal}:seal")])?;
        if text.contains(&session) {
            target = Some((seal.to_string(), text));
            break;
        }
    }
    let Some((seal, text)) = target else {
        return Err(format!("no seal of session {session}"));
    };
    let forged = forge(&text).ok_or("the seal text has no event count to forge")?;
    let tmp = dir.join(".git/forged-seal");
    std::fs::write(&tmp, forged).map_err(|e| e.to_string())?;
    let blob = plain_git(dir, &["hash-object", "-w", &tmp.display().to_string()])?;
    let tree = plain_git_stdin(
        dir,
        &["mktree"],
        &format!("100644 blob {}\tseal\n", blob.trim()),
    )?;
    let commit = plain_git(dir, &["commit-tree", tree.trim(), "-m", "forged"])?;
    plain_git(dir, &["update-ref", &seal, commit.trim()])?;
    let _ = std::fs::remove_file(tmp);
    Ok(())
}

/// Eine glaubwürdige Fälschung: eine andere Zahl von Ereignissen.
fn forge(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.starts_with("events="))?;
    let n: u64 = line.trim_start_matches("events=").trim().parse().ok()?;
    Some(text.replacen(line, &format!("events={}", n + 7), 1))
}

fn home(dir: &Path) -> PathBuf {
    dir.join(".demo-home")
}

/// `PATH` mit dem Verzeichnis des Binaries vorneweg: Die Git-Hooks finden so
/// dieses `minds`, nie ein global installiertes.
fn path_with(minds: &Path) -> std::ffi::OsString {
    let mut parts: Vec<PathBuf> = minds.parent().map(Path::to_path_buf).into_iter().collect();
    if let Some(path) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(parts).unwrap_or_default()
}

fn tool(name: &str, input: &str) -> String {
    format!(r#""hook_event_name":"PreToolUse","tool_name":"{name}","tool_input":{input}"#)
}

fn event(dir: &Path, minds: &Path, session: &str, body: &str) -> Result<(), String> {
    let payload = format!(
        r#"{{"session_id":"{session}","cwd":"{}",{body}}}"#,
        dir.display()
    );
    run(
        command(minds, dir).args(["hook", "--agent", "claude-code"]),
        Some(&payload),
    )
    .map(|_| ())
}

fn head(dir: &Path, minds: &Path) -> Result<String, String> {
    let out = git(dir, minds, &["rev-parse", "HEAD"])?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git(dir: &Path, minds: &Path, args: &[&str]) -> Result<Output, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("HOME", home(dir))
        .env("PATH", path_with(minds))
        .env("MINDS_WITNESS", "off")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    run(&mut cmd, None)
}

fn plain_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = run(Command::new("git").arg("-C").arg(dir).args(args), None)?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn plain_git_stdin(dir: &Path, args: &[&str], input: &str) -> Result<String, String> {
    let out = run(
        Command::new("git").arg("-C").arg(dir).args(args),
        Some(input),
    )?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run(cmd: &mut Command, input: Option<&str>) -> Result<Output, String> {
    use std::io::Write;
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("{cmd:?}: {e}"))?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .ok_or("no stdin")?
            .write_all(input.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(format!(
            "{cmd:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
