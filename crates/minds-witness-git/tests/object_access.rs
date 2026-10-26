//! Spec S2 v0.4.1 §13, Ebene 2: Snapshot und `GitObjectAccess` gegen
//! temporäre Repositories (s2_13 Git-Teil, s2_24 bis s2_34, s2_42).

mod support;

use std::collections::{BTreeMap, BTreeSet};

use minds_core::evidence::SealOutcome;
use minds_git::quarantine::{Limits, ReceiveEnv, ServerRepo};
use minds_witness_core::memory::MemoryAdapter;
use minds_witness_core::{
    CommitId, Mode, ObjectAccess, ObjectError, Policy, PushContext, Pusher, RefUpdate, SealError,
    SealExpectation, SealInspector, ServerInfo, ServerKind, Timestamp, pre_receive,
};
use minds_witness_git::access::GitObjectAccess;
use minds_witness_git::budget::Budget;
use minds_witness_git::fsck;
use minds_witness_git::inspector::{GitSealInspector, InspectorConfig};
use minds_witness_git::limits::AdapterLimits;
use minds_witness_git::trust::{TrustConfig, Verifier};
use support::*;

fn open(repo: &Repo, limits: &AdapterLimits) -> ServerRepo {
    ServerRepo::open_for_receive(
        &ReceiveEnv {
            git_dir: repo.git_dir.clone(),
            quarantine: None,
            namespace: false,
        },
        limits.repo_limits(),
    )
    .unwrap()
}

fn trust(fx: &GitTestFixture) -> TrustConfig {
    TrustConfig::from_text(
        &fx.signers(),
        &ssh_keygen(),
        &[PRINCIPAL.to_owned()],
        true,
        std::time::Duration::from_secs(10),
    )
    .unwrap()
}

fn cid(s: &str) -> CommitId {
    CommitId::parse(s).unwrap()
}

fn null() -> CommitId {
    CommitId::null_of_len(40).unwrap()
}

fn update(old: Option<&str>, new: &str, name: &str) -> RefUpdate {
    RefUpdate {
        old: old.map_or_else(null, cid),
        new: cid(new),
        ref_name: name.to_owned(),
    }
}

