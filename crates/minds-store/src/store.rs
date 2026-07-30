//! Der [`ContextStore`]-Trait: put, get, exists, list — nach [`SessionId`].
//!
//! # Die ID kommt nie von außen
//!
//! [`ContextStore::put`] nimmt keine ID entgegen, es *gibt* eine zurück. Der
//! Schlüssel ist der Hash des Inhalts (Architektur-Prinzip 1), und ein
//! Aufrufer, der ihn mitliefern dürfte, könnte lügen — mit dem Ergebnis, dass
//! `minds why` einen Trailer auflöst und eine fremde Session zeigt. Ein
//! Audit-Record, dessen Schlüssel und Inhalt auseinanderlaufen können, ist
//! keiner.
//!
//! Auf dem Lesepfad prüft [`ContextStore::get`] deshalb nach: Was unter der ID
//! liegt, muss auf sie hashen. Das kostet einen blake3-Durchlauf über wenige
//! Kilobyte und macht aus der Content-Adressierung einen Selbsttest — wer eine
//! Session im Store nachträglich editiert, fliegt beim nächsten Lesen auf.
//!
//! # Dedup ist Abwesenheit von Arbeit
//!
//! Gleicher Inhalt ⇒ gleiche ID ⇒ derselbe Ort. Ein Backend muss dafür nichts
//! tun, es darf es nur nicht kaputtmachen; [`Put::AlreadyPresent`] ist der
//! Beleg, dass ein wiederholtes `put` nichts geschrieben hat. Das ist genau die
//! Zusage, die der nächste Commit (`idempotentes put`) für den `InRepoStore`
//! einlöst.
//!
//! # Was `get` nicht prüft
//!
//! Es wird **nicht** neu kanonisiert und mit dem Gespeicherten verglichen. Das
//! wäre die schärfere Prüfung — und die falsche: Eine Session aus einer
//! neueren Schema-Version trägt Felder, die dieses Binary nicht kennt und beim
//! Deserialisieren verwirft (Vorwärts-Toleranz, Architektur-Prinzip 4). Neu
//! serialisiert käme etwas Kürzeres heraus, und ein alter Reader würde jede
//! neuere Session für beschädigt erklären. Geprüft wird deshalb gegen die
//! **gespeicherten Bytes**, nicht gegen das, was wir daraus wieder machen
//! würden.

use minds_core::{Evidence, Session, SessionId};
use minds_redact::RedactedSession;

use crate::bytes::SessionBytes;
use crate::error::{Result, StoreError};
use crate::index::CommitIndex;

/// Was [`ContextStore::put`] bewirkt hat.
///
/// Für den Aufrufer selten handlungsrelevant, für die CLI-Ausgabe und das
/// Protokoll aber der Unterschied zwischen „erfasst" und „kannten wir schon".
/// Gleiche Bauform wie `minds_git::RefUpdate` eine Ebene tiefer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Put {
    /// Die Session lag noch nicht im Store und wurde geschrieben.
    Written(SessionId),
    /// Die Session lag bereits unter dieser ID — nichts geschrieben.
    AlreadyPresent(SessionId),
}

impl Put {
    /// Die ID der Session, in beiden Fällen.
    pub fn id(&self) -> SessionId {
        match self {
            Put::Written(id) | Put::AlreadyPresent(id) => *id,
        }
    }

    /// Ob dabei tatsächlich geschrieben wurde.
    pub fn was_written(&self) -> bool {
        matches!(self, Put::Written(_))
    }
}

/// Was [`ContextStore::forget`] bewirkt hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forget {
    /// Der Inhalt lag vor und wurde durch einen Tombstone ersetzt.
    Forgotten(SessionId),
    /// Unter der ID lag nichts (oder schon ein Tombstone) — nichts zu tun.
    Absent(SessionId),
}

