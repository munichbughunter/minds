//! Welches Backend, welcher Ref — und wie daraus ein offener Store wird.
//!
//! Der Plan verspricht, dass der Unterschied zwischen In-Repo und Child-Repo
//! „eine Config-Zeile, kein Rewrite" ist. Dieses Modul ist die Einlösung:
//! [`StoreConfig::open`] entscheidet **einmal**, an einer sichtbaren Stelle,
//! und gibt einen `Box<dyn ContextStore>` heraus. Alles darüber — `minds
//! capture`, `minds why`, der Reader — sieht nur noch den Trait und weiß nicht,
//! wo die Sessions liegen.
//!
//! # Beide Wege sind erstklassig
//!
//! - **Ohne Child-Repo** ([`Backend::InRepo`], der Default): Der Kontext liegt
//!   im Repository des Codes. Nichts einzurichten, nichts zu erreichen, im
//!   Air-Gap vollständig.
//! - **Mit Child-Repo** ([`Backend::ChildRepo`]): Der Kontext liegt nebenan.
//!   Das Repo des Codes bleibt unberührt.
//!
//! Was in **beiden** Fällen gilt und der häufigste Denkfehler ist: Das
//! Code-Repository wird immer gebraucht. [`StoreConfig::open`] sucht es
//! deshalb auch beim Child-Backend zuerst — zum einen, weil relative Pfade
//! daran hängen (siehe unten), zum anderen, weil der Trailer dort hingehört und
//! ein „kein Git-Repository gefunden" die ehrlichere erste Fehlermeldung ist als
//! „Kontext-Repository nicht gefunden".
//!
//! # Relative Pfade hängen an der Repo-Wurzel, nicht am Arbeitsverzeichnis
//!
//! `minds` wird aus Unterverzeichnissen aufgerufen. Läge `../minds-kontext`
//! relativ zum Arbeitsverzeichnis, zeigte dieselbe Konfiguration je nach `cd`
//! woanders hin — ein Fehler, der sich als „mal geht's, mal nicht" zeigt.
//! Deshalb wird gegen die Wurzel des Code-Repositories aufgelöst. Absolute
//! Pfade bleiben, wie sie sind.
//!
//! Die Wurzel wird aus dem Git-Verzeichnis abgeleitet (`…/.git` → dessen
//! Elternverzeichnis, bares Repo → es selbst). Für verlinkte Worktrees
//! (`.git/worktrees/…`) trifft diese Ableitung daneben; dort hilft ein
//! absoluter Pfad, bis `minds-git` ein `Repo::work_dir` anbietet.
//!
//! # Noch nicht hier: das Dateiformat
//!
//! [`StoreConfig`] ist ein Wert, kein Dateiformat — bewusst ohne serde. Wo die
//! Konfiguration liegt, entscheidet `minds init` (M6), und der naheliegende Ort
//! ist `.git/config` (`minds.backend`, `minds.contextRef`, `minds.childPath`):
//! keine neue Datei, pro Klon gültig, von Git verwaltet. Ein serde-Format hier
//! würde diese Entscheidung vorwegnehmen — und ein Feld, das man später
//! umbenennen will, ist dann schon geschrieben.

use std::path::{Path, PathBuf};

use minds_git::{DEFAULT_CONTEXT_REF, Repo};

use crate::child_repo::ChildRepoStore;
use crate::error::{Result, StoreError};
use crate::in_repo::InRepoStore;
use crate::store::ContextStore;

/// Wo der Kontext liegt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// Im Repository des Codes selbst. Der Default.
    InRepo,

    /// In einem separaten Repository.
    ChildRepo {
        /// Pfad zum Kontext-Repository. Relativ wird gegen die Wurzel des
        /// Code-Repositories aufgelöst (siehe Modul-Doku).
        path: PathBuf,
    },
}

/// Backend und Ref — alles, was den Speicherort festlegt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfig {
    backend: Backend,
    reference: String,
}