/// Ein Aufruf: Repository, Updates, Grenzen; `f` bekommt Zugriff und
/// Inspektor.
fn with<R>(
    fx: &GitTestFixture,
    repo: &Repo,
    updates: &[RefUpdate],
    limits: &AdapterLimits,
    budget: Budget,
    f: impl FnOnce(&GitObjectAccess<'_>, &GitSealInspector<'_, GitObjectAccess<'_>>) -> R,
) -> R {
    let server = open(repo, limits);
    let trust = trust(fx);
    let verifier = Verifier::new(&trust, &budget);
    let git = git_program();
    let access = GitObjectAccess::new(&server, updates, &git, &budget, &trust, limits);
    let mut cfg = InspectorConfig::new(PROJECT);
    cfg.max_binding_bytes = limits.max_binding_bytes;
    cfg.max_seals_per_binding = limits.max_seals_per_binding;
    cfg.max_observations_bytes = limits.max_observations_bytes;
    let inspector = GitSealInspector::new(&access, &verifier, &budget, cfg);
    f(&access, &inspector)
}

fn changed(fx: &GitTestFixture, repo: &Repo, c: &str) -> BTreeSet<String> {
    with(
        fx,
        repo,
        &[],
        &AdapterLimits::default(),
        Budget::unlimited(),
        |a, _| a.changed_paths(&cid(c)).unwrap(),
    )
}

fn memory_range(
    commits: &BTreeMap<String, Vec<String>>,
    old: Option<&str>,
    new: &str,
    limit: u32,
) -> minds_witness_core::CommitRange {
    let ctx = ctx(vec![]);
    let mut m = MemoryAdapter::new(ctx);
    for (c, ps) in commits {
        m.add_commit(cid(c), ps.iter().map(|p| cid(p)).collect(), BTreeSet::new());
    }
    m.new_commits(old.map(cid).as_ref(), &cid(new), limit)
        .unwrap()
}

fn ctx(updates: Vec<RefUpdate>) -> PushContext {
    PushContext {
        repo: PROJECT.to_owned(),
        updates,
        pusher: Pusher {
            id: "1".to_owned(),
            username: "dev".to_owned(),
        },
        server: ServerInfo {
            host: "git.test".to_owned(),
            kind: ServerKind::Gitaly,
            time: Timestamp::parse("2026-10-26T12:00:00Z").unwrap(),
        },
    }
}

#[test]
fn s2_13_seal_via_git_store() {
    use minds_store::ContextStore;
    let fx = GitTestFixture::new();
    let server = fx.server();
    let client = fx.client(&server);
    // Echte Seals über `put_seal` und `put_seal_signature` im Client.
    let store = minds_store::InRepoStore::open(&client.path).unwrap();
    let e = Evidence {
        repo: &client,
        key: &fx.key,
        public: &fx.public,
    };
    let obs = observations(&[("src/a.rs", Obs::Content(b"a1"))]);
    let (obs_id, p) = e.observations(&obs);
    client.update_ref(&p.name, &p.target);
    let text = seal_text(
        "witness-fs/v1",
        SealOutcome::ObservationsStored {
            observations: obs_id,
        },
    );
    let id = store.put_seal(&text).unwrap();
    let sig = String::from_utf8(sign(text.as_bytes(), &fx.key, "minds-witness")).unwrap();
    store.put_seal_signature(&id, &sig).unwrap();
    let base = client.commit_files(&[("src/a.rs", "100644", b"a0")], &[]);
    let c = client.commit_files(&[("src/a.rs", "100644", b"a1")], &[&base]);
    let (_, bp) = e.binding(&c, &[id.as_str().to_owned()]);
    client.update_ref(&bp.name, &bp.target);
    let touched = with(
        &fx,
        &client,
        &[],
        &AdapterLimits::default(),
        Budget::unlimited(),
        |a, i| {
            let blob = a.seal_for_commit(&cid(&c)).unwrap().unwrap();
            i.inspect(
                &blob,
                &SealExpectation {
                    repo: PROJECT.to_owned(),
                    commit: cid(&c),
                },
            )
            .unwrap()
            .touched_paths
        },
    );
    assert_eq!(touched, BTreeSet::from(["src/a.rs".to_owned()]));
}

/// Baut Commits nach einer Topologie (Name → Eltern-Namen) mit eigenem
/// Inhalt je Commit; liefert Name → Id und Id → Eltern-Ids.
fn topology(
    repo: &Repo,
    spec: &[(&str, &[&str], &str)],
) -> (BTreeMap<String, String>, BTreeMap<String, Vec<String>>) {
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let mut graph = BTreeMap::new();
    for (name, parents, date) in spec {
        let ps: Vec<String> = parents.iter().map(|p| ids[*p].clone()).collect();
        let refs: Vec<&str> = ps.iter().map(String::as_str).collect();
        let t = repo.files(&[("f", "100644", name.as_bytes())]);
        let c = repo.commit_at(&t, &refs, date, name);
        graph.insert(c.clone(), ps);
        ids.insert((*name).to_owned(), c);
    }
    (ids, graph)
}

#[test]
fn s2_24_new_commits_match_memory_adapter() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    let d = "2026-10-26T10:00:00Z";
    let (ids, graph) = topology(
        &repo,
        &[
            ("a", &[], d),
            ("b", &["a"], d),
            ("c", &["a"], d),
            ("m", &["b", "c"], d),
            ("e", &["m"], d),
            ("x", &["a"], d),
        ],
    );
    // Leerer Commit: gleicher Baum wie sein Elternteil.
    let e_tree = repo.git(&["rev-parse", &format!("{}^{{tree}}", ids["e"])]);
    let empty = repo.commit(&e_tree, &[&ids["e"]]);
    let mut graph = graph;
    graph.insert(empty.clone(), vec![ids["e"].clone()]);
    let cases: Vec<(Option<&str>, &str)> = vec![
        (None, &empty),
        (Some(&ids["a"]), &empty),
        (Some(&ids["b"]), &ids["m"]),
        // Force-Push: old ist kein Vorfahr von new.
        (Some(&ids["x"]), &ids["e"]),
    ];
    for (old, new) in cases {
        let full = memory_range(&graph, old, new, 100);
        for limit in [1u32, 2, 3, 100] {
            let mem = memory_range(&graph, old, new, limit);
            let got = with(
                &fx,
                &repo,
                &[],
                &AdapterLimits::default(),
                Budget::unlimited(),
                |a, _| {
                    a.new_commits(old.map(cid).as_ref(), &cid(new), limit)
                        .unwrap()
                },
            );
            assert_eq!(got.truncated, mem.truncated, "{old:?} {new} {limit}");
            if got.truncated {
                assert_eq!(got.commits.len(), limit as usize);
                let set: BTreeSet<_> = got.commits.iter().collect();
                assert_eq!(set.len(), got.commits.len());
                assert!(got.commits.iter().all(|c| full.commits.contains(c)));
            } else {
                assert_eq!(got.commits, mem.commits, "{old:?} {new} {limit}");
            }
        }
    }
}

