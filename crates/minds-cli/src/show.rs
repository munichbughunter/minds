//! `minds show <commit>` — vom Commit zurück zur Absicht.
//!
//! Der erste Teil des Bug-Retrieval-Flows aus dem Plan, in einem Kommando:
//!
//! ```text
//!   Commit → Trailer lesen → Minds-Session-Id → Store auflösen → Intent zeigen
//! ```
//!
//! Es liest nur. Kein Journal, kein Schreiben, keine Redaction — die ist längst
//! passiert. Steht auf dem Commit kein Trailer, ist das kein Fehler, sondern die
//! ehrliche Auskunft „für diesen Commit ist kein Kontext erfasst".
//!
//! # Was „Attribution" hier heißt
//!
//! Die deterministisch verfügbare Zuschreibung: welcher Agent, welches Modell,
//! wie viele Token, welche Dateien, welche Kanten. Die zeilengenaue
//! Mensch/Agent-Quote (`git blame` bis zum Prompt) ist eine eigene, größere
//! Sache und gehört nicht in dieses schlanke Lese-Kommando.

use std::path::Path;
use std::process::{Command, ExitCode};

use minds_git::{CommitId, Repo};

use crate::config;
use crate::render;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds show` aus. `rev` ist eine Git-Revision (`HEAD`, ein Hash, ein
/// Tag); ohne Angabe wird HEAD gezeigt.
pub fn run(rev: Option<&str>, full: bool) -> ExitCode {
    match show(rev.unwrap_or("HEAD"), full) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds show: {err}");
            ExitCode::FAILURE
        }
    }
}

fn show(rev: &str, full: bool) -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);
    let store = config::load(&root).open(&root)?;

    let commit = resolve(&root, rev).ok_or_else(|| format!("keine solche Revision: {rev}"))?;

    // Trailer (beobachtet) und Store-Index (vermutet) zusammenführen — so
    // erscheinen auch importierte Sessions, die keinen Trailer haben.
    let trailers = repo.session_ids_of(commit)?;
    let index = store.index()?;
    let links = render::merge_links(&trailers, index.links_of(&commit.to_string()));

    // Die stabile Change-Id, falls der Commit eine trägt — sie überlebt Rebase
    // und Squash und bindet die Änderung über ihre Versionen hinweg.
    let header = match commit_change_id(&root, commit) {
        Some(change) => format!("commit {commit} · Change-Id {change}"),
        None => format!("commit {commit}"),
    };

    crate::render::show_links(&header, &links, store.as_ref(), full)
}

/// Die `Minds-Change-Id` aus der Commit-Message — `None`, wenn keine da ist.
fn commit_change_id(root: &Path, commit: CommitId) -> Option<minds_core::ChangeId> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", "-s", "--format=%B", &commit.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let message = String::from_utf8_lossy(&output.stdout);
    minds_core::Trailer::change_id(&message)
}

/// Löst eine Git-Revision zu einem vollen Commit-Hash auf — über `git
/// rev-parse`, weil das jede Schreibweise versteht (HEAD, Tags, Kurzhashes,
/// `HEAD~2`), die selbst zu implementieren müßig wäre.
fn resolve(root: &Path, rev: &str) -> Option<CommitId> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn repo_root(repo: &Repo) -> std::path::PathBuf {
    repo.git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}
