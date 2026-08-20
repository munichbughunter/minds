//! Der Kontext-Store: wo Sessions liegen — und was beim Hinlegen zugesichert ist.
//!
//! „Speicher ist ein Trait, kein Ort" (Architektur-Prinzip 2 im Plan). Dieses
//! Crate definiert mit [`ContextStore`] den Vertrag; die beiden Backends
//! (`InRepoStore` über `refs/minds/context`, `ChildRepoStore` über ein zweites
//! Repository) folgen in den nächsten Commits und unterscheiden sich nur im
//! Git-Handle. Deshalb steht hier bereits alles, was für beide gilt:
//! Kanonisierung, Content-Adressierung, fail-closed und die Prüfung beim Lesen.
//!
//! # Die Arbeitsteilung
//!
//! Der Trait hat zwei Schichten, und das ist der Kern seines Entwurfs:
//!
//! | Schicht | Methoden | wer implementiert sie |
//! |---|---|---|
//! | Bytes | [`put_bytes`](ContextStore::put_bytes), [`get_bytes`](ContextStore::get_bytes), [`list`](ContextStore::list) | jedes Backend |
//! | Sessions | [`put`](ContextStore::put), [`get`](ContextStore::get), [`exists`](ContextStore::exists) | der Trait selbst |
//!
//! Ein Backend sieht nur `SessionId → Bytes` und weiß von Redaction, blake3 und
//! JSON nichts. Die Zusagen liegen dagegen an *einer* Stelle und gelten für
//! jedes Backend, das je dazukommt — auch für eines, das heute niemand plant.
//! Wäre `put(&RedactedSession)` eine Pflichtmethode, müsste jedes Backend die
//! Kanonisierung selbst richtig machen; das zweite Backend, das es falsch macht,
//! schreibt Sessions, deren ID niemand reproduzieren kann.
//!
//! # Fail-closed ist hier ein Typ, kein Vorsatz
//!
//! [`ContextStore::put`] nimmt ein [`RedactedSession`](minds_redact::RedactedSession)
//! entgegen, nicht eine [`Session`](minds_core::Session). Diesen Typ gibt es nur
//! aus der Redaction-Pipeline — eine ungeredactete Session zu speichern ist
//! damit kein Policy-Verstoß, sondern ein Compile-Fehler (so von `minds-redact`
//! vorgesehen, siehe dortige `session`-Modul-Doku).
//!
//! Der Preis ist eine Kante am Abhängigkeitsgraphen: `store` hängt zusätzlich an
//! `redact`. Der Plan zeichnet `core, git ← store`; daraus wird
//! `core, redact, git ← store`. Kein Zyklus (`redact` kennt nur `core`), und der
//! Tausch ist es wert: Die Alternative wäre eine Laufzeitprüfung auf
//! `redaction.applied`, die genau so lange hilft, wie sie niemand vergisst.
//!
//! Auf dem **Lese**pfad bleibt sie trotzdem stehen — der Typ überlebt die
//! Serialisierung nicht, das Flag schon. Siehe [`ContextStore::get`].
//!
//! # Zwei Backends, eine Implementierung
//!
//! [`InRepoStore`] und [`ChildRepoStore`] sind Hüllen um denselben privaten
//! `GitStore` — sie setzen nur ein anderes Repository ein. „Gleiches Layout,
//! separates Repo-Handle" ist damit keine Zusage, auf die man aufpassen muss,
//! sondern eine, die sich nicht brechen lässt: Es gibt eine Stelle, die Pfade
//! baut und Refs bewegt. `both_backends_write_the_same_tree` weist nach, dass
//! zwei Repositories, die nichts voneinander wissen, bei derselben Session auf
//! denselben Baum-Hash kommen.
//!
//! Welches Backend es wird, entscheidet [`StoreConfig`] — an einer Stelle, und
//! danach sieht alles darüber nur noch `Box<dyn ContextStore>`. Beide Wege sind
//! erstklassig: ohne Child-Repo landet der Kontext im Repository des Codes
//! (Default, nichts einzurichten), mit Child-Repo nebenan.
//!
//! In **beiden** Fällen bleibt der Trailer im Produktions-Commit — die Nutzlast
//! zieht um, der Verweis nicht. Deshalb heißt der Zugriff auf das Repository
//! des Stores [`ChildRepoStore::context_repo`] und nicht `repo`: Es ist beim
//! Child-Backend nicht das Repo, an dessen Commits Trailer gehängt werden.
//!
//! # Löschen heißt hier: überschreiben, nicht entfernen
//!
//! Der Store ist ein Audit-Record und append-only — kein `remove` auf der
//! Objekt-Ebene. [`ContextStore::forget`] widerspricht dem **nicht**: Es ersetzt
//! die Nutzlast einer Session durch einen [`tombstone`], indem es einen neuen
//! Commit *anhängt*. Die Referenz bleibt auflösbar (`exists` bleibt `true`), der
//! Inhalt ist aus dem aktuellen Baum weg — die Antwort auf DSGVO-Löschung, die
//! reines Git nicht kann. Der alte Blob überlebt in der *Historie* des Refs, bis
//! ein separater History-Rewrite ihn tilgt (bewusst nicht Teil von v0.2).
//!
//! # Was hier bewusst *nicht* steht
//!
//! - **Ein verifizierender Konstruktor für fremde Bytes.** Sollte je ein
//!   Store-zu-Store-Umzug gebraucht werden (In-Repo → Child-Repo), ist das ein
//!   additiver Nachbar zu [`SessionBytes::of`], kein Umbau.
//!
//! # Kein `Send + Sync`
//!
//! Der Trait fordert es nicht, weil `minds_git::Repo` nicht `Sync` ist (gix
//! cacht intern beim Lesen). Wer parallel arbeiten will, öffnet pro Thread einen
//! eigenen Store — dieselbe Regel wie eine Ebene tiefer.

#[cfg(test)]
mod fixture;

mod error;
pub use error::{Result, Source, StoreError};

mod bytes;
pub use bytes::SessionBytes;

mod store;
pub use store::{ContextStore, Forget, ForgottenPlace, Put};

pub mod tombstone;

mod index;
pub use index::{CommitIndex, IndexLink};

mod reviews;
pub use reviews::{DEFAULT_REVIEW_REF, ReviewStore};

mod layout;

mod git_store;
pub use git_store::{TRACKING_REF_PREFIX, tombstone_at};

mod in_repo;
pub use in_repo::InRepoStore;

mod child_repo;
pub use child_repo::ChildRepoStore;

mod config;
pub use config::{Backend, StoreConfig};
