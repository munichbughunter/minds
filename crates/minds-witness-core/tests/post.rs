//! Tests der Post-Phase (Abschnitt 12): Ablage und Verifikation, Idempotenz,
//! Store- und Registrierungsfehler, Identitäts- und Schlüsselzeitprüfung,
//! Konflikte und die Receipt-Id als Content-Adresse.

mod fixtures;

use fixtures::*;
use minds_witness_core::memory::{Fault, MemoryAdapter, SealOutcome};
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
    assert_eq!(ev.fields["reason"], "before_valid_from");
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

// ---------------------------------------------------------------------------
// v1.4: Schlüsselgültigkeit gegen die Schlüsselliste (t44 bis t48)
// ---------------------------------------------------------------------------

/// Store, der alles an den `MemoryAdapter` weiterreicht, nur `public_keys`
/// liefert einen festen Wert (Fehler oder leere Liste).
struct KeysOverride<'a> {
    inner: &'a mut MemoryAdapter,
    keys: Result<Vec<PublicKeyEntry>, &'static str>,
    panic: bool,
}

impl ReceiptStore for KeysOverride<'_> {
    fn put(&mut self, key: &ReceiptKey, bytes: &[u8]) -> Result<PutResult, StoreError> {
        self.inner.put(key, bytes)
    }
    fn get(&self, key: &ReceiptKey) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get(key)
    }
    fn list(&self, commit: &CommitId) -> Result<Vec<ReceiptKey>, StoreError> {
        self.inner.list(commit)
    }
    fn public_keys(&self) -> Result<Vec<PublicKeyEntry>, StoreError> {
        assert!(!self.panic, "fault");
        self.keys
            .clone()
            .map_err(|e| StoreError::Internal(e.to_owned()))
    }
    fn register_public_key(&mut self, entry: &PublicKeyEntry) -> Result<(), StoreError> {
        self.inner.register_public_key(entry)
    }
}

fn entry(valid_from: &str, valid_until: Option<&str>) -> PublicKeyEntry {
    PublicKeyEntry {
        key: identity().key,
        valid_from: ts(valid_from),
        valid_until: valid_until.map(ts),
    }
}

fn foreign_entry(valid_until: Option<&str>) -> PublicKeyEntry {
    PublicKeyEntry {
        key: Signer::from_seed([9u8; 32], ts("2026-01-01T00:00:00Z")).public_key(),
        valid_from: ts("2026-01-01T00:00:00Z"),
        valid_until: valid_until.map(ts),
    }
}

/// Zwei Findings zu `id(2)` unter `main` und `feature`.
fn two_findings() -> (MemoryAdapter, Vec<Finding>) {
    let mut a = single_commit_push();
    a.ctx.updates = vec![
        update(1, 2, "refs/heads/main"),
        update(1, 2, "refs/heads/feature"),
    ];
    let findings = pre(&a, &policy_observe()).findings;
    assert_eq!(findings.len(), 2);
    (a, findings)
}

#[test]
fn t44_revoked_key_not_signed() {
    let mut a = single_commit_push();
    // Zurückgezogen vor dem Serverzeitpunkt 2026-10-25T10:00:00Z; gleiches
    // valid_from wie der Signer, die Registrierung ist also kein Konflikt.
    a.seed_public_key(entry("2026-01-01T00:00:00Z", Some("2026-06-01T00:00:00Z")));
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (0, 0, 1));
    let ev = only_event(&out.events, "key_not_valid_at");
    assert_eq!(ev.fields["reason"], "revoked");
    assert_eq!(ev.fields["valid_until"], "2026-06-01T00:00:00Z");
    assert_eq!(a.receipt_count(), 0);
    // Die Registrierung hat den Widerruf weder entfernt noch verschoben.
    assert_eq!(
        a.keys(),
        vec![entry("2026-01-01T00:00:00Z", Some("2026-06-01T00:00:00Z"))]
    );
}

#[test]
fn t45_valid_until_boundary() {
    // valid_until == Serverzeit: ausschließliche Grenze, nicht signiert.
    let mut a = single_commit_push();
    a.seed_public_key(entry("2026-01-01T00:00:00Z", Some("2026-10-25T10:00:00Z")));
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (0, 0, 1));
    assert_eq!(
        only_event(&out.events, "key_not_valid_at").fields["reason"],
        "revoked"
    );

    // Eine Sekunde später: gültig, und die Quittung verifiziert.
    let mut a = single_commit_push();
    a.seed_public_key(entry("2026-01-01T00:00:00Z", Some("2026-10-25T10:00:01Z")));
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (1, 0, 0));
    assert!(verify_receipt(&a.receipts_for(&id(2))[0], &a.keys()).is_ok());

    // valid_from == Serverzeit: einschließliche Grenze, signiert.
    let mut a = single_commit_push();
    let at = Signer::from_seed([7u8; 32], ts("2026-10-25T10:00:00Z"));
    let pre_out = pre(&a, &policy_observe());
    let out = post_receive(&pre_out.findings, &identity(), &at, &mut a);
    assert_eq!(counts(&out), (1, 0, 0));
    assert!(verify_receipt(&a.receipts_for(&id(2))[0], &a.keys()).is_ok());

    // Entscheidung je Finding: eines vor, eines nach valid_until.
    let mut a = single_commit_push();
    a.seed_public_key(entry("2026-01-01T00:00:00Z", Some("2026-10-25T10:00:00Z")));
    let mut early = a.ctx.clone();
    early.server.time = ts("2026-10-25T09:59:59Z");
    let mut findings = pre_receive(&early, &policy_observe(), &a, &a).findings;
    findings.extend(pre(&a, &policy_observe()).findings);
    let out = post_receive(&findings, &identity(), &signer(), &mut a);
    assert_eq!(counts(&out), (1, 0, 1));
    let ev = only_event(&out.events, "key_not_valid_at");
    assert_eq!(ev.fields["time"], "2026-10-25T10:00:00Z");
    assert_eq!(a.receipt_count(), 1);
}

