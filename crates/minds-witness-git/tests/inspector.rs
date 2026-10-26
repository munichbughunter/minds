//! Spec S2 v0.4 §13, Ebene 1: `GitSealInspector` über einer
//! In-Memory-Objektsicht (s2_06 bis s2_21, s2_37). Der Git-Pfad (Snapshot,
//! Quarantäne, `GitStore`) folgt mit Commit 5 und 6.

mod common;

use std::collections::BTreeSet;

use common::*;
use minds_core::evidence::{SCOPE_AGENT_HOOKS_V1, SCOPE_WITNESS_V1, SealOutcome};
use minds_core::observation::ObservationReason;
use minds_witness_core::{
    Decision, HashStr, Mode, Policy, PolicyVerdict, SealError, SealSignature, SealState,
    pre_receive,
};
use minds_witness_git::budget::Budget;
use minds_witness_git::inspector::{GitSealInspector, InspectorConfig, binding_hash};
use minds_witness_git::trust::{SigVerdict, TrustConfig, Verifier};
use minds_witness_git::view::{MODE_BLOB, MODE_EXEC, MODE_GITLINK, MODE_LINK};

/// Basis `B` mit `a.rs`, `b.rs`, `d.rs`; `C` ändert `a.rs`, `b.rs` und
/// löscht `d.rs`.
fn base(w: &mut World) -> (String, String) {
    let b = w.commit(
        &[],
        &[
            ("src/a.rs", MODE_BLOB, b"a0"),
            ("src/b.rs", MODE_BLOB, b"b0"),
            ("src/d.rs", MODE_BLOB, b"d0"),
        ],
    );
    let c = w.commit(
        std::slice::from_ref(&b),
        &[
            ("src/a.rs", MODE_BLOB, b"a1"),
            ("src/b.rs", MODE_BLOB, b"b1"),
        ],
    );
    (b, c)
}

fn bind(w: &mut World, c: &str, seals: &[String]) -> String {
    let b = w.binding(c, PROJECT, seals);
    w.put_binding(&b);
    b.id()
}

