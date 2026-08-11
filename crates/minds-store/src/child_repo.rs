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
        //
        // Der Reanimations-Schutz (#6) — eine vergessene Session darf nicht als
        // Klartext-`session.md` zurückkehren, auch nicht unter einem
        // nebenläufigen `forget` — steckt vollständig in
        // `put_session_branch_bytes`; siehe dessen Doku.
        let markdown = minds_core::session_markdown(bytes.id(), session.session());
        self.0.put_session_branch_bytes(&bytes, &markdown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InRepoStore;
    use crate::fixture::{TempRepo, redacted};
    use crate::store::ForgottenPlace;

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
    fn forget_erases_the_session_branch_including_the_markdown() {
        // Akzeptanzkriterium #5: put + put_session_branch + forget, danach darf
        // `session.md` den Klartext nicht mehr tragen. Ohne den Branch-Zweig in
        // `forget` bliebe die gerenderte Absicht als Forge-Branch lesbar — ein
        // DSGVO-Verstoß mit Erfolgsmeldung.
        const NEEDLE: &str = "Datenschutz-Loeschung-Pruefwort-4711";
        let (_parent, child, store) = parent_and_child();
        let session = redacted(NEEDLE);
        let id = session.session().id().unwrap();

        store.put(&session).unwrap();
        store.put_session_branch(&session).unwrap();

        let branch = child.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/sessions/",
        ]);
        let branch = branch
            .lines()
            .next()
            .expect("ein Session-Branch")
            .to_owned();

        // Vorbedingung: Der Klartext steht wirklich in beiden Dateien.
        let md_before = child.git(&["cat-file", "blob", &format!("{branch}:session.md")]);
        assert!(
            md_before.contains(NEEDLE),
            "session.md ohne Klartext:\n{md_before}"
        );

        let result = store.forget(id, "DSGVO").unwrap();

        // Beide angelegten Orte werden getilgt und in fester Reihenfolge
        // benannt: der maßgebliche Store-Ref zuerst, dann der Branch. Kein
        // Ort wird übersprungen, keiner mehrfach gezählt.
        assert_eq!(
            result.places(),
            &[ForgottenPlace::StoreRef, ForgottenPlace::SessionBranch],
            "unerwartete Orte oder Reihenfolge"
        );

        // Der Ref bleibt auflösbar (cat-file wirft sonst) — aber inhaltsfrei.
        let md_after = child.git(&["cat-file", "blob", &format!("{branch}:session.md")]);
        assert!(
            !md_after.contains(NEEDLE),
            "session.md leckt nach forget:\n{md_after}"
        );
        let json_after = child.git(&["cat-file", "blob", &format!("{branch}:session.json")]);
        assert!(
            !json_after.contains(NEEDLE),
            "session.json leckt nach forget:\n{json_after}"
        );

        // Und der Baum trägt keine dritte Datei — der frische Baum ersetzt
        // vollständig, statt aufzusetzen.
        let files = child.git(&["ls-tree", "-r", "--name-only", &branch]);
        let mut names: Vec<&str> = files.lines().map(str::trim).collect();
        names.sort();
        assert_eq!(names, vec!["session.json", "session.md"]);
    }

    #[test]
    fn a_repeated_put_session_branch_after_forget_does_not_resurrect_the_markdown() {
        // Der zweite Reanimations-Vektor (#6): Nach `forget` darf ein erneuter
        // `put_session_branch` — der nächste Capture-Lauf — den Klartext nicht
        // wieder als `session.md` auf die Forge schreiben.
        const NEEDLE: &str = "Reanimations-Pruefwort-8842";
        let (_parent, child, store) = parent_and_child();
        let session = redacted(NEEDLE);
        let id = session.session().id().unwrap();

        store.put(&session).unwrap();
        store.put_session_branch(&session).unwrap();
        store.forget(id, "DSGVO").unwrap();

        // Der Wiederholungslauf.
        store.put_session_branch(&session).unwrap();

        let branch = child.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/sessions/",
        ]);
        let branch = branch.lines().next().expect("ein Session-Branch");
        let md = child.git(&["cat-file", "blob", &format!("{branch}:session.md")]);
        assert!(!md.contains(NEEDLE), "session.md reanimiert:\n{md}");
        let json = child.git(&["cat-file", "blob", &format!("{branch}:session.json")]);
        assert!(!json.contains(NEEDLE), "session.json reanimiert:\n{json}");
    }

    #[test]
    fn writing_the_branch_over_its_tombstone_is_refused() {
        // Variante 2 des Branch-Rennens (#6): Der Branch existiert und ist nach
        // `forget` getombsteint. Ein direkter `write_session_branch` — der
        // nebenläufige Capture, dessen Vor-Check den Tombstone verpasste — darf
        // den Klartext NICHT auf den Branch-Tombstone aufsetzen. Der atomare
        // Guard prüft den Parent und lehnt ab (`Ok(None)`); ohne ihn setzte der
        // Klartext-Baum als `Advanced` auf den Tombstone auf.
        const NEEDLE: &str = "Branch-Tombstone-Overwrite-Pruefwort-6003";
        let (_parent, child, store) = parent_and_child();
        let session = redacted(NEEDLE);
        let id = session.session().id().unwrap();
        let bytes = SessionBytes::of(&session).unwrap();
        let markdown = minds_core::session_markdown(bytes.id(), session.session());

        store.put(&session).unwrap();
        store.put_session_branch(&session).unwrap();
        store.forget(id, "DSGVO").unwrap();

        // Der direkte Schreibversuch, der den Vor-Check der oberen Schicht umgeht.
        let outcome = store.0.write_session_branch(&bytes, &markdown).unwrap();
        assert!(
            outcome.is_none(),
            "der Guard hätte den Tombstone-Parent ablehnen müssen"
        );

        let branch = child.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/sessions/",
        ]);
        let branch = branch.lines().next().expect("ein Session-Branch");
        let md = child.git(&["cat-file", "blob", &format!("{branch}:session.md")]);
        assert!(!md.contains(NEEDLE), "session.md reanimiert:\n{md}");
        let json = child.git(&["cat-file", "blob", &format!("{branch}:session.json")]);
        assert!(!json.contains(NEEDLE), "session.json reanimiert:\n{json}");
    }

    #[test]
    fn a_plaintext_branch_left_over_a_forgotten_store_ref_gets_tombstoned() {
        // Variante 1 des Branch-Rennens (#6), der cross-ref-Fall: Der Vor-Check
        // sah den Store-Ref als Klartext (Rennen), der Branch wurde frisch mit
        // Klartext angelegt, während der Store-Ref bereits getombsteint war —
        // `forget` sah den Branch nie, weil er noch nicht existierte. Der nächste
        // `put_session_branch` muss diesen zurückgebliebenen Klartext-Branch
        // selbst tilgen (Post-Check gegen den maßgeblichen Store-Ref).
        const NEEDLE: &str = "Branch-Race-Reanimation-Pruefwort-6002";
        let (_parent, child, store) = parent_and_child();
        let session = redacted(NEEDLE);
        let id = session.session().id().unwrap();
        let bytes = SessionBytes::of(&session).unwrap();
        let markdown = minds_core::session_markdown(bytes.id(), session.session());

        // Store-Ref anlegen und tilgen — der Branch existiert dabei nicht.
        store.put(&session).unwrap();
        store.forget(id, "DSGVO").unwrap();

        // Der Race-Ausgang: ein frischer Klartext-Branch trotz getombsteintem
        // Store-Ref, direkt geschrieben (umgeht den Vor-Check). Er entsteht, weil
        // der Branch keinen eigenen Tombstone-Parent hat, den der Guard sähe.
        let outcome = store.0.write_session_branch(&bytes, &markdown).unwrap();
        assert!(
            outcome.is_some(),
            "der frische Branch entsteht (kein Branch-eigener Tombstone)"
        );
        let branch = child.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/sessions/",
        ]);
        let branch = branch
            .lines()
            .next()
            .expect("ein Session-Branch")
            .to_owned();
        // Vorbedingung: Der Klartext steht wirklich auf dem Branch.
        let md_before = child.git(&["cat-file", "blob", &format!("{branch}:session.md")]);
        assert!(md_before.contains(NEEDLE), "Testaufbau: kein Klartext da");

        // Der reguläre Weg. Weil der Store-Ref hier schon getombsteint ist, greift
        // der **Vor-Check** (Stufe 1): Er sieht die Session als vergessen und tilgt
        // den zurückgebliebenen Klartext-Branch, bevor er überhaupt schreibt. (Der
        // Post-Check aus Stufe 3 nutzt dieselbe `tombstone_branch_if_plaintext`,
        // nur für den nebenläufigen Fall, in dem der Vor-Check den Tombstone noch
        // nicht sah.)
        store.put_session_branch(&session).unwrap();

        let md = child.git(&["cat-file", "blob", &format!("{branch}:session.md")]);
        assert!(!md.contains(NEEDLE), "session.md nicht getilgt:\n{md}");
        let json = child.git(&["cat-file", "blob", &format!("{branch}:session.json")]);
        assert!(
            !json.contains(NEEDLE),
            "session.json nicht getilgt:\n{json}"
        );
    }

    #[test]
    fn a_cleaned_up_branch_tombstone_names_the_store_ref_reason() {
        // Wird ein im Rennen zurückgebliebener Klartext-Branch nachträglich
        // getilgt, soll sein Tombstone denselben Grund nennen wie der maßgebliche
        // Store-Ref — nicht einen generischen Platzhalter. So bleibt die
        // Löschbegründung an beiden Orten dieselbe.
        const NEEDLE: &str = "Branch-Grund-Gleichlauf-Pruefwort-6004";
        const REASON: &str = "DSGVO-Art-17-Loeschbegehren";
        let (_parent, child, store) = parent_and_child();
        let session = redacted(NEEDLE);
        let id = session.session().id().unwrap();
        let bytes = SessionBytes::of(&session).unwrap();
        let markdown = minds_core::session_markdown(bytes.id(), session.session());

        store.put(&session).unwrap();
        store.forget(id, REASON).unwrap();
        // Der Race-Ausgang: ein Klartext-Branch trotz getombsteintem Store-Ref.
        store.0.write_session_branch(&bytes, &markdown).unwrap();

        // Der Cleanup tilgt den Branch und schreibt den Store-Ref-Grund hinein.
        store.put_session_branch(&session).unwrap();

        let branch = child.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/sessions/",
        ]);
        let branch = branch.lines().next().expect("ein Session-Branch");
        let json = child.git(&["cat-file", "blob", &format!("{branch}:session.json")]);
        assert!(
            json.contains(REASON),
            "Branch-Tombstone nennt den Store-Ref-Grund nicht:\n{json}"
        );
        assert!(
            !json.contains(NEEDLE),
            "Klartext im Branch-Tombstone:\n{json}"
        );
    }

    #[test]
    fn put_session_branch_after_forget_without_a_prior_branch_stays_empty() {
        // Der scharfe Fall (#6): Eine Session, die es nur als Store-Ref gibt —
        // so legt `import` sie ab, ganz ohne Branch. `forget` tombsteint dann
        // nur den Store-Ref. Käme der Reanimations-Schutz allein aus dem
        // Branch-Ref, sähe er hier nichts (den Branch gibt es nie), und der
        // nächste Capture legte ihn mit Klartext als `session.md` neu an. Der
        // Guard muss deshalb den *Store-Ref* konsultieren.
        const NEEDLE: &str = "Branch-ohne-Store-Tombstone-Pruefwort-6001";
        let (_parent, child, store) = parent_and_child();
        let session = redacted(NEEDLE);
        let id = session.session().id().unwrap();

        // Nur der Store-Ref — bewusst KEIN put_session_branch vor dem forget.
        store.put(&session).unwrap();
        store.forget(id, "DSGVO").unwrap();

        // Der Wiederholungslauf, der den Branch erstmals anlegen wollte.
        store.put_session_branch(&session).unwrap();

        // Es darf kein Branch mit Klartext entstanden sein. Am strengsten:
        // gar kein Session-Branch (der Guard griff, bevor einer angelegt wurde).
        let branches = child.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/sessions/",
        ]);
        for branch in branches.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let md = child.git(&["cat-file", "blob", &format!("{branch}:session.md")]);
            assert!(
                !md.contains(NEEDLE),
                "session.md reanimiert auf {branch}:\n{md}"
            );
            let json = child.git(&["cat-file", "blob", &format!("{branch}:session.json")]);
            assert!(
                !json.contains(NEEDLE),
                "session.json reanimiert auf {branch}:\n{json}"
            );
        }
    }

    #[test]
    fn forgetting_without_a_branch_still_names_the_store_ref() {
        // Der In-Repo-Fall (kein Session-Branch): `forget` tilgt den Store-Ref
        // und benennt genau den — nicht den Branch, den es nicht gibt.
        let (_parent, child, store) = parent_and_child();
        let session = redacted("nur im Store");
        let id = session.session().id().unwrap();
        store.put(&session).unwrap();

        let result = store.forget(id, "DSGVO").unwrap();
        assert!(result.was_forgotten());
        assert!(result.places().contains(&ForgottenPlace::StoreRef));
        assert!(!result.places().contains(&ForgottenPlace::SessionBranch));

        // Kein Session-Branch entstand.
        let branches = child.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/sessions/",
        ]);
        assert!(
            branches.trim().is_empty(),
            "unerwarteter Branch: {branches}"
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

    #[test]
    fn a_forget_that_breaks_off_names_the_open_place() {
        // #14 (b): Bricht die Tilgung nach dem ersten Ort ab, meldet der Fehler
        // die schon getilgten Orte und den offenen — statt eines nackten
        // Backend-Fehlers, der die halbe Löschung unsichtbar ließe.
        const NEEDLE: &str = "Teiltilgung-Pruefwort-1401";
        let (_parent, child, store) = parent_and_child();
        let session = redacted(NEEDLE);
        let id = session.session().id().unwrap();
        store.put(&session).unwrap();
        store.put_session_branch(&session).unwrap();

        // Der Guard bricht am Branch ab — der Store-Ref ist da schon getilgt.
        let result = store.0.forget_guarded(id, "DSGVO", |place| {
            if place == ForgottenPlace::SessionBranch {
                Err(StoreError::backend("injizierter Schreibfehler"))
            } else {
                Ok(())
            }
        });

        let err = result.expect_err("die Tilgung bricht ab");
        match &err {
            StoreError::ForgetIncomplete {
                forgotten, pending, ..
            } => {
                assert_eq!(forgotten.as_slice(), [ForgottenPlace::StoreRef]);
                assert_eq!(*pending, ForgottenPlace::SessionBranch);
            }
            other => panic!("erwartete ForgetIncomplete, war {other:?}"),
        }
        // Die Meldung benennt den offenen Ort und rät zum erneuten forget.
        let message = err.to_string();
        assert!(
            message.contains("Session-Branch") && message.contains("erneut"),
            "Meldung ohne offenen Ort oder Rat: {message}"
        );

        // Der Store-Ref ist getilgt, der Branch trägt noch Klartext.
        assert!(matches!(store.get(id), Err(StoreError::Forgotten { .. })));
        let branch = child.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/sessions/",
        ]);
        let branch = branch.lines().next().expect("ein Session-Branch");
        let md = child.git(&["cat-file", "blob", &format!("{branch}:session.md")]);
        assert!(
            md.contains(NEEDLE),
            "der offene Branch sollte Klartext tragen"
        );
    }

    #[test]
    fn a_second_forget_completes_a_broken_off_deletion() {
        // #14 (b): Ein erneuter `forget` vollendet die abgebrochene Löschung — er
        // erkennt den schon getilgten Store-Ref an seinem Tombstone (überspringt
        // ihn) und tilgt den offen gebliebenen Branch.
        const NEEDLE: &str = "Teiltilgung-Vollenden-Pruefwort-1402";
        let (_parent, child, store) = parent_and_child();
        let session = redacted(NEEDLE);
        let id = session.session().id().unwrap();
        store.put(&session).unwrap();
        store.put_session_branch(&session).unwrap();

        // Erster Lauf bricht am Branch ab.
        let first = store.0.forget_guarded(id, "DSGVO", |place| {
            if place == ForgottenPlace::SessionBranch {
                Err(StoreError::backend("injiziert"))
            } else {
                Ok(())
            }
        });
        assert!(matches!(first, Err(StoreError::ForgetIncomplete { .. })));

        // Zweiter, regulärer Lauf vollendet die Tilgung.
        let second = store.forget(id, "DSGVO").unwrap();
        assert!(second.was_forgotten());
        assert_eq!(second.places(), [ForgottenPlace::SessionBranch]);

        let branch = child.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/sessions/",
        ]);
        let branch = branch.lines().next().expect("ein Session-Branch");
        let md = child.git(&["cat-file", "blob", &format!("{branch}:session.md")]);
        assert!(!md.contains(NEEDLE), "session.md nach zweitem forget: {md}");
        let json = child.git(&["cat-file", "blob", &format!("{branch}:session.json")]);
        assert!(
            !json.contains(NEEDLE),
            "session.json nach zweitem forget: {json}"
        );
    }
}