#[test]
fn s2_25_new_commits_random_graphs_vs_definition() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    // Deterministischer Zufall (xorshift), damit Fehler reproduzierbar sind.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    for g in 0..50 {
        let n = 3 + rnd(6) as usize;
        let mut ids: Vec<String> = Vec::new();
        let mut parents: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for i in 0..n {
            let mut ps = Vec::new();
            if i > 0 {
                for _ in 0..(1 + rnd(2)) {
                    let p = ids[rnd(i as u64) as usize].clone();
                    if !ps.contains(&p) {
                        ps.push(p);
                    }
                }
            }
            // Rückläufige Commit-Daten dürfen nichts ändern.
            let date = format!("2026-{:02}-01T00:00:00Z", 1 + rnd(12));
            let t = repo.files(&[("f", "100644", format!("{g}-{i}").as_bytes())]);
            let refs: Vec<&str> = ps.iter().map(String::as_str).collect();
            let c = repo.commit_at(&t, &refs, &date, &format!("{g}-{i}"));
            parents.insert(c.clone(), ps);
            ids.push(c);
        }
        let new = ids[rnd(n as u64) as usize].clone();
        let old = (rnd(3) > 0).then(|| ids[rnd(n as u64) as usize].clone());
        let reach = |from: &str| {
            let mut seen = BTreeSet::new();
            let mut stack = vec![from.to_owned()];
            while let Some(c) = stack.pop() {
                if seen.insert(c.clone()) {
                    stack.extend(parents[&c].iter().cloned());
                }
            }
            seen
        };
        let mut expected = reach(&new);
        if let Some(o) = &old {
            for c in reach(o) {
                expected.remove(&c);
            }
        }
        let got = with(
            &fx,
            &repo,
            &[],
            &AdapterLimits::default(),
            Budget::unlimited(),
            |a, _| {
                a.new_commits(old.as_deref().map(cid).as_ref(), &cid(&new), 100)
                    .unwrap()
            },
        );
        let got: BTreeSet<String> = got.commits.iter().map(|c| c.as_str().to_owned()).collect();
        assert_eq!(got, expected, "Graph {g}");
    }
}