impl Forget {
    /// Die betroffene ID, in beiden Fällen.
    pub fn id(&self) -> SessionId {
        match self {
            Forget::Forgotten(id) | Forget::Absent(id) => *id,
        }
    }

    /// Ob dabei tatsächlich ein Inhalt ersetzt wurde.
    pub fn was_forgotten(&self) -> bool {
        matches!(self, Forget::Forgotten(_))
    }
}

/// Ein Speicher für Sessions, adressiert über ihre [`SessionId`].
///
/// Zu implementieren sind nur die drei Byte-Methoden; [`put`](Self::put),
/// [`get`](Self::get) und [`exists`](Self::exists) kommen vom Trait und gelten
/// damit für jedes Backend gleich (siehe Crate-Doku).
///
/// Der Trait ist objekt-sicher: Die CLI wählt ihr Backend zur Laufzeit aus der
/// Konfiguration und hält es als `Box<dyn ContextStore>`.
pub trait ContextStore {
    /// Legt eine Session ab, wenn sie noch nicht da ist.
    ///
    /// Muss idempotent sein: Liegt unter `session.id()` bereits etwas, wird
    /// nichts geschrieben und [`Put::AlreadyPresent`] zurückgegeben. Weil die ID
    /// der Hash des Inhalts ist, kann „liegt bereits da" nichts anderes heißen
    /// als „liegt bereits *so* da".
    fn put_bytes(&self, session: &SessionBytes) -> Result<Put>;

    /// Holt die rohen, gespeicherten Bytes — `None`, wenn nichts unter `id`
    /// liegt.
    ///
    /// Ungeprüft: weder gehasht noch geparst. Das ist der Durchreich-Pfad für
    /// den Reader, der das JSON unverändert weitergibt. Wer eine
    /// [`Session`] will, nimmt [`get`](Self::get).
    fn get_bytes(&self, id: SessionId) -> Result<Option<Vec<u8>>>;

    /// Alle IDs im Store — sortiert und ohne Dopplungen.
    ///
    /// Die Sortierung ist eine Zusage an Tests und an den Reader-Index: Eine
    /// Reihenfolge, die von der Traversierung des Backends abhängt, wäre für
    /// beide unbrauchbar.
    fn list(&self) -> Result<Vec<SessionId>>;

    /// Die rohen Bytes des Commit-Index (`index.json`) — `None`, wenn keiner
    /// abgelegt ist.
    ///
    /// Der Index ist der einzige Nachbar der Sessions im Baum (siehe
    /// [`crate::index`] und [`crate::layout`]). Er wird über denselben Ref
    /// geschrieben und reist deshalb beim Push mit.
    fn get_index_bytes(&self) -> Result<Option<Vec<u8>>>;

    /// Schreibt die rohen Bytes des Commit-Index.
    fn put_index_bytes(&self, bytes: &[u8]) -> Result<()>;

    /// Vergisst eine Session (DSGVO): ersetzt ihre Nutzlast durch einen
    /// [`tombstone`](crate::tombstone). Die content-adressierte Referenz bleibt
    /// auflösbar — `exists` liefert weiter `true`, `get` meldet
    /// [`StoreError::Forgotten`]. `reason` wandert in den Tombstone (Audit).
    ///
    /// **Append-only bleibt:** der Tombstone wird als neuer Commit angehängt; der
    /// alte Blob überlebt in der Historie des Refs, bis ein separater
    /// History-Rewrite ihn tilgt (kein Teil von v0.2).
    fn forget(&self, id: SessionId, reason: &str) -> Result<Forget>;

    /// Ob unter `id` etwas liegt.
    ///
    /// Der Default liest den Inhalt und wirft ihn weg — korrekt, aber
    /// verschwenderisch. Ein Backend, das billiger nachsehen kann (ein
    /// Baum-Lookup ohne den Blob zu lesen), überschreibt das.
    fn exists(&self, id: SessionId) -> Result<bool> {
        Ok(self.get_bytes(id)?.is_some())
    }