fn set(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn s2_06_ref_name_consistency() {
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let seal = w.evidence(&observations(
        &[("src/a.rs", Obs::Content(b"a1"))],
        Some(separated()),
    ));
    // Ref-Id passt nicht zu den Bytes: unlesbar.
    let b = w.binding(&c, PROJECT, std::slice::from_ref(&seal));
    let bytes = b.to_bytes();
    let sig = sign(&bytes, &w.key, "minds-witness");
    w.put_raw_candidate(
        &c,
        &"0".repeat(64),
        &[
            ("binding", MODE_BLOB, &bytes),
            ("binding.sig", MODE_BLOB, &sig),
        ],
    );
    let (result, diags) = w.inspect(&c);
    assert!(matches!(result, Err(SealError::Malformed(_))), "{result:?}");
    assert_eq!(diags[0].reason, "malformed");

    // Gültig signierte Bindung für einen anderen Commit unter `<c>/`.
    let mut w = World::new();
    let (b0, c) = base(&mut w);
    let other = w.binding(&b0, PROJECT, &[seal_id(0x11)]);
    let key = w.key.clone();
    w.put_binding_as(&other, &c, Some(&key));
    let summary = w.inspect(&c).0.unwrap();
    assert_eq!(summary.signature, SealSignature::Valid);
    assert_eq!(summary.bound_commit.as_str(), b0);
    // Im Kern: Mismatched.
    let out = core_run(&w, &b0, &c, Mode::Observe);
    assert_eq!(out.findings[0].seal.status, SealState::Mismatched);
}

fn core_run(w: &World, old: &str, new: &str, mode: Mode) -> minds_witness_core::PreOutcome {
    let ctx = minds_witness_core::PushContext {
        repo: PROJECT.to_owned(),
        updates: vec![minds_witness_core::RefUpdate {
            old: minds_witness_core::CommitId::parse(old).unwrap(),
            new: minds_witness_core::CommitId::parse(new).unwrap(),
            ref_name: "refs/heads/main".to_owned(),
        }],
        pusher: minds_witness_core::Pusher {
            id: "1".to_owned(),
            username: "dev".to_owned(),
        },
        server: minds_witness_core::ServerInfo {
            host: "git.test".to_owned(),
            kind: minds_witness_core::ServerKind::Memory,
            time: minds_witness_core::Timestamp::parse("2026-10-26T12:00:00Z").unwrap(),
        },
    };
    let enforce = mode == Mode::Enforce;
    let policy = Policy {
        mode,
        enforce_refs: if enforce {
            vec!["refs/heads/main".to_owned()]
        } else {
            vec![]
        },
        ..Policy::default()
    };
    let trust = w.trust();
    let budget = Budget::unlimited();
    let verifier = Verifier::new(&trust, &budget);
    let inspector =
        GitSealInspector::new(&w.view, &verifier, &budget, InspectorConfig::new(PROJECT));
    pre_receive(&ctx, &policy, &ViewAccess(w), &inspector)
}

#[test]
fn s2_07_content_bound_coverage() {
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let seal = w.evidence(&observations(
        &[
            ("src/a.rs", Obs::Content(b"a1")),
            ("src/b.rs", Obs::Content(b"b-other")),
        ],
        Some(separated()),
    ));
    bind(&mut w, &c, &[seal]);
    assert_eq!(w.touched(&c), set(&["src/a.rs"]));
}

#[test]
fn s2_08_deletion_coverage() {
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let seal = w.evidence(&observations(
        &[("src/d.rs", Obs::Deleted)],
        Some(separated()),
    ));
    bind(&mut w, &c, &[seal]);
    assert_eq!(w.touched(&c), set(&["src/d.rs"]));

    let mut w = World::new();
    let (_, c) = base(&mut w);
    let seal = w.evidence(&observations(
        &[("src/d.rs", Obs::Content(b"d0"))],
        Some(separated()),
    ));
    bind(&mut w, &c, &[seal]);
    assert!(w.touched(&c).is_empty(), "ein Hash deckt keine Löschung");
}

#[test]
fn s2_09_session_mentions_never_count() {
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let mut seals = Vec::new();
    for scope in [SCOPE_WITNESS_V1, SCOPE_AGENT_HOOKS_V1] {
        let text = seal_text(
            scope,
            SealOutcome::Stored {
                session: seal_id(0x33),
            },
            "claude-code",
        );
        seals.push(w.put_seal(&text, "minds-witness"));
    }
    // Auch mit gültigen, getrennten Observations, deren Hashes passen:
    // Nur `witness-fs/v1` erzeugt Abdeckung.
    let obs = observations(&[("src/a.rs", Obs::Content(b"a1"))], Some(separated()));
    let obs_id = w.put_observations(&obs);
    for scope in [SCOPE_WITNESS_V1, SCOPE_AGENT_HOOKS_V1] {
        let text = seal_text(
            scope,
            SealOutcome::ObservationsStored {
                observations: obs_id.clone(),
            },
            "witness",
        );
        seals.push(w.put_seal(&text, "minds-witness"));
    }
    bind(&mut w, &c, &seals);
    let summary = w.inspect(&c).0.unwrap();
    assert_eq!(summary.signature, SealSignature::Valid);
    assert!(summary.touched_paths.is_empty());
}

#[test]
fn s2_10_opaque_reasons_never_count() {
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let mut seals = Vec::new();
    for reason in [
        ObservationReason::TooLarge,
        ObservationReason::SecretFile,
        ObservationReason::OutsideRepo,
        ObservationReason::RedactedContent,
        ObservationReason::Unscannable,
        ObservationReason::Unknown,
    ] {
        seals.push(w.evidence(&observations(
            &[
                ("src/a.rs", Obs::Reason(reason)),
                ("src/d.rs", Obs::Reason(reason)),
            ],
            Some(separated()),
        )));
    }
    bind(&mut w, &c, &seals);
    assert!(w.touched(&c).is_empty());
}

#[test]
fn s2_11_mode_rules() {
    // Jeder Fall: (Eltern-Dateien je Elternteil, Datei im Commit, abgedeckt?)
    type Files<'a> = Vec<(&'a str, u32, &'a [u8])>;
    let x: &[u8] = b"x1";
    let cases: Vec<(&str, Vec<Files>, Files, bool)> = vec![
        (
            "neue 100755",
            vec![vec![("o", MODE_BLOB, b"o")]],
            vec![("f", MODE_EXEC, x)],
            false,
        ),
        (
            "neue 100755 im Merge",
            vec![vec![("o", MODE_BLOB, b"o")], vec![("p", MODE_BLOB, b"p")]],
            vec![("f", MODE_EXEC, x)],
            false,
        ),
        (
            "Merge 100755 / 100644",
            vec![vec![("f", MODE_EXEC, b"x0")], vec![("f", MODE_BLOB, b"x0")]],
            vec![("f", MODE_EXEC, x)],
            false,
        ),
        (
            "Merge 100755 / fehlt",
            vec![vec![("f", MODE_EXEC, b"x0")], vec![("p", MODE_BLOB, b"p")]],
            vec![("f", MODE_EXEC, x)],
            true,
        ),
        (
            "100644 → 100755",
            vec![vec![("f", MODE_BLOB, b"x0")]],
            vec![("f", MODE_EXEC, x)],
            false,
        ),
        (
            "100755 → 100755",
            vec![vec![("f", MODE_EXEC, b"x0")]],
            vec![("f", MODE_EXEC, x)],
            true,
        ),
        (
            "Symlink",
            vec![vec![("o", MODE_BLOB, b"o")]],
            vec![("f", MODE_LINK, x)],
            false,
        ),
        (
            "Gitlink",
            vec![vec![("o", MODE_BLOB, b"o")]],
            vec![("f", MODE_GITLINK, x)],
            false,
        ),
        (
            "ohne Eltern 100755",
            vec![],
            vec![("f", MODE_EXEC, x)],
            false,
        ),
    ];
    for (name, parents, files, expected) in cases {
        let mut w = World::new();
        let parent_ids: Vec<String> = parents.iter().map(|f| w.commit(&[], f)).collect();
        let c = w.commit(&parent_ids, &files);
        assert!(w.view_changed(&c).contains("f"), "{name}: f nicht geändert");
        let seal = w.evidence(&observations(&[("f", Obs::Content(x))], Some(separated())));
        bind(&mut w, &c, &[seal]);
        assert_eq!(w.touched(&c).contains("f"), expected, "{name}");
    }
}

trait Changed {
    fn view_changed(&self, c: &str) -> BTreeSet<String>;
}

impl Changed for World {
    fn view_changed(&self, c: &str) -> BTreeSet<String> {
        use minds_witness_git::view::EvidenceView;
        self.view.changed_paths(c).unwrap()
    }
}

#[test]
fn s2_12_separated_witness_required() {
    let mut not_proven = separated();
    not_proven.isolation.agent_uids = vec![1001];
    let mut local = separated();
    local.profile = "local".to_owned();
    for witness in [None, Some(local), Some(not_proven)] {
        let mut w = World::new();
        let (_, c) = base(&mut w);
        let seal = w.evidence(&observations(&[("src/a.rs", Obs::Content(b"a1"))], witness));
        bind(&mut w, &c, &[seal]);
        let (result, diags) = w.inspect(&c);
        assert!(result.unwrap().touched_paths.is_empty());
        assert!(diags.iter().any(|d| d.reason == "observations_discarded"));
    }
    // Die Regel `require_separated_witness = false` + Enforce prüft die
    // Adapter-Konfiguration (s2_23, Commit 7).
}

#[test]
fn s2_13_seal_bytes_and_chain() {
    let obs = observations(&[("src/a.rs", Obs::Content(b"a1"))], Some(separated()));
    // Seal mit Kind-Commit (Wurzel `{seal}`, Tip `{seal, seal.sig}`): gilt.
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let seal = w.evidence(&obs);
    bind(&mut w, &c, &[seal]);
    assert_eq!(w.touched(&c), set(&["src/a.rs"]));

    // Bytes passen nicht zur Id (Ref unter fremder Id).
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let obs_id = w.put_observations(&obs);
    let text = seal_text(
        "witness-fs/v1",
        SealOutcome::ObservationsStored {
            observations: obs_id,
        },
        "witness",
    );
    let wrong = seal_id(0x44);
    w.put_seal_at(&text, "minds-witness", &wrong[3..]);
    bind(&mut w, &c, &[wrong]);
    let (result, diags) = w.inspect(&c);
    assert!(result.unwrap().touched_paths.is_empty());
    assert!(diags.iter().any(|d| d.reason == "seal_discarded"));

    // Nicht kanonisch (CRLF), unter der Id dieser Bytes.
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let obs_id = w.put_observations(&obs);
    let crlf = seal_text(
        "witness-fs/v1",
        SealOutcome::ObservationsStored {
            observations: obs_id,
        },
        "witness",
    )
    .replace('\n', "\r\n");
    let id = w.put_seal(&crlf, "minds-witness");
    bind(&mut w, &c, &[id]);
    assert!(w.touched(&c).is_empty());

    // Ein Seal-Ref unter fremder Id, der auf das Ziel eines gültigen Seals
    // zeigt, erbt dessen Prüfung nicht, auch nicht über das Memo eines
    // vorher geprüften Commits (Review S2 Commit 4, B1).
    let mut w = World::new();
    let (b0, c1) = base(&mut w);
    let c2 = w.commit(
        std::slice::from_ref(&b0),
        &[
            ("src/a.rs", MODE_BLOB, b"a1"),
            ("src/x.rs", MODE_BLOB, b"x"),
        ],
    );
    let x = w.evidence(&obs);
    bind(&mut w, &c1, std::slice::from_ref(&x));
    let y = seal_id(0x45);
    use minds_witness_git::view::EvidenceView;
    let target = w
        .view
        .resolve_ref(&format!("refs/minds/evidence/{}", &x[3..]))
        .unwrap()
        .unwrap();
    w.view
        .set_ref(&format!("refs/minds/evidence/{}", &y[3..]), &target);
    bind(&mut w, &c2, &[y]);
    assert!(w.touched(&c2).is_empty());
    let both = w.inspect_many(&[&c1, &c2]);
    assert_eq!(both[0].touched_paths, set(&["src/a.rs"]));
    assert!(
        both[1].touched_paths.is_empty(),
        "Memo darf die Id-Prüfung nicht überspringen"
    );

    // Nur unter `minds` signiert: zählt nicht.
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let obs_id = w.put_observations(&obs);
    let text = seal_text(
        "witness-fs/v1",
        SealOutcome::ObservationsStored {
            observations: obs_id,
        },
        "witness",
    );
    let id = w.put_seal(&text, "minds");
    bind(&mut w, &c, &[id]);
    assert!(w.touched(&c).is_empty());
}

#[test]
fn s2_14_observation_bytes() {
    let good = observations(&[("src/a.rs", Obs::Content(b"a1"))], Some(separated()));
    let bytes = good.canonical_bytes().unwrap();
    let pretty = serde_json::to_vec_pretty(&good).unwrap();
    let mut unordered = observations(
        &[
            ("src/a.rs", Obs::Content(b"a1")),
            ("src/b.rs", Obs::Content(b"b1")),
        ],
        Some(separated()),
    );
    unordered.observations.swap(0, 1);
    let unordered = minds_core::to_canonical_json(&unordered).unwrap();
    for (name, payload, at) in [
        ("Id passt nicht", bytes.clone(), Some(hex(&[0x55; 32]))),
        ("nicht kanonisch", pretty, None),
        ("check scheitert", unordered, None),
    ] {
        let mut w = World::new();
        let (_, c) = base(&mut w);
        let id = match at {
            Some(hex_id) => {
                w.put_observation_bytes_at(&payload, &hex_id);
                format!("b3-{hex_id}")
            }
            None => w.put_observation_bytes(&payload),
        };
        let text = seal_text(
            "witness-fs/v1",
            SealOutcome::ObservationsStored { observations: id },
            "witness",
        );
        let seal = w.put_seal(&text, "minds-witness");
        bind(&mut w, &c, &[seal]);
        let (result, diags) = w.inspect(&c);
        assert!(result.unwrap().touched_paths.is_empty(), "{name}");
        assert!(
            diags.iter().any(|d| d.reason == "observations_discarded"),
            "{name}"
        );
    }
}

#[test]
fn s2_15_union_of_bindings() {
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let s1 = w.evidence(&observations(
        &[("src/a.rs", Obs::Content(b"a1"))],
        Some(separated()),
    ));
    let s2 = w.evidence(&observations(
        &[("src/b.rs", Obs::Content(b"b1"))],
        Some(separated()),
    ));
    let id1 = bind(&mut w, &c, &[s1]);
    let id2 = bind(&mut w, &c, &[s2]);
    let summary = w.inspect(&c).0.unwrap();
    assert_eq!(summary.touched_paths, set(&["src/a.rs", "src/b.rs"]));
    assert_eq!(
        summary.hash,
        binding_hash("accepted_bindings", &c, vec![id1, id2])
    );
}

#[test]
fn s2_16_extra_invalid_binding_never_changes_verdict() {
    let obs = observations(&[("src/a.rs", Obs::Content(b"a1"))], Some(separated()));
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let seal = w.evidence(&obs);
    bind(&mut w, &c, std::slice::from_ref(&seal));
    let only = w.inspect(&c).0.unwrap();

    // Müll, unsigniert, fremd signiert; ihre Seals existieren nicht.
    let ghost = seal_id(0x66);
    w.put_raw_candidate(&c, &"1".repeat(64), &[("binding", MODE_BLOB, b"junk")]);
    let unsigned = w.binding(&c, PROJECT, std::slice::from_ref(&ghost));
    w.put_binding_as(&unsigned, &c.clone(), None);
    let (foreign_key, _) = keypair(w.dir.path(), "foreign");
    let foreign = w.binding(&c, PROJECT, &[ghost.clone(), seal_id(0x77)]);
    w.put_binding_as(&foreign, &c.clone(), Some(&foreign_key));
    let (result, diags) = w.inspect(&c);
    let with_junk = result.unwrap();
    assert_eq!(with_junk, only);
    assert!(
        !diags
            .iter()
            .any(|d| d.id.as_deref() == Some(ghost.as_str())),
        "Seals verworfener Bindungen werden nicht gelesen"
    );
}

#[test]
fn s2_17_mismatch_project_and_commit() {
    let mut w = World::new();
    let (b0, c) = base(&mut w);
    let other_project = w.binding(&c, "git.test/project-7", &[seal_id(0x11)]);
    w.put_binding(&other_project);
    let (s, diags) = w.inspect(&c);
    let s = s.unwrap();
    assert_eq!(s.signature, SealSignature::Valid);
    assert_eq!(s.bound_repo.as_deref(), Some("git.test/project-7"));
    assert!(s.touched_paths.is_empty());
    assert_eq!(diags[0].reason, "mismatch");
    assert_eq!(diags[0].principal.as_deref(), Some(PRINCIPAL));
    let out = core_run(&w, &b0, &c, Mode::Observe);
    assert_eq!(out.findings[0].seal.status, SealState::Mismatched);

    // Mehrere Fehlbindungen: Es gilt die erste in Ref-Namensordnung.
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let p7 = w.binding(&c, "git.test/project-7", &[seal_id(0x11)]);
    let p8 = w.binding(&c, "git.test/project-8", &[seal_id(0x11)]);
    w.put_binding(&p7);
    w.put_binding(&p8);
    let first = if p7.id() < p8.id() { &p7 } else { &p8 };
    let s = w.inspect(&c).0.unwrap();
    assert_eq!(s.bound_repo.as_deref(), Some(first.project()));
    assert_eq!(
        s.hash,
        binding_hash("mismatched_binding", &c, vec![first.id()])
    );

    let mut w = World::new();
    let (b0, c) = base(&mut w);
    let other_commit = w.binding(&b0, PROJECT, &[seal_id(0x11)]);
    let key = w.key.clone();
    w.put_binding_as(&other_commit, &c, Some(&key));
    let s = w.inspect(&c).0.unwrap();
    assert_eq!(s.bound_commit.as_str(), b0);
    let out = core_run(&w, &b0, &c, Mode::Observe);
    assert_eq!(out.findings[0].seal.status, SealState::Mismatched);
}

#[test]
fn s2_18_result_classes() {
    // Nur unlesbar.
    let mut w = World::new();
    let (_, c) = base(&mut w);
    w.put_raw_candidate(&c, &"2".repeat(64), &[("binding", MODE_BLOB, b"junk")]);
    assert!(matches!(w.inspect(&c).0, Err(SealError::Malformed(_))));

    // Nur unsigniert.
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let b = w.binding(&c, PROJECT, &[seal_id(0x11)]);
    w.put_binding_as(&b, &c.clone(), None);
    let s = w.inspect(&c).0.unwrap();
    assert_eq!(s.signature, SealSignature::Unsigned);
    assert_eq!(s.bound_repo.as_deref(), Some(PROJECT));
    assert_eq!(s.bound_commit.as_str(), c);

    // Nur fremd signiert.
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let (foreign, _) = keypair(w.dir.path(), "foreign");
    let b = w.binding(&c, PROJECT, &[seal_id(0x11)]);
    w.put_binding_as(&b, &c.clone(), Some(&foreign));
    let s = w.inspect(&c).0.unwrap();
    assert_eq!(s.signature, SealSignature::Invalid);
    assert_eq!(s.bound_repo.as_deref(), Some(PROJECT));
}

#[test]
fn s2_19_process_budget_and_cache() {
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let seal = w.evidence(&observations(
        &[("src/a.rs", Obs::Content(b"a1"))],
        Some(separated()),
    ));
    bind(&mut w, &c, &[seal]);

    // Gleiche Prüfung zweimal → ein Prozess.
    let payload = b"payload";
    let sig = sign(payload, &w.key, "minds-witness");
    let trust = w.trust();
    let budget = Budget::unlimited();
    let v = Verifier::new(&trust, &budget);
    assert!(matches!(
        v.verify(payload, &sig, None).unwrap(),
        SigVerdict::Accepted { .. }
    ));
    assert!(matches!(
        v.verify(payload, &sig, None).unwrap(),
        SigVerdict::Accepted { .. }
    ));
    assert_eq!(budget.processes(), 1);

    // Signer-Datei mit 100 Zeilen: ein Prozess.
    let mut big = String::new();
    for i in 0..99 {
        let (_, k) = keypair(w.dir.path(), &format!("k{i}"));
        big.push_str(&format!("dev{i}@test {k}\n"));
    }
    big.push_str(&w.trust_text);
    let trust = TrustConfig::from_text(
        &big,
        &ssh_keygen(),
        &[PRINCIPAL.to_owned()],
        true,
        timeout(),
    )
    .unwrap();
    let budget = Budget::unlimited();
    let v = Verifier::new(&trust, &budget);
    assert!(matches!(
        v.verify(payload, &sig, None).unwrap(),
        SigVerdict::Accepted { .. }
    ));
    assert_eq!(budget.processes(), 1);

    // Budget 0 → Internal.
    let trust = w.trust();
    let budget = Budget::new(0, u64::MAX, None);
    let verifier = Verifier::new(&trust, &budget);
    let inspector =
        GitSealInspector::new(&w.view, &verifier, &budget, InspectorConfig::new(PROJECT));
    let blob = w.blob_for(&c).unwrap();
    let expected = minds_witness_core::SealExpectation {
        repo: PROJECT.to_owned(),
        commit: minds_witness_core::CommitId::parse(&c).unwrap(),
    };
    let r = minds_witness_core::SealInspector::inspect(&inspector, &blob, &expected);
    assert!(matches!(r, Err(SealError::Internal(_))), "{r:?}");

    // Hängendes ssh-keygen → Timeout → Internal.
    let script = w.dir.path().join("slow-keygen");
    std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let trust = TrustConfig::from_text(
        &w.trust_text,
        &script,
        &[PRINCIPAL.to_owned()],
        true,
        std::time::Duration::from_millis(300),
    )
    .unwrap();
    let budget = Budget::unlimited();
    let verifier = Verifier::new(&trust, &budget);
    let inspector =
        GitSealInspector::new(&w.view, &verifier, &budget, InspectorConfig::new(PROJECT));
    let started = std::time::Instant::now();
    let r = minds_witness_core::SealInspector::inspect(&inspector, &blob, &expected);
    assert!(matches!(r, Err(SealError::Internal(_))), "{r:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn s2_20_no_object_content_in_errors() {
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let obs = observations(
        &[("src/MARKER-PATH.rs", Obs::Content(b"MARKER-CONTENT"))],
        None, // verworfen: kein Witness
    );
    let obs_id = w.put_observations(&obs);
    let text = seal_text(
        "witness-fs/v1",
        SealOutcome::ObservationsStored {
            observations: obs_id,
        },
        "MARKER-AGENT",
    );
    let seal = w.put_seal(&text, "minds");
    bind(&mut w, &c, &[seal]);
    w.put_raw_candidate(
        &c,
        &"3".repeat(64),
        &[("binding", MODE_BLOB, b"MARKER-BINDING")],
    );
    let (result, diags) = w.inspect(&c);
    let rendered = format!("{result:?} {diags:?}");
    assert!(!rendered.contains("MARKER"), "{rendered}");
    // Auch ein unlesbarer Kandidat allein.
    let mut w = World::new();
    let (_, c) = base(&mut w);
    w.put_raw_candidate(
        &c,
        &"3".repeat(64),
        &[("binding", MODE_BLOB, b"MARKER-BINDING")],
    );
    let (result, diags) = w.inspect(&c);
    let rendered = format!(
        "{result:?} {} {diags:?}",
        result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default()
    );
    assert!(!rendered.contains("MARKER"), "{rendered}");
}

#[test]
fn s2_21_core_contract_with_git_inspector() {
    let mut w = World::new();
    let (b0, c) = base(&mut w);
    let seal = w.evidence(&observations(
        &[
            ("src/a.rs", Obs::Content(b"a1")),
            ("src/b.rs", Obs::Content(b"b1")),
            ("src/d.rs", Obs::Deleted),
        ],
        Some(separated()),
    ));
    bind(&mut w, &c, &[seal]);
    let out = core_run(&w, &b0, &c, Mode::Enforce);
    assert_eq!(out.decision, Decision::Accept, "{:?}", out.events);
    assert_eq!(out.findings[0].policy.result, PolicyVerdict::Pass);
    assert_eq!(out.findings[0].coverage.paths_covered, 3);

    // Ohne Observation: Seal ohne Beobachtungen → Fail.
    let mut w = World::new();
    let (b0, c) = base(&mut w);
    let text = seal_text(
        SCOPE_WITNESS_V1,
        SealOutcome::Stored {
            session: seal_id(0x33),
        },
        "witness",
    );
    let seal = w.put_seal(&text, "minds-witness");
    bind(&mut w, &c, &[seal]);
    let out = core_run(&w, &b0, &c, Mode::Enforce);
    assert!(matches!(out.decision, Decision::Reject { .. }));
    assert_eq!(out.findings[0].policy.result, PolicyVerdict::Fail);
    assert_eq!(out.findings[0].seal.status, SealState::Valid);
}

#[test]
fn s2_37_receipt_hash_golden() {
    let h = |kind: &str, c: &str, ids: &[&str]| {
        let list = ids
            .iter()
            .map(|i| format!("\"{i}\""))
            .collect::<Vec<_>>()
            .join(",");
        HashStr::of_bytes(
            format!("{{\"bindings\":[{list}],\"commit\":\"{c}\",\"kind\":\"{kind}\"}}").as_bytes(),
        )
    };
    // Die Definition selbst, unabhängig vom Inspektor, festgenagelt mit einem
    // Wert, der außerhalb dieser Implementierung berechnet wurde (Python
    // `blake3` über die kanonischen Bytes).
    let c0 = "a1".repeat(20);
    let (i1, i2) = (seal_id(0x11), seal_id(0x22));
    assert_eq!(
        binding_hash("accepted_bindings", &c0, vec![i1.clone()]).as_str(),
        "blake3:400467de917a78a0ad1b9ae93d81f2fd99040d7914430a215db68ea2360625af"
    );
    assert_eq!(
        binding_hash("accepted_bindings", &c0, vec![i2.clone(), i1.clone()]),
        h("accepted_bindings", &c0, &[&i1, &i2])
    );

    // accepted
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let seal = w.evidence(&observations(
        &[("src/a.rs", Obs::Content(b"a1"))],
        Some(separated()),
    ));
    let id = bind(&mut w, &c, &[seal]);
    assert_eq!(
        w.inspect(&c).0.unwrap().hash,
        h("accepted_bindings", &c, &[&id])
    );

    // mismatched
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let b = w.binding(&c, "git.test/project-7", &[seal_id(0x11)]);
    w.put_binding(&b);
    assert_eq!(
        w.inspect(&c).0.unwrap().hash,
        h("mismatched_binding", &c, &[&b.id()])
    );

    // rejected (Invalid) und unsigned
    let mut w = World::new();
    let (_, c) = base(&mut w);
    let (foreign, _) = keypair(w.dir.path(), "foreign");
    let b1 = w.binding(&c, PROJECT, &[seal_id(0x11)]);
    w.put_binding_as(&b1, &c.clone(), Some(&foreign));
    let b2 = w.binding(&c, PROJECT, &[seal_id(0x22)]);
    w.put_binding_as(&b2, &c.clone(), None);
    let s = w.inspect(&c).0.unwrap();
    assert_eq!(s.signature, SealSignature::Invalid);
    assert_eq!(
        s.hash,
        h(
            "rejected_bindings",
            &c,
            &{
                let mut v = [b1.id(), b2.id()];
                v.sort();
                v
            }
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
        )
    );

    let mut w = World::new();
    let (_, c) = base(&mut w);
    let b = w.binding(&c, PROJECT, &[seal_id(0x11)]);
    w.put_binding_as(&b, &c.clone(), None);
    let s = w.inspect(&c).0.unwrap();
    assert_eq!(s.signature, SealSignature::Unsigned);
    assert_eq!(s.hash, h("rejected_bindings", &c, &[&b.id()]));
}