#[test]
fn s2_26_ref_creation_follows_s1() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    let d = "2026-10-26T10:00:00Z";
    let (ids, _) = topology(&repo, &[("a", &[], d), ("b", &["a"], d), ("c", &["b"], d)]);
    // `a` lag vorher nur auf einem nicht erzwungenen Branch.
    repo.update_ref("refs/heads/feature", &ids["a"]);
    let all = with(
        &fx,
        &repo,
        &[],
        &AdapterLimits::default(),
        Budget::unlimited(),
        |a, _| a.new_commits(None, &cid(&ids["c"]), 3).unwrap(),
    );
    assert!(!all.truncated);
    assert_eq!(all.commits.len(), 3);
    assert!(all.commits.contains(&cid(&ids["a"])));
    let cut = with(
        &fx,
        &repo,
        &[],
        &AdapterLimits::default(),
        Budget::unlimited(),
        |a, _| a.new_commits(None, &cid(&ids["c"]), 2).unwrap(),
    );
    assert!(cut.truncated);
    // Unter Enforce auf einem erzwungenen Ref: abgelehnt.
    let updates = vec![update(None, &ids["c"], "refs/heads/main")];
    let policy = Policy {
        mode: Mode::Enforce,
        enforce_refs: vec!["refs/heads/main".to_owned()],
        max_commits_per_push: 2,
        ..Policy::default()
    };
    let out = with(
        &fx,
        &repo,
        &updates,
        &AdapterLimits::default(),
        Budget::unlimited(),
        |a, i| pre_receive(&ctx(updates.clone()), &policy, a, i),
    );
    match out.decision {
        minds_witness_core::Decision::Reject { message, .. } => {
            assert!(message.contains("mehr als 2 neue Commits"), "{message}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn s2_27_changed_paths() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    let root = repo.commit_files(
        &[
            ("keep.rs", "100644", b"k"),
            ("mod.rs", "100644", b"m0"),
            ("del.rs", "100644", b"d"),
            ("old.rs", "100644", b"r"),
            ("run.sh", "100644", b"#!"),
        ],
        &[],
    );
    assert_eq!(
        changed(&fx, &repo, &root),
        ["del.rs", "keep.rs", "mod.rs", "old.rs", "run.sh"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
    let c = repo.commit_files(
        &[
            ("keep.rs", "100644", b"k"),
            ("mod.rs", "100644", b"m1"),
            ("new.rs", "100644", b"r"),
            ("run.sh", "100755", b"#!"),
            ("ä/ö.rs", "100644", b"u"),
            ("🦀.rs", "100644", b"e"),
        ],
        &[&root],
    );
    assert_eq!(
        changed(&fx, &repo, &c),
        [
            "del.rs", "mod.rs", "new.rs", "old.rs", "run.sh", "ä/ö.rs", "🦀.rs"
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    );
    let t = repo.git(&["rev-parse", &format!("{c}^{{tree}}")]);
    let empty = repo.commit(&t, &[&c]);
    assert!(changed(&fx, &repo, &empty).is_empty());
}

#[test]
fn s2_28_changed_paths_merge() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    let base = repo.commit_files(
        &[
            ("a", "100644", b"0"),
            ("b", "100644", b"0"),
            ("x", "100644", b"0"),
        ],
        &[],
    );
    let l = repo.commit_files(
        &[
            ("a", "100644", b"L"),
            ("b", "100644", b"0"),
            ("x", "100644", b"0"),
        ],
        &[&base],
    );
    let r = repo.commit_files(
        &[
            ("a", "100644", b"0"),
            ("b", "100644", b"R"),
            ("x", "100644", b"0"),
        ],
        &[&base],
    );
    // Konfliktfrei: genau die Vereinigung beider Seiten.
    let clean = repo.commit_files(
        &[
            ("a", "100644", b"L"),
            ("b", "100644", b"R"),
            ("x", "100644", b"0"),
        ],
        &[&l, &r],
    );
    assert!(changed(&fx, &repo, &clean).is_empty());
    // Konfliktauflösung und eigene Datei.
    let own = repo.commit_files(
        &[
            ("a", "100644", b"merged"),
            ("b", "100644", b"R"),
            ("x", "100644", b"0"),
            ("y", "100644", b"own"),
        ],
        &[&l, &r],
    );
    assert_eq!(
        changed(&fx, &repo, &own),
        BTreeSet::from(["a".to_owned(), "y".to_owned()])
    );
    // Reiner Moduswechsel im Merge.
    let mode = repo.commit_files(
        &[
            ("a", "100644", b"L"),
            ("b", "100644", b"R"),
            ("x", "100755", b"0"),
        ],
        &[&l, &r],
    );
    assert_eq!(changed(&fx, &repo, &mode), BTreeSet::from(["x".to_owned()]));
    // Octopus.
    let o = repo.commit_files(
        &[
            ("a", "100644", b"0"),
            ("b", "100644", b"0"),
            ("x", "100644", b"O"),
        ],
        &[&base],
    );
    let oct = repo.commit_files(
        &[
            ("a", "100644", b"L"),
            ("b", "100644", b"R"),
            ("x", "100644", b"O"),
            ("z", "100644", b"z"),
        ],
        &[&l, &r, &o],
    );
    assert_eq!(changed(&fx, &repo, &oct), BTreeSet::from(["z".to_owned()]));
}

#[test]
fn s2_29_special_entries() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    let sub = repo.commit_files(&[("s", "100644", b"s")], &[]);
    let c = repo.files_bytes(&[
        (b"link", "120000", b"target"),
        (b"mod", "160000", sub.as_bytes()),
        (b"bad\xff1", "100644", b"1"),
        (b"bad\xfe2", "100644", b"2"),
    ]);
    let c = repo.commit(&c, &[]);
    let got = changed(&fx, &repo, &c);
    assert!(got.contains("link") && got.contains("mod"));
    let non_utf8: Vec<&String> = got.iter().filter(|p| p.starts_with("\0nonutf8:")).collect();
    assert_eq!(non_utf8.len(), 2);
    assert_ne!(non_utf8[0], non_utf8[1]);
}

#[test]
fn s2_30_malformed_trees_rejected() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    let good = repo.blob(b"good");
    let evil = repo.blob(b"evil");
    let sub = repo.tree(&[("100644", b"x", &good)]);
    for entries in [
        vec![
            ("100644", b"f".as_slice(), good.as_str()),
            ("100644", b"f", evil.as_str()),
        ],
        vec![
            ("100644", b"f".as_slice(), evil.as_str()),
            ("100644", b"f", good.as_str()),
        ],
        vec![
            ("100644", b"f".as_slice(), good.as_str()),
            ("40000", b"f", sub.as_str()),
        ],
        vec![
            ("100644", b"b".as_slice(), good.as_str()),
            ("100644", b"a", good.as_str()),
        ],
        vec![("100644", b".git".as_slice(), good.as_str())],
        vec![("100644", b"..".as_slice(), good.as_str())],
        vec![("100664", b"f".as_slice(), good.as_str())],
    ] {
        let t = repo.raw_tree(&entries);
        let c = repo.commit(&t, &[]);
        let err = with(
            &fx,
            &repo,
            &[],
            &AdapterLimits::default(),
            Budget::unlimited(),
            |a, _| a.changed_paths(&cid(&c)).unwrap_err(),
        );
        assert!(
            matches!(&err, ObjectError::Internal(m) if m.contains("malformed tree")),
            "{err:?}"
        );
    }
}

#[test]
fn s2_31_replace_refs_ignored() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    let real = repo.blob(b"real");
    let fake = repo.blob(b"fake");
    repo.update_ref(&format!("refs/replace/{real}"), &fake);
    repo.git(&["config", "core.useReplaceRefs", "true"]);
    let got = with(
        &fx,
        &repo,
        &[],
        &AdapterLimits::default(),
        Budget::unlimited(),
        |a, _| minds_witness_git::view::EvidenceView::blob(a, &real, 100).unwrap(),
    );
    assert_eq!(got.unwrap(), b"real");
}

