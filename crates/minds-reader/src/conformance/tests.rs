//! Die Conformance an echten Repositories: welches Soll gilt, und jede
//! Regelart gegen einen echten Commit.

use super::*;

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fixture.git(&["init", "-q", "-b", "main"]);
        fixture
    }

    fn git(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(self.dir.path())
            .args(["-c", "user.name=A", "-c", "user.email=a@x"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn remove(&self, path: &str) {
        std::fs::remove_file(self.dir.path().join(path)).unwrap();
    }

    fn commit(&self, message: &str) -> CommitId {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.git(&["rev-parse", "HEAD"]).trim().parse().unwrap()
    }

    fn repo(&self) -> Repo {
        Repo::open(self.dir.path()).unwrap()
    }
}

const SPEC: &str = r#"{
  "version": "minds-spec-v1",
  "rules": [
    {"id": "app-not-db", "kind": "no-dependency", "crate": "app", "on": "db"},
    {"id": "db-is-leaf", "kind": "no-dependency", "crate": "db", "on": ["app", "core"]},
    {"id": "core-no-db", "kind": "no-import", "paths": "core/src/**", "import": "db::Pool"},
    {"id": "config", "kind": "json-schema", "schema": "schema/config.json", "files": "config/*.json"},
    {"id": "agents", "kind": "graph", "expect": "session", "paths": "core/**"}
  ]
}"#;

/// Ein kleiner Workspace: app → core → db.
fn workspace(fixture: &Fixture) {
    fixture.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"core\", \"db\"]\n",
    );
    fixture.write(
        "app/Cargo.toml",
        "[package]\nname = \"app\"\n[dependencies]\ncore = { path = \"../core\" }\n",
    );
    fixture.write(
        "core/Cargo.toml",
        "[package]\nname = \"core\"\n[dependencies]\ndb = { path = \"../db\" }\n",
    );
    fixture.write("db/Cargo.toml", "[package]\nname = \"db\"\n");
    fixture.write("core/src/lib.rs", "use db::{Conn, Pool};\n");
    fixture.write(
        "schema/config.json",
        r#"{"type":"object","required":["name"],"properties":{"name":{"type":"string"}}}"#,
    );
    fixture.write("config/a.json", r#"{"name":"a"}"#);
}

/// Ein Signierregister: was der Mensch signiert hat, wie `minds spec sign`
/// es tut (Pins der Schemas und `at` = der Commit). Wie der Store behält es
/// jeden Umschlag.
#[derive(Default, Clone)]
struct Signer {
    signed: Vec<SignedSpec>,
}

impl Signer {
    fn sign(&mut self, repo: &Repo, commit: CommitId) -> ContentHash {
        let Found::Spec(spec) = read_spec(repo, commit) else {
            panic!("no spec at {commit}");
        };
        let pins = minds_core::spec::pinned_paths(&spec)
            .into_iter()
            .map(|path| {
                let (_, bytes) = repo
                    .read_blob_bounded(commit, &path, 1 << 20)
                    .unwrap()
                    .unwrap();
                (path, pin_of(&bytes))
            })
            .collect();
        let signed = SignedSpec::new(spec.clone(), pins, &commit.to_string()).unwrap();
        self.signed.push(signed);
        spec.id().clone()
    }

    fn envelopes(&self) -> Vec<SignedSpec> {
        self.signed.clone()
    }

    /// Gültig ist, was dieser Mensch signiert hat.
    fn verify(&self, signed: &SignedSpec) -> Result<Option<String>, String> {
        Ok(self
            .signed
            .contains(signed)
            .then(|| "human@example".to_owned()))
    }
}

fn run(repo: &Repo, commit: CommitId, signer: &Signer, facts: GraphFacts) -> Option<Report> {
    let mut envelopes = || Ok(signer.envelopes());
    let mut verify = |signed: &SignedSpec| signer.verify(signed);
    let mut graph = move || facts.clone();
    evaluate(
        repo,
        commit,
        Inputs {
            envelopes: &mut envelopes,
            verify: &mut verify,
            graph: &mut graph,
        },
    )
}

fn outcomes(report: &Report) -> Vec<(String, Outcome)> {
    report
        .rows()
        .map(|row| (row.id.clone(), row.outcome))
        .collect()
}

fn rule<'r>(report: &'r Report, id: &str) -> &'r RuleResult {
    report.rules.iter().find(|r| r.id == id).unwrap()
}

