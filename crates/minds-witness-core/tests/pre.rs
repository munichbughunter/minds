//! Tests der Pre-Phase (Abschnitt 12): Entscheidung, Findings und Ereignisse
//! für Seal-Zustände, Abdeckung, Merges, gekürzte Bereiche, interne Fehler,
//! Panics, Kontext- und Policy-Fehler sowie den Namensraum-Schutz.

mod fixtures;

use fixtures::*;
use minds_witness_core::memory::{Fault, MemoryAdapter, SealOutcome};
use minds_witness_core::*;

fn summary_hash() -> HashStr {
    summary(2, true, &[]).hash
}

/// Kette `id(1)` … `id(6)`, jeder Commit ändert `f<n>`, jedes Seal ist an den
/// eigenen Commit gebunden und deckt diesen Pfad ab.
fn chain_push() -> MemoryAdapter {
    let mut a = MemoryAdapter::new(ctx(vec![update(1, 6, "refs/heads/main")]));
    for n in 1..=6u8 {
        let parents = if n == 1 { vec![] } else { vec![id(n - 1)] };
        let p = format!("f{n}");
        a.add_commit(id(n), parents, paths(&[p.as_str()]));
        a.add_seal(
            id(n),
            &format!("refs/minds/seals/c{n}"),
            format!("seal-{n}").into_bytes(),
            valid_seal(n, &[p.as_str()]),
        );
    }
    a
}

/// Merge-Aufbau aus t10: `id(4)` merged `id(2)` und `id(3)`.
fn merge_push(merge_paths: &[&str]) -> MemoryAdapter {
    let mut a = MemoryAdapter::new(ctx(vec![update(1, 4, "refs/heads/main")]));
    a.add_commit(id(1), vec![], paths(&["README.md"]))
        .add_commit(id(2), vec![id(1)], paths(&["a"]))
        .add_commit(id(3), vec![id(1)], paths(&["b"]))
        .add_commit(id(4), vec![id(2), id(3)], paths(merge_paths))
        .add_seal(
            id(2),
            "refs/minds/seals/s2",
            b"seal-2".to_vec(),
            valid_seal(2, &["a"]),
        )
        .add_seal(
            id(3),
            "refs/minds/seals/s3",
            b"seal-3".to_vec(),
            valid_seal(3, &["b"]),
        );
    a
}

fn mixed_length_update() -> RefUpdate {
    RefUpdate {
        old: id(1),
        new: CommitId::parse(&"2".repeat(64)).unwrap(),
        ref_name: "refs/heads/main".to_owned(),
    }
}

fn seven_paths() -> [&'static str; 7] {
    ["p1", "p2", "p3", "p4", "p5", "p6", "p7"]
}

#[test]
fn t01_valid_seal_full_coverage() {
    let a = single_commit_push();
    let out = pre(&a, &policy_observe());
    assert_eq!(out.decision, Decision::Accept);
    assert_eq!(out.findings.len(), 1);
    let f = &out.findings[0];
    assert_eq!(f.commit, id(2));
    assert_eq!(f.seal.status, SealState::Valid);
    assert_eq!(f.seal.hash, Some(summary_hash()));
    assert_eq!(f.seal.ref_name.as_deref(), Some("refs/minds/seals/s1"));
    assert_eq!(
        f.coverage,
        Coverage {
            paths_covered: 2,
            paths_in_diff: 2,
            status: CoverageStatus::Computed,
            uncovered: vec![],
            uncovered_truncated: false,
        }
    );
    assert_eq!(
        f.policy,
        PolicyResult {
            mode: Mode::Observe,
            result: PolicyVerdict::Pass,
            version: 1,
        }
    );
    assert_eq!(
        f.push,
        PushInfo {
            new: id(2),
            new_commits: 1,
            old: id(1),
            ref_name: "refs/heads/main".to_owned(),
            truncated: false,
        }
    );
    let created = only_event(&out.events, "finding_created");
    assert_eq!(created.level, LogLevel::Info);
    let keys: Vec<&str> = created.fields.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "commit",
            "paths_covered",
            "paths_in_diff",
            "ref",
            "result",
            "truncated"
        ]
    );
    assert_eq!(created.fields["result"], "pass");
}

