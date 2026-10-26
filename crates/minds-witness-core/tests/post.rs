//! Tests der Post-Phase (Abschnitt 12): Ablage und Verifikation, Idempotenz,
//! Store- und Registrierungsfehler, Identitäts- und Schlüsselzeitprüfung,
//! Konflikte und die Receipt-Id als Content-Adresse.

mod fixtures;

use fixtures::*;
use minds_witness_core::memory::{Fault, MemoryAdapter};
use minds_witness_core::*;

fn counts(o: &PostOutcome) -> (u32, u32, u32) {
    (o.stored, o.skipped_existing, o.failed)
}

/// Pre-Phase und Post-Phase mit Standard-Signer und -Identität.
fn run_standard(a: &mut MemoryAdapter) -> (PreOutcome, PostOutcome) {
    let pre_out = pre(a, &policy_observe());
    let post_out = post_receive(&pre_out.findings, &identity(), &signer(), a);
    (pre_out, post_out)
}

#[test]
fn t18_receipt_stored_and_verifies() {
    let mut a = single_commit_push();
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (1, 0, 0));
    let stored = only_event(&out.events, "receipt_stored");
    assert_eq!(stored.fields["commit"], id(2).as_str());
    assert_eq!(stored.fields["ref"], "refs/heads/main");

    let receipts = a.receipts_for(&id(2));
    assert_eq!(receipts.len(), 1);
    let v = verify_receipt(&receipts[0], &a.keys()).unwrap();
    let receipt = v.receipt;
    assert_eq!(receipt.commit, id(2));
    assert_eq!(receipt.id(), receipt_id(&receipt.body()));
    let listed = a.list(&id(2)).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(receipt.id(), listed[0].id);
    assert_eq!(stored.fields["receipt_id"], listed[0].id.as_str());

    let keys = a.keys();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].valid_from, ts("2026-01-01T00:00:00Z"));
    assert_eq!(keys[0].key, identity().key);
    assert_eq!(keys[0].valid_until, None);
}

#[test]
fn t19_idempotent_put() {
    let mut a = single_commit_push();
    let (pre_out, first) = run_standard(&mut a);
    assert_eq!(counts(&first), (1, 0, 0));
    let before = a.receipts_for(&id(2));

    let second = post_receive(&pre_out.findings, &identity(), &signer(), &mut a);
    assert_eq!(counts(&second), (0, 1, 0));
    only_event(&second.events, "receipt_exists");
    assert_eq!(a.receipts_for(&id(2)).len(), 1);
    assert_eq!(a.receipts_for(&id(2)), before);
    assert_eq!(a.keys().len(), 1);
}

#[test]
fn t20_store_failures() {
    // PutErr
    let mut a = single_commit_push();
    a.fault = Fault::PutErr;
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (0, 0, 1));
    only_event(&out.events, "store_failed");

    // RegisterErr
    let mut a = single_commit_push();
    a.fault = Fault::RegisterErr;
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (0, 0, 1));
    only_event(&out.events, "store_failed");
    assert_eq!(a.receipt_count(), 0);

    // Register-Konflikt: gleicher Key, anderes valid_from
    let mut a = single_commit_push();
    a.seed_public_key(PublicKeyEntry {
        key: identity().key,
        valid_from: ts("2025-01-01T00:00:00Z"),
        valid_until: None,
    });
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (0, 0, 1));
    only_event(&out.events, "store_failed");
    assert_eq!(a.receipt_count(), 0);
    assert_eq!(a.keys().len(), 1);
    assert_eq!(a.keys()[0].valid_from, ts("2025-01-01T00:00:00Z"));
}

#[test]
fn t21_identity_mismatch() {
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let foreign = WitnessIdentity {
        binary: identity().binary,
        key: Signer::from_seed([9u8; 32], ts("2026-01-01T00:00:00Z")).public_key(),
    };
    let out = post_receive(&pre_out.findings, &foreign, &signer(), &mut a);
    assert_eq!(counts(&out), (0, 0, 1));
    only_event(&out.events, "identity_mismatch");
    assert_eq!(a.receipt_count(), 0);
    assert!(a.keys().is_empty());
}

#[test]
fn t22_same_commit_two_refs() {
    let mut a = single_commit_push();
    a.ctx.updates = vec![
        update(1, 2, "refs/heads/main"),
        update(1, 2, "refs/heads/feature"),
    ];
    let pre_out = pre(&a, &policy_observe());
    assert_eq!(pre_out.findings.len(), 2);
    let ids: Vec<HashStr> = pre_out
        .findings
        .iter()
        .map(|f| receipt_id(&body_of(f, &identity())))
        .collect();
    assert_ne!(ids[0], ids[1]);

    let out = post_receive(&pre_out.findings, &identity(), &signer(), &mut a);
    assert_eq!(counts(&out), (2, 0, 0));
    let receipts = a.receipts_for(&id(2));
    assert_eq!(receipts.len(), 2);
    let mut refs: Vec<String> = receipts
        .iter()
        .map(|b| verify_receipt(b, &a.keys()).unwrap().receipt.push.ref_name)
        .collect();
    refs.sort();
    assert_eq!(refs, vec!["refs/heads/feature", "refs/heads/main"]);
}

#[test]
fn t37_store_conflict_is_failure() {
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let body = body_of(&pre_out.findings[0], &identity());
    a.seed_receipt(
        ReceiptKey {
            commit: id(2),
            id: receipt_id(&body),
        },
        b"fremd".to_vec(),
    );
    let out = post_receive(&pre_out.findings, &identity(), &signer(), &mut a);
    assert_eq!(counts(&out), (0, 0, 1));
    let ev = only_event(&out.events, "store_conflict");
    assert_eq!(ev.fields["receipt_id"], receipt_id(&body).as_str());
    assert_eq!(a.receipts_for(&id(2)), vec![b"fremd".to_vec()]);
}

#[test]
fn t38_key_valid_from_after_finding() {
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let late = Signer::from_seed([7u8; 32], ts("2027-01-01T00:00:00Z"));
    let ident = WitnessIdentity {
        binary: identity().binary,
        key: late.public_key(),
    };
    let out = post_receive(&pre_out.findings, &ident, &late, &mut a);
    assert_eq!(counts(&out), (0, 0, 1));
    let ev = only_event(&out.events, "key_not_valid_at");
    assert_eq!(ev.fields["time"], "2026-10-25T10:00:00Z");
    assert_eq!(ev.fields["valid_from"], "2027-01-01T00:00:00Z");
    assert_eq!(a.receipt_count(), 0);
    assert_eq!(a.keys().len(), 1);
}

#[test]
fn t39_receipt_id_is_content_address() {
    let mut a = single_commit_push();
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (1, 0, 0));

    let mut b = single_commit_push();
    let pre_b = pre(&b, &policy_observe());
    let other = WitnessIdentity {
        binary: HashStr::parse(&format!("blake3:{}", "ef".repeat(32))).unwrap(),
        key: identity().key,
    };
    let out_b = post_receive(&pre_b.findings, &other, &signer(), &mut b);
    assert_eq!(counts(&out_b), (1, 0, 0));

    let ra = verify_receipt(&a.receipts_for(&id(2))[0], &a.keys())
        .unwrap()
        .receipt;
    let rb = verify_receipt(&b.receipts_for(&id(2))[0], &b.keys())
        .unwrap()
        .receipt;
    assert_ne!(ra.id(), rb.id());
    for r in [&ra, &rb] {
        assert_eq!(
            receipt_id(&r.body()),
            HashStr::of_bytes(&canonical_bytes(&r.body()))
        );
    }
}
