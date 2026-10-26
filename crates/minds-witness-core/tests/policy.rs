//! Tests der Policy (Abschnitt 6 und 12): Validierung, Muster-Matching und
//! die konservative Abdeckungsfunktion `covers`.

mod fixtures;

use fixtures::*;
use minds_witness_core::*;

fn with_patterns(evaluate: &[&str], enforce: &[&str]) -> Policy {
    Policy {
        evaluate_refs: evaluate.iter().map(|s| (*s).to_owned()).collect(),
        enforce_refs: enforce.iter().map(|s| (*s).to_owned()).collect(),
        ..Policy::default()
    }
}

#[test]
fn t28_validate() {
    let bad = |p: &str| PolicyError::BadPattern(p.to_owned());
    assert_eq!(
        with_patterns(&["heads/*"], &[]).validate(),
        Err(bad("heads/*"))
    );
    assert_eq!(
        with_patterns(&["refs/*/x"], &[]).validate(),
        Err(bad("refs/*/x"))
    );
    assert_eq!(with_patterns(&[""], &[]).validate(), Err(bad("")));
    // Auch Enforce-Muster werden auf Form geprüft.
    assert_eq!(
        with_patterns(&["refs/heads/*"], &["refs/heads/x*"]).validate(),
        Err(bad("refs/heads/x*"))
    );
    assert_eq!(
        Policy {
            max_commits_per_push: 0,
            ..Policy::default()
        }
        .validate(),
        Err(PolicyError::MaxCommitsZero)
    );
    assert_eq!(
        Policy {
            coverage_threshold_percent: 101,
            ..Policy::default()
        }
        .validate(),
        Err(PolicyError::ThresholdOver100)
    );
    assert_eq!(
        Policy {
            pusher_mode: PusherMode::Hashed,
            ..Policy::default()
        }
        .validate(),
        Err(PolicyError::SaltMissing)
    );
    assert_eq!(Policy::default().validate(), Ok(()));
}

#[test]
fn t29_matches_pattern() {
    assert!(matches_pattern("refs/heads/*", "refs/heads/main"));
    assert!(matches_pattern("refs/heads/*", "refs/heads/release/1.0"));
    assert!(!matches_pattern("refs/heads/*", "refs/heads/"));
    assert!(!matches_pattern("refs/heads/*", "refs/headsx"));
    assert!(matches_pattern("refs/heads/main", "refs/heads/main"));
    assert!(!matches_pattern("refs/heads/main", "refs/heads/main2"));
    assert!(!matches_pattern("refs/heads/main", "refs/heads/mai"));
}

#[test]
fn t42_covers() {
    assert!(covers("refs/heads/*", "refs/heads/main"));
    assert!(covers("refs/heads/*", "refs/heads/release/*"));
    assert!(!covers("refs/heads/main", "refs/heads/*"));
    assert!(!covers("refs/tags/*", "refs/heads/main"));
    assert!(!covers("refs/heads/*", "refs/heads/"));
    assert!(!covers("refs/heads/*", "refs/headsx"));

    let policy = Policy {
        evaluate_refs: vec!["refs/heads/*".to_owned()],
        enforce_refs: vec!["refs/heads/release/*".to_owned()],
        mode: Mode::Enforce,
        ..Default::default()
    };
    assert_eq!(policy.validate(), Ok(()));

    let mut a = single_commit_push();
    a.remove_seal(&id(2));
    a.ctx.updates = vec![update(1, 2, "refs/heads/release/1.0")];
    let out = pre(&a, &policy);
    assert_eq!(reason(&out.decision), Some(RejectReason::PolicyFail));
}