#[test]
fn t02_no_seal_observe() {
    let mut a = single_commit_push();
    a.remove_seal(&id(2));
    let out = pre(&a, &policy_observe());
    assert_eq!(out.decision, Decision::Accept);
    assert_eq!(out.findings.len(), 1);
    let f = &out.findings[0];
    assert_eq!(
        f.seal,
        SealStatus {
            hash: None,
            ref_name: None,
            status: SealState::Absent,
        }
    );
    assert_eq!(
        f.coverage,
        Coverage {
            paths_covered: 0,
            paths_in_diff: 2,
            status: CoverageStatus::NoSeal,
            uncovered: vec!["src/lib.rs".to_owned(), "src/pay.rs".to_owned()],
            uncovered_truncated: false,
        }
    );
    assert_eq!(f.policy.result, PolicyVerdict::Fail);
}

#[test]
fn t03_invalid_signature() {
    let mut a = single_commit_push();
    a.add_seal(
        id(2),
        "refs/minds/seals/s1",
        b"seal-2".to_vec(),
        SealOutcome::Summary(summary(2, false, &["src/lib.rs", "src/pay.rs"])),
    );
    let out = pre(&a, &policy_observe());
    assert_eq!(out.decision, Decision::Accept);
    let f = &out.findings[0];
    assert_eq!(f.seal.status, SealState::Invalid);
    assert_eq!(f.coverage.status, CoverageStatus::Computed);
    assert_eq!(f.coverage.paths_covered, 2);
    assert_eq!(f.policy.result, PolicyVerdict::Fail);
}

#[test]
fn t04_malformed_seal() {
    let mut a = single_commit_push();
    a.add_seal(
        id(2),
        "refs/minds/seals/s1",
        b"seal-2".to_vec(),
        SealOutcome::Malformed,
    );
    let out = pre(&a, &policy_observe());
    assert_eq!(out.decision, Decision::Accept);
    let f = &out.findings[0];
    assert_eq!(
        f.seal,
        SealStatus {
            hash: Some(HashStr::of_bytes(b"seal-2")),
            ref_name: Some("refs/minds/seals/s1".to_owned()),
            status: SealState::Invalid,
        }
    );
    assert_eq!(f.coverage.status, CoverageStatus::NoSeal);
    assert_eq!(f.policy.result, PolicyVerdict::Fail);
}

#[test]
fn t05_witness_namespace_rejected_both_modes() {
    for policy in [policy_observe(), policy_enforce_main()] {
        let mut a = single_commit_push();
        a.ctx.updates.push(update(0, 2, "refs/minds/witness/abc"));
        a.fault = Fault::CommitErr;
        let out = pre(&a, &policy);
        assert_eq!(reason(&out.decision), Some(RejectReason::WitnessNamespace));
        assert!(out.findings.is_empty());
        let ev = only_event(&out.events, "push_rejected_namespace");
        assert_eq!(ev.fields["ref"], "refs/minds/witness/abc");
        assert!(!codes(&out.events).contains(&"ref_evaluation_failed"));
        assert_eq!(out.events.len(), 1);
    }
}

#[test]
fn t06_namespace_deletion_rejected() {
    let a = MemoryAdapter::new(ctx(vec![update(2, 0, "refs/minds/witness/abc")]));
    let out = pre(&a, &policy_observe());
    assert_eq!(reason(&out.decision), Some(RejectReason::WitnessNamespace));
    assert!(out.findings.is_empty());
}

#[test]
fn t07_internal_error_observe() {
    let mut a = single_commit_push();
    a.fault = Fault::ChangedPathsErr;
    let out = pre(&a, &policy_observe());
    assert_eq!(out.decision, Decision::Accept);
    assert!(out.findings.is_empty());
    let ev = only_event(&out.events, "ref_evaluation_failed");
    assert_eq!(ev.fields["ref"], "refs/heads/main");
    assert_eq!(ev.level, LogLevel::Error);
}

#[test]
fn t08_internal_error_enforce() {
    let mut a = single_commit_push();
    a.fault = Fault::ChangedPathsErr;
    let out = pre(&a, &policy_enforce_main());
    assert_eq!(reason(&out.decision), Some(RejectReason::InternalError));

    a.ctx.updates = vec![update(1, 2, "refs/heads/feature")];
    let out = pre(&a, &policy_enforce_main());
    assert_eq!(out.decision, Decision::Accept);
    assert!(out.findings.is_empty());
    let ev = only_event(&out.events, "ref_evaluation_failed");
    assert_eq!(ev.fields["ref"], "refs/heads/feature");
}

