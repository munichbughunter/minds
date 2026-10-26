//! Spec S2 v0.4.1 §13 Ebene 2 „echter Push“ und §14 (Abnahme A, B, C):
//! echter `git push file://` gegen ein Bare-Repository mit
//! `minds-witness-hook` als pre-receive-Hook. Offline, im Temp-Verzeichnis.
#![cfg(unix)]

mod support;

use minds_core::evidence::{SCOPE_WITNESS_FS_V1, SealOutcome};
use minds_witness_core::{
    CoverageStatus, Mode, Policy, PolicyVerdict, Receipt, SealState, verify_receipt,
};
use support::*;

const CLOSE: &str = "minds-witness: Push wird von minds nicht blockiert. Nachweis unvollständig; Details in der Quittung.";
const MARKER: &str = "MARKER-S2-44";

/// Server mit Basis-Commit `B` auf `main`, gepusht **vor** der
/// Hook-Installation; Client mit Remote `origin`.
struct World {
    fx: GitTestFixture,
    server: Repo,
    client: Repo,
    base: String,
}

/// Evidence, die im Client liegt und gepusht werden kann.
struct Bound {
    seal: String,
    binding: String,
    refs: Vec<String>,
}

fn world() -> World {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let client = fx.client(&server);
    let base = client.commit_files(
        &[
            ("README.md", "100644", b"readme"),
            ("src/a.rs", "100644", b"a0"),
        ],
        &[],
    );
    let w = World {
        fx,
        server,
        client,
        base,
    };
    let out = w.push_refs(&[format!("{}:refs/heads/main", w.base)]);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    w
}

impl World {
    fn evidence(&self) -> Evidence<'_> {
        Evidence {
            repo: &self.client,
            key: &self.fx.key,
            public: &self.fx.public,
        }
    }

    fn hook(&self, policy: &Policy) -> Hook {
        self.hook_with(policy, |_| {})
    }

    fn hook_with(&self, policy: &Policy, f: impl FnOnce(&mut serde_json::Value)) -> Hook {
        let mut v = self.fx.hook_config(policy);
        f(&mut v);
        self.fx.install_hook(&self.server, &v)
    }

    /// Ein Commit auf `parent` mit dessen Dateien plus `files`
    /// (überschreibt gleichnamige; alle Dateien `100644`).
    fn commit(&self, parent: &str, files: &[(&str, &[u8])]) -> String {
        let listing = self
            .client
            .git(&["ls-tree", "-r", "-z", "--name-only", parent]);
        let mut all: Vec<(String, Vec<u8>)> = listing
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let bytes = self
                    .client
                    .try_git(&["cat-file", "blob", &format!("{parent}:{p}")], None)
                    .stdout;
                (p.to_owned(), bytes)
            })
            .collect();
        for (p, b) in files {
            all.retain(|(q, _)| q != p);
            all.push(((*p).to_owned(), b.to_vec()));
        }
        let spec: Vec<(&str, &str, &[u8])> = all
            .iter()
            .map(|(p, b)| (p.as_str(), "100644", b.as_slice()))
            .collect();
        self.client.commit_files(&spec, &[parent])
    }

    /// `witness-fs/v1`-Evidence über echte minds-Seals
    /// (`InRepoStore::put_seal` + `put_seal_signature`, S1 §15) für die
    /// Inhalte `files`, dazu eine signierte Bindung für `commit`.
    fn bind(&self, commit: &str, files: &[(&str, &[u8])]) -> Bound {
        use minds_store::ContextStore;
        let e = self.evidence();
        let obs = observations(
            &files
                .iter()
                .map(|(p, b)| (*p, Obs::Content(b)))
                .collect::<Vec<_>>(),
        );
        let (obs_id, p_obs) = e.observations(&obs);
        self.client.update_ref(&p_obs.name, &p_obs.target);
        let text = seal_text(
            SCOPE_WITNESS_FS_V1,
            SealOutcome::ObservationsStored {
                observations: obs_id,
            },
        );
        let store = minds_store::InRepoStore::open(&self.client.path).unwrap();
        let id = store.put_seal(&text).unwrap();
        let sig = String::from_utf8(sign(text.as_bytes(), &self.fx.key, "minds-witness")).unwrap();
        store.put_seal_signature(&id, &sig).unwrap();
        let seal = id.as_str().to_owned();
        let (b, p_bind) = e.binding(commit, std::slice::from_ref(&seal));
        self.client.update_ref(&p_bind.name, &p_bind.target);
        Bound {
            refs: vec![
                p_obs.name,
                format!("refs/minds/evidence/{}", &seal[3..]),
                p_bind.name,
            ],
            seal,
            binding: b.id(),
        }
    }

    fn push_refs(&self, specs: &[String]) -> PushOutcome {
        let mut args = vec!["--atomic", "origin"];
        args.extend(specs.iter().map(String::as_str));
        let out = self.client.push(&args);
        // §11: Auf dem Push-Kanal steht nur, was minds schreibt.
        assert!(
            out.foreign.is_empty(),
            "fremde remote-Zeilen: {:?}",
            out.foreign
        );
        out
    }

    /// `git push --atomic origin <c>:<branch> <refs…>`.
    fn push(&self, commit: &str, branch: &str, refs: &[String]) -> PushOutcome {
        let mut specs = vec![format!("{commit}:{branch}")];
        specs.extend(refs.iter().cloned());
        self.push_refs(&specs)
    }

    fn server_ref(&self, name: &str) -> Option<String> {
        self.server.ref_value(name)
    }

    fn server_has(&self, oid: &str) -> bool {
        self.server
            .try_git(&["cat-file", "-e", oid], None)
            .status
            .success()
    }

    fn binding_ids_on_server(&self, commit: &str) -> Vec<String> {
        let prefix = format!("refs/minds/bindings/{commit}/");
        let out = self
            .server
            .git(&["for-each-ref", "--format=%(refname)", &prefix]);
        out.lines()
            .filter_map(|l| l.strip_prefix(&prefix))
            .map(|hex| format!("b3-{hex}"))
            .collect()
    }
}