#[test]
fn s2_32_repo_preconditions() {
    let fx = GitTestFixture::new();
    let open_err = |repo: &Repo, namespace: bool| {
        ServerRepo::open_for_receive(
            &ReceiveEnv {
                git_dir: repo.git_dir.clone(),
                quarantine: None,
                namespace,
            },
            Limits::default(),
        )
        .is_err()
    };
    let repo = fx.server_named("ns.git");
    assert!(open_err(&repo, true), "GIT_NAMESPACE");
    let sha = fx.server_named("sha256-base.git");
    let sha_path = fx.dir.path().join("sha256.git");
    std::fs::create_dir_all(&sha_path).unwrap();
    sha.git(&[
        "init",
        "-q",
        "--bare",
        "--object-format=sha256",
        sha_path.to_str().unwrap(),
    ]);
    let sha = Repo {
        path: sha_path.clone(),
        git_dir: sha_path,
        ..sha.clone()
    };
    assert!(open_err(&sha, false), "SHA-256");
    let rt = fx.server_named("reftable.git");
    rt.git(&["config", "core.repositoryformatversion", "1"]);
    rt.git(&["config", "extensions.refStorage", "reftable"]);
    assert!(open_err(&rt, false), "reftable");
    let sh = fx.server_named("shallow.git");
    let c = sh.commit_files(&[("a", "100644", b"a")], &[]);
    std::fs::write(sh.git_dir.join("shallow"), format!("{c}\n")).unwrap();
    assert!(open_err(&sh, false), "shallow");
    let ic = fx.server_named("ignorecase.git");
    ic.git(&["config", "core.ignoreCase", "true"]);
    assert!(open_err(&ic, false), "core.ignoreCase");
    let ok = fx.server_named("ok.git");
    assert!(!open_err(&ok, false));
}

