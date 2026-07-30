//! [`ReviewStore`] — Reviews als content-adressierte Git-Objekte (Schicht 3).
//!
//! Reviews liegen unter einem **eigenen** Ref (`refs/minds/reviews`), getrennt
//! vom Kontext-Store: Ein Review kann eigene Zugriffsrechte und einen eigenen
//! Push-Weg haben, und es soll die Session-Liste nicht verunreinigen. Das Layout
//! ist dasselbe Muster wie beim Kontext-Store — content-adressiert, flach,
//! dedup-freundlich: `reviews/<2hex>/<rest>.json`.
//!
//! # An der Change-Id, nicht am Commit
//!
//! Das Subjekt eines Reviews ist eine Change-Id (oder ersatzweise eine
//! SessionId), kein Commit-Hash — damit das Verdict den Rebase überlebt (siehe
//! [`Review`](minds_core::Review)).

use std::collections::BTreeSet;

use minds_core::{Comment, ContentHash, Review, order_key, to_canonical_json};
use minds_git::{GitError, Repo};

use crate::error::{Result, StoreError};

/// Der Ref, unter dem Reviews liegen.
pub const DEFAULT_REVIEW_REF: &str = "refs/minds/reviews";

/// Wie oft ein verlorener Wettlauf am Ref wiederholt wird.
const PUT_ATTEMPTS: u32 = 3;

/// Ein Speicher für Reviews in einem Git-Repository, unter einem Ref.
#[derive(Debug)]
pub struct ReviewStore {
    repo: Repo,
    reference: String,
}

impl ReviewStore {
    /// Ein Review-Store auf `repo` unter [`DEFAULT_REVIEW_REF`].
    pub fn new(repo: Repo) -> Self {
        Self {
            repo,
            reference: DEFAULT_REVIEW_REF.to_string(),
        }
    }

    /// Legt ein Review ab und gibt seine content-adressierte Id zurück.
    /// Idempotent: gleiches Review ⇒ gleicher Hash ⇒ derselbe Ort.
    pub fn put(&self, review: &Review) -> Result<ContentHash> {
        let hash = review.content_hash()?;
        let bytes = to_canonical_json(review)?;
        self.write_entry(
            &review_path(&hash),
            &bytes,
            &format!("minds: Review {hash}"),
        )?;
        Ok(hash)
    }

    /// Setzt einen Eintrag in den Thread-Baum — der gemeinsame Schreibweg für
    /// Verdicts, Kommentare und Signaturen.
    fn write_entry(&self, path: &str, bytes: &[u8], message: &str) -> Result<()> {
        let blob = self.repo.write_blob(bytes).map_err(StoreError::backend)?;

        let mut attempts_left = PUT_ATTEMPTS;
        loop {
            attempts_left -= 1;
            // Auf einem Wettlauf ist der Basisbaum veraltet — neu holen und
            // erneut aufsetzen. Der Blob ist content-adressiert, also unverändert.
            let base = self
                .repo
                .tree_at(&self.reference)
                .map_err(StoreError::backend)?;
            let tree = self
                .repo
                .write_tree(base, [(path, blob)])
                .map_err(StoreError::backend)?;
            match self.repo.commit_tree_to_ref(&self.reference, tree, message) {
                Ok(_) => return Ok(()),
                Err(GitError::RefRaced { .. }) if attempts_left > 0 => {}
                Err(err) => return Err(StoreError::backend(err)),
            }
        }
    }

    /// Die rohen Einträge unter `prefix` (nur `.json`), in Pfad-Reihenfolge.
    fn entries(&self, prefix: &str) -> Result<Vec<Vec<u8>>> {
        let paths = self
            .repo
            .list_blobs_at(&self.reference)
            .map_err(StoreError::backend)?;
        let mut out = Vec::new();
        for path in paths {
            if !path.starts_with(prefix) || !path.ends_with(".json") {
                continue;
            }
            if let Some(bytes) = self
                .repo
                .read_blob_at(&self.reference, &path)
                .map_err(StoreError::backend)?
            {
                out.push(bytes);
            }
        }
        Ok(out)
    }

