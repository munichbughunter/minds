//! Spec S2 v0.4.1 §7.0 und §13: Push-Vorprüfung gegen temporäre
//! Repositories (s2_22). Die Vorprüfung kennt keinen Modus; dass sie in
//! beiden Modi ablehnt, prüft der Push-Test s2_40.

mod support;

use minds_core::evidence::{SCOPE_AGENT_HOOKS_V1, SealOutcome};
use minds_git::quarantine::{ReceiveEnv, ServerRepo};
use minds_witness_core::{CommitId, RefUpdate};
use minds_witness_git::budget::Budget;
use minds_witness_git::guard::{Guard, GuardError, parse_stdin};
use minds_witness_git::limits::AdapterLimits;
use minds_witness_git::trust::{TrustConfig, Verifier};
use support::*;

fn cid(s: &str) -> CommitId {
    CommitId::parse(s).unwrap()
}

fn null() -> CommitId {
    CommitId::null_of_len(40).unwrap()
}

fn add(p: &Pending) -> RefUpdate {
    RefUpdate {
        old: null(),
        new: cid(&p.target),
        ref_name: p.name.clone(),
    }
}

fn change(name: &str, old: &str, new: &str) -> RefUpdate {
    RefUpdate {
        old: cid(old),
        new: cid(new),
        ref_name: name.to_owned(),
    }
}

fn run(
    fx: &GitTestFixture,
    repo: &Repo,
    updates: &[RefUpdate],
    limits: &AdapterLimits,
    budget: Budget,
) -> Result<(), GuardError> {
    let server = ServerRepo::open_for_receive(
        &ReceiveEnv {
            git_dir: repo.git_dir.clone(),
            quarantine: None,
            namespace: false,
        },
        limits.repo_limits(),
    )
    .unwrap();
    let trust = TrustConfig::from_text(
        &fx.signers(),
        &ssh_keygen(),
        &[PRINCIPAL.to_owned()],
        true,
        std::time::Duration::from_secs(10),
    )
    .unwrap();
    let verifier = Verifier::new(&trust, &budget);
    Guard::new(&server, &verifier, &budget, &trust, limits, PROJECT).check(updates)
}

fn check(fx: &GitTestFixture, repo: &Repo, updates: &[RefUpdate]) -> Result<(), GuardError> {
    run(
        fx,
        repo,
        updates,
        &AdapterLimits::default(),
        Budget::unlimited(),
    )
}

fn rejected(r: Result<(), GuardError>, what: &str) {
    match r {
        Err(GuardError::Reject { message }) => {
            assert!(message.starts_with("minds-witness: "), "{what}: {message}")
        }
        other => panic!("{what}: erwartet Ablehnung, war {other:?}"),
    }
}

