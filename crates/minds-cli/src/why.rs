//! `minds why <datei>:<zeile>` — von der Zeile zurück zur Session, die sie schrieb.
//!
//! Der ganze Bug-Retrieval-Flow aus dem Plan, in einem Kommando:
//!
//! ```text
//!   Zeile → git blame → Commit → Trailer → Minds-Session-Id → Store → Session
//! ```
//!
//! Das ist der Moment, um den es in der Vision geht: auf eine Zeile zeigen und
//! den Prompt dahinter sehen. `show` beantwortet „was steckt hinter diesem
//! Commit", `why` beantwortet „hinter dieser einen Zeile" — und braucht dafür
//! nur einen Schritt mehr, das Blame.
//!
//! # Der Pfad ist repo-relativ
//!
//! `git blame` spricht in Pfaden relativ zur Repo-Wurzel, nicht zum
//! Arbeitsverzeichnis. `minds why src/retry.rs:42` erwartet den Pfad deshalb so,
//! wie ihn Git im Baum führt. Das aus einem Unterverzeichnis heraus umzurechnen
//! ist eine spätere Bequemlichkeit, kein Kern des Flows.

use std::process::ExitCode;

use minds_git::{BlameProvider, Repo};

use crate::config;
use crate::render;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds why` aus. `target` ist `<datei>:<zeile>`.
pub fn run(target: Option<&str>, full: bool) -> ExitCode {
    let Some(target) = target else {
        eprintln!("minds why: erwartet <datei>:<zeile>");
        return ExitCode::FAILURE;
    };
    match why(target, full) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds why: {err}");
            ExitCode::FAILURE
        }
    }
}

fn why(target: &str, full: bool) -> Fallible<()> {
    let (path, line) = split(target)?;

    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);
    let store = config::load(&root).open(&root)?;

    let Some(head) = repo.head()?.commit() else {
        return Err("HEAD hat noch keinen Commit".into());
    };

    // Blame der Zeile, wie sie im aktuellen Baum steht.
    let Some(commit) = repo.blame().blame_line(head, path, line)? else {
        return Err(format!("{path}:{line} ist im Blame nicht auflösbar").into());
    };

    let trailers = repo.session_ids_of(commit)?;
    let index = store.index()?;
    let links = render::merge_links(&trailers, index.links_of(&commit.to_string()));

    render::show_links(
        &format!("{path}:{line} → commit {commit}"),
        &links,
        store.as_ref(),
        full,
    )
}

/// Zerlegt `<datei>:<zeile>` in Pfad und Zeilennummer. Getrennt wird am
/// **letzten** Doppelpunkt, damit Pfade mit Doppelpunkt (selten, aber möglich)
/// nicht zerbrechen.
fn split(target: &str) -> Fallible<(&str, u32)> {
    let (path, line) = target.rsplit_once(':').ok_or("erwartet <datei>:<zeile>")?;
    let line: u32 = line
        .parse()
        .map_err(|_| format!("keine Zeilennummer: {line:?}"))?;
    if path.is_empty() || line == 0 {
        return Err("erwartet <datei>:<zeile> mit Zeile ≥ 1".into());
    }
    Ok((path, line))
}

fn repo_root(repo: &Repo) -> std::path::PathBuf {
    repo.git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn splits_path_and_line() {
        assert_eq!(split("src/retry.rs:42").unwrap(), ("src/retry.rs", 42));
    }

    #[test]
    fn splits_at_the_last_colon() {
        assert_eq!(split("weird:name.rs:7").unwrap(), ("weird:name.rs", 7));
    }

    #[test]
    fn rejects_missing_or_zero_line() {
        assert!(split("src/retry.rs").is_err());
        assert!(split("src/retry.rs:0").is_err());
        assert!(split("src/retry.rs:abc").is_err());
        assert!(split(":42").is_err());
    }
}
