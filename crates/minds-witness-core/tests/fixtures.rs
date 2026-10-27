//! Gemeinsame Builder für die Integrationstests (Abschnitt 11 der Spec).
//! Jede Testdatei bindet das Modul per `mod fixtures;` ein; nicht jede nutzt
//! jede Funktion, daher `allow(dead_code)`.

#![allow(dead_code)]

use std::collections::BTreeSet;

use minds_witness_core::memory::{MemoryAdapter, SealOutcome};
use minds_witness_core::*;

/// `format!("{n:040x}")`; `id(0)` ist die Null-Id.
pub fn id(n: u8) -> CommitId {
    CommitId::parse(&format!("{n:040x}")).unwrap()
}

pub fn ts(s: &str) -> Timestamp {
    Timestamp::parse(s).unwrap()
}

pub fn ctx(updates: Vec<RefUpdate>) -> PushContext {
    PushContext {
        repo: "acme/payments".to_owned(),
        updates,
        pusher: Pusher {
            id: "user-42".to_owned(),
            username: "pdoering".to_owned(),
        },
        server: ServerInfo {
            host: "gitlab.example".to_owned(),
            kind: ServerKind::Memory,
            time: ts("2026-10-25T10:00:00Z"),
        },
    }
}

pub fn update(old: u8, new: u8, ref_name: &str) -> RefUpdate {
    RefUpdate {
        old: id(old),
        new: id(new),
        ref_name: ref_name.to_owned(),
    }
}

pub fn paths(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

pub fn summary(bound: u8, valid: bool, touched: &[&str]) -> SealSummary {
    SealSummary {
        hash: HashStr::parse(&format!("blake3:{}", "ab".repeat(32))).unwrap(),
        signature: if valid {
            SealSignature::Valid
        } else {
            SealSignature::Invalid
        },
        bound_commit: id(bound),
        bound_repo: None,
        touched_paths: paths(touched),
    }
}

pub fn signer() -> Signer {
    Signer::from_seed([7u8; 32], ts("2026-01-01T00:00:00Z"))
}

pub fn identity() -> WitnessIdentity {
    WitnessIdentity {
        binary: HashStr::parse(&format!("blake3:{}", "cd".repeat(32))).unwrap(),
        key: signer().public_key(),
    }
}

pub fn policy_observe() -> Policy {
    Policy::default()
}

pub fn policy_enforce_main() -> Policy {
    Policy {
        mode: Mode::Enforce,
        enforce_refs: vec!["refs/heads/main".to_owned()],
        ..Policy::default()
    }
}

pub fn single_commit_push() -> MemoryAdapter {
    let mut a = MemoryAdapter::new(ctx(vec![update(1, 2, "refs/heads/main")]));
    a.add_commit(id(1), vec![], paths(&["README.md"]))
        .add_commit(id(2), vec![id(1)], paths(&["src/lib.rs", "src/pay.rs"]))
        .add_seal(
            id(2),
            "refs/minds/seals/s1",
            b"seal-2".to_vec(),
            SealOutcome::Summary(summary(2, true, &["src/lib.rs", "src/pay.rs"])),
        );
    a
}

// ---------------------------------------------------------------------------
// Testhilfen über die Spec hinaus (nur Testcode, keine Crate-API).
// ---------------------------------------------------------------------------

/// Der `ReceiptBody`, den die Post-Phase aus `f` und `identity` bauen würde
/// (Abschnitt 8, Schritt 3).
pub fn body_of(f: &Finding, identity: &WitnessIdentity) -> ReceiptBody {
    ReceiptBody {
        commit: f.commit.clone(),
        coverage: f.coverage.clone(),
        policy: f.policy.clone(),
        push: f.push.clone(),
        pusher: f.pusher.clone(),
        repo: f.repo.clone(),
        schema: RECEIPT_SCHEMA.to_owned(),
        seal: f.seal.clone(),
        server: f.server.clone(),
        // S1 v1.6: `/1`-Quittungen tragen nie `trust`.
        trust: None,
        witness: identity.clone(),
    }
}

/// `pre_receive` mit dem Adapter als Objektsicht und Seal-Inspektor.
pub fn pre(a: &MemoryAdapter, policy: &Policy) -> PreOutcome {
    pre_receive(&a.ctx, policy, a, a)
}

/// Codes aller Ereignisse in Reihenfolge.
pub fn codes(events: &[LogEvent]) -> Vec<&str> {
    events.iter().map(|e| e.code.as_str()).collect()
}

/// Das einzige Ereignis mit `code`; scheitert, wenn es nicht genau eines gibt.
pub fn only_event<'a>(events: &'a [LogEvent], code: &str) -> &'a LogEvent {
    let found: Vec<&LogEvent> = events.iter().filter(|e| e.code == code).collect();
    assert_eq!(
        found.len(),
        1,
        "genau ein Ereignis {code} erwartet: {events:?}"
    );
    found[0]
}

pub fn reason(d: &Decision) -> Option<RejectReason> {
    match d {
        Decision::Accept => None,
        Decision::Reject { reason, .. } => Some(reason.clone()),
    }
}

pub fn message(d: &Decision) -> &str {
    match d {
        Decision::Accept => "",
        Decision::Reject { message, .. } => message,
    }
}

/// Seal-Ausgang „gültig, gebunden an `n`, berührt `touched`".
pub fn valid_seal(n: u8, touched: &[&str]) -> SealOutcome {
    SealOutcome::Summary(summary(n, true, touched))
}
