//! Spec S3-MVP §6.1 (Commit 5): `bind` im Witness — s3_11 bis s3_15.
//!
//! Die Evidence wird hier von Hand abgelegt (Observations eines nachweislich
//! getrennten Witness, ein signiertes `witness-fs/v1`-Seal, die Ledger-Zeile):
//! Im selben Prozess lässt sich keine echte Trennung herstellen, und nur
//! getrennte Observations rechnet der Server an.

use super::*;
use minds_capture::witness_proto::{self, Frame};
use minds_core::evidence::{SCOPE_WITNESS_FS_V1, Seal, SealOutcome};
use minds_core::observation::{
    Isolation, Observation, ObservationReason, Observations, WitnessFacts,
};
use minds_witness_git::binding::CommitBinding;

const PROJECT: &str = "git.test/project-42";

/// Ein Witness mit Schlüssel, optional mit festgehaltenem Projekt.
fn pinned(project: Option<&str>) -> Fixture {
    let f = Fixture::new();
    f.keygen();
    if project.is_some() {
        pin(&f, project).unwrap();
    }
    f
}

fn pin(f: &Fixture, project: Option<&str>) -> Fallible<String> {
    init(
        &f.home,
        &InitRequest {
            repo: f.root.to_str().unwrap(),
            mapping: None,
            profile: None,
            socket_group: None,
            child_repo: None,
            policy_rev: None,
            intent_key: None,
            replace_intent_key: false,
            project,
        },
    )
}

/// Schreibt Dateien (`None` löscht) und committet; liefert die Commit-Id.
fn commit(f: &Fixture, files: &[(&str, Option<&[u8]>)], message: &str) -> String {
    for (path, content) in files {
        let full = f.root.join(path);
        match content {
            Some(bytes) => {
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(&full, bytes).unwrap();
                git(&f.root, &["add", "--", path]);
            }
            None => {
                git(&f.root, &["rm", "-q", "--", path]);
            }
        }
    }
    git(&f.root, &["commit", "-qm", message]);
    git(&f.root, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Legt Observations (getrennt oder nicht), ein signiertes
/// `witness-fs/v1`-Seal und die Ledger-Zeile ab; liefert die Seal-Id.
fn evidence(f: &Fixture, entries: &[(&str, Option<&[u8]>)], separated: bool) -> String {
    static SALT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);
    let config = load(&f.home).unwrap();
    let repo = config.pinned_repo().unwrap();
    let store = config.open_pinned_store(&repo).unwrap();
    let list = entries
        .iter()
        .enumerate()
        .map(|(i, (path, content))| Observation {
            seq: i as u64 + 1,
            at: format!("2026-10-27T10:00:{:02}Z", i + 1),
            path: (*path).to_owned(),
            content: content
                .map(|bytes| minds_core::ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())),
            reason: content.is_none().then_some(ObservationReason::Deleted),
        })
        .collect();
    let facts = WitnessFacts {
        profile: if separated { "user" } else { "local" }.to_owned(),
        isolation: Isolation {
            witness_uid: 1001,
            agent_uids: if separated { vec![1000] } else { vec![1001] },
            home_private: true,
            binary_protected: true,
        },
        spec: None,
    };
    let obs = Observations::new("2026-10-27T10:00:00Z", list).with_witness(facts);
    let redacted = config
        .policy()
        .pipeline()
        .unwrap()
        .redact_observations(obs)
        .unwrap();
    let obs_id = store.put_observations(&redacted).unwrap();
    let salt = SALT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let text = Seal {
        root: minds_core::ContentHash::from_bytes([salt; 32]),
        agent: "witness".into(),
        scope: SCOPE_WITNESS_FS_V1.into(),
        first_seq: 1,
        last_seq: entries.len() as u64,
        events: entries.len() as u64,
        gaps: 0,
        pre_chain: 0,
        outcome: SealOutcome::ObservationsStored {
            observations: obs_id.to_string(),
        },
        previous: None,
        last_event_at: "2026-10-27T10:00:30Z".into(),
    }
    .to_text()
    .unwrap();
    let id = store.put_seal(&text).unwrap();
    let sig = minds_attest::ssh_sign_ns(
        &text,
        &f.home.join("key/witness_ed25519"),
        minds_attest::NS_WITNESS,
    )
    .unwrap();
    store.put_seal_signature(&id, &sig).unwrap();
    let mut ledger = OpenOptions::new()
        .append(true)
        .open(f.home.join("ledger"))
        .unwrap();
    writeln!(ledger, "{id} {SCOPE_WITNESS_FS_V1} 2026-10-27T10:00:30Z").unwrap();
    id.to_string()
}

