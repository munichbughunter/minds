//! Die Kandidatenmenge der Bindungen eines Commits als **ein** `SealBlob`
//! (Spec S2 §7.5, S1 v1.5 V3): kanonisches JSON mit Ref-Namen und Oids,
//! ohne Objektinhalte. `ref_name` des Blobs ist das Präfix
//! `refs/minds/bindings/<c>/`.

use minds_witness_core::SealBlob;
use serde::{Deserialize, Serialize};

use crate::binding::{REF_PREFIX, is_commit_id};

/// Wert des Feldes `kind`.
pub const KIND: &str = "binding_candidates";

/// Ein Kandidat: Ref-Name und Ziel-Commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    /// Voller Ref-Name `refs/minds/bindings/<c>/<id>`.
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// Ziel des Refs (Commit-Id).
    pub target: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    candidates: Vec<Candidate>,
    commit: String,
    kind: String,
}

/// Das Präfix der Bindungen von `commit`.
pub fn prefix(commit: &str) -> String {
    format!("{REF_PREFIX}{commit}/")
}

/// Baut den Blob; `candidates` werden nach Ref-Name sortiert.
pub fn to_blob(commit: &str, mut candidates: Vec<Candidate>) -> SealBlob {
    candidates.sort_by(|a, b| a.ref_name.cmp(&b.ref_name));
    let doc = Doc {
        candidates,
        commit: commit.to_owned(),
        kind: KIND.to_owned(),
    };
    SealBlob {
        ref_name: prefix(commit),
        // Nur Strings: Die Serialisierung kann nicht scheitern.
        bytes: minds_core::to_canonical_json(&doc).expect("Kandidatenliste ist serialisierbar"),
    }
}

/// Liest den Blob strikt (Rundlauf, `commit == expected`, nicht leer,
/// aufsteigend ohne Doppelte, jeder Ref unter dem Präfix, Ziele als
/// Commit-Ids). `None` heißt unlesbar.
pub fn parse(bytes: &[u8], expected: &str) -> Option<Vec<Candidate>> {
    let doc: Doc = serde_json::from_slice(bytes).ok()?;
    let p = prefix(expected);
    let ok = doc.kind == KIND
        && doc.commit == expected
        && !doc.candidates.is_empty()
        && doc
            .candidates
            .windows(2)
            .all(|w| w[0].ref_name < w[1].ref_name)
        && doc
            .candidates
            .iter()
            .all(|c| c.ref_name.starts_with(&p) && is_commit_id(&c.target));
    if !ok || minds_core::to_canonical_json(&doc).ok()? != bytes {
        return None;
    }
    Some(doc.candidates)
}