/// `H(kind, c, ids)` aus §8.4, im Test unabhängig nachgerechnet.
fn h(kind: &str, commit: &str, ids: &[String]) -> String {
    let mut ids = ids.to_vec();
    ids.sort();
    let list = ids
        .iter()
        .map(|i| format!("\"{i}\""))
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(r#"{{"bindings":[{list}],"commit":"{commit}","kind":"{kind}"}}"#);
    format!(
        "blake3:{}",
        hex::encode(blake3::hash(json.as_bytes()).as_bytes())
    )
}

/// Genau eine Quittung unter `<out>/<commit>/`, verifiziert gegen
/// `keys.json`.
fn receipt(hook: &Hook, commit: &str) -> Receipt {
    let all = hook.receipts(commit);
    assert_eq!(all.len(), 1, "Quittungen für {commit}");
    verify_receipt(&all[0], &hook.keys()).unwrap().receipt
}

fn c12(c: &str) -> &str {
    &c[..12]
}

fn warning(branch: &str, c: &str, state: &str, counts: &str, threshold: u8) -> String {
    format!(
        "minds-witness: Warnung: {branch} {}: {state}, {counts}, Evidence-Policy nicht erfüllt (Schwelle {threshold} %)",
        c12(c)
    )
}

/// Keine Bytes < 0x20 in den Zeilen der Gegenseite.
fn assert_clean(out: &PushOutcome) {
    for l in out.stderr.lines().filter(|l| l.starts_with("remote:")) {
        assert!(l.bytes().all(|b| b >= 0x20), "Steuerzeichen: {l:?}");
    }
}

// ---------------------------------------------------------------------

#[test]
fn s2_35_quarantine_objects_visible() {
    let w = world();
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    // Commit und Evidence liegen nur in der Quarantäne des Pushes.
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    let b = w.bind(&c, &[("src/a.rs", b"a1")]);
    let out = w.push(&c, "refs/heads/main", &b.refs);
    assert_eq!((out.exit, out.minds.len()), (0, 0), "{}", out.stderr);
    let r = receipt(&hook, &c);
    assert_eq!(r.seal.status, SealState::Valid);
    assert_eq!(r.policy.result, PolicyVerdict::Pass);

    // Ohne Quarantäne sieht der Hook den neuen Commit nicht: Ref verworfen.
    w.hook_with(&policy(Mode::Observe, &[], 100), |v| {
        v["test_switches"] = serde_json::json!({"skip_quarantine": true});
    });
    let c2 = w.commit(&c, &[("src/a.rs", b"a2")]);
    let out = w.push(&c2, "refs/heads/main", &[]);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert_eq!(
        out.minds,
        vec![
            "minds-witness: Warnung: refs/heads/main: Prüfung nicht abgeschlossen (Unknown)"
                .to_owned(),
            CLOSE.to_owned()
        ]
    );
    assert!(hook.log().contains("ref_evaluation_failed"));
    assert!(hook.receipts(&c2).is_empty());
}

#[test]
fn s2_36_reject_leaves_ref_unchanged() {
    let w = world();
    let hook = w.hook(&policy(Mode::Enforce, &["refs/heads/main"], 100));
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    let out = w.push(&c, "refs/heads/main", &[]);
    assert_ne!(out.exit, 0);
    assert_eq!(out.minds.len(), 1, "{:?}", out.minds);
    assert!(
        out.minds[0].starts_with("minds-witness: Push auf refs/heads/main abgelehnt"),
        "{:?}",
        out.minds
    );
    assert!(!out.minds[0].contains("Warnung"));
    assert_eq!(
        w.server_ref("refs/heads/main").as_deref(),
        Some(w.base.as_str())
    );
    assert!(!w.server_has(&c), "Quarantäne-Objekt übernommen");
    assert!(!hook.out.join(&c).exists(), "Quittung trotz Ablehnung");

    // s2_42 im Hook: Ohne wirksames receive.fsckObjects lehnt Enforce vor
    // dem Kern ab; Observe vermerkt es nur in der Diagnose.
    w.server.git(&["config", "receive.fsckObjects", "false"]);
    let out = w.push(&c, "refs/heads/main", &[]);
    assert_ne!(out.exit, 0);
    assert_eq!(
        out.minds,
        vec!["minds-witness: receive.fsckObjects ist nicht wirksam".to_owned()]
    );
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    let out = w.push(&c, "refs/heads/main", &[]);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert!(hook.log().contains("fsck_not_effective"));
}

#[test]
fn s2_37_receipt_hash_golden() {
    let w = world();
    let e = w.evidence();
    let (other_key, other_pub) = keypair(w.fx.dir.path(), "other");
    let fake_seal = format!("b3-{}", "ab".repeat(32));

    // Altbestand, vor der Hook-Installation angelegt (nur so sind die
    // Klassen außer „accepted“ im Git-Pfad erreichbar, §7.5).
    let ca = w.commit(&w.base, &[("src/a.rs", b"case-a")]);
    let accepted = w.bind(&ca, &[("src/a.rs", b"case-a")]);

    let cm = w.commit(&w.base, &[("src/a.rs", b"case-m")]);
    let (bm, pm) = e.binding_with(
        &cm,
        "git.test/project-43",
        std::slice::from_ref(&fake_seal),
        Some((&w.fx.key, &w.fx.public)),
    );
    let ci = w.commit(&w.base, &[("src/a.rs", b"case-i")]);
    let (bi, pi) = e.binding_with(
        &ci,
        PROJECT,
        std::slice::from_ref(&fake_seal),
        Some((&other_key, &other_pub)),
    );
    let cu = w.commit(&w.base, &[("src/a.rs", b"case-u")]);
    let (bu, pu) = e.binding_with(&cu, PROJECT, std::slice::from_ref(&fake_seal), None);
    // Alle Kandidaten unlesbar: Bindungsbaum ohne gültige Bindung.
    let cx = w.commit(&w.base, &[("src/a.rs", b"case-x")]);
    let junk = w.client.blob(b"not a binding");
    let junk_tree = w.client.tree(&[("100644", b"binding", &junk)]);
    let junk_commit = w.client.commit(&junk_tree, &[]);
    let junk_ref = format!("refs/minds/bindings/{cx}/b3-{}", "cd".repeat(32));

    let mut old = accepted.refs.clone();
    for p in [&pm, &pi, &pu] {
        old.push(Repo::spec(p));
    }
    old.push(format!("{junk_commit}:{junk_ref}"));
    let out = w.push_refs(&old);
    assert_eq!(out.exit, 0, "{}", out.stderr);

    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    for (i, c) in [&ca, &cm, &ci, &cu, &cx].iter().enumerate() {
        let out = w.push(c, &format!("refs/heads/case-{i}"), &[]);
        assert_eq!(out.exit, 0, "{}", out.stderr);
    }

    let r = receipt(&hook, &ca);
    assert_eq!(r.seal.status, SealState::Valid);
    assert_eq!(w.binding_ids_on_server(&ca), vec![accepted.binding.clone()]);
    assert_eq!(
        r.seal.hash.unwrap().as_str(),
        h("accepted_bindings", &ca, &w.binding_ids_on_server(&ca))
    );

    let r = receipt(&hook, &cm);
    assert_eq!(r.seal.status, SealState::Mismatched);
    assert_eq!(
        r.seal.hash.unwrap().as_str(),
        h("mismatched_binding", &cm, &[bm.id()])
    );

    let r = receipt(&hook, &ci);
    assert_eq!(r.seal.status, SealState::Invalid);
    assert_eq!(
        r.seal.hash.unwrap().as_str(),
        h("rejected_bindings", &ci, &[bi.id()])
    );

    let r = receipt(&hook, &cu);
    assert_eq!(r.seal.status, SealState::Unsigned);
    assert_eq!(
        r.seal.hash.unwrap().as_str(),
        h("rejected_bindings", &cu, &[bu.id()])
    );

    // Fünfter Fall (nur `malformed`): der Kern hasht die Kandidatenliste.
    let r = receipt(&hook, &cx);
    assert_eq!(r.seal.status, SealState::Invalid);
    let list = format!(
        r#"{{"candidates":[{{"ref":"{junk_ref}","target":"{junk_commit}"}}],"commit":"{cx}","kind":"binding_candidates"}}"#
    );
    assert_eq!(
        r.seal.hash.unwrap().as_str(),
        format!(
            "blake3:{}",
            hex::encode(blake3::hash(list.as_bytes()).as_bytes())
        )
    );
}

/// Abnahme A und C (§14).
#[test]
fn s2_38_e2e_valid_evidence_receipt_verifies() {
    let w = world();
    let hook = w.hook(&policy(Mode::Enforce, &["refs/heads/main"], 100));

    // A: C ändert src/a.rs und fügt src/b.rs hinzu; volle Evidence.
    let c = w.commit(&w.base, &[("src/a.rs", b"a1"), ("src/b.rs", b"b1")]);
    let b = w.bind(&c, &[("src/a.rs", b"a1"), ("src/b.rs", b"b1")]);
    let out = w.push(&c, "refs/heads/main", &b.refs);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert!(out.minds.is_empty(), "{:?}", out.minds);
    assert_eq!(w.server_ref("refs/heads/main").as_deref(), Some(c.as_str()));
    let r = receipt(&hook, &c);
    assert_eq!(r.seal.status, SealState::Valid);
    assert_eq!(
        (
            r.coverage.paths_covered,
            r.coverage.paths_in_diff,
            &r.coverage.status,
            r.coverage.uncovered.len(),
            r.coverage.uncovered_truncated
        ),
        (2, 2, &CoverageStatus::Computed, 0, false)
    );
    assert_eq!(r.policy.result, PolicyVerdict::Pass);
    assert_eq!(r.push.old.as_str(), w.base);
    assert_eq!(r.push.new_commits, 1);
    let ids = w.binding_ids_on_server(&c);
    assert_eq!(ids, vec![b.binding.clone()]);
    assert_eq!(
        r.seal.hash.unwrap().as_str(),
        h("accepted_bindings", &c, &ids)
    );
    let _ = b.seal;

    // C (Gegenprobe, Strict Enforce): C2 ändert a, b und c; Evidence nur
    // für a und b → abgelehnt, main bleibt C, keine Quittung, keine Warnung.
    let c2 = w.commit(
        &c,
        &[
            ("src/a.rs", b"a2"),
            ("src/b.rs", b"b2"),
            ("src/c.rs", b"c2"),
        ],
    );
    let b2 = w.bind(&c2, &[("src/a.rs", b"a2"), ("src/b.rs", b"b2")]);
    let out = w.push(&c2, "refs/heads/main", &b2.refs);
    assert_ne!(out.exit, 0);
    assert_eq!(out.minds.len(), 1, "{:?}", out.minds);
    assert!(
        out.minds[0].contains("(2/3 Pfade abgedeckt, Seal valid)"),
        "{:?}",
        out.minds
    );
    assert!(out.minds.iter().all(|l| !l.contains("Warnung")));
    assert_eq!(w.server_ref("refs/heads/main").as_deref(), Some(c.as_str()));
    assert!(hook.receipts(&c2).is_empty());
}

/// Abnahme B (§14).
#[test]
fn s2_43_e2e_observe_partial_coverage_warns() {
    let w = world();
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    let c2 = w.commit(
        &w.base,
        &[
            ("src/a.rs", b"a2"),
            ("src/b.rs", b"b2"),
            ("src/c.rs", b"c2"),
        ],
    );
    let b = w.bind(&c2, &[("src/a.rs", b"a2"), ("src/b.rs", b"b2")]);
    let out = w.push(&c2, "refs/heads/main", &b.refs);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert_eq!(
        w.server_ref("refs/heads/main").as_deref(),
        Some(c2.as_str())
    );
    for r in &b.refs {
        assert!(w.server_ref(r).is_some(), "Evidence-Ref fehlt: {r}");
    }
    let r = receipt(&hook, &c2);
    assert_eq!(r.seal.status, SealState::Valid);
    assert_eq!(
        (
            r.coverage.paths_covered,
            r.coverage.paths_in_diff,
            &r.coverage.status,
            r.coverage.uncovered.clone(),
            r.coverage.uncovered_truncated
        ),
        (
            2,
            3,
            &CoverageStatus::Computed,
            vec!["src/c.rs".to_owned()],
            false
        )
    );
    assert_eq!(
        (&r.policy.mode, &r.policy.result),
        (&Mode::Observe, &PolicyVerdict::Fail)
    );
    assert_eq!(
        out.minds,
        vec![
            "minds-witness: Warnung: refs/heads/main ".to_owned()
                + c12(&c2)
                + ": Partial, 2/3 Dateien nachgewiesen (66,7 %), Evidence-Policy nicht erfüllt (Schwelle 100 %)",
            CLOSE.to_owned(),
        ]
    );

    // Gegenprobe: nur Code ohne jede Evidence, ebenfalls nicht blockiert.
    let c3 = w.commit(&c2, &[("src/a.rs", b"a3")]);
    let out = w.push(&c3, "refs/heads/main", &[]);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert_eq!(
        w.server_ref("refs/heads/main").as_deref(),
        Some(c3.as_str())
    );
    assert_eq!(
        out.minds,
        vec![
            warning(
                "refs/heads/main",
                &c3,
                "Unverified",
                "0/1 Dateien nachgewiesen (0,0 %)",
                100
            ),
            CLOSE.to_owned()
        ]
    );
    assert_eq!(receipt(&hook, &c3).seal.status, SealState::Absent);
}

#[test]
fn s2_44_warning_output_is_safe() {
    let w = world();
    let e = w.evidence();
    let (other_key, other_pub) = keypair(w.fx.dir.path(), "other");

    // Altbestand vor dem Hook: (b) ungültig signierte Bindung, (d) gültige
    // Bindung, deren Prüfung im Push einen Prozess bräuchte.
    let cb = w.commit(&w.base, &[("src/a.rs", b"case-b")]);
    let (_, pb) = e.binding_with(
        &cb,
        PROJECT,
        &[format!("b3-{}", "ab".repeat(32))],
        Some((&other_key, &other_pub)),
    );
    let cd = w.commit(&w.base, &[("src/a.rs", b"case-d")]);
    let bd = w.bind(&cd, &[("src/a.rs", b"case-d")]);
    let mut old = bd.refs.clone();
    old.push(Repo::spec(&pb));
    assert_eq!(w.push_refs(&old).exit, 0);

    let observe = policy(Mode::Observe, &[], 100);
    w.hook(&observe);
    let mut results = Vec::new();

    // Alle Pushes gehen als Force-Push auf `main`, damit genau der eine
    // neue Commit bewertet wird (bei einer Ref-Anlage zählt nach S1 die
    // ganze Historie, ADR-S2-005).
    // (a) fehlende Bindung.
    let ca = w.commit(&w.base, &[("src/a.rs", b"case-a")]);
    let out = w.push(&format!("+{ca}"), "refs/heads/main", &[]);
    results.push((out, "Unverified"));
    // (b) ungültige Bindung.
    let out = w.push(&format!("+{cb}"), "refs/heads/main", &[]);
    results.push((out, "Invalid"));
    // (c) truncated: 3 neue Commits bei max_commits_per_push = 2.
    let mut small = observe.clone();
    small.max_commits_per_push = 2;
    w.hook(&small);
    let c1 = w.commit(&w.base, &[("src/a.rs", b"t1")]);
    let c2 = w.commit(&c1, &[("src/a.rs", b"t2")]);
    let c3 = w.commit(&c2, &[("src/a.rs", b"t3")]);
    let out = w.push(&format!("+{c3}"), "refs/heads/main", &[]);
    assert_eq!(
        out.minds[0],
        format!(
            "minds-witness: Warnung: refs/heads/main {}: Unverified, unvollständige Prüfung (mehr als 2 neue Commits), Evidence-Policy nicht erfüllt (Schwelle 100 %)",
            c12(&c3)
        )
    );
    results.push((out, "Unverified"));
    // (d) Internal über Prozessbudget 0.
    w.hook_with(&observe, |v| {
        v["limits"] = serde_json::json!({"max_ssh_keygen_processes": 0});
    });
    let out = w.push(&format!("+{cd}"), "refs/heads/main", &[]);
    assert_eq!(
        out.minds[0],
        "minds-witness: Warnung: refs/heads/main: Prüfung nicht abgeschlossen (Unknown)"
    );
    results.push((out, "Unknown"));
    // (e) Panic mit Marker-Text.
    w.hook_with(&observe, |v| {
        v["test_switches"] = serde_json::json!({"panic_in": "changed_paths", "panic_text": MARKER});
    });
    let ce = w.commit(&w.base, &[("src/a.rs", b"case-e")]);
    let out = w.push(&format!("+{ce}"), "refs/heads/main", &[]);
    assert_eq!(
        out.minds[0],
        "minds-witness: Warnung: refs/heads/main: Prüfung nicht abgeschlossen (Unknown)"
    );
    results.push((out, "Unknown"));

    // (f) Pfadnamen und Observation-Inhalte mit Marker-Text und
    // ANSI-Steuersequenz.
    w.hook(&observe);
    let evil = format!("src/{MARKER}-\u{1b}[31m.rs");
    let named = format!("src/{MARKER}.rs");
    let cf = w.commit(
        &w.base,
        &[
            (evil.as_str(), MARKER.as_bytes()),
            (named.as_str(), MARKER.as_bytes()),
        ],
    );
    let bf = w.bind(&cf, &[(named.as_str(), MARKER.as_bytes())]);
    let out = w.push(&format!("+{cf}"), "refs/heads/main", &bf.refs);
    assert_eq!(
        out.minds,
        vec![
            warning(
                "refs/heads/main",
                &cf,
                "Partial",
                "1/2 Dateien nachgewiesen (50,0 %)",
                100
            ),
            CLOSE.to_owned()
        ]
    );
    results.push((out, "Partial"));

    for (out, state) in &results {
        assert_eq!(out.exit, 0, "{}", out.stderr);
        assert_eq!(out.minds.len(), 2, "{state}: {:?}", out.minds);
        assert!(out.minds[0].contains(state), "{state}: {:?}", out.minds);
        assert_eq!(out.minds.last().unwrap(), CLOSE);
        assert!(!out.stderr.contains(MARKER), "{}", out.stderr);
        assert!(!out.stderr.contains("src/"), "{}", out.stderr);
        assert_clean(out);
    }

    // (g) 25 Commits ohne Bindung: 20 Zeilen plus „… und 5 weitere“.
    let mut tip = w.base.clone();
    for i in 0..25u8 {
        tip = w.commit(&tip, &[("src/a.rs", &[b'g', i])]);
    }
    let out = w.push(&format!("+{tip}"), "refs/heads/main", &[]);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert_eq!(out.minds.len(), 22, "{:?}", out.minds);
    assert_eq!(out.minds[20], "minds-witness: … und 5 weitere Befunde");
    assert_eq!(out.minds[21], CLOSE);
    assert_clean(&out);
}

#[test]
fn s2_45_threshold_never_claims_full_proof() {
    let w = world();
    let mut tip = w.base.clone();
    for (n, threshold) in [60u8, 66, 67].into_iter().enumerate() {
        let hook = w.hook(&policy(Mode::Observe, &[], threshold));
        let x = [b'x', n as u8];
        let c = w.commit(
            &tip,
            &[("src/a.rs", &x), ("src/b.rs", &x), ("src/c.rs", &x)],
        );
        let b = w.bind(&c, &[("src/a.rs", &x), ("src/b.rs", &x)]);
        let out = w.push(&c, "refs/heads/main", &b.refs);
        assert_eq!(out.exit, 0, "{}", out.stderr);
        let expected = if threshold <= 66 {
            format!(
                "minds-witness: Hinweis: refs/heads/main {}: Evidence-Policy erfüllt (Schwelle {threshold} %), 2/3 Dateien nachgewiesen (66,7 %), 1 nicht nachgewiesen",
                c12(&c)
            )
        } else {
            warning(
                "refs/heads/main",
                &c,
                "Partial",
                "2/3 Dateien nachgewiesen (66,7 %)",
                67,
            )
        };
        assert_eq!(out.minds, vec![expected, CLOSE.to_owned()]);
        let bytes = hook.receipts(&c).remove(0);
        let r = receipt(&hook, &c);
        assert_eq!(
            r.policy.result,
            if threshold <= 66 {
                PolicyVerdict::Pass
            } else {
                PolicyVerdict::Fail
            }
        );
        assert_eq!((r.coverage.paths_covered, r.coverage.paths_in_diff), (2, 3));
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            !text.contains('%') && !text.contains("66,7") && !text.contains("66.7"),
            "{text}"
        );
        tip = c;
    }
    // Volle Abdeckung: keine Zeile.
    w.hook(&policy(Mode::Observe, &[], 100));
    let c = w.commit(&tip, &[("src/a.rs", b"full")]);
    let b = w.bind(&c, &[("src/a.rs", b"full")]);
    let out = w.push(&c, "refs/heads/main", &b.refs);
    assert_eq!((out.exit, out.minds.len()), (0, 0), "{}", out.stderr);
}

