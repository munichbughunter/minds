//! EA-17: Befunde außerhalb des erklärten Bereichs — Commit, Claims und
//! Beobachtungen gegen die Globs des Intent-Ankers.

use std::path::Path;
use std::process::Command;

use minds_core::intent_anchor::IntentSource;
use minds_core::{
    Agent, ContentHash, Effect, EffectKind, Intent, Model, Role, Session, ToolCall, Turn,
};
use minds_reader::assurance::{IntentSignature, IntentState};
use minds_reader::reconcile::{Claims, FsObservation, ObservedAt};
use minds_reader::scope::{
    Finding, NoScope, Scope, ScopeFinding, ScopeSource, declared_scope, scope_findings,
};
use minds_store::{ContextStore, InRepoStore};

use ScopeSource::*;

fn call(kind: EffectKind, path: &str) -> ToolCall {
    ToolCall {
        outcome: None,
        name: if kind == EffectKind::Delete {
            "Delete"
        } else {
            "Write"
        }
        .into(),
        arguments: "{}".into(),
        capture: None,
        effect: Some(Effect {
            kind,
            path: Some(path.into()),
            content: None,
            written: Some(ContentHash::from_bytes([7; 32])),
            written_unavailable: None,
        }),
    }
}

fn read(path: &str) -> ToolCall {
    let mut call = call(EffectKind::Write, path);
    call.name = "Read".into();
    call.effect.as_mut().unwrap().kind = EffectKind::Read;
    call
}

fn session(calls: Vec<ToolCall>) -> Session {
    let mut session = Session::new(
        Agent {
            name: "claude-code".into(),
            version: "test".into(),
        },
        Model {
            provider: "test".into(),
            id: "test".into(),
        },
        Intent {
            request: "retry with backoff".into(),
            ..Intent::default()
        },
    );
    session.turns.push(Turn {
        role: Role::Assistant,
        text: String::new(),
        tool_calls: calls,
        parent: None,
        at: Some("2026-10-02T10:00:00Z".into()),
    });
    session
}

fn observation(path: &str, seq: u64) -> FsObservation {
    FsObservation {
        path: path.into(),
        observed: ObservedAt {
            hash: Some(ContentHash::from_bytes([seq as u8; 32])),
            seq,
            at: None,
        },
        content: None,
        opaque: false,
    }
}

fn finding(path: &str, sources: &[ScopeSource]) -> ScopeFinding {
    ScopeFinding {
        path: path.into(),
        sources: sources.to_vec(),
    }
}

fn scope() -> Scope {
    Scope::from_globs(&["src/retry/**", "tests/retry_*.rs"]).unwrap()
}

#[test]
fn one_out_of_scope_write_is_exactly_one_finding() {
    let session = session(vec![
        call(EffectKind::Write, "src/retry/backoff.rs"),
        call(EffectKind::Write, "tests/retry_backoff.rs"),
        call(EffectKind::Write, "docs/README.md"),
        // Gelesen ist nicht geschrieben.
        read("Cargo.toml"),
    ]);
    let claims = Claims::collect(&[&session], &[], &|_| false);
    let observations = [
        observation("src/retry/backoff.rs", 1),
        observation("docs/README.md", 2),
        observation("tests/retry_backoff.rs", 3),
        // Zweimal beobachtet: ein Befund, eine Quelle.
        observation("docs/README.md", 4),
    ];
    let changed = [
        "docs/README.md",
        "src/retry/backoff.rs",
        "tests/retry_backoff.rs",
    ];
    let findings = scope_findings(&scope(), changed, &claims, &observations);
    assert_eq!(
        findings,
        [finding("docs/README.md", &[Commit, Claim, Observation])]
    );
    assert_eq!(findings[0].finding(), Finding::OutOfScope);
}

