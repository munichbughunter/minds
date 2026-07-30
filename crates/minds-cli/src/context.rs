//! Geteilte Helfer für die Rückführungs-Kommandos `recall`/`distill`/`brief`:
//! Repo und Store öffnen, Sessions laden, ein Ziel zu Sessions auflösen.
//!
//! `show` und `why` haben ihre eigene, schlankere Auflösung (ein Commit, eine
//! Zeile). Die Rückführung braucht mehr — *alle* Sessions, Sessions zu einer
//! Datei — deshalb steht das hier gebündelt statt in jedem Kommando erneut.

use std::path::{Path, PathBuf};
use std::process::Command;

use minds_core::{Session, SessionId};
use minds_git::{BlameProvider, CommitId, Repo};
use minds_store::ContextStore;

use crate::config;
use crate::render;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Ein geöffnetes Repo samt konfiguriertem Store.
pub struct Context {
    pub repo: Repo,
    pub root: PathBuf,
    pub store: Box<dyn ContextStore>,
}

impl Context {
    /// Öffnet das Repo um das aktuelle Verzeichnis und den daran konfigurierten
    /// Store.
    pub fn open() -> Fallible<Self> {
        let cwd = std::env::current_dir()?;
        let repo = Repo::discover(&cwd)?;
        let root = repo
            .git_dir()
            .parent()
            .unwrap_or_else(|| repo.git_dir())
            .to_path_buf();
        let store = config::load(&root).open(&root)?;
        Ok(Self { repo, root, store })
    }

    /// Alle redigierten Sessions im Store, in Store-Reihenfolge (sortiert).
    pub fn all_sessions(&self) -> Fallible<Vec<Session>> {
        let mut out = Vec::new();
        for id in self.store.list()? {
            if let Some(session) = self.store.get(id)? {
                out.push(session);
            }
        }
        Ok(out)
    }

    /// Die Sessions hinter einem Commit **mit ihrer Id** — Trailer (beobachtet)
    /// und Store-Index (vermutet) zusammengeführt, wie bei `show`/`why`.
    pub fn linked_sessions(&self, commit: CommitId) -> Fallible<Vec<(SessionId, Session)>> {
        let trailers = self.repo.session_ids_of(commit)?;
        let index = self.store.index()?;
        let links = render::merge_links(&trailers, index.links_of(&commit.to_string()));
        let mut out = Vec::new();
        for (id, _) in links {
            if let Some(session) = self.store.get(id)? {
                out.push((id, session));
            }
        }
        Ok(out)
    }

    /// Wie [`linked_sessions`](Self::linked_sessions), aber ohne die Ids.
    pub fn sessions_of_commit(&self, commit: CommitId) -> Fallible<Vec<Session>> {
        Ok(self
            .linked_sessions(commit)?
            .into_iter()
            .map(|(_, session)| session)
            .collect())
    }

    /// Alle Sessions, die `path` geändert haben.
    pub fn sessions_touching(&self, path: &str) -> Fallible<Vec<Session>> {
        Ok(self
            .all_sessions()?
            .into_iter()
            .filter(|session| touches(session, path))
            .collect())
    }

    /// Löst eine Git-Revision (`HEAD`, Hash, Tag) zu einem Commit auf.
    pub fn resolve_rev(&self, rev: &str) -> Option<CommitId> {
        resolve(&self.root, rev)
    }

    /// Der Commit hinter `datei:zeile` via Blame — `None`, wenn HEAD leer ist
    /// oder die Zeile nicht auflösbar.
    pub fn blame_commit(&self, path: &str, line: u32) -> Fallible<Option<CommitId>> {
        let Some(head) = self.repo.head()?.commit() else {
            return Ok(None);
        };
        Ok(self.repo.blame().blame_line(head, path, line)?)
    }
}

/// `true`, wenn die Session `path` verändert hat — über die erzeugten Dateien
/// oder einen Effekt mit diesem Pfad.
pub fn touches(session: &Session, path: &str) -> bool {
    session.produced.files.iter().any(|f| f == path)
        || session.turns.iter().any(|turn| {
            turn.tool_calls.iter().any(|call| {
                call.effect
                    .as_ref()
                    .and_then(|effect| effect.path.as_deref())
                    .is_some_and(|p| p == path)
            })
        })
}

/// Ein grob sortierbarer Zeit-Schlüssel: die ersten 19 Zeichen von
/// `lineage.started_at` (`YYYY-MM-DDTHH:MM:SS`) — fixe Breite, lexikografisch
/// vergleichbar. Best-effort: fehlt die Herkunft, leer. Bewusst *keine* echte
/// Datums-Dependency (siehe `minds-core::lineage`); für „die jüngsten Sessions"
/// reicht der Präfix-Vergleich, weil Agent-Zeitstempel praktisch immer UTC-`Z`
/// mit gleichem Aufbau sind.
pub fn time_key(session: &Session) -> &str {
    session
        .lineage
        .as_ref()
        .and_then(|lineage| lineage.started_at.as_deref())
        .map(|ts| ts.get(..19).unwrap_or(ts))
        .unwrap_or("")
}

/// `git rev-parse` — versteht jede Schreibweise, die selbst zu implementieren
/// müßig wäre (siehe `show`).
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