#[test]
fn t09_enforce_no_seal() {
    let mut a = single_commit_push();
    a.remove_seal(&id(2));
    let out = pre(&a, &policy_enforce_main());
    assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
    assert_eq!(out.findings.len(), 1);
    assert_eq!(out.findings[0].policy.result, PolicyVerdict::Fail);
    let ev = only_event(&out.events, "push_rejected_policy");
    assert_eq!(ev.fields["grund"], "fail");

    a.ctx.updates = vec![update(1, 2, "refs/heads/feature")];
    let out = pre(&a, &policy_enforce_main());
    assert_eq!(out.decision, Decision::Accept);
    assert_eq!(out.findings.len(), 1);
    assert_eq!(out.findings[0].policy.result, PolicyVerdict::Fail);
}

#[test]
fn t10_clean_merge_passes() {
    let a = merge_push(&[]);
    let out = pre(&a, &policy_observe());
    assert_eq!(out.decision, Decision::Accept);
    let commits: Vec<CommitId> = out.findings.iter().map(|f| f.commit.clone()).collect();
    assert_eq!(commits, vec![id(2), id(3), id(4)]);
    let m = &out.findings[2];
    assert_eq!(
        m.seal,
        SealStatus {
            hash: None,
            ref_name: None,
            status: SealState::Absent,
        }
    );
    assert_eq!(
        m.coverage,
        Coverage {
            paths_covered: 0,
            paths_in_diff: 0,
            status: CoverageStatus::MergeCommit,
            uncovered: vec![],
            uncovered_truncated: false,
        }
    );
    assert_eq!(m.policy.result, PolicyVerdict::Pass);

    let out = pre(&a, &policy_enforce_main());
    assert_eq!(out.decision, Decision::Accept);
}

#[test]
fn t11_partial_coverage_threshold() {
    let mut a = single_commit_push();
    a.add_commit(id(2), vec![id(1)], paths(&seven_paths()));
    a.add_seal(
        id(2),
        "refs/minds/seals/s1",
        b"seal-2".to_vec(),
        valid_seal(2, &seven_paths()[..5]),
    );
    let expected = Coverage {
        paths_covered: 5,
        paths_in_diff: 7,
        status: CoverageStatus::Computed,
        uncovered: vec!["p6".to_owned(), "p7".to_owned()],
        uncovered_truncated: false,
    };

    let out = pre(&a, &policy_enforce_main());
    assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
    assert_eq!(out.findings.len(), 1);
    assert_eq!(out.findings[0].coverage, expected);

    let policy = Policy {
        coverage_threshold_percent: 70,
        ..policy_enforce_main()
    };
    let out = pre(&a, &policy);
    assert_eq!(out.decision, Decision::Accept);
    assert_eq!(out.findings[0].coverage, expected);
    assert_eq!(out.findings[0].policy.result, PolicyVerdict::Pass);
}

#[test]
fn t12_uncovered_cap() {
    let mut a = single_commit_push();
    a.add_commit(id(2), vec![id(1)], paths(&seven_paths()));
    a.add_seal(
        id(2),
        "refs/minds/seals/s1",
        b"seal-2".to_vec(),
        valid_seal(2, &[]),
    );

    let policy = Policy {
        uncovered_cap: 3,
        ..policy_observe()
    };
    let out = pre(&a, &policy);
    let c = &out.findings[0].coverage;
    assert_eq!(c.uncovered, vec!["p1", "p2", "p3"]);
    assert!(c.uncovered_truncated);
    assert_eq!(c.paths_in_diff, 7);
    assert_eq!(c.paths_covered, 0);

    let policy = Policy {
        uncovered_cap: 0,
        ..policy_observe()
    };
    let out = pre(&a, &policy);
    let c = &out.findings[0].coverage;
    assert!(c.uncovered.is_empty());
    assert!(c.uncovered_truncated);
}

#[test]
fn t13_range_truncated_observe() {
    let a = chain_push();
    let policy = Policy {
        max_commits_per_push: 3,
        ..policy_observe()
    };
    let out = pre(&a, &policy);
    assert_eq!(out.decision, Decision::Accept);
    assert_eq!(out.findings.len(), 1);
    let f = &out.findings[0];
    assert_eq!(f.commit, id(6));
    assert_eq!(f.push.new_commits, 3);
    assert!(f.push.truncated);
    assert_eq!(f.policy.result, PolicyVerdict::Pass);
    let ev = only_event(&out.events, "range_truncated");
    assert_eq!(ev.fields["ref"], "refs/heads/main");
    assert_eq!(ev.fields["limit"], "3");
}