/// Zwei-Push-Ablauf (ADR-PRODUCT-001, §15).
#[test]
fn s2_46_evidence_push_failure_does_not_block_code() {
    let w = world();
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    let e = w.evidence();
    let c = w.commit(&w.base, &[("src/a.rs", b"a1"), ("src/b.rs", b"b1")]);

    // Erster Push: nur Evidence, die die Vorprüfung ablehnt (unsigniertes
    // witness-fs/v1-Seal).
    let obs = observations(&[
        ("src/a.rs", Obs::Content(b"a1")),
        ("src/b.rs", Obs::Content(b"b1")),
    ]);
    let (obs_id, p_obs) = e.observations(&obs);
    let text = seal_text(
        SCOPE_WITNESS_FS_V1,
        SealOutcome::ObservationsStored {
            observations: obs_id,
        },
    );
    let (_, p_seal, _) = e.seal(&text, None);
    let out = w.push_refs(&[Repo::spec(&p_obs), Repo::spec(&p_seal)]);
    assert_ne!(out.exit, 0);
    assert_eq!(
        out.minds,
        vec![format!(
            "minds-witness: Witness-Seals sind nur signiert anlegbar (Ref: {})",
            p_seal.name
        )]
    );
    assert_eq!(w.server_ref(&p_seal.name), None);
    assert_eq!(w.server_ref(&p_obs.name), None);

    // Zweiter Push: nur der Code → nicht blockiert, Warnung Unverified.
    let out = w.push(&c, "refs/heads/main", &[]);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert_eq!(w.server_ref("refs/heads/main").as_deref(), Some(c.as_str()));
    assert_eq!(
        out.minds,
        vec![
            warning(
                "refs/heads/main",
                &c,
                "Unverified",
                "0/2 Dateien nachgewiesen (0,0 %)",
                100
            ),
            CLOSE.to_owned()
        ]
    );
    let r = receipt(&hook, &c);
    assert_eq!(r.seal.status, SealState::Absent);
    assert_eq!(r.coverage.status, CoverageStatus::NoSeal);
    assert_eq!(r.policy.result, PolicyVerdict::Fail);
}

