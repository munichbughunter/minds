//! Prüf- und Quittungslogik des minds Server-Witness als reine Bibliothek.
//! Die Pre-Phase entscheidet über einen Push und erzeugt Befunde, die
//! Post-Phase signiert sie als Quittungen, `verify_receipt` prüft sie. Der
//! Kern macht kein I/O: Objekte, Seals und Ablage erreicht er nur über die
//! Traits in `traits`, und Ereignisse gibt er zurück, statt sie zu loggen.
//! Er kennt weder Git noch GitLab noch das Seal-Format.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod assess;
mod events;
mod finding;
mod ids;
mod policy;
mod post;
mod pre;
mod push;
mod receipt;
mod traits;
mod trust;

#[doc(hidden)]
pub mod memory;

pub use assess::{Assessment, SealLookup, assess, judge};
pub use events::{LogEvent, LogLevel};
pub use finding::{
    Coverage, CoverageStatus, Finding, PolicyResult, PolicyVerdict, PushInfo, SealState, SealStatus,
};
pub use ids::{CommitId, HashStr, IdError, KeyStr, SigStr, Timestamp};
pub use policy::{Mode, Policy, PolicyError, PusherMode, covers, matches_pattern};
pub use post::{PostOutcome, post_receive, post_receive_with_trust};
pub use pre::{Decision, PreOutcome, RejectReason, pre_receive};
pub use push::{
    MAX_REF_DISPLAY, PushContext, Pusher, RefUpdate, ServerInfo, ServerKind, display_ref,
};
pub use receipt::{
    PublicKeyEntry, Receipt, ReceiptBody, ReceiptKey, Signer, VerifiedReceipt, VerifyError,
    WitnessIdentity, canonical_bytes, receipt_id, verify_receipt, verify_receipt_with_trust,
};
pub use traits::{
    CommitInfo, CommitRange, HostError, HostEvents, ObjectAccess, ObjectError, PutResult,
    ReceiptStore, SealBlob, SealError, SealExpectation, SealInspector, SealSignature, SealSummary,
    StoreError,
};
pub use trust::{TrustPolicy, TrustSnapshot};

/// Präfix des Witness-Namensraums; Refs darunter schreibt nur der Server.
/// Nicht konfigurierbar.
pub const WITNESS_NAMESPACE: &str = "refs/minds/witness/";

/// Schema-Kennung einer Quittung ohne Vertrauenskontext (S1). Bytes,
/// Signatur und Prüfregeln dieser Quittungen bleiben unverändert.
pub const RECEIPT_SCHEMA: &str = "minds.witness.receipt/1";

/// Schema-Kennung einer Quittung mit Vertrauenskontext (S1 v1.6, Spec S3
/// §5): `trust` ist Pflicht und bindet die Quittung an einen Trust-Snapshot.
pub const RECEIPT_SCHEMA_V2: &str = "minds.witness.receipt/2";

/// Schema-Kennung eines Trust-Snapshots (Spec S3 §4.3).
pub const TRUST_SCHEMA: &str = "minds.witness.trust/1";