impl Default for StoreConfig {
    /// In-Repo unter `refs/minds/context` — die Einstellung, für die niemand
    /// etwas tun muss.
    fn default() -> Self {
        Self {
            backend: Backend::InRepo,
            reference: DEFAULT_CONTEXT_REF.to_owned(),
        }
    }
}

impl StoreConfig {
    /// Der Kontext liegt im Repository des Codes.
    pub fn in_repo() -> Self {
        Self::default()
    }

    /// Der Kontext liegt im Repository unter `path`.
    pub fn child_repo(path: impl Into<PathBuf>) -> Self {
        Self {
            backend: Backend::ChildRepo { path: path.into() },
            ..Self::default()
        }
    }

    /// Legt den Ref fest (Default: [`DEFAULT_CONTEXT_REF`]).
    pub fn with_ref(self, reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            ..self
        }
    }

    /// Das konfigurierte Backend.
    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    /// Der konfigurierte Ref.
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// Öffnet den konfigurierten Store, ausgehend von `start`.
    ///
    /// `start` ist irgendein Verzeichnis im Code-Repository — typisch das
    /// Arbeitsverzeichnis der CLI. Von dort wird nach oben gesucht, wie `git`
    /// es tut.
    ///
    /// # Fehler
    ///
    /// - [`StoreError::Backend`] — ab `start` ist kein Repository zu finden.
    ///   Gilt für **beide** Backends: Ohne Code-Repo gibt es keinen Commit, an
    ///   den ein Verweis gehören könnte.
    /// - [`StoreError::ChildRepo`] — das konfigurierte Kontext-Repository ist
    ///   nicht da.
    pub fn open(&self, start: impl AsRef<Path>) -> Result<Box<dyn ContextStore>> {
        let code = Repo::discover(start.as_ref()).map_err(StoreError::backend)?;

        match &self.backend {
            Backend::InRepo => Ok(Box::new(
                InRepoStore::from_repo(code).with_ref(self.reference.as_str()),
            )),
            Backend::ChildRepo { path } => {
                let path = resolve_against_repo(&code, path);
                Ok(Box::new(
                    ChildRepoStore::open(path)?.with_ref(self.reference.as_str()),
                ))
            }
        }
    }
}

/// Löst `path` gegen die Wurzel von `repo` auf; absolute Pfade bleiben, wie sie
/// sind.
fn resolve_against_repo(repo: &Repo, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_owned();
    }
    repo_root(repo).join(path)
}

