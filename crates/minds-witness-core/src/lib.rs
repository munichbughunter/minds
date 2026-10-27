//! Prüf- und Quittungslogik des minds Server-Witness als reine Bibliothek.
//! Die Pre-Phase entscheidet über einen Push und erzeugt Befunde, die
//! Post-Phase signiert sie als Quittungen, `verify_receipt` prüft sie. Der
//! Kern macht kein I/O: Objekte, Seals und Ablage erreicht er nur über die
//! Traits in `traits`, und Ereignisse gibt er zurück, statt sie zu loggen.
//! Er kennt weder Git noch GitLab noch das Seal-Format.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod events;
mod finding;
mod ids;
mod policy;
mod post;
mod pre;
mod push;
mod receipt;
mod traits;

#[doc(hidden)]
pub mod memory;

pub use events::{LogEvent, LogLevel};
pub use finding::{
    Coverage, CoverageStatus, Finding, PolicyResult, PolicyVerdict, PushInfo, SealState, SealStatus,
};
pub use ids::{CommitId, HashStr, IdError, KeyStr, SigStr, Timestamp};
pub use policy::{Mode, Policy, PolicyError, PusherMode, covers, matches_pattern};
pub use post::{PostOutcome, post_receive};
pub use pre::{Decision, PreOutcome, RejectReason, pre_receive};
pub use push::{
    MAX_REF_DISPLAY, PushContext, Pusher, RefUpdate, ServerInfo, ServerKind, display_ref,
};
pub use receipt::{
    PublicKeyEntry, Receipt, ReceiptBody, ReceiptKey, Signer, VerifiedReceipt, VerifyError,
    WitnessIdentity, canonical_bytes, receipt_id, verify_receipt,
};
pub use traits::{
    CommitInfo, CommitRange, HostError, HostEvents, ObjectAccess, ObjectError, PutResult,
    ReceiptStore, SealBlob, SealError, SealExpectation, SealInspector, SealSignature, SealSummary,
    StoreError,
};

/// Präfix des Witness-Namensraums; Refs darunter schreibt nur der Server.
/// Nicht konfigurierbar.
pub const WITNESS_NAMESPACE: &str = "refs/minds/witness/";

/// Schema-Kennung jeder Quittung. Wechselt, sobald sich die kanonische Form
/// ändern würde.
pub const RECEIPT_SCHEMA: &str = "minds.witness.receipt/1";