    /// Legt einen Kommentar ab und gibt seine content-adressierte Id zurück.
    ///
    /// Kommentare liegen unter **demselben** Ref wie die Verdicts, nur unter
    /// `comments/`. Das ist Absicht: Verdict und Diskussion sind ein Thread, sie
    /// reisen zusammen, werden zusammen gepusht und zusammen vereinigt. Ein
    /// zweiter Ref wäre ein zweiter Ort, an dem etwas fehlen kann.
    pub fn put_comment(&self, comment: &Comment) -> Result<ContentHash> {
        let hash = comment.content_hash()?;
        let bytes = to_canonical_json(comment)?;
        self.write_entry(
            &comment_path(&hash),
            &bytes,
            &format!("minds: Kommentar {hash}"),
        )?;
        Ok(hash)
    }

    /// Die Kommentare zu einem Subjekt, in **deterministischer** Reihenfolge.
    ///
    /// Der Log ist eine Menge; die Reihenfolge kommt aus dem Inhalt
    /// ([`order_key`]), nicht daraus, wer wann gemergt hat. Zwei Maschinen mit
    /// demselben Log zeigen deshalb dasselbe.
    pub fn thread(&self, subject_id: &str) -> Result<Vec<Comment>> {
        let mut found: Vec<Comment> = self
            .entries("comments/")?
            .into_iter()
            .filter_map(|bytes| serde_json::from_slice::<Comment>(&bytes).ok())
            .filter(|comment| comment.subject.id() == subject_id)
            .collect();
        found.sort_by_key(order_key);
        Ok(found)
    }

    /// Legt die Signatur zu einem Review ab.
    ///
    /// # Warum daneben und nicht darin
    ///
    /// Ein Feld `signature` im Envelope wäre zirkulär: Der Hash deckt das ganze
    /// Envelope, die Signatur geht über den Hash. Sie liegt deshalb als eigener
    /// Blob **neben** dem Review, unter demselben Namen mit der Endung `.sig`.
    ///
    /// Die Folgen sind alle erwünscht: Der Hash eines Reviews ändert sich nicht,
    /// wenn jemand nachträglich signiert. Dasselbe Verdict kann von **mehreren**
    /// Identitäten signiert werden, ohne dass es zu mehreren Reviews wird. Und
    /// ein unsigniertes Verdict ist kein Sonderfall, sondern schlicht eines ohne
    /// Nachbarn.
    pub fn put_signature(&self, hash: &ContentHash, signature: &str) -> Result<()> {
        self.write_entry(
            &signature_path(hash),
            signature.as_bytes(),
            &format!("minds: Signatur zu Review {hash}"),
        )
    }

