//! EA-18a — das Ergebnis von Test- und Benchmark-Läufen (`ToolCall::outcome`)
//! gegen **aufgezeichnete** Runner-Ausgaben (`fixtures/claude-code/bash-*`,
//! siehe README dort).
//!
//! Jeder Test fährt den echten Pfad: Payload → `hook_event::parse` →
//! `secretwall::guard` → `Journal::append` → `adapter::checkpoint`, und für
//! das Envelope zusätzlich durch die Redaction.

use minds_capture::{Checkpoint, Journal, adapter, clock, hook_event, secretwall};
use minds_core::{BenchValue, CaptureNote, ExecClass, ExecOutcome, Session, TestCounts, ToolCall};

macro_rules! fixture {
    ($name:literal) => {
        (
            include_str!(concat!("fixtures/claude-code/", $name, ".pre.json")),
            include_str!(concat!("fixtures/claude-code/", $name, ".post.json")),
        )
    };
}

/// Golden: die kanonische Form des Ergebnisses aus `bash-cargo-test-fail`.
const CARGO_TEST_FAIL_CANONICAL: &str = r#"{"class":"test","command":["cargo","test","--features","broken"],"exit_code":101,"runner":"cargo-test","tests":{"failed":1,"ignored":1,"passed":2}}"#;

/// Golden: blake3 dieser kanonischen Bytes.
const CARGO_TEST_FAIL_BLAKE3: &str =
    "bb54377d0784fa3dc671381b73e106307dcbd7b7883c5e67e327067581902b53";

fn at(n: u64) -> (String, u64) {
    let nanos = 1_790_000_000_000_000_000 + n * 1_000_000_000;
    (clock::rfc3339_from_nanos(nanos), nanos)
}

fn feed(journal: &Journal, payload: &str, seq: u64) {
    let mut parsed =
        hook_event::parse(payload.as_bytes().to_vec(), "claude-code", None, at(seq)).unwrap();
    secretwall::guard(&mut parsed.event);
    journal.append(&parsed.key, parsed.event).unwrap();
}

fn checkpoint(journal: &Journal) -> Session {
    let keys = journal.sessions().unwrap().keys;
    assert_eq!(keys.len(), 1, "genau eine Session erwartet");
    let events = journal.read(&keys[0]).unwrap().events;
    let policy = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let ctx = Checkpoint {
        root: None,
        commit: None,
        tracked: None,
        redaction: Some(&policy),
    };
    adapter::checkpoint(&keys[0], &events, &ctx)
}

/// Alle Paare in eine Session, in Reihenfolge.
fn session_of(pairs: &[(&str, &str)]) -> Session {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(tmp.path());
    for (i, (pre, post)) in pairs.iter().enumerate() {
        feed(&journal, pre, 2 * i as u64);
        feed(&journal, post, 2 * i as u64 + 1);
    }
    checkpoint(&journal)
}

fn calls(session: &Session) -> Vec<&ToolCall> {
    session.turns.iter().flat_map(|t| &t.tool_calls).collect()
}

/// Der eine Bash-Aufruf eines Paars.
fn single_call((pre, post): (&str, &str)) -> ToolCall {
    let session = session_of(&[(pre, post)]);
    let calls = calls(&session);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "Bash");
    calls[0].clone()
}

fn outcome(pair: (&str, &str)) -> Option<ExecOutcome> {
    single_call(pair).outcome
}

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn counts(passed: u64, failed: u64, ignored: u64) -> Option<TestCounts> {
    Some(TestCounts {
        passed,
        failed,
        ignored,
    })
}

#[test]
fn outcome_cargo_test_pass() {
    // Unit-Tests (2 ok, 1 ignored) plus Doc-Tests (0): summiert. Bei Erfolg
    // trägt der Payload keinen Exit-Code.
    assert_eq!(
        outcome(fixture!("bash-cargo-test-pass")),
        Some(ExecOutcome {
            class: ExecClass::Test,
            runner: "cargo-test".into(),
            command: argv(&["cargo", "test"]),
            exit_code: None,
            tests: counts(2, 0, 1),
            benches: Vec::new(),
        })
    );
    // nextest: dieselben Tests, gezählt aus der `Summary`-Zeile (gefärbt).
    assert_eq!(
        outcome(fixture!("bash-nextest-pass")),
        Some(ExecOutcome {
            class: ExecClass::Test,
            runner: "cargo-nextest".into(),
            command: argv(&["cargo", "nextest", "run"]),
            exit_code: None,
            tests: counts(2, 0, 1),
            benches: Vec::new(),
        })
    );
    // Ein erlaubter `RUST_LOG=`-Präfix gehört nicht zum argv.
    assert_eq!(
        outcome(fixture!("bash-env-prefix")),
        Some(ExecOutcome {
            class: ExecClass::Test,
            runner: "cargo-test".into(),
            command: argv(&["cargo", "test"]),
            exit_code: None,
            tests: counts(2, 0, 1),
            benches: Vec::new(),
        })
    );
}

