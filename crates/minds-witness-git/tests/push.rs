//! Spec S2 v0.4.1 §13 Ebene 2 „echter Push“ und §14 (Abnahme A, B, C):
//! echter `git push file://` gegen ein Bare-Repository mit
//! `minds-witness-hook` als pre-receive-Hook. Offline, im Temp-Verzeichnis.
#![cfg(unix)]

mod support;

use minds_core::evidence::{SCOPE_WITNESS_FS_V1, SealOutcome};
use minds_witness_core::{CoverageStatus, Mode, Policy, PolicyVerdict, Receipt, SealState};
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
    hook.verify(&all[0])
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
    assert!(hook.receipts(&c).is_empty(), "Quittung trotz Ablehnung");
    assert!(
        hook.handoff("pending").is_empty(),
        "Datensatz trotz Ablehnung"
    );

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

/// Ablehnungsmeldungen des Kerns über einen echten Push: Ein Ref-Name mit
/// einem 8-Bit-Steuerzeichen (U+009B, von Git erlaubt) erscheint escaped,
/// nie roh auf dem Terminal des Pushers.
#[test]
fn reject_message_escapes_ref_name_on_the_wire() {
    let w = world();
    w.hook(&policy(Mode::Enforce, &["refs/heads/*"], 100));
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    let evil = "refs/heads/x\u{9b}2J";
    let out = w.push(&c, evil, &[]);
    assert_ne!(out.exit, 0);
    // Ref-Anlage: der erste unbelegte Commit der Historie ist die Basis.
    assert_eq!(
        out.minds,
        vec![format!(
            "minds-witness: Push auf refs/heads/x\\xc2\\x9b2J abgelehnt: Commit {} (0/2 Pfade abgedeckt, Seal absent)",
            w.base
        )]
    );
    assert!(!out.stderr.contains('\u{9b}'), "{:?}", out.stderr);
    assert_eq!(w.server_ref(evil), None);
}

/// Spec S3 ADR-S3-002 (Commit 2): Die Ablage-Refs des Servers sind für
/// Clients nie schreibbar, auch nicht im Observe-Modus.
#[test]
fn witness_store_refs_are_server_only() {
    let w = world();
    w.hook(&policy(Mode::Observe, &[], 100));
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    let hex = "ab".repeat(32);
    for target in [
        format!("refs/minds/witness/receipts/{c}/{hex}"),
        "refs/minds/witness/keys".to_owned(),
        format!("refs/minds/witness/trust/{hex}"),
    ] {
        let out = w.push_refs(&[format!("{c}:{target}")]);
        assert_ne!(out.exit, 0, "{target}");
        assert_eq!(
            out.minds,
            vec![format!(
                "minds-witness: Refs unter refs/minds/witness/ schreibt nur der Server (Ref: {target})"
            )]
        );
        assert_eq!(w.server_ref(&target), None);
    }
}

// ---------------------------------------------------------------------
// Spec S3 §3 (Commit 3): Quittung erst nach dem Ref-Update.
// ---------------------------------------------------------------------

use minds_witness_core::{HashStr, RefUpdate, TrustSnapshot};
use minds_witness_git::hook::pending::{CONFIRMED_SCHEMA, Confirmation, Handoff, PendingRecord};