#[test]
fn no_spec_no_report() {
    let fixture = Fixture::new();
    workspace(&fixture);
    let commit = fixture.commit("ohne Soll");
    let signer = Signer::default();
    assert!(run(&fixture.repo(), commit, &signer, GraphFacts::default()).is_none());
}

#[test]
fn each_rule_kind_against_a_signed_spec() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let commit = fixture.commit("Soll");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, commit);
    let facts = GraphFacts {
        sessions: 1,
        ..GraphFacts::default()
    };
    let report = run(&repo, commit, &signer, facts).unwrap();
    assert_eq!(
        outcomes(&report),
        [
            ("spec".into(), Outcome::Pass),
            ("app-not-db".into(), Outcome::Drift),
            ("db-is-leaf".into(), Outcome::Pass),
            ("core-no-db".into(), Outcome::Drift),
            ("config".into(), Outcome::Pass),
            ("agents".into(), Outcome::Pass),
        ]
    );
    assert_eq!(report.rules[0].observed, "app → core → db");
    assert_eq!(report.rules[2].observed, "core/src/lib.rs:1 uses db::Pool");
    assert_eq!(report.signer.as_deref(), Some("human@example"));
    assert_eq!(report.counts(), (4, 2, 0));
    assert_eq!(report.outcome(), Outcome::Drift);

    // Ohne Session: die Graph-Regel weicht ab; nicht entscheidbar: UNKNOWN.
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(rule(&report, "agents").outcome, Outcome::Drift);
    let unknown = GraphFacts {
        unknown: true,
        ..GraphFacts::default()
    };
    let report = run(&repo, commit, &signer, unknown).unwrap();
    assert_eq!(rule(&report, "agents").outcome, Outcome::Unknown);
}

/// Das Schema gehört zum Soll: Gelockert ohne neue Signatur ist DRIFT, auch
/// wenn `.minds/spec.json` unverändert ist.
#[test]
fn a_loosened_schema_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let signed_at = fixture.commit("Soll");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, signed_at);
    fixture.write("schema/config.json", "true");
    fixture.write("config/b.json", r#"{"other":1}"#);
    let commit = fixture.commit("Agent lockert das Schema");
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(report.spec.outcome, Outcome::Drift);
    assert!(
        report
            .spec
            .observed
            .contains("unsigned spec or pinned-file state"),
        "{}",
        report.spec.observed
    );
    let config = rule(&report, "config");
    assert_eq!(config.outcome, Outcome::Drift);
    assert_eq!(
        config.observed,
        "schema/config.json changed since the spec was signed"
    );
    // Neu signiert: Das neue Schema gilt.
    signer.sign(&repo, commit);
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(report.spec.outcome, Outcome::Pass);
    assert_eq!(rule(&report, "config").outcome, Outcome::Pass);
}

#[test]
fn data_contract_violations_and_unreadable_manifests() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let signed_at = fixture.commit("Soll");
    fixture.write("config/b.json", r#"{"name": 7}"#);
    fixture.write(
        "db/Cargo.toml",
        "[package]\nname = \"db\"\n[dependencies]\nx = { path = \"y\",\n  version = \"1\" }\n",
    );
    let commit = fixture.commit("kaputt");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, signed_at);
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    let config = rule(&report, "config");
    assert_eq!(config.outcome, Outcome::Drift);
    assert_eq!(config.observed, "config/b.json: /name violates `type`");
    // Ein unlesbares Manifest: nie PASS.
    let leaf = rule(&report, "db-is-leaf");
    assert_eq!(leaf.outcome, Outcome::Unknown, "{}", leaf.observed);
    // Die Graph-Regel greift nicht: nichts unter core/ geändert.
    let agents = rule(&report, "agents");
    assert_eq!(agents.outcome, Outcome::Pass);
    assert!(agents.observed.starts_with("not triggered"));
}

/// Umbenennungen verlieren keine Kante — auch nicht über
/// `[workspace.dependencies]`.
#[test]
fn renamed_dependencies_keep_their_edges() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"core\", \"db\"]\n[workspace.dependencies]\ninnocent = { package = \"db\", path = \"db\" }\n",
    );
    fixture.write(
        "app/Cargo.toml",
        "[package]\nname = \"app\"\n[dependencies]\ninnocent.workspace = true\n",
    );
    fixture.write("core/Cargo.toml", "[package]\nname = \"core\"\n");
    fixture.write("core/src/lib.rs", "use storage::Pool;\n");
    fixture.write(
        "core/Cargo.toml",
        "[package]\nname = \"core\"\n[dependencies]\nstorage = { package = \"db\", path = \"../db\" }\n",
    );
    fixture.write(SPEC_PATH, SPEC);
    let commit = fixture.commit("umbenannt");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, commit);
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(rule(&report, "app-not-db").observed, "app → db");
    let import = rule(&report, "core-no-db");
    assert_eq!(import.outcome, Outcome::Drift, "{}", import.observed);
    assert_eq!(import.observed, "core/src/lib.rs:1 uses storage::Pool");
}