fn run(f: &Fixture, c: &str, expected: &str, dry_run: bool) -> Result<String, String> {
    bind::bind(&f.home, &load(&f.home).unwrap(), c, expected, dry_run).map_err(|e| e.to_string())
}

/// Die Bindungen unter `refs/minds/bindings/<c>/`.
fn bindings(f: &Fixture, c: &str) -> Vec<CommitBinding> {
    let refs = git(
        &f.root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/minds/bindings/{c}/"),
        ],
    );
    refs.lines()
        .map(|name| {
            let bytes = Command::new("git")
                .arg("-C")
                .arg(&f.root)
                .args(["cat-file", "blob", &format!("{name}:binding")])
                .output()
                .unwrap()
                .stdout;
            let b = CommitBinding::parse(&bytes, 32).unwrap();
            assert_eq!(name, b.ref_name(), "Ref-Name = Inhalt");
            b
        })
        .collect()
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

#[test]
fn s3_11_bind_selects_content_bound_seals() {
    let f = pinned(Some(PROJECT));
    let c = commit(
        &f,
        &[("src/a.rs", Some(b"x1")), ("src/b.rs", Some(b"b1"))],
        "change a and b",
    );
    let s_a = evidence(&f, &[("src/a.rs", Some(b"x1"))], true);
    let s_b = evidence(&f, &[("src/b.rs", Some(b"b1"))], true);
    // Gleicher Pfad, anderer Inhalt; und ein nicht getrennter Witness.
    let s_other = evidence(&f, &[("src/b.rs", Some(b"b-other"))], true);
    let s_local = evidence(&f, &[("src/a.rs", Some(b"x1"))], false);

    // Trockenlauf: Auswahl, nichts signiert, nichts abgelegt.
    assert_eq!(run(&f, &c, PROJECT, true).unwrap(), "selected 2 2/2");
    assert!(bindings(&f, &c).is_empty());

    let status = run(&f, &c, PROJECT, false).unwrap();
    let found = bindings(&f, &c);
    assert_eq!(found.len(), 1);
    let b = &found[0];
    assert_eq!(status, format!("bound {} 2/2", b.id()));
    assert_eq!(b.seals().to_vec(), sorted(vec![s_a, s_b]));
    assert!(!b.seals().contains(&s_other) && !b.seals().contains(&s_local));
    assert_eq!((b.commit(), b.project()), (c.as_str(), PROJECT));
    // Signiert mit dem eigenen Schlüssel, unter `minds-witness`.
    let key = fs::read_to_string(f.home.join("key/witness_ed25519.pub")).unwrap();
    assert!(key.starts_with(b.key()), "{key} / {}", b.key());

    // Nur fremder Inhalt: keine Evidence, keine Bindung.
    let c2 = commit(&f, &[("src/b.rs", Some(b"b2"))], "change b again");
    assert_eq!(run(&f, &c2, PROJECT, false).unwrap(), "no-evidence 0/1");
    assert!(bindings(&f, &c2).is_empty());
}

