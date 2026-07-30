//! Blobs und Trees lesen und schreiben — der Objekt-Layer unter dem Store.
//!
//! Der `ContextStore` aus M4 braucht vier Dinge: `get`, `exists`, `list`, `put`.
//! Dieses Modul liefert sie in Git-Begriffen, für beide Backends identisch
//! (In-Repo und Child-Repo unterscheiden sich nur im [`Repo`]-Handle):
//!
//! | Store | hier |
//! |---|---|
//! | `get(id)` | [`Repo::read_blob_at`] |
//! | `exists(id)` | [`Repo::read_blob_at`] auf `Option` prüfen |
//! | `list()` | [`Repo::list_blobs_at`] |
//! | `put(id, bytes)` | [`Repo::write_blob`] + [`Repo::write_tree`] |
//!
//! # Ein fehlender Ref ist leer, kein Fehler
//!
//! Vor dem ersten `minds capture` gibt es `refs/minds/context` nicht. Das ist
//! der Normalzustand jedes Repos, das Minds noch nie benutzt hat — also liefert
//! [`Repo::tree_at`] dafür `Ok(None)`, [`Repo::list_blobs_at`] eine leere Liste.
//! Dieselbe Linie wie beim ungeborenen HEAD (siehe `head.rs`): Was regulär
//! vorkommt, ist kein `Err`, sonst filtert es jeder Aufrufer wieder heraus —
//! und einer vergisst es.
//!
//! # Schreiben legt Objekte an, aber macht sie nicht haltbar
//!
//! [`Repo::write_blob`] und [`Repo::write_tree`] schreiben in die
//! Objektdatenbank, rühren aber **keinen Ref** an. Bis der nächste Commit
//! (`feat(git): Custom-Ref refs/minds/context anlegen/aktualisieren`) einen Ref
//! auf sie zeigen lässt, sind sie unerreichbar und würden von `git gc --prune`
//! eingesammelt. Das ist kein Versehen, sondern die Reihenfolge, die Git
//! vorgibt: erst Inhalt, dann Baum, dann Commit, dann Ref. Wer hier aufhört,
//! hat nichts kaputtgemacht — nur nichts gespeichert.
//!
//! # Kein Löschen
//!
//! Es gibt bewusst kein `remove`. Der Kontext-Store ist ein Audit-Record und
//! damit append-only; eine Session zu entfernen, würde genau die Eigenschaft
//! zerstören, für die es Minds gibt. Muss doch je etwas weg (DSGVO-Löschauskunft),
//! ist das ein eigener, sichtbarer Vorgang mit History-Rewrite — kein
//! Store-API-Aufruf, den man versehentlich tut.
//!
//! # Blobs liegen komplett im Speicher
//!
//! [`Repo::read_blob`] gibt `Vec<u8>` zurück, nicht einen Reader. Sessions sind
//! JSON in der Größenordnung von Kilobytes bis wenige Megabytes; Streaming wäre
//! Komplexität ohne heutigen Nutzen. Sollte sich das ändern, ist es ein
//! additiver Nachbar (`open_blob`), kein Umbau.

use crate::error::{GitError, Result};
use crate::oid::{BlobId, CommitId, TreeId};
use crate::repo::Repo;

impl Repo {
    /// Der Baum, auf den `reference` zeigt — `None`, wenn es den Ref nicht gibt.
    ///
    /// `reference` ist ein voller Ref-Name (`refs/minds/context`) oder ein
    /// Teilname, den Git auflösen kann (`main`). Zeigt der Ref auf ein
    /// annotiertes Tag, wird bis zum Commit geschält.
    pub fn tree_at(&self, reference: &str) -> Result<Option<TreeId>> {
        match self.commit_at(reference)? {
            Some(commit) => self.tree_of(commit).map(Some),
            None => Ok(None),
        }
    }