/// Symlinks unter einem Glob: Ihr Inhalt ist nicht geprüft — UNKNOWN.
#[cfg(unix)]
#[test]
fn symlinks_under_a_rule_are_unknown() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write("vendor/x.rs", "use db::Pool;\n");
    std::os::unix::fs::symlink(
        "../../vendor/x.rs",
        fixture.dir.path().join("core/src/evil.rs"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "../db/Cargo.toml",
        fixture.dir.path().join("app/Other.toml"),
    )
    .unwrap();
    fixture.write(SPEC_PATH, SPEC);
    let commit = fixture.commit("Symlink");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, commit);
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    let import = rule(&report, "core-no-db");
    assert_eq!(import.outcome, Outcome::Unknown);
    assert!(import.observed.contains("symlink"), "{}", import.observed);
}

/// Eine Änderung am Soll ohne gültige Signatur ist DRIFT — und die Regeln
/// kommen weiter aus dem signierten Soll, nicht aus dem gelockerten.
#[test]
fn an_unsigned_change_to_the_spec_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let signed_at = fixture.commit("Soll");
    let loosened = SPEC.replace(
        r#"    {"id": "app-not-db", "kind": "no-dependency", "crate": "app", "on": "db"},
"#,
        "",
    );
    assert_ne!(loosened, SPEC);
    fixture.write(SPEC_PATH, &loosened);
    fixture.commit("Agent lockert das Soll");
    let commit = fixture.commit("noch einer");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    let id = signer.sign(&repo, signed_at);
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(report.spec.outcome, Outcome::Drift);
    assert!(report.spec.observed.contains("carries no valid signature"));
    assert_eq!(report.active, Some((id, signed_at)));
    assert!(report.rules.iter().any(|r| r.id == "app-not-db"));

    // Signiert der Mensch das neue Soll, gilt es.
    signer.sign(&repo, commit);
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(report.spec.outcome, Outcome::Pass);
    assert!(!report.rules.iter().any(|r| r.id == "app-not-db"));
}

/// Rückfall: v1 und v2 sind signiert, v2 später. Setzt der Agent die Datei
/// auf v1 zurück, gilt v2 weiter — bis der Mensch v1 neu signiert.
#[test]
fn a_rollback_to_an_older_signed_spec_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    let v1 = SPEC.replace(
        r#"    {"id": "app-not-db", "kind": "no-dependency", "crate": "app", "on": "db"},
"#,
        "",
    );
    fixture.write(SPEC_PATH, &v1);
    let at_v1 = fixture.commit("v1");
    fixture.write(SPEC_PATH, SPEC);
    let at_v2 = fixture.commit("v2");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, at_v1);
    let v2_id = signer.sign(&repo, at_v2);
    fixture.write(SPEC_PATH, &v1);
    let commit = fixture.commit("Agent: zurück auf v1");
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
    assert!(
        report
            .spec
            .observed
            .contains("sign the current state again")
    );
    assert_eq!(report.active, Some((v2_id, at_v2)));
    assert!(report.rules.iter().any(|r| r.id == "app-not-db"));
    // Auch ein zurückgespulter Store (nur der alte Umschlag) hilft nicht.
    let mut rewound = Signer::default();
    rewound.sign(&repo, at_v1);
    let report = run(&repo, commit, &rewound, GraphFacts::default()).unwrap();
    assert_eq!(report.spec.outcome, Outcome::Drift);
    // Ältere Commits bleiben unter ihrem Umschlag gültig.
    assert_eq!(
        run(&repo, at_v1, &signer, GraphFacts::default())
            .unwrap()
            .spec
            .outcome,
        Outcome::Pass
    );
    // Der Mensch signiert v1 bewusst neu: dann gilt es.
    signer.sign(&repo, commit);
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(report.spec.outcome, Outcome::Pass);
}