#[test]
fn t14_ignored_refs() {
    let mut a = single_commit_push();
    a.ctx.updates = vec![
        update(1, 2, "refs/tags/v1"),
        update(2, 0, "refs/heads/old"),
        update(0, 2, "refs/minds/seals/x"),
    ];
    a.fault = Fault::CommitErr;
    let out = pre(&a, &policy_observe());
    assert_eq!(out.decision, Decision::Accept);
    assert!(out.findings.is_empty());
    assert!(out.events.is_empty());
}

#[test]
fn t15_pusher_hashed() {
    let a = single_commit_push();
    let policy = Policy {
        pusher_mode: PusherMode::Hashed,
        pusher_salt: "salz".to_owned(),
        ..policy_observe()
    };
    let out = pre(&a, &policy);
    let p = &out.findings[0].pusher;
    assert_eq!(p.username, HashStr::of_bytes(b"salz\0pdoering").as_str());
    assert_eq!(p.id, HashStr::of_bytes(b"salz\0user-42").as_str());
    assert_ne!(p.username, "pdoering");
    assert_ne!(p.id, "user-42");
}

#[test]
fn t16_panic_is_fail_open() {
    let mut a = single_commit_push();
    a.fault = Fault::PanicInChangedPaths;

    let out = pre(&a, &policy_observe());
    assert_eq!(out.decision, Decision::Accept);
    assert!(out.findings.is_empty());
    let ev = only_event(&out.events, "panic");
    // v1.4: kein Panic-Payload im Ereignis.
    assert!(ev.fields.is_empty());
    assert!(!ev.message.contains("fault"));

    let out = pre(&a, &policy_enforce_main());
    assert_eq!(reason(&out.decision), Some(RejectReason::InternalError));
    assert!(out.findings.is_empty());
}

#[test]
fn t17_context_invalid() {
    let a = MemoryAdapter::new(ctx(vec![mixed_length_update()]));

    let out = pre(&a, &policy_observe());
    assert_eq!(out.decision, Decision::Accept);
    assert!(out.findings.is_empty());
    only_event(&out.events, "push_context_invalid");

    let out = pre(&a, &policy_enforce_main());
    assert_eq!(reason(&out.decision), Some(RejectReason::InternalError));
    assert!(out.findings.is_empty());
}

#[test]
fn t30_range_truncated_enforce() {
    let a = chain_push();
    let policy = Policy {
        max_commits_per_push: 3,
        ..policy_enforce_main()
    };
    let out = pre(&a, &policy);
    assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
    assert!(message(&out.decision).contains("mehr als 3 neue Commits"));
    assert_eq!(out.findings.len(), 1);
    assert!(out.findings[0].push.truncated);
    let ev = only_event(&out.events, "push_rejected_policy");
    assert_eq!(ev.fields["grund"], "truncated");
}

#[test]
fn t31_merge_with_own_changes() {
    // (a) ohne Seal für id(4)
    let a = merge_push(&["conf.toml"]);
    let out = pre(&a, &policy_enforce_main());
    assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
    let m = out.findings.iter().find(|f| f.commit == id(4)).unwrap();
    assert_eq!(m.coverage.status, CoverageStatus::NoSeal);
    assert_eq!(m.policy.result, PolicyVerdict::Fail);

    // (b) mit Seal, gebunden an id(4), berührt conf.toml
    let mut a = merge_push(&["conf.toml"]);
    a.add_seal(
        id(4),
        "refs/minds/seals/s4",
        b"seal-4".to_vec(),
        valid_seal(4, &["conf.toml"]),
    );
    let out = pre(&a, &policy_enforce_main());
    assert_eq!(out.decision, Decision::Accept);
    let m = out.findings.iter().find(|f| f.commit == id(4)).unwrap();
    assert_eq!(
        m.coverage,
        Coverage {
            paths_covered: 1,
            paths_in_diff: 1,
            status: CoverageStatus::Computed,
            uncovered: vec![],
            uncovered_truncated: false,
        }
    );
    assert_eq!(m.policy.result, PolicyVerdict::Pass);
}

#[test]
fn t32_context_invalid_still_protects_namespace() {
    let a = MemoryAdapter::new(ctx(vec![
        mixed_length_update(),
        update(0, 2, "refs/minds/witness/abc"),
    ]));
    for policy in [policy_observe(), policy_enforce_main()] {
        let out = pre(&a, &policy);
        assert_eq!(reason(&out.decision), Some(RejectReason::WitnessNamespace));
        assert!(!codes(&out.events).contains(&"push_context_invalid"));
    }
}

