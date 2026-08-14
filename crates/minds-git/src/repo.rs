//! [`Repo`] — das Handle auf ein Git-Repository.
//!
//! Zwei Wege hinein, und der Unterschied ist wichtig:
//!
//! - [`Repo::discover`] sucht **von einem Verzeichnis aus nach oben**, so wie
//!   `git` selbst. Das ist der Weg für die CLI: `minds why src/retry.rs:42`
//!   wird aus irgendeinem Unterverzeichnis aufgerufen, nicht aus der Wurzel.
//! - [`Repo::open`] öffnet **genau den angegebenen Pfad** (Arbeitsverzeichnis
//!   oder `.git` direkt). Das ist der Weg für alles Programmatische: Tests,
//!   Child-Repo-Backend (M4), Konfiguration mit explizitem Pfad. Kein Suchen,
//!   keine Überraschung, welches Repo man erwischt hat.
//!
//! `Repo` ist absichtlich fast leer: Es hält das gix-Handle und gibt es
//! crate-intern weiter. Die eigentlichen Fähigkeiten hängen als `impl`-Blöcke
//! in den Modulen, zu denen sie thematisch gehören (`head.rs`,
//! `walk.rs`, später Refs und Objekte). So bleibt jede Datei über *ein*
//! Thema lesbar, statt dass ein 800-Zeilen-`impl Repo` alles einsammelt.

use std::fmt;
use std::path::Path;

use crate::error::{GitError, Result, Source};

/// Ein geöffnetes Git-Repository.
///
/// Das Handle ist billig zu halten, aber nicht `Sync` — gix cacht intern beim
/// Lesen. Wer parallel arbeiten will, öffnet pro Thread ein eigenes `Repo`.
pub struct Repo {
    inner: gix::Repository,
}

impl Repo {
    /// Sucht ab `start` aufwärts nach einem Repository — wie `git` es tut.
    ///
    /// Findet auch dann etwas, wenn `start` tief im Arbeitsverzeichnis liegt.
    /// Wird nichts gefunden, ist das [`GitError::Discover`] — der erwartbare
    /// Fall „hier ist kein Repo", nicht ein Defekt.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        let start = start.as_ref();
        gix::discover(start)
            .map(Self::from_gix)
            .map_err(|err| GitError::discover(start, err))
    }

    /// Öffnet genau `path` — entweder das Arbeitsverzeichnis eines Repos oder
    /// dessen `.git`-Verzeichnis (auch ein bares Repo).
    ///
    /// Sucht **nicht** nach oben. Ist `path` kein Repository, ist das
    /// [`GitError::Open`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        gix::open(path)
            .map(Self::from_gix)
            .map_err(|err| GitError::open(path, err))
    }

    /// Das Git-Verzeichnis dieses Repositories (`…/.git`, bei einem baren Repo
    /// das Repo selbst).
    ///
    /// Dorthin schreibt M3 später den Kontext-Ref `refs/minds/context`.
    pub fn git_dir(&self) -> &Path {
        self.inner.git_dir()
    }

    /// Das **geteilte** Git-Verzeichnis — im verlinkten Worktree das des
    /// Hauptbaums.
    ///
    /// [`git_dir`](Self::git_dir) ist dort worktree-privat
    /// (`.git/worktrees/<name>`), die Refs unter `refs/minds/*` liegen aber im
    /// gemeinsamen Verzeichnis. Alles, was Ref-Schreiber **repo-weit**
    /// serialisieren muss (das Sidecar-Lock aus `refs.rs`), gehört hierher —
    /// sonst nähmen zwei Worktrees verschiedene Locks für dieselben Refs.
    pub(crate) fn common_dir(&self) -> &Path {
        self.inner.common_dir()
    }

    /// Das gix-Handle. Crate-intern — siehe `error.rs` zur Fassade.
    pub(crate) fn gix(&self) -> &gix::Repository {
        &self.inner
    }

    /// Baut einen Fehler, der dieses Repository benennt. Spart in jedem
    /// `map_err` den Pfad-Boilerplate.
    pub(crate) fn err_head(&self, source: impl Into<Source>) -> GitError {
        GitError::head(self.git_dir().to_path_buf(), source)
    }

    fn from_gix(inner: gix::Repository) -> Self {
        Self { inner }
    }
}

/// Zeigt den Pfad statt gix' vollständigem Innenleben — ein `{repo:?}` in einer
/// Fehlermeldung soll lesbar bleiben.
impl fmt::Debug for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Repo")
            .field("git_dir", &self.git_dir())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::TempRepo;

    #[test]
    fn open_accepts_worktree_root() {
        let fixture = TempRepo::init();
        let repo = Repo::open(fixture.path()).unwrap();
        // Nicht gegen den Fixture-Pfad vergleichen: Temp-Verzeichnisse sind auf
        // macOS über Symlinks erreichbar (`/var` → `/private/var`), gix löst
        // sie auf. Der Suffix ist die belastbare Aussage.
        assert!(repo.git_dir().ends_with(".git"));
    }

    #[test]
    fn open_accepts_git_dir_directly() {
        let fixture = TempRepo::init();
        assert!(Repo::open(fixture.path().join(".git")).is_ok());
    }

    #[test]
    fn discover_walks_up_from_subdirectory() {
        let fixture = TempRepo::init();
        let deep = fixture.path().join("crates/minds-git/src");
        std::fs::create_dir_all(&deep).unwrap();

        let repo = Repo::discover(&deep).unwrap();
        assert!(repo.git_dir().ends_with(".git"));
    }

    #[test]
    fn open_rejects_a_plain_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = Repo::open(dir.path()).unwrap_err();
        assert!(matches!(err, GitError::Open { .. }));
    }

    #[test]
    fn discover_reports_when_nothing_is_found() {
        // Setzt voraus, dass das Temp-Verzeichnis nicht selbst in einem Repo
        // liegt — bei einem TMPDIR unterhalb eines Klons schlägt das fehl.
        let dir = tempfile::tempdir().unwrap();
        let err = Repo::discover(dir.path()).unwrap_err();
        assert!(matches!(err, GitError::Discover { .. }));
    }
}
