//! Pre-Phase: entscheidet über einen Push und erzeugt je bewertetem Commit
//! ein `Finding`. Die Funktion ist deterministisch, wirft nie einen Fehler
//! nach außen und macht jeden Fehler zu einer Entscheidung plus Ereignis.
//! Sie signiert und speichert nichts; das ist Aufgabe der Post-Phase.

use serde::{Deserialize, Serialize};

/// Grund einer Ablehnung.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// Schreibversuch in `refs/minds/witness/`.
    WitnessNamespace,
    /// Ein erzwungener Ref hat die Prüfung nicht bestanden oder wurde nicht
    /// vollständig geprüft.
    PolicyFail,
    /// Interner Fehler im Enforce-Modus (fail-closed).
    InternalError,
}

/// Entscheidung über einen Push.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Push annehmen.
    Accept,
    /// Push als Ganzes ablehnen.
    Reject {
        /// Grund.
        reason: RejectReason,
        /// Einzeiliger deutscher Satz mit Präfix `minds-witness:`; geht
        /// unverändert an den Client.
        message: String,
    },
}
