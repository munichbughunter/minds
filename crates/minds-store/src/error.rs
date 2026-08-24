//! Der Fehlertyp dieses Crates — und die Naht zu den Backends.
//!
//! `minds-git` versteckt gix hinter [`Source`]-Boxen, weil gix bei 0.x steht.
//! Hier gilt derselbe Griff aus einem anderen Grund: Der Store ist ein *Trait*
//! mit austauschbaren Implementierungen. Stünde `GitError` in der Signatur, wäre
//! Git nicht mehr eine Implementierung, sondern Teil des Vertrags — und jedes
//! Backend, das kein Git ist (der In-Memory-Store der Tests, morgen vielleicht
//! ein Read-Only-Backend über ein Archiv), müsste einen Git-Fehler erfinden.
//!
//! Deshalb: Alles, was ein Backend nicht konnte, ist [`StoreError::Backend`].
//! Alles, was *unabhängig vom Backend* schiefgehen kann — Kanonisierung,
//! Content-Adressierung, fail-closed —, hat eine eigene Variante. Die Trennlinie
//! ist damit dieselbe wie beim Trait selbst.

use std::path::PathBuf;

use minds_core::evidence::SealParseError;
use minds_core::{CanonError, ContentHash, SessionId};

use crate::store::ForgottenPlace;

/// Kurzform für `Result` mit [`StoreError`].
pub type Result<T> = std::result::Result<T, StoreError>;