/// Rückfall über einen Zwischen-Commit: v2 gilt, der Agent löscht das Soll
/// und stellt dann v1 wieder her — auch nach vielen Commits.
#[test]
fn a_rollback_through_a_removal_or_padding_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    let v1 = SPEC.replace(
        r#"    {"id": "app-not-db", "kind": "no-dependency", "crate": "app", "on": "db"},
"#,
        "",
    );
    fixture.write(SPEC_PATH, &v1);
    let at_v1 = fixture.commit("v1");
    fixture.write(SPEC_PATH, SPEC);
    let at_v2 = fixture.commit("v2");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, at_v1);
    signer.sign(&repo, at_v2);
    fixture.remove(SPEC_PATH);
    fixture.commit("weg");
    fixture.write(SPEC_PATH, &v1);
    let mut commit = fixture.commit("v1 zurück");
    for i in 0..140 {
        commit = fixture.commit(&format!("pad {i}"));
    }
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
    assert!(report.rules.iter().any(|r| r.id == "app-not-db"));
}

/// Ein Merge mit der alten Seite als erstem Elternteil: v2 liegt trotzdem
/// auf dem Weg.
#[test]
fn a_merge_back_to_an_older_spec_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    let v1 = SPEC.replace(
        r#"    {"id": "app-not-db", "kind": "no-dependency", "crate": "app", "on": "db"},
"#,
        "",
    );
    fixture.write(SPEC_PATH, &v1);
    let at_v1 = fixture.commit("v1");
    fixture.write(SPEC_PATH, SPEC);
    let at_v2 = fixture.commit("v2");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, at_v1);
    signer.sign(&repo, at_v2);
    fixture.git(&["checkout", "-q", "-b", "old", &at_v1.to_string()]);
    fixture.write("notes.txt", "x");
    fixture.commit("alt");
    fixture.git(&["merge", "-q", "-s", "ours", "-m", "merge", "main"]);
    let merge: CommitId = fixture.git(&["rev-parse", "HEAD"]).trim().parse().unwrap();
    let report = run(&repo, merge, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
    assert_eq!(report.active.as_ref().map(|a| a.1), Some(at_v2));
}

/// Zwei Zweige, je mit eigenem signiertem Soll; ein Merge behält das eine:
/// Das Ergebnis braucht eine eigene Signatur.
#[test]
fn a_merge_of_differently_signed_specs_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    let base = fixture.commit("Basis");
    let v1 = SPEC.replace(
        r#"    {"id": "app-not-db", "kind": "no-dependency", "crate": "app", "on": "db"},
"#,
        "",
    );
    fixture.write(SPEC_PATH, &v1);
    let at_v1 = fixture.commit("v1 auf main");
    fixture.git(&["checkout", "-q", "-b", "feature", &base.to_string()]);
    fixture.write(SPEC_PATH, SPEC);
    let at_v2 = fixture.commit("v2 auf feature");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, at_v1);
    signer.sign(&repo, at_v2);
    fixture.git(&["checkout", "-q", "main"]);
    fixture.git(&["merge", "-q", "-s", "ours", "-m", "merge", "feature"]);
    let merge: CommitId = fixture.git(&["rev-parse", "HEAD"]).trim().parse().unwrap();
    let report = run(&repo, merge, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
    assert!(
        report
            .spec
            .observed
            .contains("sign the current state again")
    );
    // Signiert der Mensch das Ergebnis, gilt es.
    signer.sign(&repo, merge);
    let report = run(&repo, merge, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Pass,
        "{}",
        report.spec.observed
    );
}

/// Untergeschobene Umschläge außerhalb der Historie kosten keine Prüfung.
#[test]
fn envelopes_outside_the_history_are_not_verified() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let commit = fixture.commit("Soll");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, commit);
    let other = Spec::parse(SPEC.replace("app-not-db", "x").as_bytes()).unwrap();
    let pins = minds_core::spec::pinned_paths(&other)
        .into_iter()
        .map(|path| (path, pin_of(b"{}")))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut planted: Vec<SignedSpec> = (0..100)
        .map(|i| SignedSpec::new(other.clone(), pins.clone(), &format!("{i:040x}")).unwrap())
        .collect();
    planted.extend(signer.envelopes());
    let mut envelopes = || Ok(planted.clone());
    let mut verified = 0;
    let mut verify = |signed: &SignedSpec| {
        verified += 1;
        signer.verify(signed)
    };
    let report = evaluate(
        &repo,
        commit,
        Inputs {
            envelopes: &mut envelopes,
            verify: &mut verify,
            graph: &mut GraphFacts::default,
        },
    )
    .unwrap();
    assert_eq!(report.spec.outcome, Outcome::Pass);
    assert_eq!(verified, 1);
}