#[test]
fn s2_33_tampered_object_and_limits() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    // Verfälschtes loses Objekt.
    let real = repo.blob(b"original");
    let other = repo.blob(b"tampered");
    let path = |id: &str| repo.git_dir.join("objects").join(&id[..2]).join(&id[2..]);
    let bytes = std::fs::read(path(&other)).unwrap();
    let target = path(&real);
    let mut perm = std::fs::metadata(&target).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&target, perm).unwrap();
    std::fs::write(&target, bytes).unwrap();
    let t = repo.tree(&[("100644", b"f", &real)]);
    let c = repo.commit(&t, &[]);
    let r = with(
        &fx,
        &repo,
        &[],
        &AdapterLimits::default(),
        Budget::unlimited(),
        |a, _| minds_witness_git::view::EvidenceView::blob(a, &real, 100),
    );
    assert!(r.is_err());
    let _ = c;

    let internal =
        |limits: AdapterLimits,
         budget: Budget,
         f: &dyn Fn(&GitObjectAccess<'_>) -> Result<(), ObjectError>| {
            with(&fx, &repo, &[], &limits, budget, |a, _| f(a))
        };
    let base = repo.commit_files(&[("d/e/f", "100644", b"x"), ("g", "100644", b"y")], &[]);
    let second = repo.commit_files(
        &[("d/e/f", "100644", b"x2"), ("g", "100644", b"y2")],
        &[&base],
    );
    let p1 = repo.commit_files(&[("p", "100644", b"1")], &[]);
    let p2 = repo.commit_files(&[("p", "100644", b"2")], &[]);
    let p3 = repo.commit_files(&[("p", "100644", b"3")], &[]);
    let octo = repo.commit_files(&[("p", "100644", b"4")], &[&p1, &p2, &p3]);
    // max_object_bytes
    let r = internal(
        AdapterLimits {
            max_object_bytes: 10,
            ..Default::default()
        },
        Budget::unlimited(),
        &|a| a.commit(&cid(&second)).map(|_| ()),
    );
    assert!(matches!(r, Err(ObjectError::Internal(_))), "{r:?}");
    // max_parents
    let r = internal(
        AdapterLimits {
            max_parents: 2,
            ..Default::default()
        },
        Budget::unlimited(),
        &|a| a.commit(&cid(&octo)).map(|_| ()),
    );
    assert!(matches!(r, Err(ObjectError::Internal(_))), "{r:?}");
    // max_tree_depth
    let r = internal(
        AdapterLimits {
            max_tree_depth: 1,
            ..Default::default()
        },
        Budget::unlimited(),
        &|a| a.changed_paths(&cid(&second)).map(|_| ()),
    );
    assert!(matches!(r, Err(ObjectError::Internal(_))), "{r:?}");
    // max_changed_paths
    let r = internal(
        AdapterLimits {
            max_changed_paths: 1,
            ..Default::default()
        },
        Budget::unlimited(),
        &|a| a.changed_paths(&cid(&second)).map(|_| ()),
    );
    assert!(matches!(r, Err(ObjectError::Internal(_))), "{r:?}");
    // max_total_commits_per_push
    let r = internal(
        AdapterLimits {
            max_total_commits_per_push: 1,
            ..Default::default()
        },
        Budget::unlimited(),
        &|a| a.new_commits(None, &cid(&second), 10).map(|_| ()),
    );
    assert!(matches!(r, Err(ObjectError::Internal(_))), "{r:?}");
    // deadline
    let past = Budget::new(u32::MAX, u64::MAX, Some(std::time::Instant::now()));
    let r = internal(AdapterLimits::default(), past, &|a| {
        a.commit(&cid(&second)).map(|_| ())
    });
    assert!(matches!(r, Err(ObjectError::Internal(_))), "{r:?}");

    // Evidence-Grenzen über den Inspektor.
    let e = Evidence {
        repo: &repo,
        key: &fx.key,
        public: &fx.public,
    };
    let (seal, ps) = e.witness_fs(&observations(&[("g", Obs::Content(b"y2"))]));
    let (_, bp) = e.binding(&second, &[seal]);
    for p in ps.iter().chain([&bp]) {
        repo.update_ref(&p.name, &p.target);
    }
    let inspect = |limits: AdapterLimits, budget: Budget| {
        with(&fx, &repo, &[], &limits, budget, |a, i| {
            let blob = a.seal_for_commit(&cid(&second)).unwrap().unwrap();
            i.inspect(
                &blob,
                &SealExpectation {
                    repo: PROJECT.to_owned(),
                    commit: cid(&second),
                },
            )
        })
    };
    assert_eq!(
        inspect(AdapterLimits::default(), Budget::unlimited())
            .unwrap()
            .touched_paths,
        BTreeSet::from(["g".to_owned()])
    );
    // max_ssh_keygen_processes, max_evidence_bytes_per_push → Internal.
    assert!(matches!(
        inspect(AdapterLimits::default(), Budget::new(0, u64::MAX, None)),
        Err(SealError::Internal(_))
    ));
    assert!(matches!(
        inspect(AdapterLimits::default(), Budget::new(u32::MAX, 10, None)),
        Err(SealError::Internal(_))
    ));
    // max_binding_bytes, max_seals_per_binding → Kandidat unlesbar.
    assert!(matches!(
        inspect(
            AdapterLimits {
                max_binding_bytes: 10,
                ..Default::default()
            },
            Budget::unlimited()
        ),
        Err(SealError::Malformed(_))
    ));
    assert!(matches!(
        inspect(
            AdapterLimits {
                max_seals_per_binding: 0,
                ..Default::default()
            },
            Budget::unlimited()
        ),
        Err(SealError::Malformed(_))
    ));
    // max_observations_bytes → nur dieses Objekt verworfen.
    let s = inspect(
        AdapterLimits {
            max_observations_bytes: 10,
            ..Default::default()
        },
        Budget::unlimited(),
    )
    .unwrap();
    assert!(s.touched_paths.is_empty());
}