/// Die eingepackte Ursache eines Backend-Fehlers — beim Git-Backend ein
/// `minds_git::GitError`.
///
/// `Send + Sync`, damit Fehler über Thread-Grenzen wandern können (dieselbe
/// Zusage wie in `minds-git`).
pub type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Was beim Ablegen oder Holen einer Session schiefgehen kann.
///
/// `#[non_exhaustive]`: Die Backends kommen erst noch, und eine neue Variante
/// soll kein Breaking Change sein.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Die Session ließ sich nicht kanonisch serialisieren.
    ///
    /// In der Praxis heißt das: Ein Zähler liegt außerhalb des JCS-sicheren
    /// Bereichs (siehe `minds_core::canonical`). Ohne kanonische Bytes gibt es
    /// keine reproduzierbare ID — also wird nicht geschrieben.
    #[error("Session lässt sich nicht kanonisch serialisieren")]
    Canonical(#[from] CanonError),

    /// Die Kanten-Datei einer Session ist nicht lesbar.
    ///
    /// Die neue Kante wird dann **nicht** geschrieben — ein Zurückschreiben
    /// auf Basis einer frischen Liste schriebe den Verlust aller bisherigen
    /// Kanten aktiv fest. Die Lese-Seite bleibt tolerant (der Index ist eine
    /// heuristische Ergänzung); nachzugehen ist dem in `minds fsck`.
    #[error("links.json unter {reference} ist nicht lesbar — Kante nicht geschrieben")]
    CorruptLinks {
        /// Der betroffene Session-Ref.
        reference: String,
    },

    /// Die Session ist nicht als redigiert markiert.
    ///
    /// Auf dem Schreibpfad verhindert das schon der Typ
    /// ([`RedactedSession`](minds_redact::RedactedSession)); diese Variante ist
    /// der Gürtel zum Hosenträger und der reguläre Weg auf dem **Lese**pfad: Was
    /// von Hand oder von einer fremden Implementierung in den Ref geschrieben
    /// wurde, hat die Pipeline nie gesehen. Es unbesehen an den Reader zu geben,
    /// hieße, es in eine statische HTML-Seite zu rendern.
    #[error("Session {id} ist nicht als redigiert markiert")]
    Unredacted {
        /// Die betroffene Session.
        id: SessionId,
    },

    /// Der gespeicherte Inhalt hasht nicht auf die ID, unter der er liegt.
    ///
    /// Der Selbsttest, den Content-Adressierung gratis mitbringt: Wer die Datei
    /// im Store nachträglich editiert, fliegt beim nächsten Lesen auf. Ein
    /// Audit-Record, der still verändert werden kann, ist keiner.
    #[error("Inhalt unter {requested} hasht auf {actual} — der Store ist beschädigt")]
    Corrupt {
        /// Die angefragte — und damit erwartete — ID.
        requested: SessionId,
        /// Die ID, die der gespeicherte Inhalt tatsächlich ergibt.
        actual: SessionId,
    },

    /// Der gespeicherte Inhalt ist kein gültiges Session-JSON.
    ///
    /// Nicht zu verwechseln mit einer *neueren* Schema-Version: Unbekannte
    /// Felder werden toleriert (Architektur-Prinzip 4). Hier ist das JSON selbst
    /// kaputt oder ein Pflichtfeld fehlt.
    #[error("Inhalt unter {id} ist kein gültiges Session-JSON")]
    Malformed {
        /// Die betroffene Session.
        id: SessionId,
        /// Ursache aus serde_json.
        #[source]
        source: serde_json::Error,
    },

    /// Das konfigurierte Kontext-Repository lässt sich nicht öffnen.
    ///
    /// Der wahrscheinlichste Konfigurationsfehler des Child-Backends, und
    /// deshalb eine eigene Variante statt eines [`StoreError::Backend`]: Der
    /// Pfad steht in der Meldung, und die Meldung sagt, wer das Repository
    /// anlegt. Minds legt es **nicht** von sich aus an — ein Store, der
    /// nebenbei Repositories erzeugt, verwandelt einen Tippfehler im Pfad in
    /// einen zweiten, leeren Kontext-Speicher.
    #[error(
        "Kontext-Repository {path} lässt sich nicht öffnen (angelegt wird es von `minds init`)"
    )]
    ChildRepo {
        /// Der konfigurierte Pfad, bereits aufgelöst.
        path: PathBuf,
        /// Ursache aus dem Backend.
        #[source]
        source: Source,
    },

    /// Die Session wurde vergessen (`minds forget`): An ihrer Stelle liegt ein
    /// Tombstone.
    ///
    /// Kein Defekt, sondern eine bewusste Löschung — die Referenz bleibt
    /// auflösbar (`exists` bleibt `true`), nur der Inhalt ist weg. Der Reader und
    /// `show`/`why` zeigen das als „vergessen", nicht als Fehler.
    #[error("Session {id} wurde vergessen: {reason}")]
    Forgotten {
        /// Die vergessene Session.
        id: SessionId,
        /// Der beim Vergessen hinterlegte Grund (Audit).
        reason: String,
    },

    /// Ein `forget` blieb auf halbem Weg stehen: Manche Orte sind getilgt, an
    /// einem schlug das Schreiben fehl.
    ///
    /// Eine Session kann an mehreren Orten liegen (Store-Ref, Session-Branch,
    /// Kontext-Baum). Bricht die Tilgung zwischen zwei Orten ab, sind die schon
    /// getilgten weg, der offene trägt aber weiter Klartext — und das wäre von
    /// außen unsichtbar, wenn der Fehler nur „Backend" sagte: `get` fände am
    /// maßgeblichen Ort schon den Tombstone, während der Klartext anderswo
    /// liegenbliebe. Diese Variante nennt beide Seiten, damit klar ist, dass ein
    /// erneuter `minds forget` nötig ist — der die schon getilgten Orte
    /// überspringt (Idempotenz) und den offenen erneut vornimmt (#14).
    ///
    /// `pending` meint dabei „an diesem Ort ist die Löschung nicht vollständig".
    /// Das schließt den Fall ein, dass der Payload dort schon getilgt ist, aber
    /// das Umsetzen der Push-Buchhaltung (`refs/minds/remotes/*`) fehlschlug — der
    /// Klartext ist dann nur noch über einen Tracking-Ref erreichbar. Der Rat
    /// (erneut `forget`) stimmt und ist idempotent; die genaue offene Stelle steht
    /// in der Fehlerkette (`{:#}` / `source()`).
    #[error(
        "Session {id} nur teilweise vergessen: {}, aber {} blieb offen — `minds forget {id}` erneut ausführen, um die Löschung zu vollenden",
        describe_forgotten(.forgotten),
        .pending.label()
    )]
    ForgetIncomplete {
        /// Die Session, deren Tilgung unvollständig blieb.
        id: SessionId,
        /// Die Orte, die bereits getilgt sind.
        forgotten: Vec<ForgottenPlace>,
        /// Der Ort, an dem das Schreiben fehlschlug — er trägt weiter Klartext.
        pending: ForgottenPlace,
        /// Ursache aus dem Backend.
        #[source]
        source: Source,
    },

    /// Der Text ist kein gültiger Seal — er wird nicht abgelegt.
    ///
    /// Fail-closed am Schreibpfad: Ein Seal ist unser eigenes kanonisches
    /// Artefakt; was nicht parst, versiegelt nichts.
    #[error("kein gültiger Seal — nicht abgelegt")]
    InvalidSeal {
        /// Ursache aus dem Parser.
        #[source]
        source: SealParseError,
    },

    /// Der unter einer `seal_id` abgelegte Text hasht nicht auf diese Id.
    ///
    /// Dasselbe Gratis-Versprechen wie [`StoreError::Corrupt`] bei Sessions:
    /// Wer den Seal im Ref nachträglich editiert, fliegt beim Lesen auf.
    #[error("Seal unter {requested} hasht auf {actual} — der Seal wurde verändert")]
    SealMismatch {
        /// Die angefragte — und damit erwartete — Id.
        requested: ContentHash,
        /// Die Id, die der abgelegte Text tatsächlich ergibt.
        actual: ContentHash,
    },

    /// Das Backend konnte nicht lesen oder schreiben.
    ///
    /// Die Fassade: Was darunter liegt (gix, Dateisystem, Rechte), erreicht den
    /// Nutzer über die Fehlerkette (`{:#}` bzw. `source()`), nicht über den Typ.
    #[error("der Kontext-Speicher lässt sich nicht lesen oder schreiben")]
    Backend {
        /// Ursache aus dem Backend.
        #[source]
        source: Source,
    },
}