    /// Die Signatur zu einem Review — `None`, wenn es keine gibt.
    pub fn signature(&self, hash: &ContentHash) -> Result<Option<String>> {
        let found = self
            .repo
            .read_blob_at(&self.reference, &signature_path(hash))
            .map_err(StoreError::backend)?;
        Ok(found.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Übernimmt alles, was unter `other` liegt und hier fehlt — die
    /// **Vereinigung zweier Logs**. Gibt die Zahl der übernommenen Einträge
    /// zurück.
    ///
    /// # Warum das konfliktfrei ist
    ///
    /// Der Pfad eines Eintrags *ist* sein Inhalts-Hash. Zwei Logs, die denselben
    /// Pfad tragen, tragen damit denselben Inhalt — ein Konflikt im Sinne von
    /// „zwei Werte an einer Stelle" kann gar nicht entstehen. Die Vereinigung
    /// ist deshalb kommutativ und idempotent: Egal, wer zuerst mergt und wie
    /// oft, beide Seiten enden beim selben Baum.
    ///
    /// Das ist die Eigenschaft, die ein Review-Thread braucht, damit zwei
    /// Reviewer offline arbeiten können — und die `minds sync` benutzt, wenn ein
    /// Push non-fast-forward abgewiesen wird: fremden Stand holen, vereinigen,
    /// erneut pushen, ohne `--force`.
    ///
    /// `other` ist ein Ref-Name (typisch ein Tracking-Ref wie
    /// `refs/minds/remotes/origin/incoming`). Gibt es ihn nicht, ist nichts zu
    /// tun.
    pub fn merge_from(&self, other: &str) -> Result<usize> {
        let Some(theirs) = self.repo.tree_at(other).map_err(StoreError::backend)? else {
            return Ok(0);
        };
        let mine: BTreeSet<String> = self
            .repo
            .list_blobs_at(&self.reference)
            .map_err(StoreError::backend)?
            .into_iter()
            .collect();

        let mut entries: Vec<(String, minds_git::BlobId)> = Vec::new();
        for path in self
            .repo
            .list_blobs(theirs)
            .map_err(StoreError::backend)?
            .into_iter()
            .filter(|path| !mine.contains(path))
        {
            let Some(bytes) = self
                .repo
                .read_blob(theirs, &path)
                .map_err(StoreError::backend)?
            else {
                continue;
            };
            // Der Blob liegt schon in dieser Objektdatenbank — content-adressiert
            // ist das Schreiben ein Nachschlagen.
            let blob = self.repo.write_blob(&bytes).map_err(StoreError::backend)?;
            entries.push((path, blob));
        }

        if entries.is_empty() {
            return Ok(0);
        }
        let taken = entries.len();
        let message = format!("minds: {taken} Eintrag/Einträge aus {other} vereinigt");

        let mut attempts_left = PUT_ATTEMPTS;
        loop {
            attempts_left -= 1;
            let base = self
                .repo
                .tree_at(&self.reference)
                .map_err(StoreError::backend)?;
            let tree = self
                .repo
                .write_tree(
                    base,
                    entries.iter().map(|(path, blob)| (path.as_str(), *blob)),
                )
                .map_err(StoreError::backend)?;
            match self
                .repo
                .commit_tree_to_ref(&self.reference, tree, &message)
            {
                Ok(_) => return Ok(taken),
                Err(GitError::RefRaced { .. }) if attempts_left > 0 => {}
                Err(err) => return Err(StoreError::backend(err)),
            }
        }
    }

    /// Alle Reviews im Store, in Pfad-Reihenfolge. Beschädigte Einträge werden
    /// übersprungen (der Store bleibt lesbar).
    pub fn list(&self) -> Result<Vec<Review>> {
        // Nur die Verdicts — die `.sig`-Nachbarn sind Signaturen und die
        // `comments/` der Thread, kein zweites Verdict.
        Ok(self
            .entries("reviews/")?
            .into_iter()
            .filter_map(|bytes| serde_json::from_slice::<Review>(&bytes).ok())
            .collect())
    }

    /// Die Reviews zu einem Subjekt (Change-Id- oder SessionId-Text).
    pub fn for_subject(&self, subject_id: &str) -> Result<Vec<Review>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|review| review.subject.id() == subject_id)
            .collect())
    }
}

/// Der content-adressierte Pfad eines Reviews: `reviews/<2hex>/<rest>.json`.
fn review_path(hash: &ContentHash) -> String {
    let hex = hash.hex();
    format!("reviews/{}/{}.json", &hex[..2], &hex[2..])
}

/// Der content-adressierte Pfad eines Kommentars: `comments/<2hex>/<rest>.json`.
fn comment_path(hash: &ContentHash) -> String {
    let hex = hash.hex();
    format!("comments/{}/{}.json", &hex[..2], &hex[2..])
}

/// Der Pfad der Signatur daneben: derselbe Name mit `.sig`.
fn signature_path(hash: &ContentHash) -> String {
    let hex = hash.hex();
    format!("reviews/{}/{}.sig", &hex[..2], &hex[2..])
}

#[cfg(test)]
mod tests {
    use minds_core::{Decision, Review, Subject};

    use super::*;
    use crate::fixture::TempRepo;

    fn review(decision: Decision, summary: &str) -> Review {
        Review::new(
            Subject::Change(format!("I{}", "ab".repeat(20))),
            decision,
            "anna@example.org",
            summary,
            None,
        )
    }

    fn store() -> (TempRepo, ReviewStore) {
        let fixture = TempRepo::init();
        fixture.write_file("src/lib.rs", "fn main() {}\n");
        fixture.commit("code");
        let repo = Repo::open(fixture.path()).unwrap();
        (fixture, ReviewStore::new(repo))
    }

