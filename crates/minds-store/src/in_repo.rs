//! [`InRepoStore`] — der Kontext liegt im selben Repository wie der Code.
//!
//! Das Backend ohne Voraussetzungen: kein zweites Repo, kein Setup, kein
//! Remote. Wer Minds ausprobiert, klont sein Projekt und legt los; wer im
//! Air-Gap arbeitet, hat mit dem einen Klon alles beisammen. Deshalb ist es der
//! Default (siehe [`StoreConfig`](crate::StoreConfig)).
//!
//! Die Arbeit macht [`GitStore`]; dieser Typ setzt nur das Repository ein, in
//! dem auch der Code liegt. Was er zusätzlich mitbringt, sind die Wege
//! *hinein*: [`InRepoStore::discover`] sucht das Repository so, wie `git` es
//! tut — von einem Unterverzeichnis aus nach oben. Genau das braucht die CLI,
//! denn `minds why src/retry.rs:42` wird selten in der Wurzel aufgerufen.
//!
//! # Hier sind Kontext-Repo und Code-Repo dasselbe — anderswo nicht
//!
//! [`InRepoStore::context_repo`] gibt dasselbe Repository zurück, aus dem auch
//! die Commits kommen, an die der Trailer gehört. Das verleitet dazu, es für
//! beides zu benutzen. Beim [`ChildRepoStore`](crate::ChildRepoStore) ist es ein
//! **anderes**, und ein Trailer, der dort landet, hängt am falschen Commit. Wer
//! beides braucht, holt sich das Code-Repo selbst (`Repo::discover`) und nimmt
//! `context_repo` nur für den Kontext. Der Name sagt es — die
//! Verwechslungsgefahr bleibt trotzdem und ist der Grund, warum er nicht mehr
//! `repo` heißt.

use std::path::Path;

use minds_core::{Evidence, SessionId};
use minds_git::{DEFAULT_CONTEXT_REF, Repo};

use crate::bytes::SessionBytes;
use crate::error::{Result, StoreError};
use crate::git_store::GitStore;
use crate::index::CommitIndex;
use crate::store::{ContextStore, Forget, Put};

/// Der Kontext-Store im Repository des Codes, unter einem eigenen Ref.
#[derive(Debug)]
pub struct InRepoStore(GitStore);

impl InRepoStore {
    /// Sucht ab `start` aufwärts nach einem Repository — der Weg für die CLI,
    /// die aus irgendeinem Unterverzeichnis aufgerufen wird.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        Repo::discover(start)
            .map(Self::from_repo)
            .map_err(StoreError::backend)
    }

    /// Öffnet genau `path` — der Weg für Tests und für eine Konfiguration mit
    /// explizitem Pfad.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Repo::open(path)
            .map(Self::from_repo)
            .map_err(StoreError::backend)
    }

    /// Nimmt ein bereits geöffnetes Repository.
    pub fn from_repo(repo: Repo) -> Self {
        Self(GitStore::new(repo, DEFAULT_CONTEXT_REF))
    }

    /// Legt den Ref fest, unter dem der Store liegt (Default:
    /// [`DEFAULT_CONTEXT_REF`]).
    pub fn with_ref(self, reference: impl Into<String>) -> Self {
        Self(self.0.with_ref(reference))
    }

    /// Das Repository, in dem der Kontext liegt — hier dasselbe wie das des
    /// Codes. Siehe Modul-Doku, warum das keine allgemeine Zusage ist.
    pub fn context_repo(&self) -> &Repo {
        self.0.context_repo()
    }

    /// Der Ref, unter dem dieser Store liegt.
    pub fn reference(&self) -> &str {
        self.0.reference()
    }
}

impl ContextStore for InRepoStore {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{TempRepo, redacted};

    #[test]
    fn discover_finds_the_repository_from_a_subdirectory() {
        // `minds why` wird selten in der Wurzel aufgerufen.
        let fixture = TempRepo::init();
        fixture.write_file("src/lib.rs", "fn main() {}\n");
        fixture.commit("code");

        let store = InRepoStore::discover(fixture.path().join("src")).unwrap();
        let id = store
            .put(&redacted("aus dem Unterverzeichnis"))
            .unwrap()
            .id();

        assert!(store.get(id).unwrap().is_some());
        let refs = fixture.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"]);
        assert!(refs.contains("refs/minds/store/"), "{refs}");
    }

    #[test]
    fn the_wrapper_forwards_linking_to_the_session_ref() {
        // Diese Hülle leitet jede Trait-Methode einzeln durch. Eine neue Methode
        // zu vergessen fällt nicht auf — sie fällt still auf den Trait-Default
        // zurück, und der schreibt wieder in den gemeinsamen Index. Genau das
        // ist hier einmal passiert; deshalb steht der Test hier und nicht nur
        // auf dem inneren Store.
        let fixture = TempRepo::init();
        fixture.write_file("src/lib.rs", "fn main() {}\n");
        fixture.commit("code");
        let store = InRepoStore::open(fixture.path()).unwrap();
        let id = store.put(&redacted("Retry-Test reparieren")).unwrap().id();

        store.link(id, "deadbeef", Evidence::Observed).unwrap();

        assert_eq!(
            store.get_index_bytes().unwrap(),
            None,
            "der gemeinsame Index wurde doch angefasst"
        );
        assert_eq!(store.index().unwrap().links_of("deadbeef").len(), 1);
    }

    #[test]
    fn the_default_ref_is_the_context_ref() {
        let fixture = TempRepo::init();
        let store = InRepoStore::open(fixture.path()).unwrap();

        assert_eq!(store.reference(), DEFAULT_CONTEXT_REF);
    }

    #[test]
    fn the_context_repository_is_the_one_holding_the_code() {
        let fixture = TempRepo::init();
        let store = InRepoStore::open(fixture.path()).unwrap();

        assert!(store.context_repo().git_dir().ends_with(".git"));
    }

    #[test]
    fn the_in_repo_backend_creates_no_session_branch() {
        // Punkt 8 der Definition of Done: Wer Minds in-repo nutzt, bekommt keine
        // sichtbaren Branches. put_session_branch ist hier der Trait-Default —
        // ein No-op —, damit die Session-Branches nur im *Child*-Repo entstehen
        // und nie in dem des Codes.
        let fixture = TempRepo::init();
        fixture.write_file("src/lib.rs", "fn main() {}\n");
        fixture.commit("code");
        let store = InRepoStore::open(fixture.path()).unwrap();

        store.put(&redacted("Retry-Test reparieren")).unwrap();
        store
            .put_session_branch(&redacted("Retry-Test reparieren"))
            .unwrap();

        assert!(
            fixture
                .git(&[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/minds/sessions/"
                ])
                .trim()
                .is_empty(),
            "das In-Repo-Backend hat einen Session-Branch angelegt"
        );
    }
}