#[test]
fn outcome_cargo_test_fail() {
    let got = outcome(fixture!("bash-cargo-test-fail")).unwrap();
    assert_eq!(
        got,
        ExecOutcome {
            class: ExecClass::Test,
            runner: "cargo-test".into(),
            command: argv(&["cargo", "test", "--features", "broken"]),
            exit_code: Some(101),
            tests: counts(2, 1, 1),
            benches: Vec::new(),
        }
    );
    // Absolut, nicht nur relativ: kanonische Bytes und ihr Hash.
    let canonical = minds_core::to_canonical_string(&got).unwrap();
    assert_eq!(canonical, CARGO_TEST_FAIL_CANONICAL);
    assert_eq!(
        blake3::hash(canonical.as_bytes()).to_hex().as_str(),
        CARGO_TEST_FAIL_BLAKE3
    );

    // nextest zählt die eingebettete libtest-Zeile des gescheiterten Tests
    // (`0 passed; 1 failed`) nicht mit — nur die `Summary`.
    assert_eq!(
        outcome(fixture!("bash-nextest-fail")),
        Some(ExecOutcome {
            class: ExecClass::Test,
            runner: "cargo-nextest".into(),
            command: argv(&["cargo", "nextest", "run", "--features", "broken"]),
            exit_code: Some(100),
            tests: counts(2, 1, 1),
            benches: Vec::new(),
        })
    );
}

#[test]
fn outcome_criterion_bench_ns() {
    // Median des Tripels, ganzzahlig in ns: 298.47 ns → 298,
    // 2.8631 µs → 2863 (Name auf eigener Zeile), 26.484 ns → 26.
    let bench = |name: &str, value: u64| BenchValue {
        name: name.into(),
        value,
        unit: "ns".into(),
    };
    assert_eq!(
        outcome(fixture!("bash-criterion-bench")),
        Some(ExecOutcome {
            class: ExecClass::Bench,
            runner: "cargo-bench-criterion".into(),
            command: argv(&["cargo", "bench", "--bench", "sort"]),
            exit_code: None,
            tests: None,
            benches: vec![
                bench("sort/1k", 298),
                bench("sort/reverse_sorted_input/10000", 2863),
                bench("sort/10", 26),
            ],
        })
    );
}

#[test]
fn outcome_pytest_summary() {
    assert_eq!(
        outcome(fixture!("bash-pytest-pass")),
        Some(ExecOutcome {
            class: ExecClass::Test,
            runner: "pytest".into(),
            command: argv(&["pytest", "py/test_calc.py"]),
            exit_code: None,
            tests: counts(2, 0, 1),
            benches: Vec::new(),
        })
    );
    assert_eq!(
        outcome(fixture!("bash-pytest-fail")),
        Some(ExecOutcome {
            class: ExecClass::Test,
            runner: "pytest".into(),
            command: argv(&["pytest", "py"]),
            exit_code: Some(1),
            tests: counts(2, 1, 1),
            benches: Vec::new(),
        })
    );
}

#[test]
fn outcome_compound_command_not_interpreted() {
    // `cargo test 2>&1 | tail -3`: Die Ausgabe enthielte sogar eine
    // Zusammenfassung — gedeutet wird trotzdem nicht, ein Replay soll nie
    // eine Shell brauchen.
    let call = single_call(fixture!("bash-compound-command"));
    assert_eq!(call.outcome, None);
    let capture = call.capture.unwrap();
    assert_eq!(
        capture.note,
        Some(CaptureNote::CompoundCommandNotInterpreted)
    );
    assert_eq!(
        capture.note.unwrap().as_str(),
        "compound command not interpreted"
    );
    assert_eq!(capture.adapter_version, 3);

    // Ein unbekanntes Kommando: kein Ergebnis, aber auch kein Hinweis.
    let call = single_call(fixture!("bash-unknown-command"));
    assert_eq!(call.outcome, None);
    assert_eq!(call.capture.unwrap().note, None);
}