fn write_hook(server: &Repo, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = server.git_dir.join("hooks").join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn witness_refs(server: &Repo) -> Vec<String> {
    server
        .git(&["for-each-ref", "--format=%(refname)", "refs/minds/witness/"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Ruft das Binary direkt auf, wie receive-pack es täte (cwd = Bare-Repo,
/// `GIT_DIR=.`); der Elternprozess ist dann dieser Testprozess.
fn run_bin(sub: &str, hook: &Hook, stdin: &str) -> (i32, String) {
    use std::io::Write;
    let mut child = std::process::Command::new(HOOK_BIN)
        .arg(sub)
        .arg(&hook.config)
        .current_dir(&hook.server.git_dir)
        .env("GIT_DIR", ".")
        .env("HOME", &hook.server.home)
        .env("GIT_CONFIG_GLOBAL", hook.server.home.join("no-such-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

fn handoff(w: &World, hook: &Hook) -> Handoff {
    let _ = w;
    let mut seed = [0u8; 32];
    hex::decode_to_slice("42".repeat(32), &mut seed).unwrap();
    Handoff::new(&hook.state, &seed)
}

/// Kriterium 2: Nur tatsächlich ausgeführte Updates werden bestätigt.
#[test]
fn s3_02_post_receive_only_for_performed_updates() {
    let w = world();
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    // Git verweigert `feature` nach dem pre-receive (update-Hook).
    write_hook(
        &w.server,
        "update",
        r#"[ "$1" = "refs/heads/feature" ] && exit 1; exit 0"#,
    );
    let c1 = w.commit(&w.base, &[("src/a.rs", b"main")]);
    let c2 = w.commit(&w.base, &[("src/a.rs", b"feature")]);
    let out = w.client.push(&[
        "origin",
        &format!("{c1}:refs/heads/main"),
        &format!("{c2}:refs/heads/feature"),
    ]);
    assert_ne!(out.exit, 0, "feature wurde verweigert");
    assert_eq!(
        w.server_ref("refs/heads/main").as_deref(),
        Some(c1.as_str())
    );
    assert_eq!(w.server_ref("refs/heads/feature"), None);
    assert_eq!(hook.receipts(&c1).len(), 1);
    assert!(
        hook.receipts(&c2).is_empty(),
        "Quittung für ein verweigertes Update"
    );
    let r = hook.verify(&hook.receipts(&c1)[0]);
    assert_eq!(r.push.ref_name, "refs/heads/main");
    assert_eq!(r.server.kind, minds_witness_core::ServerKind::Git);
    assert!(hook.handoff("pending").is_empty() && hook.handoff("confirmed").is_empty());
}

/// Kriterium 3: Ablage scheitert → Bestätigung bleibt; `reconcile` holt nach,
/// idempotent; Absturz zwischen Bestätigung und Ablage verliert nichts.
#[test]
fn s3_03_reconcile_is_idempotent() {
    let w = world();
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    // Verwaiste Sperre der Schlüsselliste (abgebrochener früherer Prozess).
    let lock = w.server.git_dir.join("refs/minds/witness/keys.lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, b"").unwrap();
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    let out = w.push(&c, "refs/heads/main", &[]);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert_eq!(w.server_ref("refs/heads/main").as_deref(), Some(c.as_str()));
    assert!(
        out.minds
            .iter()
            .any(|l| l
                == "minds-witness: Warnung: 1 Quittung(en) nicht abgelegt; werden nachgeholt."),
        "{:?}",
        out.minds
    );
    assert!(hook.receipts(&c).is_empty());
    assert_eq!(hook.handoff("confirmed").len(), 1);
    // Zu jung: `reconcile` lässt die Sperre (vielleicht lebt ein Prozess).
    let (code, text) = run_bin("reconcile", &hook, "");
    assert_eq!(code, 2, "{text}");
    assert!(lock.exists());
    // Nachweislich verwaist: entfernt, Quittung abgelegt.
    std::fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120))
        .unwrap();
    let (code, text) = run_bin("reconcile", &hook, "");
    assert_eq!(code, 0, "{text}");
    assert_eq!(hook.receipts(&c).len(), 1);
    assert!(hook.handoff("confirmed").is_empty());
    assert!(hook.log().contains("stale_lock_removed"));
    let refs = witness_refs(&w.server);
    let (code, _) = run_bin("reconcile", &hook, "");
    assert_eq!(code, 0);
    assert_eq!(witness_refs(&w.server), refs, "Wiederholung ohne neue Refs");

    // Absturz zwischen Bestätigung und Ablage: Die Bestätigung liegt vor,
    // Quittungen fehlen. `reconcile` legt genau diese ab, zweimal dasselbe.
    let c2 = w.commit(&c, &[("src/a.rs", b"a2")]);
    write_hook(&w.server, "post-receive", "cat >/dev/null; exit 0");
    let out = w.push(&c2, "refs/heads/main", &[]);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    let h = handoff(&w, &hook);
    let (pid_name, record) = {
        let names = hook.handoff("pending");
        assert_eq!(names.len(), 1, "{names:?}");
        let r = h.read_pending(&names[0]).unwrap();
        (names[0].clone(), r)
    };
    let pending_file = hook.state.join("pending").join(&pid_name);
    let pending_bytes = std::fs::read(&pending_file).unwrap();
    // Kriterium 4: Die Quittung nennt das Binary des pre-receive, auch wenn
    // inzwischen ein anderes läuft (hier: anderer Hash im Datensatz).
    let mut record = record;
    let checked_by = HashStr::of_bytes(b"binary at pre-receive");
    record.witness_binary = checked_by.clone();
    let conf = Confirmation {
        confirmed_at: "2026-10-27T12:00:00Z".to_owned(),
        confirmed_updates: record.updates.clone(),
        pending: record,
        schema: CONFIRMED_SCHEMA.to_owned(),
    };
    assert!(h.confirm(&pid_name, &conf).unwrap());
    // Absturz vor dem Entfernen des Datensatzes: Bestätigung und Datensatz
    // liegen beide vor.
    std::fs::write(&pending_file, &pending_bytes).unwrap();
    assert!(hook.receipts(&c2).is_empty());
    let (code, text) = run_bin("reconcile", &hook, "");
    assert_eq!(code, 0, "{text}");
    let first = hook.receipts(&c2);
    assert_eq!(first.len(), 1);
    assert_eq!(hook.verify(&first[0]).witness.binary, checked_by);
    let (code, _) = run_bin("reconcile", &hook, "");
    assert_eq!(code, 0);
    assert_eq!(hook.receipts(&c2), first);
    // Der verbliebene Datensatz erzeugt nichts: `reconcile` liest nur
    // Bestätigungen; zuordnen könnte ihn nur derselbe receive-pack (PID und
    // Startzeit), dessen post-receive bereits lief. Er verfällt.
    assert!(hook.handoff("pending").contains(&pid_name));
    let (code, _) = run_bin("reconcile", &hook, "");
    assert_eq!(code, 0);
    assert_eq!(hook.receipts(&c2), first, "genau eine Quittung");
}

/// Kriterium 1: pre-receive signiert nie; ohne post-receive verfällt der
/// Datensatz, und es entsteht nie eine Quittung.
#[test]
fn s3_04_pending_expiry_never_signs() {
    let w = world();
    let hook = w.hook_with(&policy(Mode::Observe, &[], 100), |v| {
        v["pending_ttl_secs"] = serde_json::json!(1);
    });
    write_hook(&w.server, "update", "exit 1");
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    // Gits eigene Ablehnung (`update`-Hook) erscheint als fremde Zeile;
    // daher direkt `git push`.
    let out = w.client.push(&["origin", &format!("{c}:refs/heads/main")]);
    assert_ne!(out.exit, 0);
    // pre-receive hat nur gewarnt (Zustand, nie „angenommen“).
    assert!(
        out.minds.iter().all(|l| !l.contains("angenommen")),
        "{:?}",
        out.minds
    );
    // pre-receive hat angenommen und nur den Datensatz geschrieben.
    assert_eq!(hook.handoff("pending").len(), 1);
    assert!(
        witness_refs(&w.server).is_empty(),
        "pre-receive schrieb Refs"
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (code, text) = run_bin("reconcile", &hook, "");
    assert_eq!(code, 0, "{text}");
    assert!(hook.handoff("pending").is_empty());
    assert!(hook.log().contains("pending_expired"));
    assert!(witness_refs(&w.server).is_empty());
}

/// Kriterium 4: Signer-Datei zwischen pre- und post-receive geändert → die
/// Quittung trägt den Snapshot des pre-receive, und genau der liegt ab.
#[test]
fn s3_06_trust_snapshot_written_only_after_update() {
    let w = world();
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    let signers = w.fx.dir.path().join("allowed_signers");
    let original = std::fs::read_to_string(&signers).unwrap();
    let (_, other) = keypair(w.fx.dir.path(), "other");
    // Läuft nach pre-receive und vor post-receive.
    write_hook(
        &w.server,
        "update",
        &format!(
            "printf '%s\\n' 'other@test {other}' >> '{}'; exit 0",
            signers.display()
        ),
    );
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    let b = w.bind(&c, &[("src/a.rs", b"a1")]);
    let out = w.push(&c, "refs/heads/main", &b.refs);
    assert_eq!(out.exit, 0, "{}", out.stderr);
    assert_ne!(std::fs::read_to_string(&signers).unwrap(), original);
    let r = hook.verify(&hook.receipts(&c)[0]);
    let snap = hook.trust(r.trust.as_ref().unwrap()).unwrap();
    let parsed = TrustSnapshot::from_bytes(&snap).unwrap();
    assert_eq!(
        parsed.signers, original,
        "Snapshot aus den pre-receive-Bytes"
    );
    assert_eq!(r.seal.status, SealState::Valid);
    // Ein abgelehnter Push schreibt nichts unter refs/minds/witness/.
    let w2 = world();
    w2.hook(&policy(Mode::Enforce, &["refs/heads/main"], 100));
    let c2 = w2.commit(&w2.base, &[("src/a.rs", b"x")]);
    assert_ne!(w2.push(&c2, "refs/heads/main", &[]).exit, 0);
    assert!(witness_refs(&w2.server).is_empty());
}

/// Laufzeitmessung ohne Inhalte; `stats` wertet aus.
#[test]
fn s3_08_push_stats_without_content() {
    let w = world();
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    let path = format!("src/{MARKER}.rs");
    let c = w.commit(&w.base, &[(path.as_str(), MARKER.as_bytes())]);
    let b = w.bind(&c, &[(path.as_str(), MARKER.as_bytes())]);
    assert_eq!(w.push(&c, "refs/heads/main", &b.refs).exit, 0);
    let stats: Vec<serde_json::Value> = hook
        .log()
        .lines()
        .filter(|l| l.contains("push_stats"))
        .map(|l| {
            assert!(!l.contains(MARKER) && !l.contains("src/"), "{l}");
            serde_json::from_str(l).unwrap()
        })
        .collect();
    assert_eq!(stats.len(), 1);
    let s = &stats[0]["push_stats"];
    let procs = s["ssh_keygen_processes"].as_u64().unwrap();
    assert!(procs >= 2, "Bindung und Seal geprüft: {s}");
    assert_eq!(s["ssh_keygen_us"].as_array().unwrap().len() as u64, procs);
    assert!(s["total_us"].as_u64().unwrap() > 0);
    let out = std::process::Command::new(HOOK_BIN)
        .arg("stats")
        .arg(hook.state.join("log.jsonl"))
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        out.status.success() && text.contains("P50=") && text.contains("P99="),
        "{text}"
    );
}

/// Kriterium 2: Nie fremde Befunde bestätigen; Mehrdeutigkeit → keine
/// Quittung; Altlasten derselben PID werden nie verwendet.
#[test]
fn s3_09_parallel_pushes_never_cross_attest() {
    let w = world();
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    let h = handoff(&w, &hook);
    let me = std::process::id();
    let my_start = minds_witness_git::process::start_time(me);
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    // Objekte auf den Server, ohne Hooks (Ref später von Hand).
    let hooks_off = w.server.git_dir.join("hooks.off");
    std::fs::rename(w.server.git_dir.join("hooks"), &hooks_off).unwrap();
    assert_eq!(w.push(&c, "refs/heads/tmp", &[]).exit, 0);
    std::fs::rename(&hooks_off, w.server.git_dir.join("hooks")).unwrap();

    // Ein echter Befund für `c` aus einem pre-receive (Pusher alice).
    let base_update = format!("{} {c} refs/heads/main\n", w.base);
    let template = {
        let (code, text) = run_bin_env(
            "pre-receive",
            &hook,
            &base_update,
            &[("GL_USERNAME", "alice")],
        );
        assert_eq!(code, 0, "{text}");
        let mut c = h.candidates(me, my_start.as_deref()).unwrap();
        assert_eq!(c.len(), 1);
        c.remove(0)
    };
    let (alice_name, alice) = template;
    assert_eq!(alice.findings[0].pusher.username, "alice");
    // (a) Datensatz eines anderen Pushes (andere PID, Pusher mallory) mit
    // demselben Tripel: wird nie verwendet.
    let mut mallory = alice.clone();
    mallory.rp_pid = me + 1;
    mallory.nonce = "ff".repeat(16);
    for f in &mut mallory.findings {
        f.pusher.username = "mallory".to_owned();
    }
    h.write_pending(&mallory).unwrap();
    w.server.update_ref("refs/heads/main", &c);
    let (code, _) = run_bin("post-receive", &hook, &base_update);
    assert_eq!(code, 0);
    let receipts = hook.receipts(&c);
    assert_eq!(receipts.len(), 1);
    assert_eq!(hook.verify(&receipts[0]).pusher.username, "alice");
    let _ = alice_name;

    // (b) Zwei Datensätze derselben PID: mehrdeutig → keine Quittung.
    let c2 = w.commit(&c, &[("src/a.rs", b"a2")]);
    std::fs::rename(w.server.git_dir.join("hooks"), &hooks_off).unwrap();
    assert_eq!(w.push(&c2, "refs/heads/tmp2", &[]).exit, 0);
    std::fs::rename(&hooks_off, w.server.git_dir.join("hooks")).unwrap();
    let upd = format!("{c} {c2} refs/heads/main\n");
    let (code, _) = run_bin("pre-receive", &hook, &upd);
    assert_eq!(code, 0);
    let mut twin = h.candidates(me, my_start.as_deref()).unwrap().remove(0).1;
    twin.nonce = "ee".repeat(16);
    h.write_pending(&twin).unwrap();
    w.server.update_ref("refs/heads/main", &c2);
    let (code, _) = run_bin("post-receive", &hook, &upd);
    assert_eq!(code, 0);
    assert!(hook.receipts(&c2).is_empty());
    assert!(hook.log().contains("pending_ambiguous"));
    // Beide aus `pending/` genommen (nach `stale/`, dort von `reconcile`
    // verworfen), nie bestätigt.
    let mine = format!("{me}-");
    assert!(
        hook.handoff("pending")
            .iter()
            .all(|n| !n.starts_with(&mine)),
        "{:?}",
        hook.handoff("pending")
    );
    assert!(hook.handoff("confirmed").is_empty());

    // (c) + (e) Altlast derselben PID ohne Startzeit: Ein späterer
    // pre-receive (leerer Push) verschiebt sie, bevor er seinen eigenen
    // Datensatz schreibt; post-receive findet nur den leeren.
    let c3 = w.commit(&c2, &[("src/a.rs", b"a3")]);
    let mut old = alice.clone();
    old.rp_start = None;
    old.nonce = "dd".repeat(16);
    old.updates = vec![RefUpdate {
        old: minds_witness_core::CommitId::parse(&c2).unwrap(),
        new: minds_witness_core::CommitId::parse(&c3).unwrap(),
        ref_name: "refs/heads/main".to_owned(),
    }];
    for f in &mut old.findings {
        f.push.old = old.updates[0].old.clone();
        f.push.new = old.updates[0].new.clone();
    }
    h.write_pending(&old).unwrap();
    let (code, _) = run_bin("pre-receive", &hook, "");
    assert_eq!(code, 0);
    assert!(hook.log().contains("pending_stale"));
    std::fs::rename(w.server.git_dir.join("hooks"), &hooks_off).unwrap();
    assert_eq!(w.push(&c3, "refs/heads/main", &[]).exit, 0);
    std::fs::rename(&hooks_off, w.server.git_dir.join("hooks")).unwrap();
    let (code, _) = run_bin(
        "post-receive",
        &hook,
        &format!("{c2} {c3} refs/heads/main\n"),
    );
    assert_eq!(code, 0);
    assert!(
        hook.receipts(&c3).is_empty(),
        "Befund einer Altlast signiert"
    );
    let _ = HashStr::of_bytes(b"");
    let _: Option<PendingRecord> = None;
}

fn run_bin_env(sub: &str, hook: &Hook, stdin: &str, env: &[(&str, &str)]) -> (i32, String) {
    use std::io::Write;
    let mut cmd = std::process::Command::new(HOOK_BIN);
    cmd.arg(sub)
        .arg(&hook.config)
        .current_dir(&hook.server.git_dir)
        .env("GIT_DIR", ".")
        .env("HOME", &hook.server.home)
        .env("GIT_CONFIG_GLOBAL", hook.server.home.join("no-such-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Kriterium 2 mit echtem Doppel-Push: zwei Pushes desselben Tripels
/// gleichzeitig, nur einer gewinnt → genau eine Quittung, mit dessen Pusher.
#[test]
fn s3_09_real_double_push_yields_one_receipt() {
    let w = world();
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    let barrier = w.fx.dir.path().join("arrived");
    std::fs::create_dir_all(&barrier).unwrap();
    // Beide pre-receive sind durch, bevor eines der Updates läuft.
    write_hook(
        &w.server,
        "update",
        &format!(
            "touch '{d}/'$PPID; i=0; while [ $(ls '{d}' | wc -l) -lt 2 ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done; exit 0",
            d = barrier.display()
        ),
    );
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    let pushes: Vec<_> = ["alice", "bob"]
        .into_iter()
        .map(|who| {
            let client = w.client.clone();
            let spec = format!("{c}:refs/heads/main");
            std::thread::spawn(move || {
                let mut cmd = client.command();
                cmd.env("GL_USERNAME", who)
                    .args(["push", "origin", &spec])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                (who, cmd.status().unwrap().success())
            })
        })
        .collect();
    let results: Vec<_> = pushes.into_iter().map(|h| h.join().unwrap()).collect();
    let winners: Vec<&str> = results.iter().filter(|r| r.1).map(|r| r.0).collect();
    assert_eq!(winners.len(), 1, "genau ein Push gewinnt: {results:?}");
    assert_eq!(w.server_ref("refs/heads/main").as_deref(), Some(c.as_str()));
    let receipts = hook.receipts(&c);
    assert_eq!(receipts.len(), 1, "{results:?}");
    let pusher = hook.verify(&receipts[0]).pusher.username;
    assert!(
        winners.contains(&pusher.as_str()),
        "{pusher} nicht unter {winners:?}"
    );
}

/// Ein Hook ohne `exec` (Elternprozess ist eine Shell): post-receive findet
/// keinen Datensatz, es entsteht keine Quittung.
#[test]
fn s3_10_hook_without_exec_signs_nothing() {
    let w = world();
    let hook = w.hook(&policy(Mode::Observe, &[], 100));
    write_hook(
        &w.server,
        "post-receive",
        &format!(
            "'{HOOK_BIN}' post-receive '{}'; exit 0",
            hook.config.display()
        ),
    );
    let c = w.commit(&w.base, &[("src/a.rs", b"a1")]);
    assert_eq!(w.push(&c, "refs/heads/main", &[]).exit, 0);
    assert!(hook.receipts(&c).is_empty());
    assert!(hook.log().contains("receipt_without_pending"));
}