    /// Der Baum eines Commits.
    pub fn tree_of(&self, commit: CommitId) -> Result<TreeId> {
        let tree_id = self
            .gix()
            .find_commit(commit.to_gix())
            .map_err(|err| GitError::read_object(commit, err))?
            .tree_id()
            .map_err(|err| GitError::read_object(commit, err))?
            .detach();

        Ok(TreeId::from_gix(tree_id))
    }

    /// Liest den Blob unter `path` in `tree`.
    ///
    /// `None`, wenn es den Pfad nicht gibt **oder** wenn dort kein Blob steht
    /// (ein Verzeichnis, ein Symlink, ein Submodul). Die zweite Hälfte ist
    /// Absicht: Der Store fragt nach einer Session, und ein Verzeichnis ist
    /// keine — „ist nicht da" ist die ehrlichere Antwort als ein Fehler über
    /// Dateimodi, mit dem der Aufrufer nichts anfangen kann.
    pub fn read_blob(&self, tree: TreeId, path: &str) -> Result<Option<Vec<u8>>> {
        validate_path(path)?;

        let tree_object = self
            .gix()
            .find_tree(tree.to_gix())
            .map_err(|err| GitError::read_object(tree, err))?;

        let Some(entry) = tree_object
            .lookup_entry_by_path(path)
            .map_err(|err| GitError::read_object(tree, err))?
        else {
            return Ok(None);
        };

        if !entry.mode().is_blob() {
            return Ok(None);
        }

        let object = entry
            .object()
            .map_err(|err| GitError::read_object(tree, err))?;

        // `detach()` statt `object.data`: `gix::Object` hat ein `Drop` (es gibt
        // seinen Puffer an das Repository zurück), aus dem sich kein Feld
        // herausbewegen lässt. `detach()` kappt die Verbindung zum Repository
        // und gibt die Daten heraus — ohne zu kopieren.
        Ok(Some(object.detach().data))
    }

    /// Alle Blob-Pfade in `tree`, rekursiv und sortiert.
    ///
    /// Die Sortierung ist eine Zusage: Der Store listet Sessions, und eine
    /// Reihenfolge, die von der Traversierung abhängt, wäre für Tests und für
    /// den Reader-Index gleichermaßen unbrauchbar.
    ///
    /// Verzeichnisse selbst tauchen nicht auf — sie tragen keine Daten. Andere
    /// Eintragsarten (Symlinks, Submodule) werden übergangen: Im Kontext-Baum
    /// gibt es sie nicht, und sie als Sessions zu listen wäre irreführend.
    pub fn list_blobs(&self, tree: TreeId) -> Result<Vec<String>> {
        let tree_object = self
            .gix()
            .find_tree(tree.to_gix())
            .map_err(|err| GitError::read_object(tree, err))?;

        let mut recorder = gix::traverse::tree::Recorder::default();
        tree_object
            .traverse()
            .breadthfirst(&mut recorder)
            .map_err(|err| GitError::read_object(tree, err))?;

        let mut paths = Vec::new();
        for record in recorder.records {
            if !record.mode.is_blob() {
                continue;
            }
            let path = String::from_utf8(record.filepath.into()).map_err(|_| {
                GitError::invalid_path(
                    "<nicht darstellbar>",
                    "Pfad im Baum ist kein gültiges UTF-8",
                )
            })?;
            paths.push(path);
        }

        paths.sort();
        Ok(paths)
    }

    /// Liest den Blob unter `path` im Baum von `reference`.
    ///
    /// Der Weg, den `ContextStore::get` geht. `None` bedeutet zweierlei — den
    /// Ref gibt es nicht, oder der Pfad ist nicht drin — und für den Aufrufer
    /// ist beides dasselbe: Die Session liegt hier nicht.
    pub fn read_blob_at(&self, reference: &str, path: &str) -> Result<Option<Vec<u8>>> {
        match self.tree_at(reference)? {
            Some(tree) => self.read_blob(tree, path),
            None => Ok(None),
        }
    }