/// Ein Signatur-Commit ohne den älteren als Vorfahren (verwaist), dann ein
/// Merge auf das ältere Soll: Beide sind maximal — das Ergebnis braucht eine
/// Signatur.
#[test]
fn an_orphan_signing_commit_merged_back_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    let v1 = SPEC.replace(
        r#"    {"id": "app-not-db", "kind": "no-dependency", "crate": "app", "on": "db"},
"#,
        "",
    );
    fixture.write(SPEC_PATH, &v1);
    let at_v1 = fixture.commit("v1");
    fixture.git(&["checkout", "-q", "--orphan", "strict"]);
    fixture.write(SPEC_PATH, SPEC);
    let at_v2 = fixture.commit("v2 (verwaist)");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, at_v1);
    signer.sign(&repo, at_v2);
    fixture.git(&["checkout", "-q", "main"]);
    fixture.git(&[
        "merge",
        "-q",
        "--allow-unrelated-histories",
        "-s",
        "ours",
        "-m",
        "merge",
        "strict",
    ]);
    let merge: CommitId = fixture.git(&["rev-parse", "HEAD"]).trim().parse().unwrap();
    let report = run(&repo, merge, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
}

/// Derselbe Merge, aber der Agent hat den neueren Umschlag aus dem Store
/// entfernt: Die Frische hängt an der Historie, nicht am Store.
#[test]
fn a_merge_back_stays_drift_when_the_newer_envelope_is_gone() {
    let fixture = Fixture::new();
    workspace(&fixture);
    let base = fixture.commit("Basis");
    let v1 = SPEC.replace(
        r#"    {"id": "app-not-db", "kind": "no-dependency", "crate": "app", "on": "db"},
"#,
        "",
    );
    fixture.write(SPEC_PATH, &v1);
    let at_v1 = fixture.commit("v1 auf main");
    fixture.git(&["checkout", "-q", "-b", "feature", &base.to_string()]);
    fixture.write(SPEC_PATH, SPEC);
    fixture.commit("v2 auf feature");
    let repo = fixture.repo();
    let mut only_v1 = Signer::default();
    only_v1.sign(&repo, at_v1);
    fixture.git(&["checkout", "-q", "main"]);
    fixture.git(&["merge", "-q", "-s", "ours", "-m", "merge", "feature"]);
    let merge: CommitId = fixture.git(&["rev-parse", "HEAD"]).trim().parse().unwrap();
    let report = run(&repo, merge, &only_v1, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
}

/// Dasselbe Soll, auf einem Zweig mit lockerem Schema signiert, auf main mit
/// strengem: Ein Merge mit dem lockeren Schema ist DRIFT.
#[test]
fn a_looser_pin_of_the_same_spec_merged_back_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let base = fixture.commit("Soll");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, base);
    fixture.git(&["checkout", "-q", "-b", "loose", &base.to_string()]);
    fixture.write("schema/config.json", "true");
    let at_loose = fixture.commit("locker");
    signer.sign(&repo, at_loose);
    fixture.git(&["checkout", "-q", "main"]);
    fixture.write(
        "schema/config.json",
        r#"{"type":"object","required":["name"],"additionalProperties":false,"properties":{"name":{"type":"string"}}}"#,
    );
    let at_strict = fixture.commit("streng");
    signer.sign(&repo, at_strict);
    fixture.git(&["merge", "-q", "-X", "theirs", "-m", "merge", "loose"]);
    fixture.write("config/b.json", r#"{"other":1}"#);
    let merge = fixture.commit("Agent");
    let report = run(&repo, merge, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
}

/// Fehlt das Soll und lässt sich die Historie nicht vollständig laufen,
/// verschwindet der Bericht nicht: UNKNOWN.
#[test]
fn a_removed_spec_in_a_shallow_history_is_unknown() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let signed_at = fixture.commit("Soll");
    fixture.remove(SPEC_PATH);
    fixture.commit("weg");
    fixture.write("notes.txt", "x");
    fixture.commit("weiter");
    let shallow = tempfile::tempdir().unwrap();
    let source = format!("file://{}", fixture.dir.path().display());
    let out = std::process::Command::new("git")
        .args(["clone", "-q", "--depth", "2", &source])
        .arg(shallow.path().join("c"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let repo = Repo::open(shallow.path().join("c")).unwrap();
    let head = repo.head().unwrap().commit().unwrap();
    let mut signer = Signer::default();
    signer.sign(&fixture.repo(), signed_at);
    // `at` liegt im flachen Klon nicht vor — ein Umschlag mit lokalem `at`
    // macht es entscheidungsrelevant: hier der Kopf selbst als `at`.
    let mut local = signer.clone();
    let spec = Spec::parse(SPEC.as_bytes()).unwrap();
    let pins = minds_core::spec::pinned_paths(&spec)
        .into_iter()
        .map(|path| (path, pin_of(b"{}")))
        .collect();
    local
        .signed
        .push(SignedSpec::new(spec, pins, &head.to_string()).unwrap());
    let report = run(&repo, head, &local, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Unknown,
        "{}",
        report.spec.observed
    );
}

/// ABA: strict signiert, loose signiert, strict wieder signiert — der Agent
/// setzt loose zurück. Der loose-Umschlag gilt nicht mehr: Nach ihm kam ein
/// anderer Stand.
#[test]
fn a_rollback_to_a_state_that_existed_before_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    let loose = SPEC.replace(
        r#"    {"id": "app-not-db", "kind": "no-dependency", "crate": "app", "on": "db"},
"#,
        "",
    );
    fixture.write(SPEC_PATH, SPEC);
    let a1 = fixture.commit("strict");
    fixture.write(SPEC_PATH, &loose);
    let a2 = fixture.commit("loose");
    fixture.write(SPEC_PATH, SPEC);
    let a3 = fixture.commit("strict wieder");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, a1);
    signer.sign(&repo, a2);
    signer.sign(&repo, a3);
    fixture.write(SPEC_PATH, &loose);
    let commit = fixture.commit("Agent: loose");
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
    assert_eq!(report.active.as_ref().map(|a| a.1), Some(a3));
    // Dasselbe mit einem Schema-Pin.
    let strict_schema =
        r#"{"type":"object","required":["name"],"properties":{"name":{"type":"string"}}}"#;
    fixture.write(SPEC_PATH, SPEC);
    fixture.write("schema/config.json", "true");
    let b1 = fixture.commit("Schema locker");
    fixture.write("schema/config.json", strict_schema);
    let b2 = fixture.commit("Schema streng");
    signer.sign(&repo, b1);
    signer.sign(&repo, b2);
    fixture.write("schema/config.json", "true");
    let commit = fixture.commit("Agent: Schema locker");
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
}

