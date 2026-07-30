//! `minds render --out ./site` — die statische Seite bauen.
//!
//! Zustandslos und wiederholbar: Es gibt nichts einzurichten und nichts zu
//! betreiben. Der Aufruf liest Git und Store, schreibt ein Verzeichnis und
//! endet. Zweimal aufgerufen entsteht dasselbe Ergebnis.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use minds_git::Repo;

use crate::config;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Voreinstellung für `--out`.
const DEFAULT_OUT: &str = "site";

/// Führt `minds render` aus.
pub fn run(out: Option<&str>) -> ExitCode {
    match render(Path::new(out.unwrap_or(DEFAULT_OUT))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds render: {err}");
            ExitCode::FAILURE
        }
    }
}

fn render(out: &Path) -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);
    let store = config::load(&root).open(&root)?;

    let site = minds_reader::render(&repo, store.as_ref(), out)?;

    println!(
        "  {} Datei(en), {} Session(s) → {}",
        site.files,
        site.sessions,
        site.out.display()
    );
    if site.skipped > 0 {
        println!(
            "  {} Datei(en) übersprungen (kein UTF-8 oder Blame nicht möglich)",
            site.skipped
        );
    }
    println!("  öffnen: {}", site.out.join("index.html").display());
    Ok(())
}

fn repo_root(repo: &Repo) -> PathBuf {
    repo.git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}