    /// Der Commit-Index, geparst.
    ///
    /// Fehlt er, ist er leer. Ist er beschädigt, ebenfalls leer statt ein Fehler:
    /// Ein kaputter Index darf `minds show` nicht abschießen — er ist eine
    /// heuristische Ergänzung, kein tragendes Teil. `minds fsck` ist der Ort, dem
    /// nachzugehen.
    fn index(&self) -> Result<CommitIndex> {
        match self.get_index_bytes()? {
            Some(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            None => Ok(CommitIndex::default()),
        }
    }

    /// Legt den Commit-Index ab.
    fn set_index(&self, index: &CommitIndex) -> Result<()> {
        // Ein Index aus Strings und Enums serialisiert immer; ein Fehler hier
        // wäre ein Bug im Typ, kein Laufzeitzustand.
        let bytes = serde_json::to_vec(index).expect("CommitIndex serialisiert immer");
        self.put_index_bytes(&bytes)
    }

    /// Trägt eine einzelne Kante `commit → session` ein.
    ///
    /// # Warum nicht einfach [`set_index`](Self::set_index)
    ///
    /// `set_index` schreibt den **ganzen** Index — und damit an einer Stelle, die
    /// alle teilen. Für den heißen Pfad (jeder Checkpoint trägt seine Kante ein)
    /// ist das die letzte verbliebene Enge: zwei Agents, die gleichzeitig
    /// eintragen, rennen in einen Compare-and-Swap, und zwei Maschinen, die
    /// beide eintragen, divergieren beim Push.
    ///
    /// `link` schreibt stattdessen **an die Session**, der die Kante gehört. Ein
    /// Backend, das Sessions einzeln ablegt, fasst damit nur einen Ref an — den,
    /// den ohnehin nur diese eine Session benutzt.
    ///
    /// Der Default bleibt der alte Weg (lesen, ergänzen, ganz zurückschreiben),
    /// damit ein Store, der keine Session-Refs kennt, weiter funktioniert.
    fn link(&self, session: SessionId, commit_hex: &str, evidence: Evidence) -> Result<()> {
        let mut index = self.index()?;
        index.link(commit_hex.to_owned(), session, evidence);
        self.set_index(&index)
    }

    /// Legt eine geprüfte Session ab und gibt ihre ID zurück.
    ///
    /// Der reguläre Schreibweg. Kanonisiert, hasht und reicht an
    /// [`put_bytes`](Self::put_bytes) weiter — mehr passiert nicht, und genau
    /// deshalb ist es an einer Stelle richtig statt in jedem Backend erneut.
    ///
    /// Dass hier eine [`RedactedSession`] verlangt wird und keine [`Session`],
    /// ist die fail-closed-Zusage in Typform (siehe Crate-Doku).
    fn put(&self, session: &RedactedSession) -> Result<Put> {
        self.put_bytes(&SessionBytes::of(session)?)
    }

    /// Macht `session` als eigenständigen **Branch** in der Forge sichtbar.
    ///
    /// Legt einen Ref pro Session an (`refs/minds/sessions/<hash>`), dessen Baum
    /// die Session als `session.json` trägt. Der Push des Child-Backends mappt
    /// ihn auf `refs/heads/minds/session/<hash>` — so erscheint jede Session in
    /// GitLab als eigener, auswählbarer Branch mit ihrer `session.json`, ein
    /// Branch je Session.
    ///
    /// Der Default tut **nichts**. Nur das Child-Repo-Backend legt diese
    /// Branches an; im In-Repo-Backend lägen sie im Repository des Codes und
    /// tauchten beim Push in dessen Branch-Liste auf — das verstieße gegen
    /// Punkt 8 der Definition of Done („Wer Minds nicht nutzt, merkt nichts").
    fn put_session_branch(&self, _session: &RedactedSession) -> Result<()> {
        Ok(())
    }

    /// Holt die Session unter `id` — `None`, wenn sie hier nicht liegt.
    ///
    /// `None` heißt „nicht in diesem Store", nicht „gibt es nicht": Beim
    /// Child-Repo-Backend kann derselbe Trailer auflösbar werden, sobald der
    /// Kontext-Ref gefetcht ist. Das ist die *graceful degradation* aus dem Plan
    /// — ein leeres Ergebnis, kein harter Fehler.
    ///
    /// # Fehler
    ///
    /// - [`StoreError::Corrupt`] — der Inhalt hasht nicht auf `id`.
    /// - [`StoreError::Malformed`] — der Inhalt ist kein gültiges Session-JSON.
    /// - [`StoreError::Unredacted`] — die Session ist nicht als redigiert
    ///   markiert. Sie stammt dann nicht aus diesem Werkzeug; sie an den Reader
    ///   zu geben hieße, ungeprüften Text in eine HTML-Seite zu rendern. Wer die
    ///   Bytes trotzdem braucht (Forensik), nimmt
    ///   [`get_bytes`](Self::get_bytes).
    fn get(&self, id: SessionId) -> Result<Option<Session>> {
        let Some(bytes) = self.get_bytes(id)? else {
            return Ok(None);
        };

        // Ein Tombstone kommt vor dem Hash-Test: er hasht bewusst nicht auf `id`
        // (der Inhalt ist ersetzt), ist aber kein Defekt, sondern eine Löschung.
        if let Some(reason) = crate::tombstone::reason(&bytes) {
            return Err(StoreError::Forgotten { id, reason });
        }

        let actual = SessionId::from_canonical_bytes(&bytes);
        if actual != id {
            return Err(StoreError::Corrupt {
                requested: id,
                actual,
            });
        }

        let session: Session =
            serde_json::from_slice(&bytes).map_err(|err| StoreError::malformed(id, err))?;

        if !session.redaction.applied {
            return Err(StoreError::Unredacted { id });
        }

        Ok(Some(session))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use minds_core::{Agent, Intent, Model, to_canonical_json};

    use super::*;
    use crate::fixture::redacted;

    /// Ein Store im Speicher — Referenz-Implementierung für den Trait-Vertrag.
    ///
    /// Er hält genau das, was jedes Backend hält: `SessionId → Bytes`. Was diese
    /// Tests prüfen, sind deshalb die Zusagen des *Traits*, nicht die von Git;
    /// die Git-Seite bekommt ihre eigenen Tests mit dem `InRepoStore`.
    #[derive(Default)]
    struct MemoryStore {
        entries: RefCell<BTreeMap<SessionId, Vec<u8>>>,
        index: RefCell<Option<Vec<u8>>>,
    }

    impl MemoryStore {
        /// Schreibt am Trait vorbei — nur so lässt sich ein beschädigter Store
        /// herstellen, den es sonst nicht geben kann.
        fn insert_raw(&self, id: SessionId, bytes: impl Into<Vec<u8>>) {
            self.entries.borrow_mut().insert(id, bytes.into());
        }
    }

    impl ContextStore for MemoryStore {
        fn put_bytes(&self, session: &SessionBytes) -> Result<Put> {
            let mut entries = self.entries.borrow_mut();
            if entries.contains_key(&session.id()) {
                return Ok(Put::AlreadyPresent(session.id()));
            }
            entries.insert(session.id(), session.as_bytes().to_vec());
            Ok(Put::Written(session.id()))
        }

        fn get_bytes(&self, id: SessionId) -> Result<Option<Vec<u8>>> {
            Ok(self.entries.borrow().get(&id).cloned())
        }

        fn list(&self) -> Result<Vec<SessionId>> {
            // BTreeMap iteriert sortiert — die Zusage des Traits fällt hier
            // gratis ab.
            Ok(self.entries.borrow().keys().copied().collect())
        }

        fn get_index_bytes(&self) -> Result<Option<Vec<u8>>> {
            Ok(self.index.borrow().clone())
        }

        fn put_index_bytes(&self, bytes: &[u8]) -> Result<()> {
            *self.index.borrow_mut() = Some(bytes.to_vec());
            Ok(())
        }

        fn forget(&self, id: SessionId, reason: &str) -> Result<Forget> {
            let mut entries = self.entries.borrow_mut();
            match entries.get(&id) {
                None => Ok(Forget::Absent(id)),
                Some(bytes) if crate::tombstone::reason(bytes).is_some() => Ok(Forget::Absent(id)),
                Some(_) => {
                    entries.insert(id, crate::tombstone::bytes(reason));
                    Ok(Forget::Forgotten(id))
                }
            }
        }
    }

    // --- Der Kern-Vertrag ----------------------------------------------------

    #[test]
    fn put_returns_the_id_the_session_hashes_to() {
        let store = MemoryStore::default();
        let session = redacted("Retry-Test reparieren");

        let put = store.put(&session).unwrap();

        assert!(put.was_written());
        assert_eq!(put.id(), session.session().id().unwrap());
    }

    #[test]
    fn put_then_get_roundtrips_the_session() {
        let store = MemoryStore::default();
        let session = redacted("Retry-Test reparieren");

        let id = store.put(&session).unwrap().id();

        assert_eq!(store.get(id).unwrap().as_ref(), Some(session.session()));
    }

    #[test]
    fn putting_the_same_session_twice_stores_it_once() {
        // Dedup per Hash: Der zweite Lauf schreibt nichts und liefert dieselbe
        // ID. Zwei `minds capture` auf dieselbe Session kosten einen Eintrag.
        let store = MemoryStore::default();

        let first = store.put(&redacted("gleicher Inhalt")).unwrap();
        let second = store.put(&redacted("gleicher Inhalt")).unwrap();

        assert!(first.was_written());
        assert!(!second.was_written());
        assert_eq!(first.id(), second.id());
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn get_is_none_for_an_id_that_is_not_here() {
        // Nicht hier heißt nicht „gibt es nicht" — beim Child-Backend kann
        // dieselbe ID nach einem Fetch auflösbar werden.
        let store = MemoryStore::default();
        let unknown = redacted("nie gespeichert").session().id().unwrap();

        assert_eq!(store.get(unknown).unwrap(), None);
        assert_eq!(store.get_bytes(unknown).unwrap(), None);
    }

    #[test]
    fn exists_mirrors_what_was_put() {
        let store = MemoryStore::default();
        let session = redacted("Retry-Test reparieren");
        let absent = redacted("etwas anderes").session().id().unwrap();

        let id = store.put(&session).unwrap().id();

        assert!(store.exists(id).unwrap());
        assert!(!store.exists(absent).unwrap());
    }

    #[test]
    fn list_is_sorted_and_holds_every_session() {
        let store = MemoryStore::default();
        let mut expected: Vec<SessionId> = ["Fall A", "Fall B", "Fall C"]
            .into_iter()
            .map(|request| store.put(&redacted(request)).unwrap().id())
            .collect();
        expected.sort();

        assert_eq!(store.list().unwrap(), expected);
    }

    #[test]
    fn an_empty_store_lists_nothing() {
        // Der Zustand jedes Repos vor dem ersten `minds capture`.
        let store = MemoryStore::default();
        assert!(store.list().unwrap().is_empty());
    }

    // --- Prüfungen beim Lesen ------------------------------------------------

    #[test]
    fn content_that_does_not_hash_to_its_id_is_refused() {
        // Jemand hat den Blob im Store editiert. Content-Adressierung heißt,
        // dass genau das auffliegt.
        let store = MemoryStore::default();
        let session = redacted("Retry-Test reparieren");
        let wrong_id = redacted("eine ganz andere Session").session().id().unwrap();

        store.insert_raw(wrong_id, to_canonical_json(session.session()).unwrap());

        let err = store.get(wrong_id).unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::Corrupt { requested, actual }
                    if requested == wrong_id && actual == session.session().id().unwrap()
            ),
            "erwartet Corrupt, war: {err:?}"
        );
    }

    #[test]
    fn content_that_is_not_session_json_is_refused() {
        let store = MemoryStore::default();
        let junk = b"kein JSON".to_vec();
        let id = SessionId::from_canonical_bytes(&junk);

        store.insert_raw(id, junk);

        let err = store.get(id).unwrap_err();
        assert!(
            matches!(err, StoreError::Malformed { .. }),
            "erwartet Malformed, war: {err:?}"
        );
    }

    #[test]
    fn an_unredacted_session_in_the_store_is_refused() {
        // Über `put` nicht herstellbar — aber von Hand oder von einer fremden
        // Implementierung in den Ref geschrieben schon. Der Reader bekommt es
        // nicht zu sehen.
        let store = MemoryStore::default();
        let raw = Session::new(
            Agent {
                name: "fremd".into(),
                version: "0".into(),
            },
            Model {
                provider: "fremd".into(),
                id: "0".into(),
            },
            Intent::default(),
        );
        assert!(!raw.redaction.applied);

        let bytes = to_canonical_json(&raw).unwrap();
        let id = SessionId::from_canonical_bytes(&bytes);
        store.insert_raw(id, bytes);

        let err = store.get(id).unwrap_err();
        assert!(
            matches!(err, StoreError::Unredacted { id: refused } if refused == id),
            "erwartet Unredacted, war: {err:?}"
        );
        // Die Bytes bleiben erreichbar — Forensik ja, Rendern nein.
        assert!(store.get_bytes(id).unwrap().is_some());
    }

    // --- Bauform -------------------------------------------------------------

    // --- Vergessen (DSGVO) ---------------------------------------------------

    #[test]
    fn forgetting_a_session_leaves_a_resolvable_tombstone() {
        let store = MemoryStore::default();
        let session = redacted("streng geheim");
        let id = store.put(&session).unwrap().id();

        let outcome = store.forget(id, "DSGVO-Antrag").unwrap();
        assert!(outcome.was_forgotten());

        // get meldet Forgotten (kein Corrupt, kein None), mit Grund.
        match store.get(id).unwrap_err() {
            StoreError::Forgotten { reason, .. } => assert_eq!(reason, "DSGVO-Antrag"),
            other => panic!("erwartet Forgotten, war: {other:?}"),
        }

        // Die Referenz bleibt auflösbar — exists true, list führt sie weiter.
        assert!(store.exists(id).unwrap());
        assert_eq!(store.list().unwrap(), vec![id]);

        // Der Inhalt ist weg.
        let raw = store.get_bytes(id).unwrap().unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("streng geheim"));
    }

    #[test]
    fn forgetting_is_idempotent_and_absent_when_nothing_is_there() {
        let store = MemoryStore::default();
        let unknown = redacted("nie da").session().id().unwrap();
        assert!(!store.forget(unknown, "x").unwrap().was_forgotten());

        let id = store.put(&redacted("da")).unwrap().id();
        assert!(store.forget(id, "erst").unwrap().was_forgotten());
        // Ein zweiter Lauf sieht den Tombstone und tut nichts.
        assert!(!store.forget(id, "zweit").unwrap().was_forgotten());
    }

    #[test]
    fn the_trait_is_object_safe() {
        // Die CLI wählt ihr Backend zur Laufzeit; ohne das hier ginge das nicht.
        let store = MemoryStore::default();
        let boxed: Box<dyn ContextStore> = Box::new(store);

        let id = boxed.put(&redacted("über dyn")).unwrap().id();
        assert!(boxed.exists(id).unwrap());
    }
}