/// Der Agent pflanzt das strenge Schema vorab in einen Seitenzweig, der in
/// den signierten lockeren Stand eingeht; später wird streng signiert, dann
/// lockert der Agent wieder: DRIFT.
#[test]
fn a_preplanted_strict_pin_does_not_bless_a_rollback() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    fixture.write("schema/config.json", "true");
    let base = fixture.commit("locker");
    let strict = r#"{"type":"object","required":["name"],"additionalProperties":false,"properties":{"name":{"type":"string"}}}"#;
    fixture.git(&["checkout", "-q", "-b", "plant", &base.to_string()]);
    fixture.write("schema/config.json", strict);
    fixture.commit("gepflanzt");
    fixture.git(&["checkout", "-q", "main"]);
    fixture.git(&["merge", "-q", "-s", "ours", "-m", "merge", "plant"]);
    let at_loose: CommitId = fixture.git(&["rev-parse", "HEAD"]).trim().parse().unwrap();
    fixture.write("schema/config.json", strict);
    let at_strict = fixture.commit("streng");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, at_loose);
    signer.sign(&repo, at_strict);
    fixture.write("schema/config.json", "true");
    fixture.write("config/b.json", r#"{"other":1}"#);
    let commit = fixture.commit("Agent: locker");
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
}

/// Untergeschobene Umschläge mit dem Soll des Commits, aber falschem Pin,
/// kosten keine Signaturprüfung.
#[test]
fn forged_envelopes_cost_no_check_before_state_match() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let commit = fixture.commit("Soll");
    let repo = fixture.repo();
    let spec = Spec::parse(SPEC.as_bytes()).unwrap();
    let forged: Vec<SignedSpec> = (0..100)
        .map(|i| {
            let pins = minds_core::spec::pinned_paths(&spec)
                .into_iter()
                .map(|path| (path, pin_of(format!("{i}").as_bytes())))
                .collect();
            SignedSpec::new(spec.clone(), pins, &commit.to_string()).unwrap()
        })
        .collect();
    let mut envelopes = || Ok(forged.clone());
    let mut verified = 0;
    let mut verify = |_: &SignedSpec| {
        verified += 1;
        Ok(None)
    };
    let report = evaluate(
        &repo,
        commit,
        Inputs {
            envelopes: &mut envelopes,
            verify: &mut verify,
            graph: &mut GraphFacts::default,
        },
    )
    .unwrap();
    assert_eq!(report.spec.outcome, Outcome::Drift);
    assert_eq!(verified, 0);
}