#[test]
fn t33_seal_bound_to_other_commit() {
    let mut a = single_commit_push();
    a.add_seal(
        id(2),
        "refs/minds/seals/s1",
        b"seal-2".to_vec(),
        valid_seal(3, &["src/lib.rs", "src/pay.rs"]),
    );
    for (policy, enforce) in [(policy_observe(), false), (policy_enforce_main(), true)] {
        let out = pre(&a, &policy);
        let f = &out.findings[0];
        assert_eq!(f.seal.status, SealState::Mismatched);
        assert_eq!(f.seal.hash, Some(summary_hash()));
        assert_eq!(
            f.coverage,
            Coverage {
                paths_covered: 2,
                paths_in_diff: 2,
                status: CoverageStatus::Computed,
                uncovered: vec![],
                uncovered_truncated: false,
            }
        );
        assert_eq!(f.policy.result, PolicyVerdict::Fail);
        if enforce {
            assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
        } else {
            assert_eq!(out.decision, Decision::Accept);
        }
    }
}

#[test]
fn t40_enforce_without_evaluate_is_invalid() {
    let mut a = single_commit_push();
    a.remove_seal(&id(2));
    let mut policy = Policy {
        mode: Mode::Enforce,
        evaluate_refs: vec!["refs/tags/*".to_owned()],
        enforce_refs: vec!["refs/heads/main".to_owned()],
        ..Policy::default()
    };
    assert_eq!(
        policy.validate(),
        Err(PolicyError::EnforceNotEvaluated(
            "refs/heads/main".to_owned()
        ))
    );

    let out = pre(&a, &policy);
    assert_eq!(reason(&out.decision), Some(RejectReason::InternalError));
    assert!(out.findings.is_empty());
    only_event(&out.events, "policy_invalid");

    policy.mode = Mode::Observe;
    let out = pre(&a, &policy);
    assert_eq!(out.decision, Decision::Accept);
    assert!(out.findings.is_empty());
    only_event(&out.events, "policy_invalid");
}

#[test]
fn t41_seal_bound_to_other_repo() {
    for (repo, expected) in [
        ("acme/other", SealState::Mismatched),
        ("acme/payments", SealState::Valid),
    ] {
        let mut a = single_commit_push();
        let mut s = summary(2, true, &["src/lib.rs", "src/pay.rs"]);
        s.bound_repo = Some(repo.to_owned());
        a.add_seal(
            id(2),
            "refs/minds/seals/s1",
            b"seal-2".to_vec(),
            SealOutcome::Summary(s),
        );
        let out = pre(&a, &policy_enforce_main());
        assert_eq!(out.findings[0].seal.status, expected);
        if expected == SealState::Valid {
            assert_eq!(out.decision, Decision::Accept);
        } else {
            assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
        }
    }
}

// ---------------------------------------------------------------------------
// v1.4: reservierte Ref-Namen und Panic-Payload (t49, t50)
// ---------------------------------------------------------------------------

#[test]
fn t49_reserved_namespace_names_rejected() {
    for name in ["refs/minds/witness", "refs/minds"] {
        for (old, new) in [(0u8, 2u8), (1, 2), (2, 0)] {
            for policy in [policy_observe(), policy_enforce_main()] {
                let mut a = single_commit_push();
                a.ctx.updates.push(update(old, new, name));
                a.fault = Fault::CommitErr;
                let out = pre(&a, &policy);
                assert_eq!(
                    reason(&out.decision),
                    Some(RejectReason::WitnessNamespace),
                    "{name} {old}->{new}"
                );
                assert_eq!(
                    message(&out.decision),
                    format!(
                        "minds-witness: Ref {name} ist reserviert, weil er den Witness-Namensraum refs/minds/witness/ blockieren würde"
                    )
                );
                assert!(out.findings.is_empty());
                assert_eq!(out.events.len(), 1);
                assert_eq!(
                    only_event(&out.events, "push_rejected_namespace").fields["ref"],
                    name
                );
            }
        }
    }
    // Nachbarn sind nicht reserviert: kein Namensraum-Reject, keine Bewertung.
    for name in [
        "refs/minds/witnessx",
        "refs/mindsx",
        "refs/minds/evidence/abc",
        "refs/minds/context",
    ] {
        let mut a = single_commit_push();
        a.ctx.updates = vec![update(0, 2, name)];
        let out = pre(&a, &policy_enforce_main());
        assert_eq!(out.decision, Decision::Accept, "{name}");
        assert!(out.events.is_empty(), "{name}");
    }
}

