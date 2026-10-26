//! Prüf- und Quittungslogik des minds Server-Witness als reine Bibliothek.
//! Die Pre-Phase entscheidet über einen Push und erzeugt Befunde, die
//! Post-Phase signiert sie als Quittungen, `verify_receipt` prüft sie. Der
//! Kern macht kein I/O: Objekte, Seals und Ablage erreicht er nur über die
//! Traits in `traits`, und Ereignisse gibt er zurück, statt sie zu loggen.
//! Er kennt weder Git noch GitLab noch das Seal-Format.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod events;
mod ids;
mod push;

pub use events::{LogEvent, LogLevel};
pub use ids::{CommitId, HashStr, IdError, KeyStr, SigStr, Timestamp};
pub use push::{PushContext, Pusher, RefUpdate, ServerInfo, ServerKind};

/// Präfix des Witness-Namensraums; Refs darunter schreibt nur der Server.
/// Nicht konfigurierbar.
pub const WITNESS_NAMESPACE: &str = "refs/minds/witness/";

/// Schema-Kennung jeder Quittung. Wechselt, sobald sich die kanonische Form
/// ändern würde.
pub const RECEIPT_SCHEMA: &str = "minds.witness.receipt/1";