/// Die Wurzel eines Repositories, aus seinem Git-Verzeichnis abgeleitet.
///
/// `…/projekt/.git` → `…/projekt`; ein bares Repository ist seine eigene
/// Wurzel. Zu den Grenzen dieser Ableitung siehe Modul-Doku.
fn repo_root(repo: &Repo) -> PathBuf {
    let git_dir = repo.git_dir();

    if git_dir.file_name().is_some_and(|name| name == ".git") {
        git_dir.parent().unwrap_or(git_dir).to_owned()
    } else {
        git_dir.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{TempRepo, init_bare_at, redacted};

    /// Ein Code-Repository mit einem Commit.
    fn code_repo() -> TempRepo {
        let fixture = TempRepo::init();
        fixture.write_file("src/lib.rs", "fn main() {}\n");
        fixture.commit("code");
        fixture
    }

    // --- Ohne Child-Repo -----------------------------------------------------

    #[test]
    fn the_default_writes_into_the_repository_of_the_code() {
        let code = code_repo();

        let store = StoreConfig::default().open(code.path()).unwrap();
        let id = store.put(&redacted("Retry-Test reparieren")).unwrap().id();

        assert!(store.get(id).unwrap().is_some());
        let refs = code.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"]);
        assert!(refs.contains("refs/minds/store/"), "{refs}");
    }

    #[test]
    fn the_default_is_in_repo_under_the_context_ref() {
        let config = StoreConfig::default();

        assert_eq!(config.backend(), &Backend::InRepo);
        assert_eq!(config.reference(), DEFAULT_CONTEXT_REF);
    }

    // --- Mit Child-Repo ------------------------------------------------------

    #[test]
    fn the_child_backend_writes_next_door() {
        let code = code_repo();
        let child = TempRepo::init_bare();

        let store = StoreConfig::child_repo(child.path())
            .open(code.path())
            .unwrap();
        let id = store.put(&redacted("Retry-Test reparieren")).unwrap().id();

        assert!(store.get(id).unwrap().is_some());
        let refs = child.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"]);
        assert!(refs.contains("refs/minds/store/"), "{refs}");
        assert!(
            code.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"])
                .trim()
                .is_empty(),
            "der Parent hat einen Minds-Ref bekommen"
        );
    }

    #[test]
    fn a_relative_child_path_is_resolved_against_the_repository_root() {
        // Und nicht gegen das Arbeitsverzeichnis: Derselbe Aufruf aus einem
        // Unterverzeichnis muss dasselbe Kontext-Repo treffen. Läge die
        // Auflösung am `cd`, zeigte dieselbe Konfiguration mal hierhin und mal
        // dorthin.
        let code = code_repo();
        init_bare_at(&code.path().join("kontext.git"));
        let config = StoreConfig::child_repo("kontext.git");

        let id = config
            .open(code.path())
            .unwrap()
            .put(&redacted("Retry-Test reparieren"))
            .unwrap()
            .id();
        let from_subdir = config.open(code.path().join("src")).unwrap();

        assert!(from_subdir.get(id).unwrap().is_some());
    }

    #[test]
    fn a_missing_child_repository_is_reported_before_anything_is_written() {
        let code = code_repo();

        // Kein `unwrap_err`: Der Erfolgsfall ist ein `Box<dyn ContextStore>`,
        // und der Trait fordert kein `Debug` — zu Recht, denn es hätte nur
        // diese eine Zeile bequemer gemacht.
        let Err(err) = StoreConfig::child_repo("gibt-es-nicht").open(code.path()) else {
            panic!("ein fehlendes Kontext-Repository muss auffallen");
        };

        assert!(
            matches!(err, StoreError::ChildRepo { .. }),
            "erwartet ChildRepo, war: {err:?}"
        );
        assert!(
            code.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"])
                .trim()
                .is_empty(),
            "es wurde ersatzweise in den Parent geschrieben"
        );
    }

    // --- Gemeinsames ---------------------------------------------------------

    #[test]
    fn without_a_repository_there_is_nothing_to_open() {
        // Gilt für beide Backends: Ohne Code-Repo gibt es keinen Commit, an den
        // ein Verweis gehören könnte.
        let nowhere = tempfile::tempdir().unwrap();
        let child = TempRepo::init_bare();

        for config in [
            StoreConfig::in_repo(),
            StoreConfig::child_repo(child.path()),
        ] {
            let Err(err) = config.open(nowhere.path()) else {
                panic!("ohne Code-Repository darf kein Store entstehen: {config:?}");
            };
            assert!(
                matches!(err, StoreError::Backend { .. }),
                "erwartet Backend, war: {err:?}"
            );
        }
    }

    #[test]
    fn the_configured_ref_is_used_by_both_backends() {
        let local_ref = "refs/minds/local/test";
        let code = code_repo();
        let child = TempRepo::init_bare();

        // Die Nutzlast liegt seit dem Umzug unter ihrem eigenen Ref; der
        // konfigurierte Ref trägt den Index. Geprüft wird deshalb an ihm.
        let index = crate::CommitIndex::new();
        StoreConfig::in_repo()
            .with_ref(local_ref)
            .open(code.path())
            .unwrap()
            .set_index(&index)
            .unwrap();
        StoreConfig::child_repo(child.path())
            .with_ref(local_ref)
            .open(code.path())
            .unwrap()
            .set_index(&index)
            .unwrap();

        assert_eq!(
            code.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"])
                .trim(),
            local_ref
        );
        assert_eq!(
            child
                .git(&["for-each-ref", "--format=%(refname)", "refs/minds/"])
                .trim(),
            local_ref
        );
    }
}