/// Objektsicht, die beim Pfad-Diff mit einem Text panict, der wie ein
/// Objektinhalt aussieht.
struct LeakyPanic<'a>(&'a MemoryAdapter);

impl ObjectAccess for LeakyPanic<'_> {
    fn commit(&self, id: &CommitId) -> Result<CommitInfo, ObjectError> {
        self.0.commit(id)
    }
    fn new_commits(
        &self,
        old: Option<&CommitId>,
        new: &CommitId,
        limit: u32,
    ) -> Result<CommitRange, ObjectError> {
        self.0.new_commits(old, new, limit)
    }
    fn changed_paths(
        &self,
        _id: &CommitId,
    ) -> Result<std::collections::BTreeSet<String>, ObjectError> {
        panic!("SECRET-PAYLOAD aws_secret_access_key=abc123 in src/creds.env");
    }
    fn seal_for_commit(&self, id: &CommitId) -> Result<Option<SealBlob>, ObjectError> {
        self.0.seal_for_commit(id)
    }
}

#[test]
fn t50_panic_payload_never_logged() {
    let a = single_commit_push();
    for policy in [policy_observe(), policy_enforce_main()] {
        let out = pre_receive(&a.ctx, &policy, &LeakyPanic(&a), &a);
        let ev = only_event(&out.events, "panic");
        assert!(ev.fields.is_empty());
        for e in &out.events {
            assert!(!e.message.contains("SECRET"));
            assert!(e.fields.values().all(|v| !v.contains("SECRET")));
        }
        assert!(!message(&out.decision).contains("SECRET"));
    }
}

// ---------------------------------------------------------------------------
// Neu in Spec S1 v1.5: Regressionstests zu den Vertragsänderungen V1 bis V8.
// ---------------------------------------------------------------------------

/// Standard-Push, dessen Seal für `id(2)` den Ausgang `s` hat.
fn push_with_summary(s: SealSummary) -> MemoryAdapter {
    let mut a = single_commit_push();
    a.add_seal(
        id(2),
        "refs/minds/seals/s1",
        b"seal-2".to_vec(),
        SealOutcome::Summary(s),
    );
    a
}

#[test]
fn t51_binding_check_precedes_signature() {
    let touched = ["src/lib.rs", "src/pay.rs"];
    let with = |signature: SealSignature, bound: u8, repo: Option<&str>| SealSummary {
        signature,
        bound_commit: id(bound),
        bound_repo: repo.map(str::to_owned),
        ..summary(2, true, &touched)
    };
    let cases = [
        // (a) Inspektor übernimmt die Erwartung bei ungültiger Signatur.
        (
            with(SealSignature::Invalid, 2, Some("acme/payments")),
            SealState::Invalid,
            "invalid",
        ),
        // (b) dasselbe ohne Signatur.
        (
            with(SealSignature::Unsigned, 2, Some("acme/payments")),
            SealState::Unsigned,
            "unsigned",
        ),
        // (c) verfehlte Repo-Bindung, sonst wie (a).
        (
            with(SealSignature::Invalid, 2, Some("acme/other")),
            SealState::Mismatched,
            "mismatched",
        ),
        // (d) verfehlte Commit-Bindung ohne Signatur.
        (
            with(SealSignature::Unsigned, 3, None),
            SealState::Mismatched,
            "mismatched",
        ),
    ];
    for (s, state, word) in cases {
        let a = push_with_summary(s);
        let out = pre(&a, &policy_observe());
        assert_eq!(out.decision, Decision::Accept);
        assert_eq!(out.findings.len(), 1);
        let f = &out.findings[0];
        assert_eq!(f.seal.status, state);
        assert_eq!(f.coverage.status, CoverageStatus::Computed);
        assert_eq!(f.policy.result, PolicyVerdict::Fail);

        let out = pre(&a, &policy_enforce_main());
        assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
        assert!(
            message(&out.decision).contains(&format!("Seal {word})")),
            "{:?}",
            out.decision
        );
    }
}

/// Inspektor, der an den `MemoryAdapter` weiterreicht und ab dem
/// `budget + 1`-ten Aufruf wie ein erschöpftes push-weites Budget
/// `SealError::Internal` liefert.
struct BudgetInspector<'a> {
    inner: &'a MemoryAdapter,
    budget: u32,
    calls: std::cell::Cell<u32>,
}

impl<'a> BudgetInspector<'a> {
    fn new(inner: &'a MemoryAdapter, budget: u32) -> Self {
        Self {
            inner,
            budget,
            calls: std::cell::Cell::new(0),
        }
    }
}