#[test]
fn s2_34_binding_discovery() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    let e = Evidence {
        repo: &repo,
        key: &fx.key,
        public: &fx.public,
    };
    let base = repo.commit_files(&[("a", "100644", b"0"), ("b", "100644", b"0")], &[]);
    let c = repo.commit_files(&[("a", "100644", b"1"), ("b", "100644", b"1")], &[&base]);
    let (sa, pa) = e.witness_fs(&observations(&[("a", Obs::Content(b"1"))]));
    let (sb, pb) = e.witness_fs(&observations(&[("b", Obs::Content(b"1"))]));
    for p in pa.iter().chain(pb.iter()) {
        repo.update_ref(&p.name, &p.target);
    }
    // Altbestand: Müll neben gültigen Bindungen.
    let junk_blob = repo.blob(b"junk");
    let junk = repo.commit(&repo.tree(&[("100644", b"binding", &junk_blob)]), &[]);
    repo.update_ref(
        &format!("refs/minds/bindings/{c}/{}", "0".repeat(64)),
        &junk,
    );
    let (_, other_commit) = e.binding(&base, std::slice::from_ref(&sa));
    repo.update_ref(
        &format!("refs/minds/bindings/{c}/{}", "1".repeat(64)),
        &other_commit.target,
    );
    let (_, foreign_project) = e.binding_with(
        &c,
        "git.test/project-7",
        std::slice::from_ref(&sa),
        Some((&fx.key, &fx.public)),
    );
    repo.update_ref(&foreign_project.name, &foreign_project.target);
    let (fk, fp) = keypair(fx.dir.path(), "foreign");
    let (_, copied_key) = e.binding_with(&c, PROJECT, &[sb.clone(), sa.clone()], Some((&fk, &fp)));
    repo.update_ref(&copied_key.name, &copied_key.target);
    let (_, valid_a) = e.binding(&c, std::slice::from_ref(&sa));
    repo.update_ref(&valid_a.name, &valid_a.target);
    let expected = |a: &GitObjectAccess<'_>, i: &GitSealInspector<'_, GitObjectAccess<'_>>| {
        let blob = a.seal_for_commit(&cid(&c)).unwrap().unwrap();
        i.inspect(
            &blob,
            &SealExpectation {
                repo: PROJECT.to_owned(),
                commit: cid(&c),
            },
        )
    };
    let s = with(
        &fx,
        &repo,
        &[],
        &AdapterLimits::default(),
        Budget::unlimited(),
        expected,
    )
    .unwrap();
    assert_eq!(s.touched_paths, BTreeSet::from(["a".to_owned()]));
    // Neue Bindung aus dem Push wird mit dem Vorzustand vereinigt.
    let (_, valid_b) = e.binding(&c, std::slice::from_ref(&sb));
    let updates = vec![update(None, &valid_b.target, &valid_b.name)];
    let s = with(
        &fx,
        &repo,
        &updates,
        &AdapterLimits::default(),
        Budget::unlimited(),
        expected,
    )
    .unwrap();
    assert_eq!(
        s.touched_paths,
        BTreeSet::from(["a".to_owned(), "b".to_owned()])
    );
    // Mehr Kandidaten als erlaubt: anerkannte Schlüssel zuerst (drei
    // davon), Müll und fremder Schlüssel fallen heraus, kein Internal.
    let limits = AdapterLimits {
        max_binding_candidates: 3,
        ..Default::default()
    };
    let (s, picked) = with(&fx, &repo, &[], &limits, Budget::unlimited(), |a, i| {
        let blob = a.seal_for_commit(&cid(&c)).unwrap().unwrap();
        let picked = minds_witness_git::candidates::parse(&blob.bytes, &c).unwrap();
        let s = i.inspect(
            &blob,
            &SealExpectation {
                repo: PROJECT.to_owned(),
                commit: cid(&c),
            },
        );
        (s, picked)
    });
    assert_eq!(s.unwrap().touched_paths, BTreeSet::from(["a".to_owned()]));
    assert_eq!(picked.len(), 3);
    assert!(
        !picked
            .iter()
            .any(|p| p.ref_name.ends_with(&"0".repeat(64)) || p.ref_name == copied_key.name)
    );
    // Reihenfolge nach Ref-Name in der Kandidatenliste.
    let names = with(
        &fx,
        &repo,
        &updates,
        &AdapterLimits::default(),
        Budget::unlimited(),
        |a, _| {
            let blob = a.seal_for_commit(&cid(&c)).unwrap().unwrap();
            minds_witness_git::candidates::parse(&blob.bytes, &c).unwrap()
        },
    );
    assert!(names.windows(2).all(|w| w[0].ref_name < w[1].ref_name));
    assert_eq!(names.len(), 6);
}

