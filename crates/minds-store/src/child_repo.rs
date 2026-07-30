//! [`ChildRepoStore`] — der Kontext liegt in einem **eigenen** Repository.
//!
//! Derselbe Baum, dieselben Pfade, dasselbe Ref — nur ein anderes
//! [`Repo`]-Handle. Das ist keine Beschreibung, sondern die Bauform: Beide
//! Backends sind Hüllen um denselben [`GitStore`], und
//! `both_backends_write_the_same_tree` weist nach, dass dabei bis auf den
//! Baum-Hash genau dasselbe herauskommt.
//!
//! # Wofür man das will
//!
//! - Das Produktions-Repo bleibt sauber und klont schnell — Transkripte können
//!   groß werden, und niemand will sie bei jedem CI-Klon mitziehen.
//! - Eigene Zugriffsrechte und eigene Aufbewahrungsfristen für den Kontext.
//! - Kein CI-Lärm im Parent durch Kontext-Pushes.
//!
//! Der Preis ist ein zweites Repository, das existieren, erreichbar und
//! eingerichtet sein muss. Deshalb ist das In-Repo-Backend der Default und
//! dieses hier eine Entscheidung.
//!
//! # Die eine Regel, die man nicht brechen darf
//!
//! **Der Trailer bleibt im Parent-Commit.** Die Nutzlast zieht um, der Verweis
//! nicht — sonst bricht das Bug-Retrieval, sobald jemand nur den Parent
//! ausgecheckt hat. Praktisch heißt das: [`ChildRepoStore::context_repo`] ist
//! **nicht** das Repository, an dessen Commits Trailer gehängt werden. Wer
//! beides braucht, öffnet das Code-Repo getrennt (`Repo::discover`).
//!
//! ```text
//! Parent-Repo                        Child-Repo
//! ├─ Code + Commits mit Trailer  →   └─ refs/minds/context (nur JSON)
//! ```
//!
//! # Was hier bewusst nicht passiert: anlegen
//!
//! [`ChildRepoStore::open`] öffnet ein vorhandenes Repository und meldet
//! [`StoreError::ChildRepo`], wenn dort keines ist. Es legt keines an. Ein
//! Store, der bei einem Tippfehler im Pfad stillschweigend ein zweites, leeres
//! Kontext-Repo erzeugt, verwandelt einen Konfigurationsfehler in
//! Datenverlust-mit-Ansage. Anlegen und Klonen ist Sache von `minds init` (M6)
//! — dafür braucht `minds-git` noch ein `Repo::init_bare`.
//!
//! # Die Identität wird aus dem Child-Repo gelesen
//!
//! `commit_tree_to_ref` besteht auf einer Git-Identität, und gix liest sie aus
//! der Konfiguration **des Repositories, in das geschrieben wird**. Bei einem
//! frisch angelegten, baren Child-Repo ohne lokale `user.name`/`user.email`
//! greift die globale Konfiguration des Entwicklers — und wo es die nicht gibt
//! (CI-Container, Air-Gap), scheitert der erste `put` mit
//! `GitError::Identity`. `minds init` sollte die Identität deshalb beim Anlegen
//! mitschreiben.

use std::path::Path;

use minds_core::{Evidence, SessionId};
use minds_git::{DEFAULT_CONTEXT_REF, Repo};
use minds_redact::RedactedSession;

use crate::bytes::SessionBytes;
use crate::error::{Result, StoreError};
use crate::git_store::GitStore;
use crate::index::CommitIndex;
use crate::store::{ContextStore, Forget, Put};

/// Der Kontext-Store in einem separaten Repository.
#[derive(Debug)]
pub struct ChildRepoStore(GitStore);

impl ChildRepoStore {
    /// Öffnet das Kontext-Repository unter `path`.
    ///
    /// `path` zeigt auf das Arbeitsverzeichnis oder direkt auf das
    /// Git-Verzeichnis; ein bares Repository ist der Normalfall, weil dort
    /// niemand arbeitet. Es wird **nicht** nach oben gesucht: Ein konfigurierter
    /// Pfad soll genau das Repository treffen, das dasteht, und nicht
    /// versehentlich dessen Elternverzeichnis.
    ///
    /// # Fehler
    ///
    /// [`StoreError::ChildRepo`], wenn dort kein Repository liegt — mit dem
    /// Pfad in der Meldung, denn das ist die Information, die fehlt.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Repo::open(path)
            .map(Self::from_repo)
            .map_err(|err| StoreError::child_repo(path, err))
    }

    /// Nimmt ein bereits geöffnetes Kontext-Repository.
    pub fn from_repo(repo: Repo) -> Self {
        Self(GitStore::new(repo, DEFAULT_CONTEXT_REF))
    }

    /// Legt den Ref fest, unter dem der Store liegt (Default:
    /// [`DEFAULT_CONTEXT_REF`]).
    ///
    /// Derselbe Name wie beim In-Repo-Backend, und das ist Absicht: Ein Umzug
    /// zwischen den Backends soll nichts am Layout ändern.
    pub fn with_ref(self, reference: impl Into<String>) -> Self {
        Self(self.0.with_ref(reference))
    }

    /// Das Repository, in dem der Kontext liegt — **nicht** das des Codes.
    /// Siehe Modul-Doku.
    pub fn context_repo(&self) -> &Repo {
        self.0.context_repo()
    }

    /// Der Ref, unter dem dieser Store liegt.
    pub fn reference(&self) -> &str {
        self.0.reference()
    }
}

