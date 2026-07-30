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

use minds_core::{CanonError, SessionId};

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