impl SealInspector for BudgetInspector<'_> {
    fn inspect(
        &self,
        blob: &SealBlob,
        expected: &SealExpectation,
    ) -> Result<SealSummary, SealError> {
        let n = self.calls.get() + 1;
        self.calls.set(n);
        if n > self.budget {
            return Err(SealError::Internal("budget".to_owned()));
        }
        self.inner.inspect(blob, expected)
    }
}

/// `id(2)` und `id(3)` zweigen von `id(1)` ab, jeder mit eigenem, voll
/// abdeckendem Seal.
fn fork_push(updates: Vec<RefUpdate>) -> MemoryAdapter {
    let mut a = MemoryAdapter::new(ctx(updates));
    a.add_commit(id(1), vec![], paths(&["README.md"]))
        .add_commit(id(2), vec![id(1)], paths(&["a"]))
        .add_commit(id(3), vec![id(1)], paths(&["b"]))
        .add_seal(
            id(2),
            "refs/minds/seals/s2",
            b"seal-2".to_vec(),
            valid_seal(2, &["a"]),
        )
        .add_seal(
            id(3),
            "refs/minds/seals/s3",
            b"seal-3".to_vec(),
            valid_seal(3, &["b"]),
        );
    a
}

fn pre_with_budget(a: &MemoryAdapter, policy: &Policy) -> PreOutcome {
    pre_receive(&a.ctx, policy, a, &BudgetInspector::new(a, 1))
}

fn assert_same(x: &PreOutcome, y: &PreOutcome) {
    assert_eq!(x.decision, y.decision);
    assert_eq!(x.findings, y.findings);
    assert_eq!(x.events, y.events);
}

#[test]
fn t53_budget_exhaustion_is_internal_and_deterministic() {
    // (a) feature zuerst: das Budget reicht nur für `id(2)`.
    let a = fork_push(vec![
        update(1, 2, "refs/heads/feature"),
        update(1, 3, "refs/heads/main"),
    ]);
    let out = pre_with_budget(&a, &policy_enforce_main());
    assert_eq!(reason(&out.decision), Some(RejectReason::InternalError));
    assert_eq!(
        message(&out.decision),
        "minds-witness: interner Fehler bei refs/heads/main"
    );
    let failed = only_event(&out.events, "ref_evaluation_failed");
    assert_eq!(failed.fields["ref"], "refs/heads/main");
    assert!(failed.fields["error"].contains("budget"));
    assert_eq!(out.findings.len(), 1);
    assert_eq!(out.findings[0].push.ref_name, "refs/heads/feature");
    assert_same(&out, &pre_with_budget(&a, &policy_enforce_main()));

    let out = pre_with_budget(&a, &policy_observe());
    assert_eq!(out.decision, Decision::Accept);
    assert_eq!(out.findings.len(), 1);
    assert_eq!(out.findings[0].push.ref_name, "refs/heads/feature");
    assert_same(&out, &pre_with_budget(&a, &policy_observe()));

    // (b) main zuerst: `main` wird bewertet, `feature` verworfen.
    let b = fork_push(vec![
        update(1, 3, "refs/heads/main"),
        update(1, 2, "refs/heads/feature"),
    ]);
    let out = pre_with_budget(&b, &policy_enforce_main());
    assert_eq!(out.decision, Decision::Accept);
    assert_eq!(out.findings.len(), 1);
    let f = &out.findings[0];
    assert_eq!(f.push.ref_name, "refs/heads/main");
    assert_eq!(f.commit, id(3));
    assert_eq!(f.policy.result, PolicyVerdict::Pass);
    let failed = only_event(&out.events, "ref_evaluation_failed");
    assert_eq!(failed.fields["ref"], "refs/heads/feature");
    assert_same(&out, &pre_with_budget(&b, &policy_enforce_main()));
}

/// Objektsicht, die bei `truncated == true` eine andere zulässige Auswahl in
/// anderer Reihenfolge liefert und jede Commit-Id protokolliert, für die
/// `commit`, `changed_paths` oder `seal_for_commit` aufgerufen wird.
struct SubsetRange<'a> {
    inner: &'a MemoryAdapter,
    subset: Vec<CommitId>,
    seen: std::cell::RefCell<Vec<CommitId>>,
}