#[test]
fn s2_42_effective_fsck_config() {
    let fx = GitTestFixture::new();
    let cases: Vec<(&[(&str, &str)], bool)> = vec![
        (&[("receive.fsckObjects", "true")], true),
        (&[("transfer.fsckObjects", "true")], true),
        (
            &[
                ("receive.fsckObjects", "false"),
                ("transfer.fsckObjects", "true"),
            ],
            false,
        ),
        (&[], false),
        (
            &[
                ("receive.fsckObjects", "true"),
                ("receive.fsck.duplicateEntries", "ignore"),
            ],
            false,
        ),
        (
            &[
                ("receive.fsckObjects", "true"),
                ("receive.fsck.skipList", "/tmp/skip"),
            ],
            false,
        ),
        (
            &[
                ("receive.fsckObjects", "true"),
                ("fsck.duplicateEntries", "ignore"),
            ],
            true,
        ),
        (
            &[
                ("receive.fsckObjects", "true"),
                ("receive.fsck.duplicateEntries", "warn"),
            ],
            false,
        ),
        (
            &[
                ("receive.fsckObjects", "true"),
                ("receive.fsck.duplicateEntries", "error"),
            ],
            true,
        ),
    ];
    for (i, (settings, expected)) in cases.into_iter().enumerate() {
        let repo = fx.server_named(&format!("fsck{i}.git"));
        repo.git(&["config", "--unset", "receive.fsckObjects"]);
        for (k, v) in settings {
            repo.git(&["config", k, v]);
        }
        let got = fsck::effective(&git_program(), &repo.git_dir).unwrap();
        assert_eq!(got, expected, "{settings:?}");
    }
    // Systemkonfiguration true, Repository false: der letzte Wert gewinnt.
    let repo = fx.server_named("fsck-sys.git");
    let system = fx.dir.path().join("system.gitconfig");
    std::fs::write(&system, "[receive]\n\tfsckObjects = true\n").unwrap();
    repo.git(&["config", "receive.fsckObjects", "false"]);
    let sys = system.to_str().unwrap();
    let g = git_program();
    assert!(
        !fsck::effective_with(
            &g,
            &repo.git_dir,
            &[("GIT_CONFIG_SYSTEM", sys), ("GIT_CONFIG_NOSYSTEM", "0")]
        )
        .unwrap()
    );
    // `receive.fsckObjects=false` über GIT_CONFIG_COUNT (wie `-c` von
    // receive-pack) schlägt die Repository-Konfiguration.
    let on = fx.server_named("fsck-count.git");
    assert!(fsck::effective(&g, &on.git_dir).unwrap());
    assert!(
        !fsck::effective_with(
            &g,
            &on.git_dir,
            &[
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "receive.fsckObjects"),
                ("GIT_CONFIG_VALUE_0", "false")
            ]
        )
        .unwrap()
    );
    // Ein ungültiger Wert ist ein Fehler, kein „aus“.
    let bad = fx.server_named("fsck-bad.git");
    bad.git(&["config", "receive.fsckObjects", "vielleicht"]);
    assert!(fsck::effective(&g, &bad.git_dir).is_err());
}