/// In einem Shallow Clone fehlt der Weg zum Signatur-Commit: UNKNOWN.
#[test]
fn a_shallow_history_is_unknown() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let signed_at = fixture.commit("Soll");
    fixture.write("notes.txt", "x");
    fixture.commit("weiter");
    let shallow = tempfile::tempdir().unwrap();
    let source = format!("file://{}", fixture.dir.path().display());
    let out = std::process::Command::new("git")
        .args(["clone", "-q", "--depth", "1", &source])
        .arg(shallow.path().join("c"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let repo = Repo::open(shallow.path().join("c")).unwrap();
    let head = repo.head().unwrap().commit().unwrap();
    let mut signer = Signer::default();
    signer.sign(&fixture.repo(), signed_at);
    let report = run(&repo, head, &signer, GraphFacts::default()).unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Unknown,
        "{}",
        report.spec.observed
    );
}

/// Ein Schema wird verschärft und neu signiert; der Agent spult den Store
/// auf den alten Umschlag zurück und stellt das alte Schema wieder her.
#[test]
fn a_rewound_store_does_not_restore_an_old_schema() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let at_a = fixture.commit("Soll");
    let repo = fixture.repo();
    let mut old = Signer::default();
    old.sign(&repo, at_a);
    let strict = r#"{"type":"object","required":["name"],"additionalProperties":false,"properties":{"name":{"type":"string"}}}"#;
    fixture.write("schema/config.json", strict);
    let at_b = fixture.commit("Schema verschärft");
    let mut both = old.clone();
    both.sign(&repo, at_b);
    assert_eq!(
        run(&repo, at_b, &both, GraphFacts::default())
            .unwrap()
            .spec
            .outcome,
        Outcome::Pass
    );
    fixture.write(
        "schema/config.json",
        r#"{"type":"object","required":["name"],"properties":{"name":{"type":"string"}}}"#,
    );
    let commit = fixture.commit("Agent: altes Schema");
    for signer in [&old, &both] {
        let report = run(&repo, commit, signer, GraphFacts::default()).unwrap();
        assert_eq!(
            report.spec.outcome,
            Outcome::Drift,
            "{}",
            report.spec.observed
        );
    }
    // Ältere Commits bleiben unter ihrem Umschlag gültig.
    assert_eq!(
        run(&repo, at_a, &both, GraphFacts::default())
            .unwrap()
            .spec
            .outcome,
        Outcome::Pass
    );
}

/// Ein Umschlag, der für eine andere Historie signiert wurde, gilt hier
/// nicht.
#[test]
fn a_spec_signed_elsewhere_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let commit = fixture.commit("Soll");
    let repo = fixture.repo();
    let spec = Spec::parse(SPEC.as_bytes()).unwrap();
    let pins = minds_core::spec::pinned_paths(&spec)
        .into_iter()
        .map(|path| (path, pin_of(b"{}")))
        .collect();
    let foreign = SignedSpec::new(spec, pins, &"ab".repeat(20)).unwrap();
    let mut envelopes = || Ok(vec![foreign.clone()]);
    let mut verify = |_: &SignedSpec| Ok(Some("human@example".to_owned()));
    let report = evaluate(
        &repo,
        commit,
        Inputs {
            envelopes: &mut envelopes,
            verify: &mut verify,
            graph: &mut GraphFacts::default,
        },
    )
    .unwrap();
    assert_eq!(
        report.spec.outcome,
        Outcome::Drift,
        "{}",
        report.spec.observed
    );
    assert!(report.spec.observed.contains("outside this history"));
    assert!(report.rules.is_empty());
}

#[test]
fn a_removed_or_broken_spec_is_drift() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let signed_at = fixture.commit("Soll");
    fixture.remove(SPEC_PATH);
    let removed = fixture.commit("weg");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, signed_at);
    let report = run(&repo, removed, &signer, GraphFacts::default()).unwrap();
    assert_eq!(report.spec.outcome, Outcome::Drift);
    assert!(report.spec.observed.ends_with("was removed"));
    assert_eq!(report.rules.len(), 5);

    fixture.write(SPEC_PATH, "{ kaputt");
    let broken = fixture.commit("kaputt");
    let report = run(&repo, broken, &signer, GraphFacts::default()).unwrap();
    assert_eq!(report.spec.outcome, Outcome::Drift);
    assert!(report.spec.observed.contains("unreadable"));
    assert_eq!(report.here, None);
}