#[test]
fn t46_public_keys_error_signs_nothing() {
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let mut store = KeysOverride {
        inner: &mut a,
        keys: Err("fault"),
        panic: false,
    };
    let out = post_receive(&pre_out.findings, &identity(), &signer(), &mut store);
    assert_eq!(counts(&out), (0, 0, 1));
    let ev = only_event(&out.events, "store_failed");
    assert!(ev.fields["error"].contains("fault"));
    assert!(!codes(&out.events).contains(&"receipt_stored"));
    assert_eq!(a.receipt_count(), 0);

    // Mehrere Findings: alle failed, keines signiert.
    let (mut a, findings) = two_findings();
    let mut store = KeysOverride {
        inner: &mut a,
        keys: Err("fault"),
        panic: false,
    };
    let out = post_receive(&findings, &identity(), &signer(), &mut store);
    assert_eq!(counts(&out), (0, 0, 2));
    only_event(&out.events, "store_failed");
    assert!(!codes(&out.events).contains(&"receipt_stored"));
    assert_eq!(a.receipt_count(), 0);

    // Panic in public_keys: fail-open nach außen, failed = alle.
    let (mut a, findings) = two_findings();
    let mut store = KeysOverride {
        inner: &mut a,
        keys: Ok(vec![]),
        panic: true,
    };
    let out = post_receive(&findings, &identity(), &signer(), &mut store);
    assert_eq!(counts(&out), (0, 0, 2));
    let ev = only_event(&out.events, "panic");
    // v1.4: kein Panic-Payload im Ereignis.
    assert!(ev.fields.is_empty());
    assert!(!ev.message.contains("fault"));
    assert_eq!(a.receipt_count(), 0);
}

#[test]
fn t47_conflicting_entries_most_restrictive_wins() {
    // Zwei Einträge desselben Schlüssels: einer offen, einer zurückgezogen.
    // Der Kern wählt nicht den günstigeren.
    let mut a = single_commit_push();
    a.seed_public_key(entry("2026-01-01T00:00:00Z", None));
    a.seed_public_key(entry("2026-01-01T00:00:00Z", Some("2026-03-01T00:00:00Z")));
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (0, 0, 1));
    assert_eq!(
        only_event(&out.events, "key_not_valid_at").fields["reason"],
        "revoked"
    );
    assert_eq!(a.receipt_count(), 0);

    // Ein späteres valid_from in einem zweiten Eintrag verhindert ebenso.
    let mut a = single_commit_push();
    a.seed_public_key(entry("2026-01-01T00:00:00Z", None));
    a.seed_public_key(entry("2026-12-01T00:00:00Z", None));
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (0, 0, 1));
    let ev = only_event(&out.events, "key_not_valid_at");
    assert_eq!(ev.fields["reason"], "before_valid_from");
    assert_eq!(ev.fields["valid_from"], "2026-12-01T00:00:00Z");

    // Bei `revoked` nennt das Ereignis valid_from und valid_until des
    // Eintrags, der gegriffen hat, nicht das valid_from des Signers.
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let mut store = KeysOverride {
        inner: &mut a,
        keys: Ok(vec![entry(
            "2025-06-01T00:00:00Z",
            Some("2026-03-01T00:00:00Z"),
        )]),
        panic: false,
    };
    let out = post_receive(&pre_out.findings, &identity(), &signer(), &mut store);
    assert_eq!(counts(&out), (0, 0, 1));
    let ev = only_event(&out.events, "key_not_valid_at");
    assert_eq!(ev.fields["reason"], "revoked");
    assert_eq!(ev.fields["valid_from"], "2025-06-01T00:00:00Z");
    assert_eq!(ev.fields["valid_until"], "2026-03-01T00:00:00Z");
}

