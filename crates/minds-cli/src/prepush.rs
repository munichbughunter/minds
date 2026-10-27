//! `minds hook pre-push <remote> <url>` — der pre-push-Orchestrator (Spec
//! S3-MVP §6.2, ADR-S3-006).
//!
//! Das Hook-Skript (`enable::PRE_PUSH_BODY`) liest stdin **einmal** in eine
//! Temp-Datei, führt einen verketteten fremden Hook mit denselben Bytes aus
//! und ruft erst danach dieses Programm mit genau diesen Bytes. Hier:
//!
//! 1. Wächter `MINDS_SYNCING`: sofort Ende.
//! 2. stdin einmal lesen, begrenzt, in Ref-Zeilen.
//! 3. Binden (`minds bind`, §6.1) für die Commit-Menge dieser Zeilen — nur,
//!    wenn `minds.project` gesetzt ist.
//! 4. Evidence-Menge bestimmen und 5. synchron übertragen
//!    ([`crate::evidence_cmd`]), mit `--no-verify` und `MINDS_SYNCING`.
//! 6. Hintergrund-Sync wie bisher, **ohne** `MINDS_SYNCING`.
//!
//! minds blockiert den Push nie: Jede Störung ist eine sichtbare Warnung,
//! der Exit-Code ist immer 0 (auch nach einem Panic). Über den Push
//! entscheiden allein ein verketteter fremder Hook (im Skript) und der
//! Server.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use crate::bind_cmd;
use crate::evidence_cmd::{self, GUARD_ENV};

/// Höchstzahl der Ref-Zeilen (wie S2 Regel 1, `max_refs_per_push`).
const MAX_LINES: usize = 1000;
/// Längste Zeile.
const MAX_LINE: usize = 4096;

/// Liest die pre-push-Eingabe begrenzt: höchstens [`MAX_LINES`] Zeilen à
/// [`MAX_LINE`] Bytes; mehr ist ein Fehler (dann bindet und überträgt minds
/// nichts).
fn read_lines(input: &mut dyn Read) -> Result<Vec<bind_cmd::PushLine>, &'static str> {
    let mut bytes = Vec::new();
    input
        .take((MAX_LINES * (MAX_LINE + 1) + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "input unreadable")?;
    if bytes.len() > MAX_LINES * (MAX_LINE + 1) {
        return Err("input too large");
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "input not UTF-8")?;
    let raw: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    if raw.len() > MAX_LINES || raw.iter().any(|l| l.len() > MAX_LINE) {
        return Err("input too large");
    }
    let lines = bind_cmd::parse_push_lines(text);
    if lines.len() != raw.len() {
        return Err("input malformed");
    }
    Ok(lines)
}

/// Der Orchestrator. Immer Exit 0.
pub(crate) fn run(remote: Option<&str>) -> ExitCode {
    if std::env::var_os(GUARD_ENV).is_some() {
        return ExitCode::SUCCESS;
    }
    let remote = remote.unwrap_or("origin").to_owned();
    let _ = std::panic::catch_unwind(|| orchestrate(&remote, &mut std::io::stdin().lock()));
    ExitCode::SUCCESS
}

fn warn(line: &str) {
    println!("minds: warning: {line}; the code push continues");
}

fn orchestrate(remote: &str, input: &mut dyn Read) {
    // 2. Einmal lesen — alle weiteren Schritte arbeiten auf dieser Liste;
    // kein Kindprozess bekommt stdin.
    let lines = match read_lines(input) {
        Ok(lines) => lines,
        Err(reason) => {
            warn(&format!("push refs not read ({reason})"));
            Vec::new()
        }
    };
    let root = std::env::current_dir()
        .ok()
        .and_then(|here| crate::hook::witness::repo_root(&here));
    match &root {
        Some(root) if !lines.is_empty() => evidence_first(root, remote, &lines),
        Some(_) => {}
        None => warn("repository not found; evidence not transferred"),
    }
    background_sync(remote);
}

/// Schritt 3 bis 5.
fn evidence_first(root: &Path, remote: &str, lines: &[bind_cmd::PushLine]) {
    let commits = (|| {
        let git_dir = bind_cmd::git_dir(root).ok_or("no git directory")?;
        let repo = bind_cmd::open_view(&git_dir)?;
        let git = bind_cmd::which_git().ok_or("git not found on PATH")?;
        let commits = bind_cmd::commits_for_push(&repo, &git, lines)?;
        Ok::<_, String>((repo, commits))
    })();
    let (repo, commits) = match commits {
        Ok(found) => found,
        Err(err) => {
            warn(&format!(
                "commits not determined ({})",
                crate::text::sanitize(&err)
            ));
            return;
        }
    };
    if commits.is_empty() {
        return;
    }
    // 3. Binden — nur, wer ein Projekt nennt, bindet.
    match bind_cmd::expected_project(root) {
        Ok(Some(project)) => bind(root, &project, &commits),
        Ok(None) => {}
        Err(err) => warn(&format!("commits not bound ({err})")),
    }
    // 4. und 5. Danach erst lesen: Die Bindungen eben gehören dazu. Eine
    // frische Sicht — die alte hat die Refs vor dem Binden memoisiert.
    let refs = match bind_cmd::git_dir(root).map(|d| bind_cmd::open_view(&d)) {
        Some(Ok(fresh)) => evidence_cmd::evidence_refs(&fresh, &commits),
        _ => evidence_cmd::evidence_refs(&repo, &commits),
    };
    if let Some(line) = evidence_cmd::transfer(root, remote, &refs).line() {
        println!("{line}");
    }
}

#[cfg(unix)]
fn bind(root: &Path, project: &str, commits: &[String]) {
    match bind_cmd::bind_commits(root, project, commits, false, true) {
        Ok(true) => {}
        Ok(false) => warn("not every commit was bound"),
        Err(err) => warn(&format!(
            "commits not bound ({})",
            crate::text::sanitize(&err)
        )),
    }
}

#[cfg(not(unix))]
fn bind(_: &Path, _: &str, _: &[String]) {}

/// Schritt 6: der bisherige Hintergrund-Sync — **ohne** `MINDS_SYNCING`
/// (sonst endete er sofort); er setzt die Variable selbst nur für seinen
/// `git push` und pusht mit `--no-verify`. stdin nie, stderr wie bisher
/// ins Leere (der Fehler steht im Log), stdout bleibt.
fn background_sync(remote: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = Command::new(exe)
        .args(["sync", "--remote", remote, "--detach"])
        .env_remove(GUARD_ENV)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_input_is_bounded_and_strict() {
        let a = "a".repeat(40);
        let z = "0".repeat(40);
        let good = format!("refs/heads/main {a} refs/heads/main {z}\n");
        assert_eq!(read_lines(&mut good.as_bytes()).unwrap().len(), 1);
        let bad = format!("{good}refs/heads/x {a} refs/heads/x\n");
        assert_eq!(read_lines(&mut bad.as_bytes()), Err("input malformed"));
        let many = good.repeat(MAX_LINES + 1);
        assert_eq!(read_lines(&mut many.as_bytes()), Err("input too large"));
        let long = format!(
            "refs/heads/{} {a} refs/heads/main {z}\n",
            "x".repeat(MAX_LINE)
        );
        assert_eq!(read_lines(&mut long.as_bytes()), Err("input too large"));
        assert!(read_lines(&mut "".as_bytes()).unwrap().is_empty());
    }
}
