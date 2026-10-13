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

/// Größter Commit oder Baum, den [`Repo::read_blob_bounded`] nachprüft.
pub(crate) const MAX_VERIFIED_OBJECT: u64 = 16 * 1024 * 1024;

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

    /// Liest den Blob unter `path` im Baum von `commit` — **nachgeprüft**:
    /// Commit, jeder Baum auf dem Weg und der Blob werden aus ihren gelesenen
    /// Bytes neu gehasht und müssen ihre Id tragen. Der Blob ist höchstens
    /// `max_bytes` groß, geprüft am Objekt-Header, bevor der Inhalt gelesen
    /// wird, und noch einmal am Gelesenen. Gibt die volle Blob-Id (Hex) mit
    /// (EA-10: die festgehaltene Witness-Policy).
    ///
    /// Warum nachprüfen: Die Objektdatenbank gehört womöglich dem
    /// beobachteten Agenten, und gix prüft beim Lesen nicht nach. Wer die
    /// Bytes hinter einer geprüften Id austauscht, fiele sonst nicht auf — die
    /// gedruckte Id stimmte, der Inhalt nicht.
    ///
    /// `None` nur, wenn es den Pfad im Baum nicht gibt oder dort kein Blob
    /// steht; jeder andere Fehler ist ein Fehler.
    pub fn read_blob_bounded(
        &self,
        commit: CommitId,
        path: &str,
        max_bytes: u64,
    ) -> Result<Option<(String, Vec<u8>)>> {
        validate_path(path)?;
        let repo = self.gix();
        let hash_kind = repo.object_hash();
        let verified = |id: gix::ObjectId, kind: gix::objs::Kind, limit: u64| {
            self.verified_object(id, kind, limit)
        };
        let commit_data = verified(
            commit.to_gix(),
            gix::objs::Kind::Commit,
            MAX_VERIFIED_OBJECT,
        )?;
        let mut tree = gix::objs::CommitRef::from_bytes(&commit_data, hash_kind)
            .map_err(|err| GitError::read_object(commit, err))?
            .tree();
        let parts: Vec<&str> = path.split('/').collect();
        for (index, part) in parts.iter().enumerate() {
            let tree_data = verified(tree, gix::objs::Kind::Tree, MAX_VERIFIED_OBJECT)?;
            let parsed = gix::objs::TreeRef::from_bytes(&tree_data, hash_kind)
                .map_err(|err| GitError::read_object(tree, err))?;
            // Sortierung wie `git fsck` (siehe `strictly_sorted`).
            if !strictly_sorted(&parsed) {
                return Err(GitError::read_object(
                    tree,
                    "tree is not sorted or has duplicate entries",
                ));
            }
            let Some(entry) = parsed
                .entries
                .iter()
                .find(|entry| entry.filename == part.as_bytes())
            else {
                return Ok(None);
            };
            let id = entry.oid.to_owned();
            if index + 1 < parts.len() {
                if !entry.mode.is_tree() {
                    return Ok(None);
                }
                tree = id;
                continue;
            }
            if !entry.mode.is_blob() {
                return Ok(None);
            }
            let data = verified(id, gix::objs::Kind::Blob, max_bytes)?;
            return Ok(Some((id.to_string(), data)));
        }
        Ok(None)
    }

    /// Ein Objekt, erst am Header begrenzt, dann gelesen und neu gehasht —
    /// es muss seine Id tragen und die erwartete Art haben.
    ///
    /// Jedes Objekt erst am Header begrenzen, dann lesen — auch Commit und
    /// Bäume: Ein Commit mit gigabytegroßer Nachricht (als zlib-Bombe nur
    /// wenige MB) füllte sonst den Speicher des Menschen, der `init`
    /// ausführt.
    pub(crate) fn verified_object(
        &self,
        id: gix::ObjectId,
        kind: gix::objs::Kind,
        limit: u64,
    ) -> Result<Vec<u8>> {
        let repo = self.gix();
        let hash_kind = repo.object_hash();
        let size = repo
            .find_header(id)
            .map_err(|err| GitError::read_object(id, err))?
            .size();
        if size > limit {
            return Err(GitError::TooLarge {
                id: id.to_string(),
                size,
                limit,
            });
        }
        let object = repo
            .find_object(id)
            .map_err(|err| GitError::read_object(id, err))?
            .detach();
        if object.data.len() as u64 > limit {
            return Err(GitError::read_object(id, "object grew beyond the limit"));
        }
        if object.kind != kind {
            return Err(GitError::read_object(
                id,
                format!("expected a {kind}, found a {}", object.kind),
            ));
        }
        let actual = gix::objs::compute_hash(hash_kind, kind, &object.data)
            .map_err(|err| GitError::read_object(id, err))?;
        if actual != id {
            return Err(GitError::read_object(
                id,
                "object content does not match its id",
            ));
        }
        Ok(object.data)
    }

    /// Alle Blob-Pfade eines Commits samt Blob-Id — wie [`Repo::list_blobs`],
    /// aber jeder Commit und jeder Baum am Header begrenzt, neu gehasht und
    /// auf Gits Sortierung geprüft (wie [`Repo::read_blob_bounded`]). Für
    /// Aussagen über den Inhalt eines Commits, dessen Objektdatenbank dem
    /// beobachteten Agenten gehören kann: Eine ungeprüfte Liste ließe ihn
    /// Dateien verstecken.
    ///
    /// Höchstens `max_entries` Blobs und `max_tree_bytes` gelesene
    /// Baum-Bytes; darüber [`GitError::TooLarge`]. Symlinks und Submodule
    /// werden übergangen. Sortiert nach Pfad.
    pub fn list_blobs_verified(
        &self,
        commit: CommitId,
        max_entries: usize,
        max_tree_bytes: u64,
    ) -> Result<VerifiedListing> {
        let hash_kind = self.gix().object_hash();
        let commit_data = self.verified_object(
            commit.to_gix(),
            gix::objs::Kind::Commit,
            MAX_VERIFIED_OBJECT,
        )?;
        let root = gix::objs::CommitRef::from_bytes(&commit_data, hash_kind)
            .map_err(|err| GitError::read_object(commit, err))?
            .tree();
        let too_large = |size: u64, limit: u64| GitError::TooLarge {
            id: commit.to_string(),
            size,
            limit,
        };
        // Gelesene Baum-Bytes und gebaute Pfad-Bytes je gegen
        // `max_tree_bytes`: Ein tiefer Baum mit langen Namen bleibt in den
        // Objekten klein, seine Pfade nicht.
        let mut spent: u64 = 0;
        let mut path_bytes: u64 = 0;
        let mut entries: usize = 0;
        let mut listing = VerifiedListing::default();
        let mut pending = vec![(String::new(), root)];
        while let Some((prefix, tree)) = pending.pop() {
            let left = max_tree_bytes.saturating_sub(spent);
            let data = self
                .verified_object(tree, gix::objs::Kind::Tree, left.min(MAX_VERIFIED_OBJECT))
                .map_err(|err| match err {
                    GitError::TooLarge { .. } => too_large(spent, max_tree_bytes),
                    other => other,
                })?;
            spent = spent.saturating_add(data.len() as u64);
            let parsed = gix::objs::TreeRef::from_bytes(&data, hash_kind)
                .map_err(|err| GitError::read_object(tree, err))?;
            if !strictly_sorted(&parsed) {
                return Err(GitError::read_object(
                    tree,
                    "tree is not sorted or has duplicate entries",
                ));
            }
            for entry in &parsed.entries {
                // Wie `git fsck`: kein leerer Name, kein `.`/`..`/`.git`,
                // kein `/` oder NUL — sonst entstünden doppelte oder
                // fremde Pfade.
                let Ok(name) = std::str::from_utf8(entry.filename) else {
                    return Err(GitError::read_object(tree, "a path is not UTF-8"));
                };
                if name.is_empty()
                    || matches!(name, "." | "..")
                    || name.eq_ignore_ascii_case(".git")
                    || name.contains(['/', '\0'])
                {
                    return Err(GitError::read_object(
                        tree,
                        "a tree entry has an invalid name",
                    ));
                }
                entries += 1;
                if entries > max_entries {
                    return Err(too_large(entries as u64, max_entries as u64));
                }
                let length = prefix.len() + 1 + name.len();
                path_bytes = path_bytes.saturating_add(length as u64);
                if length > MAX_LISTED_PATH || path_bytes > max_tree_bytes {
                    return Err(too_large(path_bytes, max_tree_bytes));
                }
                let path = if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}/{name}")
                };
                if entry.mode.is_tree() {
                    pending.push((path, entry.oid.to_owned()));
                } else if entry.mode.is_blob() {
                    listing
                        .blobs
                        .push((path, BlobId::from_gix(entry.oid.to_owned())));
                } else {
                    // Symlink oder Gitlink: Inhalt, den die Liste nicht
                    // zeigt — der Aufrufer entscheidet, ob er zählt.
                    listing.links.push(path);
                }
            }
        }
        listing.blobs.sort_by(|a, b| a.0.cmp(&b.0));
        listing.links.sort();
        Ok(listing)
    }

    /// Ein Blob über seine Id, höchstens `max_bytes` groß (am Header geprüft)
    /// und neu gehasht.
    pub fn read_blob_verified(&self, blob: BlobId, max_bytes: u64) -> Result<Vec<u8>> {
        self.verified_object(blob.to_gix(), gix::objs::Kind::Blob, max_bytes)
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
                GitError::invalid_path("<not representable>", "path in tree is not valid UTF-8")
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

    /// Die Blob-Id (Hex), die `content` in diesem Repository hätte —
    /// gerechnet mit dessen Hash-Art, **nicht** geschrieben. Für den Abgleich
    /// „diese Bytes sind genau dieser Blob" (EA-14: Intent-Anker mit
    /// Datei-Quelle).
    pub fn blob_id_of(&self, content: &[u8]) -> Result<String> {
        let id = gix::objs::compute_hash(self.gix().object_hash(), gix::objs::Kind::Blob, content)
            .map_err(GitError::write_object)?;
        Ok(id.to_hex().to_string())
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
pub(crate) fn validate_path(path: &str) -> Result<()> {
    let reason = if path.is_empty() {
        Some("empty path")
    } else if path.starts_with('/') || path.ends_with('/') {
        Some("leading or trailing slash")
    } else if path.contains('\0') {
        Some("null byte in path")
    } else if path.split('/').any(str::is_empty) {
        Some("empty path component")
    } else if path.split('/').any(|part| part == "." || part == "..") {
        Some("path component \".\" or \"..\"")
    } else {
        None
    };

    match reason {
        Some(reason) => Err(GitError::invalid_path(path, reason)),
        None => Ok(()),
    }
}

/// Höchstlänge eines Pfads in [`Repo::list_blobs_verified`].
pub const MAX_LISTED_PATH: usize = 4096;

/// Was [`Repo::list_blobs_verified`] in einem Commit findet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifiedListing {
    /// Reguläre Dateien samt Blob-Id, nach Pfad sortiert.
    pub blobs: Vec<(String, BlobId)>,
    /// Symlinks und Gitlinks (Submodule) — Inhalt, den die Liste nicht
    /// zeigt. Sortiert.
    pub links: Vec<String>,
}

/// Wie `git fsck`: streng sortiert nach Gits Regel (Verzeichnisse mit
/// nachgestelltem `/`), ohne Duplikate. Sonst fänden git und gix in
/// demselben Baum verschiedene Einträge — der Mensch sähe mit `git` einen
/// anderen als den festgehaltenen.
pub(crate) fn strictly_sorted(tree: &gix::objs::TreeRef<'_>) -> bool {
    let sort_key = |entry: &gix::objs::tree::EntryRef<'_>| {
        let mut key = entry.filename.to_vec();
        if entry.mode.is_tree() {
            key.push(b'/');
        }
        key
    };
    let sorted = !tree
        .entries
        .windows(2)
        .any(|pair| sort_key(&pair[0]) >= sort_key(&pair[1]));
    // Auch ein Blob `a` neben einem Baum `a` (verschiedene Sortierschlüssel,
    // derselbe Name) ist für `git fsck` ein Duplikat.
    let mut names = std::collections::HashSet::new();
    sorted
        && tree
            .entries
            .iter()
            .all(|entry| names.insert(entry.filename))
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

    /// Die geprüfte Liste trifft die ungeprüfte, samt lesbaren Blob-Ids —
    /// und hält ihre Deckel.
    #[test]
    fn list_blobs_verified_matches_and_is_bounded() {
        let (fixture, repo) = repo_with_context();
        let head: CommitId = fixture.hash("HEAD").parse().unwrap();
        let listed = repo.list_blobs_verified(head, 10, 1 << 20).unwrap().blobs;
        let paths: Vec<&str> = listed.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths,
            ["index.json", "sessions/b3/aa.json", "sessions/b3/bb.json"]
        );
        assert_eq!(
            repo.read_blob_verified(listed[1].1, 1024).unwrap(),
            b"{\"session\":\"aa\"}"
        );
        assert!(matches!(
            repo.read_blob_verified(listed[1].1, 4),
            Err(GitError::TooLarge { .. })
        ));
        // Bäume zählen mit: 3 Dateien und 2 Verzeichnisse.
        assert!(repo.list_blobs_verified(head, 5, 1 << 20).is_ok());
        assert!(matches!(
            repo.list_blobs_verified(head, 4, 1 << 20),
            Err(GitError::TooLarge { .. })
        ));
        assert!(matches!(
            repo.list_blobs_verified(head, 10, 16),
            Err(GitError::TooLarge { .. })
        ));
    }

    /// Ein roher Baum aus `(mode, name, oid-hex)`.
    fn raw_tree(fixture: &TempRepo, entries: &[(&str, &[u8], &str)]) -> String {
        let mut bytes = Vec::new();
        for (mode, name, oid) in entries {
            bytes.extend_from_slice(mode.as_bytes());
            bytes.push(b' ');
            bytes.extend_from_slice(name);
            bytes.push(0);
            let oid: Vec<u8> = (0..oid.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&oid[i..i + 2], 16).unwrap())
                .collect();
            bytes.extend_from_slice(&oid);
        }
        fixture.write_raw_object("tree", &bytes)
    }

    fn raw_commit(fixture: &TempRepo, tree: &str) -> CommitId {
        let commit =
            format!("tree {tree}\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nraw\n");
        fixture
            .write_raw_object("commit", commit.as_bytes())
            .parse()
            .unwrap()
    }

    /// Namen, die `git fsck` ablehnt, ergeben keinen Pfad — kein doppelter
    /// oder fremder Eintrag in der Liste.
    #[test]
    fn list_blobs_verified_refuses_names_git_refuses() {
        let fixture = TempRepo::init();
        let blob = fixture.write_raw_object("blob", b"x");
        let repo = Repo::open(fixture.path()).unwrap();
        for name in [&b"a/b"[..], b"..", b".", b".git", b".GIT"] {
            let tree = raw_tree(&fixture, &[("100644", name, &blob)]);
            let commit = raw_commit(&fixture, &tree);
            assert!(
                repo.list_blobs_verified(commit, 10, 1 << 20).is_err(),
                "{}",
                String::from_utf8_lossy(name)
            );
        }
    }

    /// Symlinks und Gitlinks stehen in der Liste — nie still übergangen.
    #[test]
    fn list_blobs_verified_reports_links() {
        let fixture = TempRepo::init();
        let blob = fixture.write_raw_object("blob", b"target");
        let commit_id = fixture.commit("leer").to_string();
        let tree = raw_tree(
            &fixture,
            &[
                ("100644", b"a.rs", &blob),
                ("120000", b"link.rs", &blob),
                ("160000", b"sub", &commit_id),
            ],
        );
        let repo = Repo::open(fixture.path()).unwrap();
        let listing = repo
            .list_blobs_verified(raw_commit(&fixture, &tree), 10, 1 << 20)
            .unwrap();
        assert_eq!(listing.blobs.len(), 1);
        assert_eq!(listing.links, ["link.rs", "sub"]);
    }

    /// Lange Namen über viele Ebenen: Die Pfade sind gedeckelt, nicht nur
    /// die Baum-Bytes.
    #[test]
    fn list_blobs_verified_caps_path_length() {
        let fixture = TempRepo::init();
        let blob = fixture.write_raw_object("blob", b"x");
        let name = vec![b'n'; 250];
        let mut tree = raw_tree(&fixture, &[("100644", b"leaf", &blob)]);
        for _ in 0..20 {
            tree = raw_tree(&fixture, &[("40000", &name, &tree)]);
        }
        let repo = Repo::open(fixture.path()).unwrap();
        assert!(matches!(
            repo.list_blobs_verified(raw_commit(&fixture, &tree), 100, 1 << 20),
            Err(GitError::TooLarge { .. })
        ));
    }

    /// Elternteile werden geprüft gelesen.
    #[test]
    fn parents_and_bounded_ancestry() {
        let fixture = TempRepo::init();
        let c1 = fixture.commit("c1");
        let c2 = fixture.commit("c2");
        let c3 = fixture.commit("c3");
        let repo = Repo::open(fixture.path()).unwrap();
        assert_eq!(repo.parents(c3).unwrap(), vec![c2]);
        assert_eq!(repo.first_parent(c1).unwrap(), None);
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