    #[test]
    fn a_review_roundtrips_and_is_found_by_subject() {
        let (_fixture, store) = store();
        let hash = store
            .put(&review(Decision::Approve, "sieht gut aus"))
            .unwrap();
        assert!(hash.as_str().starts_with("b3-"));

        let found = store.for_subject(&format!("I{}", "ab".repeat(20))).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].decision, Decision::Approve);
        assert_eq!(found[0].summary, "sieht gut aus");
    }

    #[test]
    fn the_same_review_dedups() {
        let (_fixture, store) = store();
        let a = store.put(&review(Decision::Approve, "gleich")).unwrap();
        let b = store.put(&review(Decision::Approve, "gleich")).unwrap();
        assert_eq!(a, b);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn several_verdicts_coexist() {
        let (_fixture, store) = store();
        store
            .put(&review(Decision::NeedsWork, "erst nacharbeiten"))
            .unwrap();
        store.put(&review(Decision::Approve, "jetzt gut")).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
    }

    #[test]
    fn a_signature_lives_beside_the_review_and_leaves_it_untouched() {
        let (_fixture, store) = store();
        let verdict = review(Decision::Approve, "geprüft");
        let hash = store.put(&verdict).unwrap();

        assert_eq!(store.signature(&hash).unwrap(), None);
        store
            .put_signature(&hash, "-----BEGIN SSH SIGNATURE-----\n…\n")
            .unwrap();

        assert!(
            store
                .signature(&hash)
                .unwrap()
                .unwrap()
                .contains("SSH SIGNATURE")
        );
        // Der Hash des Reviews bleibt derselbe — die Signatur ist ein Nachbar,
        // kein Feld.
        assert_eq!(store.put(&verdict).unwrap(), hash);
        // Und sie zählt nicht als zweites Verdict.
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn several_identities_can_sign_the_same_verdict() {
        // Ein Verdict, zwei Unterschriften: Weil die Signatur nicht im Envelope
        // steht, wird daraus kein zweites Review.
        let (_fixture, store) = store();
        let hash = store.put(&review(Decision::Approve, "geprüft")).unwrap();

        store.put_signature(&hash, "erste").unwrap();
        store.put_signature(&hash, "zweite").unwrap();

        assert_eq!(store.signature(&hash).unwrap().as_deref(), Some("zweite"));
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn two_divergent_logs_merge_to_the_same_state() {
        // Der Kern von R2: Zwei Reviewer arbeiten offline am selben Change.
        // Beide Logs müssen sich zum selben Zustand vereinigen — in beiden
        // Richtungen, ohne Konflikt.
        let (_fixture, store) = store();
        let other = ReviewStore {
            repo: Repo::open(_fixture.path()).unwrap(),
            reference: "refs/minds/reviews-anna".to_string(),
        };

        // Gemeinsamer Anfang, dann divergierende Fortsetzung.
        let common = review(Decision::NeedsWork, "erst nacharbeiten");
        store.put(&common).unwrap();
        other.put(&common).unwrap();
        store
            .put(&review(Decision::Approve, "von Bea geprüft"))
            .unwrap();
        other
            .put(&review(Decision::Reject, "von Anna abgelehnt"))
            .unwrap();

        assert_eq!(store.merge_from("refs/minds/reviews-anna").unwrap(), 1);
        assert_eq!(other.merge_from(DEFAULT_REVIEW_REF).unwrap(), 1);

        // Beide Seiten sehen jetzt dasselbe.
        let mut theirs: Vec<String> = other
            .list()
            .unwrap()
            .into_iter()
            .map(|review| review.summary)
            .collect();
        let mut ours: Vec<String> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|review| review.summary)
            .collect();
        ours.sort();
        theirs.sort();
        assert_eq!(ours, theirs);
        assert_eq!(ours.len(), 3);
    }

    #[test]
    fn two_divergent_threads_merge_conflict_free_to_the_same_state() {
        // Die Zusage von R2, wörtlich: Zwei Reviewer, beide offline, beide
        // kommentieren — und danach sehen beide dasselbe, ohne dass jemand
        // etwas auflösen musste.
        use minds_core::{Anchor, Comment};

        let (fixture, anna) = store();
        let bea = ReviewStore {
            repo: Repo::open(fixture.path()).unwrap(),
            reference: "refs/minds/reviews-bea".to_string(),
        };
        let subject = || Subject::Change(format!("I{}", "ab".repeat(20)));

        // Gemeinsamer Anfang.
        let common = Comment::new(
            subject(),
            Anchor::Whole,
            "anna@example.org",
            "Ich sehe mir den Backoff an.",
            Some("2026-07-28T10:00:00Z".into()),
        );
        anna.put_comment(&common).unwrap();
        bea.put_comment(&common).unwrap();

        // Dann divergierend, an verschiedenen Ankern.
        anna.put_comment(&Comment::new(
            subject(),
            Anchor::File {
                path: "src/retry.rs".into(),
                line: 42,
            },
            "anna@example.org",
            "Hier verdoppelt er zu früh.",
            Some("2026-07-28T10:05:00Z".into()),
        ))
        .unwrap();
        bea.put_comment(&Comment::new(
            subject(),
            Anchor::Turn { index: 3 },
            "bea@example.org",
            "Der Prompt sagt das aber so.",
            Some("2026-07-28T10:06:00Z".into()),
        ))
        .unwrap();

        // Jede zieht die andere — in beliebiger Reihenfolge.
        assert_eq!(anna.merge_from("refs/minds/reviews-bea").unwrap(), 1);
        assert_eq!(bea.merge_from(DEFAULT_REVIEW_REF).unwrap(), 1);

        let id = format!("I{}", "ab".repeat(20));
        let hers: Vec<String> = anna
            .thread(&id)
            .unwrap()
            .into_iter()
            .map(|c| format!("{} {}", c.anchor.as_text(), c.body))
            .collect();
        let theirs: Vec<String> = bea
            .thread(&id)
            .unwrap()
            .into_iter()
            .map(|c| format!("{} {}", c.anchor.as_text(), c.body))
            .collect();

        assert_eq!(hers.len(), 3);
        // Nicht nur derselbe Inhalt — dieselbe **Reihenfolge**. Sonst zeigten
        // zwei Maschinen denselben Thread verschieden an.
        assert_eq!(hers, theirs);
    }

    #[test]
    fn comments_and_verdicts_share_the_ref_without_mixing() {
        use minds_core::{Anchor, Comment};

        let (_fixture, store) = store();
        store.put(&review(Decision::Approve, "gut")).unwrap();
        store
            .put_comment(&Comment::new(
                Subject::Change(format!("I{}", "ab".repeat(20))),
                Anchor::Whole,
                "anna@example.org",
                "eine Anmerkung",
                None,
            ))
            .unwrap();

        // Ein Verdict, ein Kommentar — keines zählt als das andere.
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(
            store
                .thread(&format!("I{}", "ab".repeat(20)))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn merging_is_idempotent_and_needs_no_source() {
        let (_fixture, store) = store();
        let other = ReviewStore {
            repo: Repo::open(_fixture.path()).unwrap(),
            reference: "refs/minds/reviews-anna".to_string(),
        };
        other.put(&review(Decision::Approve, "von Anna")).unwrap();

        assert_eq!(store.merge_from("refs/minds/reviews-anna").unwrap(), 1);
        // Nochmal: nichts Neues, kein Leer-Commit.
        assert_eq!(store.merge_from("refs/minds/reviews-anna").unwrap(), 0);
        // Ein Ref, den es nicht gibt, ist kein Fehler.
        assert_eq!(store.merge_from("refs/minds/gibt-es-nicht").unwrap(), 0);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn reviews_live_under_their_own_ref() {
        let (fixture, store) = store();
        store.put(&review(Decision::Approve, "x")).unwrap();
        let refs = fixture.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"]);
        assert!(refs.contains("refs/minds/reviews"), "{refs}");
        // Nicht im Code-Branch sichtbar.
        assert!(!fixture.git(&["branch", "--list"]).contains("minds"));
    }
}
