//! Effektive Git-Objektprüfung von `receive-pack` (Spec S2 §7.1): Der
//! Adapter liest die Werte mit dem konfigurierten `git` in der geerbten
//! Umgebung und wertet sie wie Git aus. `fsck.*` wirkt nicht auf
//! receive-pack und wird nicht gelesen.

use std::path::Path;
use std::process::{Command, Stdio};

/// Meldungen, deren Herabstufung die Prüfung unwirksam macht (klein
/// geschrieben, wie `git config --get-regexp` sie ausgibt).
const CRITICAL: [&str; 13] = [
    "duplicateentries",
    "treenotsorted",
    "zeropaddedfilemode",
    "emptyname",
    "fullpathname",
    "hasdot",
    "hasdotdot",
    "hasdotgit",
    "missingtree",
    "badtree",
    "gitmodulesname",
    "gitmodulespath",
    "gitmodulesurl",
];

/// `git config` scheiterte (Exit ≠ 0/1 oder unlesbare Ausgabe).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("git config failed")]
pub struct FsckConfigError;

fn config(
    git: &Path,
    git_dir: &Path,
    env: &[(&str, &str)],
    args: &[&str],
) -> Result<Option<String>, FsckConfigError> {
    let out = Command::new(git)
        .envs(env.iter().copied())
        .arg("--git-dir")
        .arg(git_dir)
        .arg("config")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| FsckConfigError)?;
    match out.status.code() {
        Some(0) => String::from_utf8(out.stdout)
            .map(Some)
            .map_err(|_| FsckConfigError),
        Some(1) => Ok(None),
        _ => Err(FsckConfigError),
    }
}

fn boolean(
    git: &Path,
    git_dir: &Path,
    env: &[(&str, &str)],
    key: &str,
) -> Result<Option<bool>, FsckConfigError> {
    match config(git, git_dir, env, &["--type=bool", "--get", key])? {
        None => Ok(None),
        Some(v) => match v.trim() {
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            _ => Err(FsckConfigError),
        },
    }
}

/// Ob die Git-Objektprüfung beim Empfang effektiv aktiv und nicht
/// herabgestuft ist, in der geerbten Umgebung.
pub fn effective(git: &Path, git_dir: &Path) -> Result<bool, FsckConfigError> {
    effective_with(git, git_dir, &[])
}

/// Wie [`effective`], mit zusätzlichen Umgebungsvariablen für den
/// `git`-Aufruf (für Tests der Prioritäten, etwa `GIT_CONFIG_COUNT`).
pub fn effective_with(
    git: &Path,
    git_dir: &Path,
    env: &[(&str, &str)],
) -> Result<bool, FsckConfigError> {
    let on = match boolean(git, git_dir, env, "receive.fsckObjects")? {
        Some(v) => v,
        None => boolean(git, git_dir, env, "transfer.fsckObjects")?.unwrap_or(false),
    };
    if !on {
        return Ok(false);
    }
    let lowered = config(git, git_dir, env, &["--get-regexp", r"^receive\.fsck\."])?;
    for line in lowered.unwrap_or_default().lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        let Some(name) = key
            .to_ascii_lowercase()
            .strip_prefix("receive.fsck.")
            .map(str::to_owned)
        else {
            continue;
        };
        if name == "skiplist" {
            return Ok(false);
        }
        if CRITICAL.contains(&name.as_str()) && !value.trim().eq_ignore_ascii_case("error") {
            return Ok(false);
        }
    }
    Ok(true)
}
