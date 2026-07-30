//! `minds import` — der Backfill: bestehende Transkripte in den Store, verlinkt
//! über den Index.
//!
//! Ausgelöst wird das normalerweise **automatisch** von `minds enable` (im
//! Hintergrund); von Hand aufrufbar ist es trotzdem — für einen erneuten Lauf
//! oder zum Nachsehen.
//!
//! ```text
//!   Transkripte ──import──► Sessions ──redact──► Store
//!                                │
//!                     match(Dateien+Zeit) mit git log ──► Index (inferred)
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use minds_capture::{CommitInfo, SessionInfo, match_sessions};
use minds_core::Evidence;
use minds_git::Repo;
use minds_redact::RedactionConfig;

use crate::config;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds import` aus.
pub fn run() -> ExitCode {
    match import() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds import: {err}");
            ExitCode::FAILURE
        }
    }
}

fn import() -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);
    let store = config::load(&root).open(&root)?;
    let home = home_dir()?;

    // 1. Transkripte aller bekannten Agents lesen.
    let reports = minds_capture::for_repo(&root, &home);

    // 2. Jede Session redigieren und ablegen; dabei den Fingerabdruck fürs
    //    Matching aus der *redigierten* Session ziehen (ihre Id und ihre
    //    repo-relativen Pfade sind das, was im Store liegt).
    let pipeline = RedactionConfig::default().pipeline()?;
    let mut infos: Vec<SessionInfo> = Vec::new();
    let mut stored = 0usize;

    for report in &reports {
        for session in &report.sessions {
            match pipeline.redact_session(session.clone()) {
                Ok(redacted) => {
                    let id = store.put(&redacted)?.id();
                    infos.push(SessionInfo::of(id, redacted.session()));
                    stored += 1;
                }
                Err(err) => eprintln!("  Session übersprungen (Redaction): {err}"),
            }
        }
        let count = report.sessions.len();
        match &report.note {
            Some(note) => println!("  {}: {note}", report.agent),
            None => println!("  {}: {count} Transkript(e)", report.agent),
        }
    }

    if stored == 0 {
        println!("  nichts zu importieren");
        return Ok(());
    }

    // 3. Commits aus der Historie holen und zuordnen.
    let commits = gather_commits(&root)?;
    let links = match_sessions(&infos, &commits);

    // 4. Den Store-Index um die (vermuteten) Kanten ergänzen — mergend, nicht
    //    ersetzend: eine zweite Ausführung darf bestehende Kanten nicht
    //    verlieren.
    let mut index = store.index()?;
    for link in &links {
        index.link(&link.commit, link.session, Evidence::Inferred);
    }
    store.set_index(&index)?;

    println!(
        "  {stored} Session(s) gespeichert, {} Verknüpfung(en) vermutet",
        links.len()
    );
    Ok(())
}

/// Sammelt die erreichbare Historie ab HEAD als [`CommitInfo`] — Hash, Autor-Zeit
/// und geänderte Dateien.
///
/// Ein einziger `git log`-Lauf: `%x01` markiert die Kopfzeile jedes Commits,
/// darunter stehen seine Dateien (`--name-only`). `--no-renames`, damit ein Pfad
/// so heißt wie im Baum. Merges tragen ohne Diff keine Dateien und fallen damit
/// ohnehin aus dem Matching.
fn gather_commits(root: &Path) -> Fallible<Vec<CommitInfo>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "log",
            "HEAD",
            "--no-renames",
            "--name-only",
            "--format=%x01%H %at",
        ])
        .output()?;
    if !output.status.success() {
        // Kein HEAD (leeres Repo) o. Ä. — dann gibt es nichts zuzuordnen.
        return Ok(Vec::new());
    }
    Ok(parse_log(&String::from_utf8_lossy(&output.stdout)))
}

/// Zerlegt die `git log`-Ausgabe in [`CommitInfo`]s.
fn parse_log(text: &str) -> Vec<CommitInfo> {
    let mut commits: Vec<CommitInfo> = Vec::new();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix('\u{1}') {
            let (hex, epoch) = header.split_once(' ').unwrap_or((header, "0"));
            commits.push(CommitInfo {
                hex: hex.to_string(),
                epoch: epoch.trim().parse().unwrap_or(0),
                files: Vec::new(),
            });
        } else if !line.trim().is_empty() {
            if let Some(current) = commits.last_mut() {
                current.files.push(line.to_string());
            }
        }
    }
    commits
}

fn home_dir() -> Fallible<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME ist nicht gesetzt".into())
}

fn repo_root(repo: &Repo) -> PathBuf {
    repo.git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_log_reads_commits_and_their_files() {
        let text = "\u{1}aaaa 1000\nsrc/a.rs\nsrc/b.rs\n\n\u{1}bbbb 2000\ndocs/x.md\n";
        let commits = parse_log(text);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hex, "aaaa");
        assert_eq!(commits[0].epoch, 1000);
        assert_eq!(commits[0].files, vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(commits[1].hex, "bbbb");
        assert_eq!(commits[1].files, vec!["docs/x.md"]);
    }

    #[test]
    fn a_merge_without_files_is_kept_but_empty() {
        let text = "\u{1}mmmm 3000\n\n\u{1}aaaa 1000\nsrc/a.rs\n";
        let commits = parse_log(text);
        assert_eq!(commits.len(), 2);
        assert!(commits[0].files.is_empty());
        assert_eq!(commits[1].files, vec!["src/a.rs"]);
    }

    #[test]
    fn empty_output_yields_no_commits() {
        assert!(parse_log("").is_empty());
    }
}