#[test]
fn s2_39_evidence_pushed_earlier() {
    let w = world();
    let hook = w.hook(&policy(Mode::Enforce, &["refs/heads/main"], 100));
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    let b = w.bind(&c, &[("src/a.rs", b"a1")]);
    // Erster Push: Seal und Observations; zweiter: Commit und Bindung.
    let out = w.push_refs(&b.refs[..2]);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    let out = w.push(&c, "refs/heads/main", &b.refs[2..]);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert!(out.minds.is_empty(), "{:?}", out.minds);
    assert_eq!(receipt(&hook, &c).policy.result, PolicyVerdict::Pass);
}

#[test]
fn s2_40_protected_refs_rejected_by_push() {
    let w = world();
    w.server.symref("refs/heads/alias", "refs/heads/main");
    w.hook(&policy(Mode::Observe, &[], 100));
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    for (target, message) in [
        (
            "refs/minds/witness/x",
            "Refs unter refs/minds/witness/ schreibt nur der Server (Ref: refs/minds/witness/x)",
        ),
        (
            "refs/minds/witness",
            "Ref refs/minds/witness ist reserviert, weil er den Witness-Namensraum refs/minds/witness/ blockieren würde",
        ),
        (
            "refs/minds/bindings",
            "unzulässige Form eines Evidence-Refs (Ref: refs/minds/bindings)",
        ),
        (
            "refs/heads/alias",
            "Update eines symbolischen Refs ist unzulässig (Ref: refs/heads/alias)",
        ),
        (
            "refs/replace/x",
            "refs/replace/ ist unzulässig (Ref: refs/replace/x)",
        ),
    ] {
        let out = w.push_refs(&[format!("{c}:refs/heads/feature"), format!("{c}:{target}")]);
        assert_ne!(out.exit, 0, "{target}: {}", out.stderr);
        assert_eq!(out.minds, vec![format!("minds-witness: {message}")]);
        assert_eq!(w.server_ref("refs/heads/feature"), None, "{target}");
        assert_eq!(
            w.server_ref("refs/heads/main").as_deref(),
            Some(w.base.as_str()),
            "{target}"
        );
        if target != "refs/heads/alias" {
            assert_eq!(w.server_ref(target), None, "{target}");
        }
    }
}

#[test]
fn s2_41_panic_payload_not_on_stderr() {
    let w = world();
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    for (mode, enforce) in [
        (Mode::Observe, vec![]),
        (Mode::Enforce, vec!["refs/heads/main"]),
    ] {
        w.hook_with(&policy(mode.clone(), &enforce, 100), |v| {
            v["test_switches"] =
                serde_json::json!({"panic_in": "changed_paths", "panic_text": MARKER});
        });
        let branch = format!("refs/heads/p-{}", enforce.len());
        let out = w.push(&c, &branch, &[]);
        assert!(!out.stderr.contains(MARKER), "{}", out.stderr);
        assert!(!out.stderr.contains("panicked"), "{}", out.stderr);
        match mode {
            Mode::Observe => {
                assert_eq!(out.exit, 0);
                assert_eq!(
                    out.minds,
                    vec![
                        format!(
                            "minds-witness: Warnung: {branch}: Prüfung nicht abgeschlossen (Unknown)"
                        ),
                        CLOSE.to_owned()
                    ]
                );
            }
            Mode::Enforce => {
                assert_ne!(out.exit, 0);
                assert_eq!(out.minds, vec!["minds-witness: interner Fehler".to_owned()]);
            }
        }
    }
}