#[test]
fn outcome_no_floats_in_envelope() {
    let session = session_of(&[
        fixture!("bash-cargo-test-pass"),
        fixture!("bash-cargo-test-fail"),
        fixture!("bash-nextest-pass"),
        fixture!("bash-nextest-fail"),
        fixture!("bash-criterion-bench"),
        fixture!("bash-pytest-pass"),
        fixture!("bash-pytest-fail"),
        fixture!("bash-unknown-command"),
        fixture!("bash-compound-command"),
        fixture!("bash-env-prefix"),
    ]);
    assert_eq!(
        calls(&session)
            .iter()
            .filter(|c| c.outcome.is_some())
            .count(),
        8
    );
    // Durch die Redaction: Die Bench-Namen und das argv überstehen sie
    // unverändert (sie tragen nichts Geheimes).
    let policy = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let redacted = policy.redact_session(session.clone()).unwrap();
    let before: Vec<_> = calls(&session).iter().map(|c| c.outcome.clone()).collect();
    let after: Vec<_> = calls(redacted.session())
        .iter()
        .map(|c| c.outcome.clone())
        .collect();
    assert_eq!(before, after);

    let canonical = minds_core::to_canonical_string(redacted.session()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&canonical).unwrap();
    fn walk(value: &serde_json::Value) {
        match value {
            serde_json::Value::Number(n) => {
                assert!(n.is_u64() || n.is_i64(), "Fließkommazahl im Envelope: {n}")
            }
            serde_json::Value::Array(items) => items.iter().for_each(walk),
            serde_json::Value::Object(map) => map.values().for_each(walk),
            _ => {}
        }
    }
    walk(&value);

    // Kein Text aus stdout/stderr jenseits der Bench-Namen: Wörter, die nur
    // in den Ausgaben stehen, fehlen im Envelope.
    for needle in [
        "Benchmarking",
        "panicked",
        "assertion",
        "Compiling",
        "Summary",
        "test_div",
        "outliers",
        "test result",
        "sorts_wrongly",
        "Exit code",
    ] {
        assert!(
            !canonical.contains(needle),
            "{needle:?} aus der Ausgabe steht im Envelope"
        );
    }
}

/// Ein Post-Payload, an dem `edit` etwas verändert hat.
fn edited(post: &str, edit: impl FnOnce(&mut serde_json::Value)) -> String {
    let mut value: serde_json::Value = serde_json::from_str(post).unwrap();
    edit(&mut value);
    serde_json::to_string(&value).unwrap()
}

#[test]
fn an_incomplete_or_foreign_post_payload_yields_no_outcome() {
    let (pre, post) = fixture!("bash-cargo-test-pass");

    // Gekürzte Ausgabe (Claude Code legt sie ab ~30 000 Zeichen in eine
    // Datei): Teilsummen wären falsch.
    let truncated = edited(post, |v| {
        v["tool_response"]["persistedOutputPath"] = "/home/anna/.claude/x.txt".into();
    });
    assert_missed((pre, &truncated));

    // Abgebrochen.
    let interrupted = edited(post, |v| v["tool_response"]["interrupted"] = true.into());
    assert_missed((pre, &interrupted));

    // Ein Post, der ein anderes Kommando nennt, gehört nicht zu diesem Aufruf.
    let foreign = edited(post, |v| {
        v["tool_input"]["command"] = "cargo test -p x".into()
    });
    assert_missed((pre, &foreign));

    // Ohne Post-Event kein Ergebnis.
    let tmp = tempfile::tempdir().unwrap();
    let journal = Journal::open(tmp.path());
    feed(&journal, pre, 0);
    let session = checkpoint(&journal);
    assert_eq!(calls(&session)[0].outcome, None);
    assert_eq!(
        calls(&session)[0].capture.as_ref().unwrap().note,
        Some(CaptureNote::ResultNotCaptured)
    );

    // Vom Nutzer abgebrochener Fehlschlag, und ein Fehler ohne Exit-Code-Zeile.
    let (pre, post) = fixture!("bash-cargo-test-fail");
    let user_abort = edited(post, |v| v["is_interrupt"] = true.into());
    assert_missed((pre, &user_abort));
    let timeout = edited(post, |v| v["error"] = "Command timed out after 2m".into());
    assert_missed((pre, &timeout));
    // Der Präfix aus dem Transkript (`Error: `) wird toleriert.
    let prefixed = edited(post, |v| {
        let error = v["error"].as_str().unwrap().to_string();
        v["error"] = format!("Error: {error}").into();
    });
    assert_eq!(
        outcome((pre, &prefixed)).and_then(|o| o.exit_code),
        Some(101)
    );
}