    /// Alle Blob-Pfade unter `reference`, rekursiv und sortiert; leer, wenn es
    /// den Ref nicht gibt.
    pub fn list_blobs_at(&self, reference: &str) -> Result<Vec<String>> {
        match self.tree_at(reference)? {
            Some(tree) => self.list_blobs(tree),
            None => Ok(Vec::new()),
        }
    }

    /// Schreibt `content` als Blob und gibt seine Id zurück.
    ///
    /// Idempotent, und zwar von Git aus: Die Id ist der Hash des Inhalts, also
    /// erzeugt derselbe Inhalt dieselbe Id und belegt keinen zusätzlichen
    /// Platz. Das ist das „Dedup per Hash" aus M4 — der Store muss dafür nichts
    /// tun, er muss es nur nicht kaputtmachen.
    pub fn write_blob(&self, content: &[u8]) -> Result<BlobId> {
        let id = self
            .gix()
            .write_blob(content)
            .map_err(GitError::write_object)?
            .detach();

        Ok(BlobId::from_gix(id))
    }

    /// Baut aus `base` und `entries` einen neuen Baum und schreibt ihn.
    ///
    /// `base` ist der Ausgangsbaum (typisch: [`Repo::tree_at`] auf
    /// `refs/minds/context`); `None` beginnt bei einem leeren Baum — der Fall
    /// beim allerersten `minds capture`. Jeder Eintrag legt einen Pfad an oder
    /// **ersetzt** ihn; Zwischenverzeichnisse entstehen automatisch. Was in
    /// `base` steht und nicht genannt wird, bleibt unverändert.
    ///
    /// Auch das ist idempotent: Gleiche Eingabe ⇒ gleicher Baum-Hash ⇒
    /// derselbe Baum. Zweimal dieselbe Session zu schreiben kostet nichts und
    /// ändert nichts.
    ///
    /// # Fehler
    ///
    /// [`GitError::InvalidPath`], wenn ein Pfad nicht als Baum-Eintrag taugt
    /// (siehe `validate_path`) — geprüft **bevor** irgendetwas geschrieben
    /// wird, damit ein krummer Pfad keinen halben Baum hinterlässt.
    pub fn write_tree<S: AsRef<str>>(
        &self,
        base: Option<TreeId>,
        entries: impl IntoIterator<Item = (S, BlobId)>,
    ) -> Result<TreeId> {
        // Alles einsammeln und prüfen, bevor der erste Eintrag gesetzt wird:
        // Der Editor arbeitet im Speicher, aber ein Abbruch in der Mitte einer
        // Schleife ist trotzdem schwerer zu verstehen als eine Prüfung vorweg.
        let entries: Vec<(S, BlobId)> = entries.into_iter().collect();
        for (path, _) in &entries {
            validate_path(path.as_ref())?;
        }

        // Der Ausgangsbaum. `empty_tree()` liefert ihn in-memory, ohne dass
        // dafür etwas in der Objektdatenbank liegen müsste — der Fall beim
        // allerersten `capture`.
        let base_tree = match base {
            Some(tree) => self
                .gix()
                .find_tree(tree.to_gix())
                .map_err(|err| GitError::read_object(tree, err))?,
            None => self.gix().empty_tree(),
        };
        let base_id = TreeId::from_gix(base_tree.id().detach());

        let mut editor = base_tree
            .edit()
            .map_err(|err| GitError::read_object(base_id, err))?;

        for (path, blob) in &entries {
            editor
                .upsert(
                    path.as_ref(),
                    gix::object::tree::EntryKind::Blob,
                    blob.to_gix(),
                )
                .map_err(GitError::write_object)?;
        }

        let id = editor.write().map_err(GitError::write_object)?.detach();
        Ok(TreeId::from_gix(id))
    }
}