#[test]
fn t48_only_entries_of_the_own_key_count() {
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let mut store = KeysOverride {
        inner: &mut a,
        keys: Ok(vec![]),
        panic: false,
    };
    let out = post_receive(&pre_out.findings, &identity(), &signer(), &mut store);
    assert_eq!(counts(&out), (0, 0, 1));
    assert_eq!(
        only_event(&out.events, "key_not_valid_at").fields["reason"],
        "not_listed"
    );
    assert_eq!(a.receipt_count(), 0);

    // Nur ein fremder Schlüssel in der Liste: der eigene fehlt → not_listed.
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let mut store = KeysOverride {
        inner: &mut a,
        keys: Ok(vec![foreign_entry(None)]),
        panic: false,
    };
    let out = post_receive(&pre_out.findings, &identity(), &signer(), &mut store);
    assert_eq!(counts(&out), (0, 0, 1));
    assert_eq!(
        only_event(&out.events, "key_not_valid_at").fields["reason"],
        "not_listed"
    );

    // Der Signer selbst ist erst später gültig, auch wenn die Liste (gegen
    // den Vertrag) nur einen früheren Eintrag führt: nicht signiert.
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let late = Signer::from_seed([7u8; 32], ts("2027-01-01T00:00:00Z"));
    let ident = WitnessIdentity {
        binary: identity().binary,
        key: late.public_key(),
    };
    let mut store = KeysOverride {
        inner: &mut a,
        keys: Ok(vec![entry("2026-01-01T00:00:00Z", None)]),
        panic: false,
    };
    let out = post_receive(&pre_out.findings, &ident, &late, &mut store);
    assert_eq!(counts(&out), (0, 0, 1));
    let ev = only_event(&out.events, "key_not_valid_at");
    assert_eq!(ev.fields["reason"], "before_valid_from");
    assert_eq!(ev.fields["valid_from"], "2027-01-01T00:00:00Z");

    // Ein widerrufener fremder Schlüssel blockiert den eigenen nicht.
    let mut a = single_commit_push();
    a.seed_public_key(foreign_entry(Some("2026-02-01T00:00:00Z")));
    let (_, out) = run_standard(&mut a);
    assert_eq!(counts(&out), (1, 0, 0));
    assert!(verify_receipt(&a.receipts_for(&id(2))[0], &a.keys()).is_ok());
}

// ---------------------------------------------------------------------------
// Neu in Spec S1 v1.5: Regressionstests zu V3 und V6.
// ---------------------------------------------------------------------------

#[test]
fn t52_candidate_set_ref_passes_through() {
    let prefix = format!("refs/minds/bindings/{}/", id(2).as_str());

    let mut a = single_commit_push();
    a.remove_seal(&id(2)).add_seal(
        id(2),
        &prefix,
        b"kandidaten".to_vec(),
        valid_seal(2, &["src/lib.rs", "src/pay.rs"]),
    );
    let (pre_out, out) = run_standard(&mut a);
    let f = &pre_out.findings[0];
    assert_eq!(f.seal.ref_name.as_deref(), Some(prefix.as_str()));
    assert_eq!(f.seal.status, SealState::Valid);
    assert_eq!(counts(&out), (1, 0, 0));
    let receipts = a.receipts_for(&id(2));
    let v = verify_receipt(&receipts[0], &a.keys()).unwrap();
    assert_eq!(v.receipt.seal.ref_name.as_deref(), Some(prefix.as_str()));

    let bytes = b"kandidaten-unlesbar".to_vec();
    let mut b = single_commit_push();
    b.remove_seal(&id(2))
        .add_seal(id(2), &prefix, bytes.clone(), SealOutcome::Malformed);
    let out = pre(&b, &policy_observe());
    let seal = &out.findings[0].seal;
    assert_eq!(seal.status, SealState::Invalid);
    assert_eq!(seal.hash, Some(HashStr::of_bytes(&bytes)));
    assert_eq!(seal.ref_name.as_deref(), Some(prefix.as_str()));
}

#[test]
fn t54_nonutf8_path_form() {
    let marker = "\0nonutf8:ff";
    let mut a = MemoryAdapter::new(ctx(vec![update(1, 2, "refs/heads/main")]));
    a.add_commit(id(1), vec![], paths(&["README.md"]))
        .add_commit(id(2), vec![id(1)], paths(&[marker, "src/lib.rs"]))
        .add_seal(
            id(2),
            "refs/minds/seals/s1",
            b"seal-2".to_vec(),
            valid_seal(2, &["src/lib.rs"]),
        );
    let (pre_out, out) = run_standard(&mut a);
    assert_eq!(
        pre_out.findings[0].coverage,
        Coverage {
            paths_covered: 1,
            paths_in_diff: 2,
            status: CoverageStatus::Computed,
            uncovered: vec![marker.to_owned()],
            uncovered_truncated: false,
        }
    );
    assert_eq!(counts(&out), (1, 0, 0));
    let receipts = a.receipts_for(&id(2));
    let text = String::from_utf8(receipts[0].clone()).unwrap();
    assert!(
        text.contains(r#""uncovered":["\u0000nonutf8:ff"]"#),
        "{text}"
    );
    verify_receipt(&receipts[0], &a.keys()).unwrap();
    let back = Receipt::from_bytes(&receipts[0]).unwrap();
    assert_eq!(back.coverage.uncovered, vec![marker.to_owned()]);

    let enforce = pre(&a, &policy_enforce_main());
    assert_eq!(reason(&enforce.decision), Some(RejectReason::PolicyFail));
}