/// Nie signiert: Die Regeln des Agenten werden nicht bewertet; nicht
/// prüfbar: Sie stehen da, als ungeprüft markiert.
#[test]
fn never_signed_and_unchecked_specs() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let commit = fixture.commit("Soll");
    let repo = fixture.repo();
    let report = run(&repo, commit, &Signer::default(), GraphFacts::default()).unwrap();
    assert_eq!(report.spec.outcome, Outcome::Drift);
    assert!(report.rules.is_empty());
    assert_eq!(report.signer, None);

    let mut signer = Signer::default();
    signer.sign(&repo, commit);
    let mut envelopes = || Ok(signer.envelopes());
    let mut unchecked = |_: &SignedSpec| Err("no ssh-keygen".to_owned());
    let report = evaluate(
        &repo,
        commit,
        Inputs {
            envelopes: &mut envelopes,
            verify: &mut unchecked,
            graph: &mut GraphFacts::default,
        },
    )
    .unwrap();
    assert_eq!(report.spec.outcome, Outcome::Unknown);
    assert!(report.spec.observed.contains("no ssh-keygen"));
    assert!(
        report
            .rules
            .iter()
            .all(|r| r.expected.ends_with("(unverified spec)"))
    );
    // Ohne geprüften Pin ist kein Schema bestanden.
    assert_eq!(rule(&report, "config").outcome, Outcome::Unknown);
}

/// Die Umschläge werden einmal gefragt; die Graph-Fakten nur, wenn eine
/// Graph-Regel sie braucht.
#[test]
fn envelopes_and_facts_are_asked_sparingly() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let signed_at = fixture.commit("Soll");
    fixture.write(SPEC_PATH, &SPEC.replace("app-not-db", "app-not-db-2"));
    for i in 0..5 {
        fixture.commit(&format!("c{i}"));
    }
    let commit = fixture.commit("spitze");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, signed_at);
    let mut asked = 0;
    let mut envelopes = || {
        asked += 1;
        Ok(signer.envelopes())
    };
    let mut verified = 0;
    let mut verify = |signed: &SignedSpec| {
        verified += 1;
        signer.verify(signed)
    };
    let mut facts_asked = 0;
    let mut graph = || {
        facts_asked += 1;
        GraphFacts::default()
    };
    let report = evaluate(
        &repo,
        commit,
        Inputs {
            envelopes: &mut envelopes,
            verify: &mut verify,
            graph: &mut graph,
        },
    )
    .unwrap();
    assert_eq!(report.spec.outcome, Outcome::Drift);
    assert_eq!(asked, 1);
    assert_eq!(verified, 1);
    // `agents` greift nicht (nichts unter core/ geändert): keine Fakten.
    assert_eq!(facts_asked, 0);
}

/// Ein Baum, dessen Bytes nicht zu seiner Id passen: UNKNOWN, nie PASS.
#[test]
fn a_damaged_tree_is_unknown() {
    let fixture = Fixture::new();
    workspace(&fixture);
    fixture.write(SPEC_PATH, SPEC);
    let commit = fixture.commit("Soll");
    let repo = fixture.repo();
    let mut signer = Signer::default();
    signer.sign(&repo, commit);
    // Das lose Objekt des `config`-Baums durch fremde Bytes ersetzen.
    let tree = fixture.git(&["rev-parse", "HEAD:config"]);
    let tree = tree.trim();
    let other = fixture.git(&["rev-parse", "HEAD:schema"]);
    let objects = fixture.dir.path().join(".git/objects");
    let target = objects.join(&tree[..2]).join(&tree[2..]);
    let source = objects.join(&other.trim()[..2]).join(&other.trim()[2..]);
    let mut perms = std::fs::metadata(&target).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&target, perms).unwrap();
    std::fs::copy(source, &target).unwrap();
    let report = run(&repo, commit, &signer, GraphFacts::default()).unwrap();
    for id in ["app-not-db", "core-no-db", "config"] {
        assert_eq!(rule(&report, id).outcome, Outcome::Unknown, "{id}");
    }
}

#[test]
fn path_to_finds_the_shortest_chain() {
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    edges
        .entry("a".into())
        .or_default()
        .extend(["b".into(), "x".into()]);
    edges.entry("b".into()).or_default().insert("c".into());
    edges.entry("c".into()).or_default().insert("a".into());
    assert_eq!(
        path_to(&edges, "a", &["c".into()]),
        Some(vec!["a".into(), "b".into(), "c".into()])
    );
    assert_eq!(path_to(&edges, "a", &["z".into()]), None);
    // Ein Zyklus zurück zum Start ist kein Treffer auf sich selbst.
    assert_eq!(path_to(&edges, "a", &["a".into()]), None);
}