/// Erkannter Runner, aber kein Ergebnis — und der Grund steht dabei.
fn assert_missed(pair: (&str, &str)) {
    let call = single_call(pair);
    assert_eq!(call.outcome, None);
    assert_eq!(
        call.capture.unwrap().note,
        Some(CaptureNote::ResultNotCaptured)
    );
}

#[test]
fn a_failure_text_near_the_truncation_limit_yields_no_outcome() {
    // Wie Claude Code einen langen Fehlertext kürzt, ist nicht aufgezeichnet;
    // libtest summiert über Binaries — eine gekürzte Ausgabe ergäbe still zu
    // kleine Zähler. Also: kein Ergebnis.
    let (pre, post) = fixture!("bash-cargo-test-fail");
    let long = edited(post, |v| {
        let error = v["error"].as_str().unwrap().to_string();
        v["error"] = format!("{error}\n{}", "x".repeat(29_000)).into();
    });
    assert_missed((pre, &long));
}

/// Ein Pre/Post-Paar für ein eigenes Kommando und eine eigene Ausgabe —
/// der Umschlag stammt aus der Fixture.
fn pair_with(command: &str, stdout: &str) -> (String, String) {
    let (pre, post) = fixture!("bash-cargo-test-pass");
    let pre = edited(pre, |v| v["tool_input"]["command"] = command.into());
    let post = edited(post, |v| {
        v["tool_input"]["command"] = command.into();
        v["tool_response"]["stdout"] = stdout.into();
    });
    (pre, post)
}

#[test]
fn secrets_in_the_argv_or_a_bench_name_never_reach_the_envelope() {
    let policy = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let post: serde_json::Value = serde_json::from_str(fixture!("bash-cargo-test-pass").1).unwrap();
    let pass = post["tool_response"]["stdout"].as_str().unwrap();

    for (command, secret) in [
        // Flag und Wert landen in getrennten argv-Elementen.
        ("cargo test -- --db-password hunter2", "hunter2"),
        // Gequoteter Wert: im argv ohne Quotes, mit Leerzeichen.
        (
            "cargo test -- --password='correct horse battery staple'",
            "horse",
        ),
    ] {
        let (pre, post) = pair_with(command, pass);
        let session = session_of(&[(&pre, &post)]);
        assert!(calls(&session)[0].outcome.is_some(), "{command}");
        let redacted = policy.redact_session(session).unwrap();
        let canonical = minds_core::to_canonical_string(redacted.session()).unwrap();
        assert!(!canonical.contains(secret), "{command}: {canonical}");
        assert_eq!(calls(redacted.session())[0].outcome, None, "{command}");
    }

    // Eine beliebige Ausgabezeile wird kein Bench-Name.
    let forged = "Password:\nhunter2\n                        time:   [1.0 ns 2.0 ns 3.0 ns]\n";
    let (pre, post) = pair_with("cargo bench", forged);
    let session = session_of(&[(&pre, &post)]);
    let canonical = minds_core::to_canonical_string(&session).unwrap();
    assert!(!canonical.contains("hunter2"), "{canonical}");
}

#[test]
fn a_command_that_only_quotes_a_runner_gets_no_note() {
    // Kein Runner, nur ein Text, der „cargo test" enthält: weder Ergebnis
    // noch Hinweis — sonst stünde im Record eine falsche Aussage.
    let (pre, post) = pair_with("git commit -m 'fix cargo test'", "[main abc1234] fix\n");
    let call = single_call((&pre, &post));
    assert_eq!(call.outcome, None);
    assert_eq!(call.capture.unwrap().note, None);
}

#[test]
fn a_wrapped_runner_or_a_bare_exit_code_says_the_result_was_not_captured() {
    // Ein einfaches Kommando, das den Runner nur umhüllt: gedeutet wird nur
    // `argv[0]`, der Grund steht dabei.
    let post: serde_json::Value = serde_json::from_str(fixture!("bash-cargo-test-pass").1).unwrap();
    let pass = post["tool_response"]["stdout"].as_str().unwrap();
    let (pre, post) = pair_with("timeout 600 cargo test", pass);
    assert_missed((&pre, &post));

    // Build-Fehler: nur ein Exit-Code, keine Zusammenfassung. Der Exit-Code
    // bleibt, der Hinweis sagt, dass keine Zähler gelesen wurden.
    let (pre, post) = fixture!("bash-cargo-test-fail");
    let build_error = edited(post, |v| {
        v["error"] = "Exit code 101\nerror[E0425]: cannot find value `x`".into()
    });
    let call = single_call((pre, &build_error));
    let outcome = call.outcome.unwrap();
    assert_eq!((outcome.exit_code, outcome.tests), (Some(101), None));
    assert_eq!(
        call.capture.unwrap().note,
        Some(CaptureNote::ResultNotCaptured)
    );

    // Ein vollständiges Ergebnis trägt keinen Hinweis.
    assert_eq!(
        single_call(fixture!("bash-cargo-test-fail"))
            .capture
            .unwrap()
            .note,
        None
    );
}

