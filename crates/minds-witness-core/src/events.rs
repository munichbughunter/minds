//! Log-Ereignisse als Rückgabewerte. Der Kern protokolliert nicht selbst:
//! er sammelt Ereignisse in Entstehungsreihenfolge und gibt sie dem Adapter
//! zurück, der entscheidet, wohin sie gehen. Ereignisse enthalten nie
//! Objektinhalte, nur Ids, Ref-Namen, Pfade, Zähler und Fehlertexte.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Schwere eines Ereignisses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Normaler Ablauf.
    Info,
    /// Auffälligkeit ohne Fehler (etwa Ablehnung oder gekürzte Prüfung).
    Warn,
    /// Fehler; der Kern ist trotzdem zu einem Ergebnis gekommen.
    Error,
}

/// Ein strukturiertes Ereignis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEvent {
    /// Schwere.
    pub level: LogLevel,
    /// Maschinenlesbarer Code aus den Listen in Abschnitt 7 und 8 der Spec.
    pub code: String,
    /// Deutscher Satz ohne Zeilenumbruch.
    pub message: String,
    /// Genau die für den Code festgelegten Felder, alle als String.
    pub fields: BTreeMap<String, String>,
}