/// Prüft, ob `path` als Eintrag in einem Git-Baum taugt.
///
/// Die Regeln sind eng, weil Minds seine Pfade selbst baut (`sessions/b3/<hex>.json`,
/// `index.json`): Alles, was diese Form verlässt, ist ein Bug und keine
/// Nutzereingabe. Verboten sind
///
/// - leere Pfade und leere Komponenten (`a//b`),
/// - führende und abschließende Schrägstriche,
/// - die Komponenten `.` und `..`,
/// - Nullbytes.
///
/// Erlaubt bleibt alles andere, was Git erlaubt — insbesondere Unicode. Der
/// Trenner ist immer `/`, auch unter Windows: Das ist Gits Baumformat, nicht
/// das des Betriebssystems.
fn validate_path(path: &str) -> Result<()> {
    let reason = if path.is_empty() {
        Some("leerer Pfad")
    } else if path.starts_with('/') || path.ends_with('/') {
        Some("führender oder abschließender Schrägstrich")
    } else if path.contains('\0') {
        Some("Nullbyte im Pfad")
    } else if path.split('/').any(str::is_empty) {
        Some("leere Pfadkomponente")
    } else if path.split('/').any(|part| part == "." || part == "..") {
        Some("Pfadkomponente \".\" oder \"..\"")
    } else {
        None
    };

    match reason {
        Some(reason) => Err(GitError::invalid_path(path, reason)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::TempRepo;

    /// Ein Repo mit einem Kontext-ähnlichen Baum auf `main`.
    fn repo_with_context() -> (TempRepo, Repo) {
        let fixture = TempRepo::init();
        fixture.write_file("index.json", "{}");
        fixture.write_file("sessions/b3/aa.json", "{\"session\":\"aa\"}");
        fixture.write_file("sessions/b3/bb.json", "{\"session\":\"bb\"}");
        fixture.commit("kontext");
        let repo = Repo::open(fixture.path()).unwrap();
        (fixture, repo)
    }

    // --- Lesen ---------------------------------------------------------------

    #[test]
    fn tree_at_matches_what_git_reports() {
        let (fixture, repo) = repo_with_context();
        let tree = repo.tree_at("refs/heads/main").unwrap().unwrap();
        assert_eq!(tree.to_string(), fixture.hash("HEAD^{tree}"));
    }

    #[test]
    fn tree_at_is_none_for_a_ref_that_does_not_exist() {
        // Der Zustand jedes Repos, das Minds noch nie benutzt hat.
        let (_fixture, repo) = repo_with_context();
        assert_eq!(repo.tree_at("refs/minds/context").unwrap(), None);
    }

    #[test]
    fn read_blob_returns_the_content() {
        let (_fixture, repo) = repo_with_context();
        let tree = repo.tree_at("refs/heads/main").unwrap().unwrap();

        let content = repo.read_blob(tree, "sessions/b3/aa.json").unwrap();
        assert_eq!(content.as_deref(), Some(&b"{\"session\":\"aa\"}"[..]));
    }

    #[test]
    fn read_blob_is_none_for_an_unknown_path() {
        let (_fixture, repo) = repo_with_context();
        let tree = repo.tree_at("refs/heads/main").unwrap().unwrap();
        assert_eq!(repo.read_blob(tree, "sessions/b3/zz.json").unwrap(), None);
    }

    #[test]
    fn read_blob_is_none_for_a_directory() {
        // Ein Verzeichnis ist keine Session — „nicht da" ist die richtige
        // Antwort, kein Fehler über Dateimodi.
        let (_fixture, repo) = repo_with_context();
        let tree = repo.tree_at("refs/heads/main").unwrap().unwrap();
        assert_eq!(repo.read_blob(tree, "sessions/b3").unwrap(), None);
    }

    #[test]
    fn list_blobs_is_recursive_and_sorted() {
        let (_fixture, repo) = repo_with_context();
        let tree = repo.tree_at("refs/heads/main").unwrap().unwrap();

        assert_eq!(
            repo.list_blobs(tree).unwrap(),
            vec![
                "index.json".to_owned(),
                "sessions/b3/aa.json".to_owned(),
                "sessions/b3/bb.json".to_owned(),
            ]
        );
    }

    #[test]
    fn list_blobs_matches_git_ls_tree() {
        // Gegenprobe gegen echtes git: gleiche Menge, gleiche Pfade.
        let (fixture, repo) = repo_with_context();
        let tree = repo.tree_at("refs/heads/main").unwrap().unwrap();

        let from_git = fixture.git(&["ls-tree", "-r", "--name-only", &tree.to_string()]);
        let mut expected: Vec<String> = from_git.lines().map(str::to_owned).collect();
        expected.sort();

        assert_eq!(repo.list_blobs(tree).unwrap(), expected);
    }

    #[test]
    fn reading_a_missing_ref_yields_nothing_rather_than_an_error() {
        let (_fixture, repo) = repo_with_context();
        let missing = "refs/minds/context";

        assert_eq!(repo.read_blob_at(missing, "index.json").unwrap(), None);
        assert!(repo.list_blobs_at(missing).unwrap().is_empty());
    }

    #[test]
    fn read_blob_at_finds_content_through_the_ref() {
        let (_fixture, repo) = repo_with_context();
        let content = repo.read_blob_at("refs/heads/main", "index.json").unwrap();
        assert_eq!(content.as_deref(), Some(&b"{}"[..]));
    }

    // --- Schreiben -----------------------------------------------------------

    #[test]
    fn write_blob_produces_the_same_hash_as_git() {
        // Der Vertrag mit dem Ökosystem: Was Minds schreibt, ist gewöhnliches
        // Git — bis auf den Hash identisch mit dem, was `git add` erzeugt hätte.
        let fixture = TempRepo::init();
        fixture.write_file("datei.txt", "hallo");
        fixture.commit("c1");
        let repo = Repo::open(fixture.path()).unwrap();

        let blob = repo.write_blob(b"hallo").unwrap();
        assert_eq!(blob.to_string(), fixture.hash("HEAD:datei.txt"));
    }

    #[test]
    fn write_blob_is_idempotent() {
        let (_fixture, repo) = repo_with_context();
        let first = repo.write_blob(b"derselbe Inhalt").unwrap();
        let second = repo.write_blob(b"derselbe Inhalt").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn write_tree_from_scratch_is_readable_by_git() {
        let fixture = TempRepo::init();
        fixture.commit("leer");
        let repo = Repo::open(fixture.path()).unwrap();

        let blob = repo.write_blob(b"{\"session\":\"cc\"}").unwrap();
        let tree = repo
            .write_tree(None, [("sessions/b3/cc.json", blob)])
            .unwrap();

        // Gegenprobe mit echtem git: Der Baum muss für Git wohlgeformt sein,
        // inklusive der Zwischenverzeichnisse.
        let listed = fixture.git(&["ls-tree", "-r", "--name-only", &tree.to_string()]);
        assert_eq!(listed.trim(), "sessions/b3/cc.json");
    }

    #[test]
    fn write_tree_roundtrips_without_any_ref() {
        // Der Baum ist noch nirgends verankert — lesbar ist er trotzdem. Genau
        // darauf baut der nächste Commit (Ref setzen) auf.
        let fixture = TempRepo::init();
        fixture.commit("leer");
        let repo = Repo::open(fixture.path()).unwrap();

        let blob = repo.write_blob(b"inhalt").unwrap();
        let tree = repo.write_tree(None, [("a/b/c.json", blob)]).unwrap();

        assert_eq!(
            repo.read_blob(tree, "a/b/c.json").unwrap().as_deref(),
            Some(&b"inhalt"[..])
        );
        assert_eq!(
            repo.list_blobs(tree).unwrap(),
            vec!["a/b/c.json".to_owned()]
        );
    }

    #[test]
    fn write_tree_extends_an_existing_tree() {
        let (_fixture, repo) = repo_with_context();
        let base = repo.tree_at("refs/heads/main").unwrap().unwrap();

        let blob = repo.write_blob(b"{\"session\":\"cc\"}").unwrap();
        let extended = repo
            .write_tree(Some(base), [("sessions/b3/cc.json", blob)])
            .unwrap();

        // Neu drin, Altes unangetastet.
        assert_eq!(
            repo.list_blobs(extended).unwrap(),
            vec![
                "index.json".to_owned(),
                "sessions/b3/aa.json".to_owned(),
                "sessions/b3/bb.json".to_owned(),
                "sessions/b3/cc.json".to_owned(),
            ]
        );
        assert_eq!(
            repo.read_blob(extended, "sessions/b3/aa.json")
                .unwrap()
                .as_deref(),
            Some(&b"{\"session\":\"aa\"}"[..])
        );
    }

    #[test]
    fn write_tree_replaces_an_existing_path() {
        let (_fixture, repo) = repo_with_context();
        let base = repo.tree_at("refs/heads/main").unwrap().unwrap();

        let blob = repo.write_blob(b"{\"neu\":true}").unwrap();
        let updated = repo.write_tree(Some(base), [("index.json", blob)]).unwrap();

        assert_eq!(
            repo.read_blob(updated, "index.json").unwrap().as_deref(),
            Some(&b"{\"neu\":true}"[..])
        );
        // Die Pfadmenge bleibt gleich — ersetzt, nicht hinzugefügt.
        assert_eq!(repo.list_blobs(updated).unwrap().len(), 3);
    }

    #[test]
    fn write_tree_is_idempotent() {
        // Zweimal dasselbe schreiben ⇒ derselbe Baum. Das ist das idempotente
        // `put` aus M4, eine Ebene tiefer.
        let (_fixture, repo) = repo_with_context();
        let base = repo.tree_at("refs/heads/main").unwrap().unwrap();
        let blob = repo.write_blob(b"{\"session\":\"cc\"}").unwrap();

        let first = repo
            .write_tree(Some(base), [("sessions/b3/cc.json", blob)])
            .unwrap();
        let second = repo
            .write_tree(Some(base), [("sessions/b3/cc.json", blob)])
            .unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn write_tree_accepts_several_entries_at_once() {
        let (_fixture, repo) = repo_with_context();
        let base = repo.tree_at("refs/heads/main").unwrap().unwrap();

        let session = repo.write_blob(b"{\"session\":\"cc\"}").unwrap();
        let index = repo.write_blob(b"{\"cc\":1}").unwrap();
        let tree = repo
            .write_tree(
                Some(base),
                [("sessions/b3/cc.json", session), ("index.json", index)],
            )
            .unwrap();

        assert_eq!(repo.list_blobs(tree).unwrap().len(), 4);
        assert_eq!(
            repo.read_blob(tree, "index.json").unwrap().as_deref(),
            Some(&b"{\"cc\":1}"[..])
        );
    }

    // --- Pfad-Prüfung --------------------------------------------------------

    #[test]
    fn write_tree_rejects_malformed_paths() {
        let (_fixture, repo) = repo_with_context();
        let blob = repo.write_blob(b"egal").unwrap();

        for bad in [
            "",
            "/sessions/a.json",
            "sessions/",
            "a//b.json",
            "../a.json",
        ] {
            let result = repo.write_tree(None, [(bad, blob)]);
            assert!(
                matches!(result, Err(GitError::InvalidPath { .. })),
                "{bad:?} hätte abgewiesen werden müssen"
            );
        }
    }

    #[test]
    fn valid_paths_pass_the_check() {
        for good in ["index.json", "sessions/b3/aa.json", "a/b/c/d/e.json"] {
            assert!(validate_path(good).is_ok(), "{good:?} ist gültig");
        }
    }

    #[test]
    fn read_blob_rejects_malformed_paths_too() {
        // Lesen prüft mit derselben Regel: Ein Pfad, den wir nie schreiben
        // würden, ist auch beim Lesen ein Bug — und still `None` zu liefern
        // würde ihn verstecken.
        let (_fixture, repo) = repo_with_context();
        let tree = repo.tree_at("refs/heads/main").unwrap().unwrap();

        let result = repo.read_blob(tree, "../etc/passwd");
        assert!(matches!(result, Err(GitError::InvalidPath { .. })));
    }
}
