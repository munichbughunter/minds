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

/// Feste Fristen für Ref-Locks, gesetzt über der Repo-Konfiguration.
///
/// `core.filesRefLockTimeout` und `core.packedRefsTimeout` stehen in
/// `.git/config`, und ein negativer Wert heißt dort „ewig warten". Seit EA-06d
/// schreibt der Witness in ein Repo, dessen Konfiguration der beobachtete
/// Agent ändern kann: Ein `-1` plus eine liegengelassene `.lock`-Datei hielte
/// seinen einzigen Schreiber für immer an. Zwei Sekunden liegen über Gits
/// Vorgaben (100 ms bzw. 1 s); wer bewusst länger warten lässt, wartet bei
/// Minds trotzdem höchstens zwei Sekunden — Minds schreibt Refs nur kurz und
/// selten, ein verpasster Lock ist dort ein vertagter Schreibvorgang.
const LOCK_TIMEOUTS: [&str; 2] = [
    "core.filesRefLockTimeout=2000",
    "core.packedRefsTimeout=2000",
];

/// Ein geöffnetes Git-Repository.
///
/// Das Handle ist billig zu halten, aber nicht `Sync` — gix cacht intern beim
/// Lesen. Wer parallel arbeiten will, öffnet pro Thread ein eigenes `Repo`.
///
/// `Clone` teilt die einmal geladene Konfiguration: Wer ein festgehaltenes
/// Repo an einen Store weitergibt, prüft und schreibt über dieselbe
/// Konfiguration — nicht über eine zweite, später gelesene (EA-10).
#[derive(Clone)]
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
        let mut options = gix::sec::trust::Mapping::<gix::open::Options>::default();
        options
            .full
            .modify(|opts| opts.config_overrides(LOCK_TIMEOUTS));
        options
            .reduced
            .modify(|opts| opts.config_overrides(LOCK_TIMEOUTS));
        gix::ThreadSafeRepository::discover_opts(
            start,
            gix::discover::upwards::Options::default(),
            options,
        )
        .map(|repo| Self::from_gix(repo.into()))
        .map_err(|err| GitError::discover(start, err))
    }

    /// Öffnet genau `path` — entweder das Arbeitsverzeichnis eines Repos oder
    /// dessen `.git`-Verzeichnis (auch ein bares Repo).
    ///
    /// Sucht **nicht** nach oben. Ist `path` kein Repository, ist das
    /// [`GitError::Open`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        gix::open_opts(
            path,
            gix::open::Options::default().config_overrides(LOCK_TIMEOUTS),
        )
        .map(Self::from_gix)
        .map_err(|err| GitError::open(path, err))
    }

    /// Öffnet genau das Git-Verzeichnis `git_dir`, ohne `include.path` und
    /// `includeIf` aus seiner Konfiguration zu folgen.
    ///
    /// Der Weg für den Witness (EA-10): Er arbeitet auf dem Host in einem
    /// `.git`, dessen Konfiguration der beobachtete Agent schreiben kann. Ein
    /// Include auf ein FIFO hielte ihn an, eines auf eine fremde Datei lenkte
    /// seine Konfiguration um. Das Verzeichnis selbst ist in `witness.json`
    /// festgehalten; gesucht wird nichts. Die übrigen Quellen (System, Nutzer)
    /// bleiben, damit etwa die Committer-Identität des Hosts gilt.
    pub fn open_pinned(git_dir: impl AsRef<Path>) -> Result<Self> {
        let git_dir = git_dir.as_ref();
        let mut permissions = gix::open::Permissions::all();
        permissions.config.includes = false;
        gix::open_opts(
            git_dir,
            gix::open::Options::default()
                .permissions(permissions)
                .open_path_as_is(true)
                // Eine Liste: `config_overrides` ersetzt, statt zu ergänzen.
                // `refs/replace/*` tauschte Objekte still aus — ein Blob, den
                // niemand im Review sah, stünde für den Witness an HEAD.
                // `gitoxide.objects.noReplace` liest gix 0.85 nicht; wirksam
                // ist `core.useReplaceRefs=true` — gix liest es verkehrt
                // (`true` schaltet die Ersatzobjekte ab). Ein Test mit beiden
                // Repo-Einstellungen pinnt das.
                .config_overrides(LOCK_TIMEOUTS.iter().copied().chain([
                    "gitoxide.objects.noReplace=true",
                    "core.useReplaceRefs=true",
                ])),
        )
        .map(Self::from_gix)
        .map_err(|err| GitError::open(git_dir, err))
    }

    /// Die repo-relativen Pfade im Index — was `git ls-files` liefert, ohne
    /// einen `git`-Prozess, der Includes oder `core.fsmonitor` aus der
    /// Repo-Konfiguration folgte (EA-10).
    ///
    /// `None`, wenn der Index nicht lesbar ist oder größer als `max_bytes`:
    /// Ein Index, den jemand auf Millionen Einträge aufbläht, füllt so nicht
    /// den Speicher. Fehlt der Index, ist die Menge leer. Pfade, die kein
    /// UTF-8 sind, fehlen in der Menge — wie bei `git ls-files` zuvor: Sie
    /// gelten als untracked und bekommen keinen Read-Hash (die fail-closed
    /// Richtung). Die Größe wird vor dem Öffnen geprüft; wer die Datei
    /// dazwischen tauscht, trifft im Witness den Worker mit Frist und
    /// Ressourcengrenzen.
    pub fn tracked_paths(&self, max_bytes: u64) -> Option<Vec<String>> {
        match std::fs::symlink_metadata(self.inner.index_path()) {
            Ok(meta) if meta.is_file() && meta.len() <= max_bytes => {}
            Ok(_) => return None,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
            Err(_) => return None,
        }
        let index = self.inner.open_index().ok()?;
        Some(
            index
                .entries()
                .iter()
                .filter_map(|entry| std::str::from_utf8(entry.path(&index)).ok())
                .map(str::to_owned)
                .collect(),
        )
    }

    /// Entfernt die Lock-Dateien, die gix gerade hält — aus einem
    /// Signal-Handler heraus, best effort (gix vermeidet Locks und
    /// Freigaben; das Entfernen eines sehr langen Pfads kann allokieren). Der
    /// Witness-Worker ruft das bei `SIGTERM` an der Frist, damit kein
    /// `HEAD.lock` oder `refs/minds/….lock` den nächsten Lauf oder das nächste
    /// `git commit` blockiert (EA-10).
    pub fn cleanup_lock_files_signal_safe() {
        gix::tempfile::registry::cleanup_tempfiles_signal_safe();
    }

    /// Ob gix Refs in einem Namespace führt (`gitoxide.core.refsNamespace`,
    /// `GIT_NAMESPACE`): Dann liegen alle Refs — auch `refs/minds/*` — unter
    /// `refs/namespaces/<ns>/…`.
    pub fn has_ref_namespace(&self) -> bool {
        self.inner.namespace().is_some()
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
    pub fn common_dir(&self) -> &Path {
        self.inner.common_dir()
    }

    /// Der Arbeitsbaum mit den ausgecheckten Dateien — `None` bei einem
    /// baren Repository. Im verlinkten Worktree dessen eigenes Verzeichnis,
    /// nicht das Elternverzeichnis von [`git_dir`](Self::git_dir).
    pub fn workdir(&self) -> Option<&Path> {
        self.inner.workdir()
    }

    /// Das gix-Handle. Crate-intern — siehe `error.rs` zur Fassade.
    pub(crate) fn gix(&self) -> &gix::Repository {
        &self.inner
    }

    /// Ein zweiter Zugriff auf dieselbe Objektdatenbank für Ansichten, die
    /// Objekte lesen, die ein Agent geschrieben haben kann: Jede Allokation
    /// beim Lesen eines Objekts — auch beim Auflösen von Delta-Ketten in
    /// Packs — ist auf `alloc_limit` Bytes begrenzt (eine zlib-Bombe wird
    /// nie entpackt); Objekt-Caches sind aus (ihre Größe stünde sonst in der
    /// vom Agenten beschreibbaren Konfiguration); Ersatzobjekte sind aus;
    /// Includes der Konfiguration und Objekt-Umgebungsvariablen
    /// (`GIT_ALLOC_LIMIT`) werden nicht gelesen. Die Overrides gewinnen über
    /// die Repo-Konfiguration.
    pub(crate) fn bounded(&self, alloc_limit: u64) -> Result<gix::Repository> {
        let git_dir = self.git_dir();
        let mut permissions = gix::open::Permissions::all();
        permissions.config.includes = false;
        // Kein `git`-Binary, um dessen Konfigurationsorte zu erfragen.
        permissions.config.git_binary = false;
        permissions.env.objects = gix::sec::Permission::Deny;
        let overrides = [
            format!("gitoxide.objects.allocLimit={alloc_limit}"),
            "gitoxide.objects.cacheLimit=0".to_owned(),
            "core.deltaBaseCacheLimit=0".to_owned(),
            // gix 0.85 liest `core.useReplaceRefs` verkehrt: `true` schaltet
            // die Ersatzobjekte **ab** (`gitoxide.objects.noReplace` liest es
            // gar nicht). Ein Test pinnt das — und bricht, wenn gix es
            // korrigiert.
            "core.useReplaceRefs=true".to_owned(),
        ];
        let mut repo = gix::open_opts(
            git_dir,
            gix::open::Options::default()
                .permissions(permissions)
                .open_path_as_is(true)
                .config_overrides(overrides),
        )
        .map_err(|err| GitError::open(git_dir, err))?;
        // Ein fehlendes Objekt liest das Pack-Verzeichnis nicht jedes Mal neu
        // ein (das könnte ein Agent aufblähen).
        repo.objects.refresh_never();
        Ok(repo)
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
    fn open_pinned_ignores_config_includes() {
        let fixture = TempRepo::init();
        let git_dir = fixture.path().join(".git");
        let included = fixture.path().join("included.config");
        std::fs::write(&included, "[gitoxide \"core\"]\n\trefsNamespace = agent\n").unwrap();
        let config = git_dir.join("config");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str(&format!("[include]\n\tpath = {}\n", included.display()));
        std::fs::write(&config, text).unwrap();

        // Gegenprobe: Der gewöhnliche Weg folgt dem Include.
        assert!(Repo::open(&git_dir).unwrap().has_ref_namespace());
        let pinned = Repo::open_pinned(&git_dir).unwrap();
        assert!(!pinned.has_ref_namespace());
        assert!(pinned.git_dir().ends_with(".git"));
    }

    /// `refs/replace/*` tauscht für den festgehaltenen Zugriff nichts aus.
    #[test]
    fn open_pinned_ignores_replace_refs() {
        let fixture = TempRepo::init();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(fixture.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        std::fs::write(fixture.path().join("policy.json"), "reviewed").unwrap();
        git(&["add", "policy.json"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "policy",
        ]);
        let original = git(&["rev-parse", "HEAD:policy.json"]);
        std::fs::write(fixture.path().join("other"), "swapped").unwrap();
        let swapped = git(&["hash-object", "-w", "other"]);
        git(&["replace", &original, &swapped]);
        assert_eq!(git(&["cat-file", "blob", "HEAD:policy.json"]), "swapped");

        let head: crate::oid::CommitId = git(&["rev-parse", "HEAD"]).parse().unwrap();
        // Auch wenn die Repo-Konfiguration (vom Agenten beschreibbar) die
        // Ersatzobjekte ausdrücklich einschaltet — gix 0.85 liest den
        // Schlüssel verkehrt, `false` hieße dort „an".
        for setting in ["true", "false"] {
            git(&["config", "core.useReplaceRefs", setting]);
            let repo = Repo::open_pinned(fixture.path().join(".git")).unwrap();
            let (id, bytes) = repo
                .read_blob_bounded(head, "policy.json", 1024)
                .unwrap()
                .unwrap();
            assert_eq!(bytes, b"reviewed", "{setting}");
            assert_eq!(id, original, "{setting}");
        }
        let repo = Repo::open_pinned(fixture.path().join(".git")).unwrap();
        assert!(repo.read_blob_bounded(head, "policy.json", 3).is_err());
        assert_eq!(repo.read_blob_bounded(head, "missing", 1024).unwrap(), None);

        // Die Bytes hinter der geprüften Id austauschen: Die Loose-Datei des
        // geprüften Blobs bekommt den Inhalt eines anderen. gix prüft beim
        // Lesen nicht nach — `read_blob_bounded` schon.
        let objects = fixture.path().join(".git/objects");
        let loose = |id: &str| objects.join(&id[..2]).join(&id[2..]);
        std::fs::remove_file(loose(&original)).unwrap();
        std::fs::copy(loose(&swapped), loose(&original)).unwrap();
        let repo = Repo::open_pinned(fixture.path().join(".git")).unwrap();
        let err = repo
            .read_blob_bounded(head, "policy.json", 1024)
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("does not match its id"),
            "{err:?}"
        );
    }

    /// Ein unsortierter Baum (git und gix fänden darin Verschiedenes) wird
    /// abgelehnt, nicht gelesen.
    #[test]
    fn read_blob_bounded_refuses_an_unsorted_tree() {
        let fixture = TempRepo::init();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(fixture.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        std::fs::write(fixture.path().join("policy"), "weak").unwrap();
        let blob = git(&["hash-object", "-w", "policy"]);
        let raw = |hex: &str| {
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<u8>>()
        };
        // Innerer Baum `.minds/redact.json`, sortiert.
        let mut inner = b"100644 redact.json\0".to_vec();
        inner.extend(raw(&blob));
        std::fs::write(fixture.path().join("inner"), &inner).unwrap();
        let inner_id = git(&["hash-object", "-t", "tree", "-w", "--literally", "inner"]);
        // Äußerer Baum: `zz` vor `.minds` — falsch herum.
        let mut outer = b"100644 zz\0".to_vec();
        outer.extend(raw(&blob));
        outer.extend(b"40000 .minds\0");
        outer.extend(raw(&inner_id));
        std::fs::write(fixture.path().join("outer"), &outer).unwrap();
        let outer_id = git(&["hash-object", "-t", "tree", "-w", "--literally", "outer"]);
        let commit = git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit-tree",
            &outer_id,
            "-m",
            "unsorted",
        ]);
        let repo = Repo::open_pinned(fixture.path().join(".git")).unwrap();
        let commit: crate::oid::CommitId = commit.parse().unwrap();
        let err = repo
            .read_blob_bounded(commit, ".minds/redact.json", 1024)
            .unwrap_err();
        assert!(format!("{err:?}").contains("not sorted"), "{err:?}");
    }

    #[test]
    fn tracked_paths_reads_the_index_and_respects_the_limit() {
        let fixture = TempRepo::init();
        let repo = Repo::open_pinned(fixture.path().join(".git")).unwrap();
        assert_eq!(repo.tracked_paths(1024), Some(Vec::new()), "kein Index");
        std::fs::write(fixture.path().join("a.txt"), "a").unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(fixture.path())
            .args(["add", "a.txt"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            repo.tracked_paths(1024 * 1024),
            Some(vec!["a.txt".to_owned()])
        );
        assert_eq!(repo.tracked_paths(8), None, "über der Grenze");
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