impl ObjectAccess for SubsetRange<'_> {
    fn commit(&self, id: &CommitId) -> Result<CommitInfo, ObjectError> {
        self.seen.borrow_mut().push(id.clone());
        self.inner.commit(id)
    }
    fn new_commits(
        &self,
        old: Option<&CommitId>,
        new: &CommitId,
        limit: u32,
    ) -> Result<CommitRange, ObjectError> {
        let range = self.inner.new_commits(old, new, limit)?;
        if range.truncated {
            return Ok(CommitRange {
                commits: self.subset.clone(),
                truncated: true,
            });
        }
        Ok(range)
    }
    fn changed_paths(
        &self,
        id: &CommitId,
    ) -> Result<std::collections::BTreeSet<String>, ObjectError> {
        self.seen.borrow_mut().push(id.clone());
        self.inner.changed_paths(id)
    }
    fn seal_for_commit(&self, id: &CommitId) -> Result<Option<SealBlob>, ObjectError> {
        self.seen.borrow_mut().push(id.clone());
        self.inner.seal_for_commit(id)
    }
}

#[test]
fn t55_truncated_uses_only_new_and_length() {
    let a = chain_push();
    for base in [policy_observe(), policy_enforce_main()] {
        let policy = Policy {
            max_commits_per_push: 3,
            ..base
        };
        let reference = pre(&a, &policy);
        let subset = SubsetRange {
            inner: &a,
            subset: vec![id(5), id(3), id(4)],
            seen: std::cell::RefCell::new(Vec::new()),
        };
        let out = pre_receive(&a.ctx, &policy, &subset, &a);
        assert_eq!(out.decision, reference.decision);
        assert_eq!(out.findings, reference.findings);
        assert_eq!(out.findings.len(), 1);
        let f = &out.findings[0];
        assert_eq!(f.commit, id(6));
        assert_eq!(f.push.new_commits, 3);
        assert!(f.push.truncated);
        let seen = subset.seen.borrow();
        assert!(!seen.is_empty());
        assert!(seen.iter().all(|c| *c == id(6)), "{seen:?}");
        if policy.mode == Mode::Enforce {
            assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
            let ev = only_event(&out.events, "push_rejected_policy");
            assert_eq!(ev.fields["grund"], "truncated");
        } else {
            assert_eq!(out.decision, Decision::Accept);
        }
    }
}

/// Kette `id(1)` ← `id(2)` ← `id(3)` mit Seals unter `refs/minds/seals/s<n>`,
/// angelegt als neuer Ref `refs/heads/main`.
fn creation_push() -> MemoryAdapter {
    let mut a = MemoryAdapter::new(ctx(vec![update(0, 3, "refs/heads/main")]));
    for n in 1..=3u8 {
        let parents = if n == 1 { vec![] } else { vec![id(n - 1)] };
        let p = format!("f{n}");
        a.add_commit(id(n), parents, paths(&[p.as_str()]));
        a.add_seal(
            id(n),
            &format!("refs/minds/seals/s{n}"),
            format!("seal-{n}").into_bytes(),
            valid_seal(n, &[p.as_str()]),
        );
    }
    a
}

#[test]
fn t56_ref_creation_evaluates_all_reachable() {
    // (a) alle drei erreichbaren Commits, ältester zuerst.
    let a = creation_push();
    let out = pre(&a, &policy_enforce_main());
    assert_eq!(out.decision, Decision::Accept);
    let commits: Vec<CommitId> = out.findings.iter().map(|f| f.commit.clone()).collect();
    assert_eq!(commits, vec![id(1), id(2), id(3)]);
    for f in &out.findings {
        assert!(f.push.old.is_null());
        assert_eq!(f.push.new_commits, 3);
        assert!(!f.push.truncated);
    }

    // (b) gekürzt: nur `new` bewertet, erzwungener Ref abgelehnt.
    let policy = Policy {
        max_commits_per_push: 2,
        ..policy_enforce_main()
    };
    let out = pre(&a, &policy);
    assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
    assert!(message(&out.decision).contains("mehr als 2 neue Commits"));
    assert_eq!(out.findings.len(), 1);
    assert_eq!(out.findings[0].commit, id(3));
    let ev = only_event(&out.events, "push_rejected_policy");
    assert_eq!(ev.fields["grund"], "truncated");

    // (c) auch der älteste erreichbare Commit braucht Evidence.
    let mut c = creation_push();
    c.remove_seal(&id(1));
    let out = pre(&c, &policy_enforce_main());
    assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
    let ev = only_event(&out.events, "push_rejected_policy");
    assert_eq!(ev.fields["grund"], "fail");
    assert_eq!(ev.fields["commit"], id(1).as_str());
}