#[test]
fn s2_22_guard_rules() {
    let fx = GitTestFixture::new();
    let repo = fx.server();
    let e = Evidence {
        repo: &repo,
        key: &fx.key,
        public: &fx.public,
    };
    let base = repo.commit_files(&[("a", "100644", b"0")], &[]);
    let c = repo.commit_files(&[("a", "100644", b"1")], &[&base]);
    let (seal, ps) = e.witness_fs(&observations(&[("a", Obs::Content(b"1"))]));
    let evidence: Vec<RefUpdate> = ps.iter().map(add).collect();
    assert_eq!(check(&fx, &repo, &evidence), Ok(()), "gültige Evidence");

    // Neue Bindungen: gültig angenommen, alle Fehlformen abgelehnt.
    let (_, good) = e.binding(&c, std::slice::from_ref(&seal));
    assert_eq!(check(&fx, &repo, &[add(&good)]), Ok(()));
    let (_, unsigned) = e.binding_with(&c, PROJECT, std::slice::from_ref(&seal), None);
    rejected(check(&fx, &repo, &[add(&unsigned)]), "unsigniert");
    let junk_blob = repo.blob(b"junk");
    let junk_sig = repo.blob(b"junk");
    let junk = repo.commit(
        &repo.tree(&[
            ("100644", b"binding", &junk_blob),
            ("100644", b"binding.sig", &junk_sig),
        ]),
        &[],
    );
    rejected(
        check(
            &fx,
            &repo,
            &[add(&Pending {
                name: format!("refs/minds/bindings/{c}/{}", "0".repeat(64)),
                target: junk,
            })],
        ),
        "unlesbar",
    );
    rejected(
        check(
            &fx,
            &repo,
            &[add(&Pending {
                name: format!("refs/minds/bindings/{c}/{}", "1".repeat(64)),
                target: good.target.clone(),
            })],
        ),
        "falsche Id",
    );
    let (_, for_base) = e.binding(&base, std::slice::from_ref(&seal));
    let wrong_commit = Pending {
        name: for_base.name.replace(&base, &c),
        target: for_base.target.clone(),
    };
    rejected(check(&fx, &repo, &[add(&wrong_commit)]), "falscher Commit");
    let (_, foreign_project) = e.binding_with(
        &c,
        "git.test/project-7",
        std::slice::from_ref(&seal),
        Some((&fx.key, &fx.public)),
    );
    rejected(
        check(&fx, &repo, &[add(&foreign_project)]),
        "fremdes Projekt",
    );
    let (fk, fp) = keypair(fx.dir.path(), "foreign");
    let (_, untrusted) = e.binding_with(&c, PROJECT, std::slice::from_ref(&seal), Some((&fk, &fp)));
    rejected(check(&fx, &repo, &[add(&untrusted)]), "nicht anerkannt");
    // Signiert von einem fremden Schlüssel, aber `key` nennt den Witness.
    let b = minds_witness_git::binding::CommitBinding::new(
        &c,
        &fx.public,
        PROJECT,
        vec![seal.clone()],
        32,
    )
    .unwrap();
    let bytes = b.to_bytes();
    let bad_sig = repo.blob(&sign(&bytes, &fk, "minds-witness"));
    let bb = repo.blob(&bytes);
    let bad = repo.commit(
        &repo.tree(&[
            ("100644", b"binding", &bb),
            ("100644", b"binding.sig", &bad_sig),
        ]),
        &[],
    );
    rejected(
        check(
            &fx,
            &repo,
            &[add(&Pending {
                name: b.ref_name(),
                target: bad,
            })],
        ),
        "ungültige Signatur",
    );

    // Deckel: vier gültige im Vorzustand, die fünfte wird abgelehnt.
    let mut seals = Vec::new();
    for i in 0..5u8 {
        let (s, ps) = e.witness_fs(&observations(&[("a", Obs::Content(&[b'1', i]))]));
        for p in &ps {
            repo.update_ref(&p.name, &p.target);
        }
        seals.push(s);
    }
    let capped = repo.commit_files(&[("a", "100644", b"cap")], &[&base]);
    for s in &seals[..4] {
        let (_, p) = e.binding(&capped, std::slice::from_ref(s));
        repo.update_ref(&p.name, &p.target);
    }
    let (_, fifth) = e.binding(&capped, std::slice::from_ref(&seals[4]));
    rejected(check(&fx, &repo, &[add(&fifth)]), "Deckel");
    // Bindungen eines nicht (mehr) anerkannten Schlüssels belegen den
    // Deckel nicht.
    let other = repo.commit_files(&[("a", "100644", b"other")], &[&base]);
    for s in &seals[..4] {
        let (_, p) = e.binding_with(&other, PROJECT, std::slice::from_ref(s), Some((&fk, &fp)));
        repo.update_ref(&p.name, &p.target);
    }
    let (_, ok) = e.binding(&other, std::slice::from_ref(&seals[4]));
    assert_eq!(
        check(&fx, &repo, &[add(&ok)]),
        Ok(()),
        "widerrufene zählen nicht"
    );

    // Seals.
    let text = seal_text(
        "witness-fs/v1",
        SealOutcome::ObservationsStored {
            observations: seal_id(0x21),
        },
    );
    let (_, unsigned_seal, _) = e.seal(&text, None);
    rejected(
        check(&fx, &repo, &[add(&unsigned_seal)]),
        "Witness-Seal unsigniert",
    );
    let blob = repo.blob(text.as_bytes());
    let xsig = repo.blob(b"x");
    let foreign_file = repo.commit(
        &repo.tree(&[("100644", b"seal", &blob), ("100644", b"x.sig", &xsig)]),
        &[],
    );
    rejected(
        check(
            &fx,
            &repo,
            &[add(&Pending {
                name: unsigned_seal.name.clone(),
                target: foreign_file,
            })],
        ),
        "Fremddatei",
    );
    // Altbestand `{seal}` → `{seal, seal.sig}`: Nachreichen erlaubt.
    let (_, signed_seal, root) = e.seal(&text, Some("minds-witness"));
    repo.update_ref(&signed_seal.name, &root);
    assert_eq!(
        check(
            &fx,
            &repo,
            &[change(&signed_seal.name, &root, &signed_seal.target)]
        ),
        Ok(()),
        "Nachreichen"
    );
    // … aber nicht mit ungültiger Signatur.
    let bad_seal_sig = repo.blob(&sign(text.as_bytes(), &fk, "minds-witness"));
    let bad_tip = repo.commit(
        &repo.tree(&[
            ("100644", b"seal", &blob),
            ("100644", b"seal.sig", &bad_seal_sig),
        ]),
        &[&root],
    );
    rejected(
        check(&fx, &repo, &[change(&signed_seal.name, &root, &bad_tip)]),
        "ungültige Seal-Signatur",
    );
    // Ein Seal mit Signatur ist nicht mehr änderbar.
    repo.update_ref(&signed_seal.name, &signed_seal.target);
    rejected(
        check(
            &fx,
            &repo,
            &[change(&signed_seal.name, &signed_seal.target, &bad_tip)],
        ),
        "Seal ändern",
    );
    // `agent-hooks/v1` unter `minds`: nur Formprüfung.
    let hooks = seal_text(
        SCOPE_AGENT_HOOKS_V1,
        SealOutcome::Stored {
            session: seal_id(0x31),
        },
    );
    let (_, hooks_seal, _) = e.seal(&hooks, Some("minds"));
    assert_eq!(
        check(&fx, &repo, &[add(&hooks_seal)]),
        Ok(()),
        "agent-hooks"
    );
    // Seal-Bytes passen nicht zur Ref-Id.
    let (_, s2, _) = e.seal(&text, Some("minds-witness"));
    rejected(
        check(
            &fx,
            &repo,
            &[add(&Pending {
                name: format!("refs/minds/evidence/{}", "2".repeat(64)),
                target: s2.target,
            })],
        ),
        "Seal-Id",
    );

    // Observations.
    let (_, obs_p) = e.observations(&observations(&[("a", Obs::Content(b"9"))]));
    rejected(
        check(
            &fx,
            &repo,
            &[add(&Pending {
                name: format!("refs/minds/observations/{}", "3".repeat(64)),
                target: obs_p.target.clone(),
            })],
        ),
        "Observation-Id",
    );
    repo.update_ref(&obs_p.name, &obs_p.target);
    rejected(
        check(&fx, &repo, &[change(&obs_p.name, &obs_p.target, &base)]),
        "Observation ändern",
    );
    repo.update_ref(&good.name, &good.target);
    rejected(
        check(
            &fx,
            &repo,
            &[change(&good.name, &good.target, &junk_tip(&repo))],
        ),
        "Bindung ändern",
    );
    rejected(
        check(
            &fx,
            &repo,
            &[RefUpdate {
                old: cid(&obs_p.target),
                new: null(),
                ref_name: obs_p.name.clone(),
            }],
        ),
        "Löschen",
    );

    // Formen, Schreibweisen, Symrefs, Replace.
    for name in [
        "refs/minds/bindings".to_owned(),
        "refs/minds/evidence".to_owned(),
        "refs/minds/observations".to_owned(),
        format!("refs/minds/bindings/{c}"),
        format!("refs/minds/evidence/{}", "A".repeat(64)),
        format!("refs/minds/EVIDENCE/{}", "a".repeat(64)),
        "refs/Minds/x".to_owned(),
        "refs/Minds".to_owned(),
        "refs/replace/abc".to_owned(),
    ] {
        rejected(
            check(
                &fx,
                &repo,
                &[RefUpdate {
                    old: null(),
                    new: cid(&c),
                    ref_name: name.clone(),
                }],
            ),
            &name,
        );
    }
    repo.update_ref("refs/heads/main", &base);
    repo.symref("refs/heads/alias", "refs/heads/main");
    rejected(
        check(&fx, &repo, &[change("refs/heads/alias", &base, &c)]),
        "Symref",
    );
    assert_eq!(
        check(&fx, &repo, &[change("refs/heads/main", &base, &c)]),
        Ok(()),
        "gewöhnlicher Branch"
    );

    // Technischer Fehler (Prozessbudget 0) → interne Ablehnung.
    let (_, good2) = e.binding(&capped, std::slice::from_ref(&seal));
    assert_eq!(
        run(
            &fx,
            &repo,
            &[add(&good2)],
            &AdapterLimits::default(),
            Budget::new(0, u64::MAX, None)
        ),
        Err(GuardError::Internal)
    );
    // max_refs_per_push.
    let many: Vec<RefUpdate> = (0..3)
        .map(|i| RefUpdate {
            old: null(),
            new: cid(&c),
            ref_name: format!("refs/heads/b{i}"),
        })
        .collect();
    rejected(
        run(
            &fx,
            &repo,
            &many,
            &AdapterLimits {
                max_refs_per_push: 2,
                ..Default::default()
            },
            Budget::unlimited(),
        ),
        "max_refs_per_push",
    );
    rejected(
        parse_stdin(
            &format!("{} {} refs/heads/x\n", "0".repeat(40), "1".repeat(40)).repeat(3),
            2,
        )
        .map(|_| ()),
        "stdin max_refs",
    );
    rejected(parse_stdin("kaputt\n", 10).map(|_| ()), "stdin unlesbar");
    rejected(
        parse_stdin(
            &format!(
                "{} {} {}\n",
                "0".repeat(40),
                "1".repeat(40),
                "r".repeat(5000)
            ),
            10,
        )
        .map(|_| ()),
        "stdin zu lang",
    );
}

fn junk_tip(repo: &Repo) -> String {
    let b = repo.blob(b"x");
    repo.commit(&repo.tree(&[("100644", b"binding", &b)]), &[])
}

fn seal_id(byte: u8) -> String {
    format!("b3-{}", hex::encode([byte; 32]))
}