#[test]
fn every_source_is_named_on_its_own() {
    let session = session(vec![
        call(EffectKind::Write, "/repo/notes/claimed.md"),
        call(EffectKind::Delete, "LICENSE"),
        // Außerhalb des Repositorys: keine Scope-Frage.
        call(EffectKind::Write, "/elsewhere/plan.md"),
        call(EffectKind::Write, "../sibling/x.rs"),
    ]);
    let claims = Claims::collect(&[&session], &[Path::new("/repo")], &|_| false);
    let findings = scope_findings(
        &scope(),
        ["Cargo.lock", "src/retry/a.rs"],
        &claims,
        &[observation(".env", 1), observation("src/retry/a.rs", 2)],
    );
    assert_eq!(
        findings,
        [
            finding(".env", &[Observation]),
            finding("Cargo.lock", &[Commit]),
            finding("LICENSE", &[Claim]),
            finding("notes/claimed.md", &[Claim]),
        ]
    );
}

#[test]
fn findings_do_not_depend_on_input_order() {
    let one = session(vec![
        call(EffectKind::Write, "b.rs"),
        call(EffectKind::Write, "a.rs"),
    ]);
    let claims = Claims::collect(&[&one], &[], &|_| false);
    let observations = [observation("c.rs", 1), observation("a.rs", 2)];
    let reversed: Vec<FsObservation> = observations.iter().rev().cloned().collect();
    let forward = scope_findings(&scope(), ["d.rs", "a.rs"], &claims, &observations);
    let backward = scope_findings(&scope(), ["a.rs", "d.rs"], &claims, &reversed);
    assert_eq!(forward, backward);
    assert_eq!(
        forward.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        ["a.rs", "b.rs", "c.rs", "d.rs"]
    );
}

#[test]
fn everything_in_scope_is_no_finding() {
    let session = session(vec![call(EffectKind::Write, "src/retry/x.rs")]);
    let claims = Claims::collect(&[&session], &[], &|_| false);
    assert_eq!(
        scope_findings(
            &scope(),
            ["tests/retry_x.rs"],
            &claims,
            &[observation("src/retry/y.rs", 1)]
        ),
        []
    );
}

// ---------------------------------------------------------------------------
// Der Bereich aus dem Store
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn bound(anchor_id: ContentHash) -> IntentState {
    IntentState::Bound {
        anchor_id,
        chained: false,
        signature: IntentSignature::Unsigned,
        snapshot_matches: true,
        from_session_start: false,
        changed_mid_session: false,
    }
}

#[test]
fn declared_scope_reads_the_bound_anchor() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    let put = |scope: Vec<String>| {
        let intent = minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact_intent(IntentSource::Prompt, scope, b"retry".to_vec())
            .unwrap();
        store.put_intent(&intent).unwrap()
    };

    let scoped = put(vec!["src/retry/**".into()]);
    let declared = declared_scope(&store, &bound(scoped.clone())).unwrap();
    assert!(declared.contains("src/retry/a.rs"));
    assert!(!declared.contains("docs/README.md"));

    // Wechselte der Anker mitten in der Session, ist offen, welcher
    // Bereich für welche Arbeit galt: nicht beurteilbar.
    let mut changed = bound(scoped);
    if let IntentState::Bound {
        changed_mid_session,
        ..
    } = &mut changed
    {
        *changed_mid_session = true;
    }
    assert_eq!(
        declared_scope(&store, &changed),
        Err(NoScope::IntentChangedMidSession)
    );

    // Eine Verneinung kennt der Matcher nicht: nicht beurteilbar statt
    // eines weiteren Bereichs als freigegeben.
    let negated = put(vec!["src/**".into(), "!src/auth/**".into()]);
    assert_eq!(
        declared_scope(&store, &bound(negated)),
        Err(NoScope::UnsupportedGlob)
    );

    let open = put(Vec::new());
    assert_eq!(
        declared_scope(&store, &bound(open)),
        Err(NoScope::NotDeclared)
    );
    assert_eq!(
        declared_scope(&store, &IntentState::Unbound),
        Err(NoScope::IntentNotBound)
    );
    assert_eq!(
        declared_scope(&store, &bound(ContentHash::from_bytes([9; 32]))),
        Err(NoScope::AnchorMissing)
    );
}
