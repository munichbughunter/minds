//!
//! Dieses Crate definiert das Session-Envelope (den dauerhaften Record einer
//! Agent-Session). In späteren Commits kommen die kanonische
//! JSON-Serialisierung, die blake3-`SessionId`, die Trailer-Typen und das
//! Attribution-Modell hinzu. Abhängigkeiten: nur `serde` (und später `blake3`).
//! Kein Git, kein Netz, keine Seiteneffekte.

mod session;
pub use session::{
    Agent, Capture, CaptureStatus, Intent, Model, Produced, Redaction, RedactionCounts, Role,
    SCHEMA_VERSION, Session, ToolCall, Turn, Usage,
};

mod lineage;
pub use lineage::{
    CONTENT_HASH_PREFIX, ContentHash, ContentHashParseError, Edge, EdgeKind, Effect, EffectKind,
    Endpoint, Evidence, EvidenceMark, EvidenceSource, EvidenceStatus, Lineage,
};

/// Die Evidence-Chain-Primitive (ADR-0011): Hashes, Lücken, Fold.
///
/// Bewusst als Modul-Pfad (`minds_core::evidence::…`) statt flach re-exportiert
/// — die Namen (`EventFacts`, `Coverage`) sind generisch und würden auf
/// Crate-Ebene mit Nachbarn kollidieren.
pub mod evidence;

mod attribution;
pub use attribution::{Attribution, AttributionError};

mod attest;
pub use attest::{ATTESTATION_VERSION, PayloadError, attestation_payload};

mod comment;
pub use comment::{Anchor, COMMENT_SCHEMA_VERSION, Comment, order_key};

mod review;
pub use review::{
    Decision, REVIEW_ATTESTATION_VERSION, REVIEW_SCHEMA_VERSION, Review, Subject, review_payload,
};

pub mod extract;
pub use extract::{CoChange, CommandFact, Correction, Extract, FileFact, Rework, ReworkKind};

pub mod markdown;
pub use markdown::session_markdown;

mod change;
pub use change::{CHANGE_ID_PREFIX, ChangeId, ChangeIdParseError};

mod trailer; // nach `mod session;`
pub use trailer::{CHANGE_ID_TRAILER_KEY, SESSION_ID_TRAILER_KEY, Trailer, TrailerParseError};

pub mod canonical;
pub use canonical::{CanonError, to_canonical_json, to_canonical_string};

mod id;
pub use id::{SESSION_ID_PREFIX, SessionId, SessionIdParseError};