impl ContextStore for ChildRepoStore {
    fn put_bytes(&self, session: &SessionBytes) -> Result<Put> {
        self.0.put_bytes(session)
    }

    fn get_bytes(&self, id: SessionId) -> Result<Option<Vec<u8>>> {
        self.0.get_bytes(id)
    }

    fn list(&self) -> Result<Vec<SessionId>> {
        self.0.list()
    }

    fn get_index_bytes(&self) -> Result<Option<Vec<u8>>> {
        self.0.get_index_bytes()
    }

    fn put_index_bytes(&self, bytes: &[u8]) -> Result<()> {
        self.0.put_index_bytes(bytes)
    }

    fn forget(&self, id: SessionId, reason: &str) -> Result<Forget> {
        self.0.forget(id, reason)
    }

    fn index(&self) -> Result<CommitIndex> {
        self.0.index()
    }

    fn link(&self, session: SessionId, commit_hex: &str, evidence: Evidence) -> Result<()> {
        self.0.link(session, commit_hex, evidence)
    }

    /// Legt den Session-Branch im Kontext-Repo an — hier, nicht im Default,
    /// weil die Branches beim Push dieses separaten Repos in der Forge sichtbar
    /// werden sollen (siehe [`ContextStore::put_session_branch`]).
    fn put_session_branch(&self, session: &RedactedSession) -> Result<()> {
        let bytes = SessionBytes::of(session)?;
        // Neben der maßgeblichen session.json eine gerenderte session.md, damit
        // GitLab den Branch nativ als lesbare Seite zeigt (Track C). Der Renderer
        // lebt in minds-core, weil der Store nicht vom Reader abhängen darf.
        let markdown = minds_core::session_markdown(bytes.id(), session.session());
        self.0
            .write_session_branch(&bytes, &markdown)
            .map(|_| ())
            .map_err(StoreError::backend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InRepoStore;
    use crate::fixture::{TempRepo, redacted};

    /// Ein Parent mit Code und ein bares Child daneben.
    fn parent_and_child() -> (TempRepo, TempRepo, ChildRepoStore) {
        let parent = TempRepo::init();
        parent.write_file("src/lib.rs", "fn main() {}\n");
        parent.commit("code");

        let child = TempRepo::init_bare();
        let store = ChildRepoStore::open(child.path()).unwrap();

        (parent, child, store)
    }

    #[test]
    fn a_bare_child_repository_holds_the_context() {
        // Bar ist der Normalfall: Im Kontext-Repo arbeitet niemand, es gibt
        // dort nichts auszuchecken.
        let (_parent, child, store) = parent_and_child();
        let session = redacted("Retry-Test reparieren");

        let id = store.put(&session).unwrap().id();

        assert_eq!(store.get(id).unwrap().as_ref(), Some(session.session()));
        assert_eq!(store.list().unwrap(), vec![id]);
        let refs = child.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"]);
        assert!(
            refs.contains("refs/minds/store/"),
            "die Nutzlast liegt im Child, unter ihrem eigenen Ref: {refs}"
        );
    }

    #[test]
    fn both_backends_write_the_same_tree() {
        // Die zentrale Zusage des Plans: „identischer Baum, nur in einem
        // separaten Repo". Zwei Repositories, die nichts voneinander wissen,
        // kommen bei derselben Session auf denselben Baum-Hash — schärfer als
        // jeder Vergleich von Pfadlisten.
        let session = redacted("Retry-Test reparieren");

        let (_parent, child, child_store) = parent_and_child();
        child_store.put(&session).unwrap();

        let alone = TempRepo::init();
        alone.write_file("src/lib.rs", "fn main() {}\n");
        alone.commit("code");
        InRepoStore::open(alone.path())
            .unwrap()
            .put(&session)
            .unwrap();

        // Derselbe Session-Ref, derselbe Baum-Hash — in zwei Repositories, die
        // nichts voneinander wissen.
        let id = session.session().id().unwrap();
        let reference = format!(
            "refs/minds/store/{}^{{tree}}",
            id.to_string().strip_prefix("b3-").unwrap()
        );
        assert_eq!(child.hash(&reference), alone.hash(&reference));
    }

    #[test]
    fn the_parent_repository_never_learns_about_it() {
        // Der Grund, warum man das Backend überhaupt wählt: Das Repo des Codes
        // bleibt unberührt — kein Ref, kein Objekt, kein Klon-Ballast.
        let (parent, _child, store) = parent_and_child();
        let objects_before = parent.object_count();

        store.put(&redacted("Retry-Test reparieren")).unwrap();

        assert!(
            parent
                .git(&["for-each-ref", "--format=%(refname)", "refs/minds/"])
                .trim()
                .is_empty(),
            "der Parent hat einen Minds-Ref bekommen"
        );
        assert_eq!(parent.object_count(), objects_before);
    }

    #[test]
    fn a_missing_child_repository_is_reported_with_its_path() {
        // Der wahrscheinlichste Konfigurationsfehler. Die Meldung muss den Pfad
        // nennen, sonst sucht man im falschen Repo.
        let empty = tempfile::tempdir().unwrap();

        let err = ChildRepoStore::open(empty.path()).unwrap_err();

        assert!(
            matches!(err, StoreError::ChildRepo { .. }),
            "erwartet ChildRepo, war: {err:?}"
        );
        assert!(
            err.to_string()
                .contains(&empty.path().display().to_string())
        );
    }

    #[test]
    fn each_session_becomes_its_own_browsable_branch() {
        // Der Kern des Child-Backends für die Forge: eine Session → ein Ref
        // unter refs/minds/sessions/, dessen Baum sie allein als session.json
        // trägt. Der Push mappt ihn später auf einen Branch minds/session/<hash>,
        // den GitLab anzeigt und auswählbar macht.
        let (_parent, child, store) = parent_and_child();
        let session = redacted("Retry-Test reparieren");

        store.put_session_branch(&session).unwrap();

        let refs: Vec<String> = child
            .git(&[
                "for-each-ref",
                "--format=%(refname)",
                "refs/minds/sessions/",
            ])
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            refs.len(),
            1,
            "erwartet genau einen Session-Ref, war: {refs:?}"
        );

        // Der Baum trägt beide Dateien: die maßgebliche session.json und die von
        // GitLab nativ gerenderte session.md (Track C).
        let files = child.git(&["ls-tree", "-r", "--name-only", &refs[0]]);
        let mut names: Vec<&str> = files.lines().map(str::trim).collect();
        names.sort();
        assert_eq!(names, vec!["session.json", "session.md"]);

        // Die session.md ist echtes Markdown mit der Absicht.
        let md = child.git(&["cat-file", "blob", &format!("{}:session.md", refs[0])]);
        assert!(md.contains("# Retry-Test reparieren"), "session.md:\n{md}");

        // Elternlos: ein Branch, eine Session — keine gemeinsame Kette, die ein
        // Klon nur eines Branches mitzöge.
        let parents = child.git(&["log", "--format=%P", "-1", &refs[0]]);
        assert!(
            parents.trim().is_empty(),
            "Session-Branch hat Eltern: {parents}"
        );
    }

    #[test]
    fn two_sessions_get_two_distinct_branches() {
        let (_parent, child, store) = parent_and_child();

        store.put_session_branch(&redacted("Fall A")).unwrap();
        store.put_session_branch(&redacted("Fall B")).unwrap();

        let count = child
            .git(&[
                "for-each-ref",
                "--format=%(refname)",
                "refs/minds/sessions/",
            ])
            .lines()
            .count();
        assert_eq!(count, 2, "zwei Sessions, aber nicht zwei Branches");
    }

    #[test]
    fn the_session_branch_is_idempotent() {
        // Content-adressiert und create-once: ein zweiter Lauf legt weder einen
        // zweiten Ref noch einen neuen Commit an. Genau das lässt den Push ohne
        // --force auskommen — nie ein non-fast-forward.
        let (_parent, child, store) = parent_and_child();
        let session = redacted("Retry-Test reparieren");

        store.put_session_branch(&session).unwrap();
        let reference = child
            .git(&[
                "for-each-ref",
                "--format=%(refname)",
                "refs/minds/sessions/",
            ])
            .trim()
            .to_owned();
        let before = child.hash(&reference);

        store.put_session_branch(&session).unwrap();

        let after: Vec<String> = child
            .git(&[
                "for-each-ref",
                "--format=%(refname)",
                "refs/minds/sessions/",
            ])
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(after, vec![reference.clone()], "ein zweiter Ref entstand");
        assert_eq!(
            child.hash(&reference),
            before,
            "neuer Commit trotz gleicher Session"
        );
    }

    #[test]
    fn the_two_backends_read_each_others_layout() {
        // Ein Umzug zwischen den Backends ist ein Kopiervorgang, kein Umbau:
        // Was das eine schreibt, findet das andere unter derselben ID.
        let (_parent, child, store) = parent_and_child();
        let session = redacted("Retry-Test reparieren");
        let id = store.put(&session).unwrap().id();

        // Dasselbe Repo, nur durch die andere Hülle gelesen.
        let as_in_repo = InRepoStore::open(child.path()).unwrap();

        assert_eq!(
            as_in_repo.get(id).unwrap().as_ref(),
            Some(session.session())
        );
    }
}