/// Zählt die getilgten Orte lesbar auf — für die Meldung von
/// [`StoreError::ForgetIncomplete`].
fn describe_forgotten(places: &[ForgottenPlace]) -> String {
    if places.is_empty() {
        // Der erste Ort schlug fehl — es ist noch nichts getilgt.
        "noch nichts getilgt".to_string()
    } else {
        format!(
            "{} bereits getilgt",
            places
                .iter()
                .map(|place| place.label())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl StoreError {
    /// Packt einen Backend-Fehler ein — der übliche `map_err`-Partner in einer
    /// Store-Implementierung.
    pub fn backend(source: impl Into<Source>) -> Self {
        Self::Backend {
            source: source.into(),
        }
    }

    pub(crate) fn child_repo(path: impl Into<PathBuf>, source: impl Into<Source>) -> Self {
        Self::ChildRepo {
            path: path.into(),
            source: source.into(),
        }
    }

    pub(crate) fn malformed(id: SessionId, source: serde_json::Error) -> Self {
        Self::Malformed { id, source }
    }

    /// Verpackt einen Schreibfehler mitten in einer mehrörtigen Tilgung: welche
    /// Orte schon getilgt sind und an welchem es hakte.
    pub(crate) fn forget_incomplete(
        id: SessionId,
        forgotten: Vec<ForgottenPlace>,
        pending: ForgottenPlace,
        source: impl Into<Source>,
    ) -> Self {
        Self::ForgetIncomplete {
            id,
            forgotten,
            pending,
            source: source.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    fn id(byte: u8) -> SessionId {
        format!("b3-{}", format!("{byte:02x}").repeat(32))
            .parse()
            .unwrap()
    }

    #[test]
    fn a_corrupt_store_names_both_ids() {
        // Die Meldung muss beide IDs tragen: Ohne die erwartete weiß niemand,
        // wonach gesucht wurde, ohne die tatsächliche nicht, was dort liegt.
        let err = StoreError::Corrupt {
            requested: id(0xaa),
            actual: id(0xbb),
        };
        let text = err.to_string();
        assert!(text.contains(&id(0xaa).to_string()));
        assert!(text.contains(&id(0xbb).to_string()));
    }

    #[test]
    fn backend_cause_survives_in_the_error_chain() {
        // Die Fassade darf die Diagnose nicht verschlucken.
        let err = StoreError::backend("Ref lässt sich nicht auflösen");
        assert_eq!(
            err.source().unwrap().to_string(),
            "Ref lässt sich nicht auflösen"
        );
    }
}