#[test]
fn a_shell_quoted_secret_is_caught_through_the_argv() {
    // `'--password'`: Die Quotes brechen den Flag-Kontext im Text der
    // `arguments`, die Shell entfernt sie — das argv zeigt das Geheimnis,
    // und die Redaction ersetzt `arguments` ganz.
    let post: serde_json::Value = serde_json::from_str(fixture!("bash-pytest-pass").1).unwrap();
    let stdout = post["tool_response"]["stdout"].as_str().unwrap();
    let (pre, post) = pair_with("pytest '--password' hunter2", stdout);
    let session = session_of(&[(&pre, &post)]);
    assert!(calls(&session)[0].outcome.is_some());

    let policy = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let redacted = policy.redact_session(session).unwrap();
    let canonical = minds_core::to_canonical_string(redacted.session()).unwrap();
    assert!(!canonical.contains("hunter2"), "{canonical}");
    let call = calls(redacted.session())[0];
    assert_eq!(call.outcome, None);
    assert_eq!(
        call.capture.as_ref().unwrap().note,
        Some(CaptureNote::ResultNotCaptured)
    );
}

#[test]
fn a_long_stdout_without_a_truncation_marker_yields_no_outcome() {
    // Eine andere Claude-Code-Version könnte ohne `persistedOutputPath`
    // kürzen; libtest summiert über Binaries.
    let (pre, post) = fixture!("bash-cargo-test-pass");
    let long = edited(post, |v| {
        let stdout = v["tool_response"]["stdout"].as_str().unwrap().to_string();
        v["tool_response"]["stdout"] = format!("{}\n{stdout}", "x".repeat(29_000)).into();
    });
    assert_missed((pre, &long));

    // Dieselbe Schranke für `stderr`: Der Payload ist fremd.
    let long_stderr = edited(post, |v| {
        v["tool_response"]["stderr"] = "y".repeat(29_000).into();
    });
    assert_missed((pre, &long_stderr));

    // Gemessen in Bytes: multibyte-lastige Ausgabe unter 29 000 Zeichen,
    // aber über 29 000 Bytes, gilt ebenfalls als möglicherweise gekürzt.
    let multibyte = edited(post, |v| {
        let stdout = v["tool_response"]["stdout"].as_str().unwrap().to_string();
        v["tool_response"]["stdout"] = format!("{}\n{stdout}", "µ".repeat(15_000)).into();
    });
    assert_missed((pre, &multibyte));
}

#[test]
fn a_count_beyond_the_canonical_range_never_blocks_the_session() {
    // Eine Zahl aus Programmausgabe (`harness = false`) über 2^53 − 1 ließe
    // die Kanonisierung der ganzen Session scheitern: kein Ergebnis, die
    // Session bleibt speicherbar.
    let stdout = "     Running tests/x.rs (target/debug/deps/x)\n\
        test result: ok. 9007199254740992 passed; 0 failed; 0 ignored\n";
    let (pre, post) = pair_with("cargo test", stdout);
    let session = session_of(&[(&pre, &post)]);
    assert_eq!(calls(&session)[0].outcome, None);
    let policy = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let redacted = policy.redact_session(session).unwrap();
    assert!(minds_core::to_canonical_string(redacted.session()).is_ok());
}

#[test]
fn outcome_is_deterministic_per_adapter_version() {
    assert_eq!(minds_capture::normalize::CLAUDE_ADAPTER_VERSION, 3);
    let pairs = [
        fixture!("bash-cargo-test-fail"),
        fixture!("bash-criterion-bench"),
    ];
    let a = minds_core::to_canonical_string(&session_of(&pairs)).unwrap();
    let b = minds_core::to_canonical_string(&session_of(&pairs)).unwrap();
    assert_eq!(a, b);
}