#[test]
fn s3_12_bind_after_amend_and_rebase() {
    let f = pinned(Some(PROJECT));
    let c = commit(&f, &[("src/a.rs", Some(b"x1"))], "first");
    evidence(&f, &[("src/a.rs", Some(b"x1"))], true);
    // Trailer-Amend: ein neuer Commit mit demselben Inhalt.
    git(
        &f.root,
        &[
            "commit",
            "-q",
            "--amend",
            "-m",
            "first\n\nMinds-Session-Id: x",
        ],
    );
    let amended = git(&f.root, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_ne!(amended, c);
    assert!(
        run(&f, &amended, PROJECT, false)
            .unwrap()
            .starts_with("bound ")
    );
    assert_eq!(bindings(&f, &amended)[0].commit(), amended);
    assert!(bindings(&f, &c).is_empty(), "alter Commit unberührt");

    // Rebase auf eine neue Basis: wieder ein neuer Commit, Inhalt gleich.
    git(&f.root, &["checkout", "-q", "-b", "side", "HEAD~1"]);
    let base = commit(&f, &[("other.txt", Some(b"o"))], "other");
    git(&f.root, &["checkout", "-q", "-"]);
    git(&f.root, &["rebase", "-q", &base]);
    let rebased = git(&f.root, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_ne!(rebased, amended);
    assert!(
        run(&f, &rebased, PROJECT, false)
            .unwrap()
            .starts_with("bound ")
    );
    assert_eq!(bindings(&f, &rebased).len(), 1);
    assert_eq!(bindings(&f, &amended).len(), 1, "frühere Bindung bleibt");
}

#[test]
fn s3_13_bind_idempotent_and_capped() {
    let f = pinned(Some(PROJECT));
    let files: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| (format!("f{i}"), format!("v{i}").into_bytes()))
        .collect();
    let refs: Vec<(&str, Option<&[u8]>)> = files
        .iter()
        .map(|(p, b)| (p.as_str(), Some(b.as_slice())))
        .collect();
    let c = commit(&f, &refs, "six files");
    evidence(&f, &refs[..1], true);
    let first = run(&f, &c, PROJECT, false).unwrap();
    assert!(
        first.starts_with("bound ") && first.ends_with(" 1/6"),
        "{first}"
    );
    // Ohne neue Seals: dieselbe Bindung, keine neue.
    let again = run(&f, &c, PROJECT, false).unwrap();
    assert_eq!(again, first.replacen("bound", "unchanged", 1));
    assert_eq!(bindings(&f, &c).len(), 1);
    // Dieselben Bytes erneut versiegelt (anderes Seal, gleiche Abdeckung):
    // keine neue Bindung, wie auch immer die Ids liegen.
    for _ in 0..3 {
        evidence(&f, &refs[..1], true);
        assert_eq!(run(&f, &c, PROJECT, false).unwrap(), again);
    }
    assert_eq!(bindings(&f, &c).len(), 1);
    // Jede neue Evidence erweitert die Auswahl: eine neue Bindung, bis vier.
    for n in 2..=4 {
        evidence(&f, &refs[n - 1..n], true);
        let status = run(&f, &c, PROJECT, false).unwrap();
        assert!(status.ends_with(&format!(" {n}/6")), "{status}");
        assert_eq!(bindings(&f, &c).len(), n);
    }
    // Die fünfte: abgelehnt, nichts angelegt.
    evidence(&f, &refs[4..5], true);
    assert_eq!(run(&f, &c, PROJECT, false).unwrap_err(), bind::BINDING_CAP);
    // Auch der Trockenlauf sagt es, statt eine Auswahl zu versprechen.
    assert_eq!(run(&f, &c, PROJECT, true).unwrap_err(), bind::BINDING_CAP);
    assert_eq!(bindings(&f, &c).len(), 4);
    // Höchstens 32 Seals je Bindung, auch wenn mehr beitrügen.
    let many: Vec<(String, Vec<u8>)> = (0..33)
        .map(|i| (format!("m/{i}"), format!("m{i}").into_bytes()))
        .collect();
    let many_refs: Vec<(&str, Option<&[u8]>)> = many
        .iter()
        .map(|(p, b)| (p.as_str(), Some(b.as_slice())))
        .collect();
    let c2 = commit(&f, &many_refs, "thirty-three files");
    for entry in &many_refs {
        evidence(&f, std::slice::from_ref(entry), true);
    }
    let status = run(&f, &c2, PROJECT, false).unwrap();
    assert!(status.ends_with(" 32/33"), "{status}");
    assert_eq!(bindings(&f, &c2)[0].seals().len(), 32);
}

/// Die Vorschau des Witness ist die Abdeckung des Server-Inspektors für die
/// abgelegte Bindung (mit dem Witness-Schlüssel in der Signer-Datei).
#[test]
fn s3_14_bind_preview_matches_server() {
    let f = pinned(Some(PROJECT));
    commit(
        &f,
        &[("keep.rs", Some(b"k")), ("gone.rs", Some(b"g"))],
        "base",
    );
    let c = commit(
        &f,
        &[
            ("src/a.rs", Some(b"x1")),
            ("src/b.rs", Some(b"b1")),
            ("gone.rs", None),
        ],
        "change",
    );
    evidence(
        &f,
        &[
            ("src/a.rs", Some(b"x1")),
            ("gone.rs", None),
            ("keep.rs", Some(b"k")),
        ],
        true,
    );
    evidence(&f, &[("src/b.rs", Some(b"wrong"))], true);
    let status = run(&f, &c, PROJECT, false).unwrap();
    assert!(status.ends_with(" 2/3"), "{status}");

    let touched = server_touched(&f, &f.root.join(".git"), &c);
    let touched: Vec<&str> = touched.iter().map(String::as_str).collect();
    assert_eq!(touched, ["gone.rs", "src/a.rs"]);
    assert_eq!(touched.len(), 2, "Vorschau 2/3 = Server");
}

/// Was der Server-Inspektor für `c` im Repository `git_dir` anrechnet — mit
/// dem Schlüssel dieses Witness in der Signer-Datei.
fn server_touched(f: &Fixture, git_dir: &Path, c: &str) -> std::collections::BTreeSet<String> {
    use minds_witness_core::{CommitId, SealExpectation, SealInspector};
    use minds_witness_git::budget::Budget;
    use minds_witness_git::candidates::{self, Candidate};
    use minds_witness_git::inspector::{GitSealInspector, InspectorConfig};
    use minds_witness_git::trust::{TrustConfig, Verifier};

    let signers = super::super::super::existing_signers_line(&f.home)
        .unwrap()
        .unwrap();
    let principal = signers.split_whitespace().next().unwrap().to_owned();
    let program = intent::witness_keygen().expect("ssh-keygen");
    let trust = TrustConfig::from_text(
        &format!("{signers}\n"),
        &program,
        &[principal],
        false,
        std::time::Duration::from_secs(10),
    )
    .unwrap();
    let server = minds_git::quarantine::ServerRepo::open_for_receive(
        &minds_git::quarantine::ReceiveEnv {
            git_dir: git_dir.to_path_buf(),
            quarantine: None,
            namespace: false,
        },
        minds_git::quarantine::Limits::default(),
    )
    .unwrap();
    let view = minds_witness_git::access::RepoView::new(&server);
    let budget = Budget::unlimited();
    let verifier = Verifier::new(&trust, &budget);
    let inspector = GitSealInspector::new(&view, &verifier, &budget, InspectorConfig::new(PROJECT));
    let list: Vec<Candidate> = server
        .prestate()
        .read_prefix(&candidates::prefix(c))
        .unwrap()
        .into_iter()
        .map(|(ref_name, target)| Candidate { ref_name, target })
        .collect();
    let summary = inspector
        .inspect(
            &candidates::to_blob(c, list),
            &SealExpectation {
                repo: PROJECT.to_owned(),
                commit: CommitId::parse(c).unwrap(),
            },
        )
        .unwrap();
    summary.touched_paths
}

#[test]
fn s3_15_bind_refuses_foreign_project() {
    // Ohne Pin: nichts.
    let f = pinned(None);
    let c = commit(&f, &[("src/a.rs", Some(b"x1"))], "first");
    evidence(&f, &[("src/a.rs", Some(b"x1"))], true);
    assert_eq!(
        run(&f, &c, PROJECT, false).unwrap_err(),
        bind::PROJECT_UNPINNED
    );
    // Pin genau einmal: gleich → unverändert, anders → Abbruch, ungültig →
    // Abbruch; der Pin bleibt.
    let out = pin(&f, Some(PROJECT)).unwrap();
    assert!(out.contains("project pinned"), "{out}");
    assert!(out.contains(&format!("pinned project: {PROJECT} (newly pinned")));
    let out = pin(&f, Some(PROJECT)).unwrap();
    assert!(out.contains("already initialized"), "{out}");
    let err = pin(&f, Some("git.test/project-7")).unwrap_err().to_string();
    assert!(err.contains("project"), "{err}");
    let err = pin(&f, Some("git.test/group/repo"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("--project must have the form"), "{err}");
    assert_eq!(load(&f.home).unwrap().project(), Some(PROJECT));
    // Die `.git/config` des Repos ändert den Pin nicht.
    git(&f.root, &["config", "minds.project", "evil.test/project-1"]);
    assert_eq!(load(&f.home).unwrap().project(), Some(PROJECT));

    // Abweichung: abgelehnt, bevor irgendetwas signiert wird — auch ohne
    // Schlüssel derselbe Grund.
    let key = f.home.join("key/witness_ed25519");
    fs::rename(&key, f.home.join("key.moved")).unwrap();
    assert_eq!(
        run(&f, &c, "evil.test/project-1", false).unwrap_err(),
        bind::PROJECT_MISMATCH
    );
    fs::rename(f.home.join("key.moved"), &key).unwrap();
    assert!(bindings(&f, &c).is_empty());

    // Über den Socket: fester Grund im `Nack`, der Schreiber läuft weiter.
    let mut writer = f.writer();
    let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut client = agent_client(ours);
    let frame = witness_proto::encode(&Frame::Bind {
        commit: c.clone(),
        expected_project: "evil.test/project-1".into(),
        dry_run: false,
        request_id: [7; 16],
    })
    .unwrap();
    theirs.write_all(&frame).unwrap();
    for _ in 0..4 {
        if !socket::step(&mut client, &mut writer).unwrap() {
            break;
        }
    }
    theirs
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut answer = vec![0u8; 4096];
    let n = theirs.read(&mut answer).unwrap();
    let (frame, _) = witness_proto::decode(&answer[..n]).unwrap();
    assert_eq!(
        frame,
        Frame::Nack {
            request_id: [7; 16],
            reason: bind::PROJECT_MISMATCH.into(),
        }
    );
    assert!(bindings(&f, &c).is_empty());
    // Dasselbe mit dem richtigen Projekt bindet.
    let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut client = agent_client(ours);
    let frame = witness_proto::encode(&Frame::Bind {
        commit: c.clone(),
        expected_project: PROJECT.into(),
        dry_run: false,
        request_id: [8; 16],
    })
    .unwrap();
    theirs.write_all(&frame).unwrap();
    for _ in 0..4 {
        if !socket::step(&mut client, &mut writer).unwrap() {
            break;
        }
    }
    let n = theirs.read(&mut answer).unwrap();
    match witness_proto::decode(&answer[..n]).unwrap().0 {
        Frame::Ack { request_id, status } => {
            assert_eq!(request_id, [8; 16]);
            assert!(status.starts_with("bound "), "{status}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(bindings(&f, &c).len(), 1);
}

#[test]
fn bind_requires_a_known_commit_and_the_in_repo_store() {
    let f = pinned(Some(PROJECT));
    assert_eq!(
        run(&f, &"1".repeat(40), PROJECT, false).unwrap_err(),
        bind::UNKNOWN_COMMIT
    );
    assert_eq!(
        run(&f, "HEAD", PROJECT, false).unwrap_err(),
        bind::UNKNOWN_COMMIT
    );
    // Ein Baum ist kein Commit.
    let tree = git(&f.root, &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_owned();
    assert_eq!(
        run(&f, &tree, PROJECT, false).unwrap_err(),
        bind::UNKNOWN_COMMIT
    );
}

/// Was unter `refs/minds/bindings/` liegt, aber nicht nachweislich eine
/// eigene, signierte Bindung unter ihrem Namen ist, zählt nie als „schon
/// gebunden“; belegt es den Namen der neuen Bindung, sagt `bind` das.
#[test]
fn bind_never_reports_a_planted_binding_as_its_own() {
    let f = pinned(Some(PROJECT));
    let c = commit(&f, &[("src/a.rs", Some(b"x1"))], "first");
    evidence(&f, &[("src/a.rs", Some(b"x1"))], true);
    let bound = run(&f, &c, PROJECT, false).unwrap();
    let name = bindings(&f, &c)[0].ref_name();
    let bytes = git(&f.root, &["cat-file", "blob", &format!("{name}:binding")]);
    // Eine unsignierte Kopie unter dem richtigen Namen.
    let unsigned = {
        let blob = Command::new("git")
            .arg("-C")
            .arg(&f.root)
            .args(["hash-object", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child.stdin.take().unwrap().write_all(bytes.as_bytes())?;
                child.wait_with_output()
            })
            .unwrap();
        let blob = String::from_utf8(blob.stdout).unwrap().trim().to_owned();
        let tree = Command::new("git")
            .arg("-C")
            .arg(&f.root)
            .args(["mktree"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(format!("100644 blob {blob}\tbinding\n").as_bytes())?;
                child.wait_with_output()
            })
            .unwrap();
        let tree = String::from_utf8(tree.stdout).unwrap().trim().to_owned();
        git(&f.root, &["commit-tree", &tree, "-m", "planted"])
            .trim()
            .to_owned()
    };
    git(&f.root, &["update-ref", &name, &unsigned]);
    let err = run(&f, &c, PROJECT, false).unwrap_err();
    assert_eq!(
        err,
        bind::REF_OCCUPIED,
        "nie „unchanged“ für eine unsignierte Kopie"
    );
    // Der echte Inhalt zurück: wieder die eigene Bindung.
    let real = bound.split(' ').nth(1).unwrap().to_owned();
    git(&f.root, &["update-ref", "-d", &name]);
    assert!(
        run(&f, &c, PROJECT, false)
            .unwrap()
            .starts_with(&format!("bound {real}"))
    );
    assert_eq!(
        run(&f, &c, PROJECT, false).unwrap(),
        bound.replacen("bound", "unchanged", 1)
    );
}

/// Spec S3 §6.2, der Durchstich von Commit 4 bis 6: `bind` legt eine neue
/// Bindung an, die Evidence-Übertragung schickt **genau** deren Objekte
/// (Observations, Seals, Bindung — in dieser Reihenfolge, nichts sonst),
/// und erst danach der Code; der Server-Inspektor findet beim Code-Push die
/// Evidence vor und rechnet dieselbe Abdeckung an wie die Vorschau.
#[test]
fn s3_19_bound_evidence_reaches_the_server_before_the_code() {
    let f = pinned(Some(PROJECT));
    let remote = f.root.parent().unwrap().join("remote.git");
    git(&f.root, &["init", "-q", "--bare", remote.to_str().unwrap()]);
    git(
        &f.root,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    let c = commit(
        &f,
        &[("src/a.rs", Some(b"x1")), ("src/b.rs", Some(b"y1"))],
        "change a and b",
    );
    let s_a = evidence(&f, &[("src/a.rs", Some(b"x1"))], true);
    // Ein eigener Seal, der zu diesem Commit nichts beiträgt: bleibt lokal.
    let s_other = evidence(&f, &[("src/other.rs", Some(b"o"))], true);
    let status = run(&f, &c, PROJECT, false).unwrap();
    assert!(
        status.starts_with("bound ") && status.ends_with(" 1/2"),
        "{status}"
    );
    let binding = bindings(&f, &c).remove(0);

    let view = crate::bind_cmd::open_view(&f.root.join(".git")).unwrap();
    let refs = crate::evidence_cmd::evidence_refs(&view, std::slice::from_ref(&c));
    let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(
        names[0].starts_with("refs/minds/observations/"),
        "{names:?}"
    );
    assert_eq!(names[1], format!("refs/minds/evidence/{}", &s_a[3..]));
    assert_eq!(names[2], binding.ref_name());
    assert!(!names.iter().any(|n| n.contains(&s_other[3..])));

    // Übertragen, dann noch einmal: nichts mehr zu tun.
    let sent = crate::evidence_cmd::transfer(&f.root, "origin", &refs);
    assert_eq!(sent, crate::evidence_cmd::Transfer::Sent(3));
    assert_eq!(
        crate::evidence_cmd::transfer(&f.root, "origin", &refs),
        crate::evidence_cmd::Transfer::Nothing
    );
    let on_remote = git(
        &remote,
        &["for-each-ref", "--format=%(refname)", "refs/minds/"],
    );
    let mut on_remote: Vec<&str> = on_remote.lines().collect();
    on_remote.sort_unstable();
    let mut wanted = names.clone();
    wanted.sort_unstable();
    assert_eq!(on_remote, wanted, "genau die nötigen Objekte");

    // Der Code danach; der Server rechnet an, was die Vorschau meldete.
    git(
        &f.root,
        &[
            "push",
            "-q",
            "--no-verify",
            "origin",
            "HEAD:refs/heads/main",
        ],
    );
    let touched = server_touched(&f, &remote, &c);
    assert_eq!(
        touched.into_iter().collect::<Vec<_>>(),
        vec!["src/a.rs".to_owned()]
    );
}
