use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use ReconClass::*;
use minds_core::{
    Agent, ContentHash, Effect, EffectKind, Intent, Model, Role, Session, SessionId, ToolCall, Turn,
};
use minds_git::{CommitId, DiffKind, Repo};
use minds_reader::{Index, reconcile::*};
use minds_store::{ContextStore, InRepoStore};
use serde_json::json;

fn hash(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}
fn commit() -> CommitId {
    "1234567890123456789012345678901234567890".parse().unwrap()
}
fn call(name: &str, path: &str, arguments: serde_json::Value, written: Option<&[u8]>) -> ToolCall {
    ToolCall {
        name: name.into(),
        arguments: arguments.to_string(),
        capture: None,
        effect: Some(Effect {
            kind: if name == "Delete" {
                EffectKind::Delete
            } else {
                EffectKind::Write
            },
            path: Some(path.into()),
            content: None,
            written: written.map(hash),
            written_unavailable: None,
        }),
    }
}
fn write(path: &str, text: &str) -> ToolCall {
    call(
        "Write",
        path,
        json!({"content": text}),
        Some(text.as_bytes()),
    )
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
            request: "write the files".into(),
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
fn file<'a>(path: &'a str, base: Option<&'a [u8]>, committed: Option<&'a [u8]>) -> ChangedFile<'a> {
    ChangedFile {
        path,
        base,
        committed,
        added_lines: 0,
    }
}
fn observation(path: &str, bytes: Option<&[u8]>, seq: u64) -> FsObservation {
    FsObservation {
        path: path.into(),
        observed: ObservedAt {
            hash: bytes.map(hash),
            seq,
            at: None,
        },
        content: bytes.map(<[u8]>::to_vec),
    }
}
fn run(
    changed: &[ChangedFile<'_>],
    sessions: &[&Session],
    observations: &[FsObservation],
) -> Reconciliation {
    reconcile(&ReconInput {
        commit: commit(),
        base: None,
        changed,
        sessions,
        observations,
    })
}
fn lines(file: &FileRecon) -> Vec<(u32, ReconClass)> {
    match &file.line_level {
        LineLevel::Available(lines) => lines.iter().map(|l| (l.line, l.class)).collect(),
        other => panic!("expected lines, got {other:?}"),
    }
}

#[test]
fn reconcile_agent_only_commit_is_reported_only() {
    let session = session(vec![write("a", "one\ntwo\n"), write("b", "three\n")]);
    let result = run(
        &[
            file("b", None, Some(b"three\n")),
            file("a", None, Some(b"one\ntwo\n")),
        ],
        &[&session],
        &[],
    );
    assert_eq!(
        result
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(result.files.iter().all(|f| f.class == ReportedOnly));
    assert_eq!(
        lines(&result.files[0]),
        [(1, ReportedOnly), (2, ReportedOnly)]
    );
    assert_eq!((result.explained_lines, result.total_changed_lines), (0, 3));
    assert_eq!(result.base, None);
}

#[test]
fn reconcile_human_line_is_unexplained() {
    let session = session(vec![write("a", "one\ntwo\nthree\n")]);
    let result = run(
        &[file("a", None, Some(b"one\nHUMAN\nthree\n"))],
        &[&session],
        &[],
    );
    assert_eq!(result.files[0].class, Unexplained);
    assert_eq!(
        lines(&result.files[0]),
        [(1, ReportedOnly), (2, Unexplained), (3, ReportedOnly)]
    );
    assert_eq!(result.total_changed_lines, 3);
    // Insertion shifts the existing agent lines; equality is not positional.
    let result = run(
        &[file("a", None, Some(b"HUMAN\none\ntwo\nthree\n"))],
        &[&session],
        &[],
    );
    assert_eq!(
        lines(&result.files[0]),
        [
            (1, Unexplained),
            (2, ReportedOnly),
            (3, ReportedOnly),
            (4, ReportedOnly)
        ]
    );
}

#[test]
fn reconcile_shell_write_is_unexplained_without_observer() {
    let mut shell = call("Bash", "a", json!({"command": "echo shell > a"}), None);
    shell.effect.as_mut().unwrap().kind = EffectKind::Exec;
    shell.effect.as_mut().unwrap().content = Some(hash(b"shell\n"));
    let session = session(vec![shell]);
    let result = run(&[file("a", None, Some(b"shell\n"))], &[&session], &[]);
    assert_eq!(result.files[0].class, Unexplained);
    assert_eq!(lines(&result.files[0]), [(1, Unexplained)]);
}

#[test]
fn reconcile_observed_and_claimed_is_explained() {
    let session = session(vec![write("a", "one\ntwo\n")]);
    let mut observed = observation("a", Some(b"one\ntwo\n"), 42);
    observed.content = None;
    let result = run(
        &[file("a", None, Some(b"one\ntwo\n"))],
        &[&session],
        &[observed.clone()],
    );
    assert_eq!(result.files[0].class, Explained);
    assert_eq!(result.files[0].last_observed, Some(observed.observed));
    assert_eq!(lines(&result.files[0]), [(1, Explained), (2, Explained)]);
    assert_eq!((result.explained_lines, result.total_changed_lines), (2, 2));
}

#[test]
fn reconcile_fs_only() {
    let session = session(vec![call("Bash", "a", json!({}), None)]);
    let result = run(
        &[file("a", None, Some(b"shell\n"))],
        &[&session],
        &[observation("a", Some(b"shell\n"), 1)],
    );
    assert_eq!(result.files[0].class, ExplainedFsOnly);
    assert_eq!(lines(&result.files[0]), [(1, ExplainedFsOnly)]);
    assert_eq!(result.explained_lines, 1);
}

#[test]
fn reconcile_delete_and_rename() {
    let session = session(vec![
        call("Delete", "old", json!({}), None),
        write("new", "text\n"),
    ]);
    let changed = [
        file("old", Some(b"text\n"), None),
        file("new", None, Some(b"text\n")),
        file("other", Some(b"gone"), None),
    ];
    let result = run(&changed, &[&session], &[]);
    assert_eq!(
        result.files.iter().map(|f| f.class).collect::<Vec<_>>(),
        [ReportedOnly, ReportedOnly, Unexplained]
    );
    assert!(result.files[1].deleted);
    assert!(lines(&result.files[1]).is_empty());
    assert_eq!(result.total_changed_lines, 1);
    let result = run(
        &changed,
        &[&session],
        &[observation("old", None, 1), observation("other", None, 2)],
    );
    assert_eq!(result.files[1].class, Explained);
    assert_eq!(result.files[2].class, ExplainedFsOnly);
    // Empty is not absent.
    let result = run(
        &[file("old", None, Some(b""))],
        &[&session],
        &[observation("old", None, 1)],
    );
    assert_eq!(result.files[0].class, Unexplained);
    assert!(!result.files[0].deleted);
}

#[test]
fn reconcile_binary_file_file_level_only() {
    let session = session(vec![write("a", "a\0b")]);
    let changed = [file("a", None, Some(b"a\0b"))];
    let result = run(&changed, &[&session], &[]);
    assert_eq!(result.files[0].class, ReportedOnly);
    assert_eq!(
        result.files[0].line_level,
        LineLevel::Unavailable(Reason::Binary)
    );
    assert_eq!(result.total_changed_lines, 1);
    let result = run(&changed, &[&session], &[observation("a", Some(b"a\0b"), 0)]);
    assert_eq!(result.explained_lines, 1);
}

#[test]
fn edit_replay_chains_and_counts_only_commit_changes() {
    let session = session(vec![
        call(
            "Edit",
            "a",
            json!({"old_string": "two", "new_string": "TWO"}),
            Some(b"one\nTWO\nthree\n"),
        ),
        call(
            "MultiEdit",
            "a",
            json!({"edits": [{"old_string": "one", "new_string": "ONE"}, {"old_string": "three", "new_string": "THREE"}]}),
            Some(b"ONE\nTWO\nTHREE\n"),
        ),
    ]);
    let result = run(
        &[file(
            "a",
            Some(b"one\ntwo\nthree\n"),
            Some(b"ONE\nHUMAN\nTHREE\n"),
        )],
        &[&session],
        &[],
    );
    assert_eq!(
        lines(&result.files[0]),
        [(1, ReportedOnly), (2, Unexplained), (3, ReportedOnly)]
    );
    let session = super_session_edit_all();
    let result = run(
        &[file("a", Some(b"x\nx\nkeep\n"), Some(b"y\ny\nkeep\n"))],
        &[&session],
        &[],
    );
    assert_eq!(
        lines(&result.files[0]),
        [(1, ReportedOnly), (2, ReportedOnly)]
    );
    assert_eq!(result.total_changed_lines, 2);
}
fn super_session_edit_all() -> Session {
    session(vec![call(
        "Edit",
        "a",
        json!({"old_string": "x", "new_string": "y", "replace_all": true}),
        Some(b"y\ny\nkeep\n"),
    )])
}

#[test]
fn reconstructions_must_match_stored_hashes() {
    let session = session(vec![call(
        "Write",
        "a",
        json!({"content": "[REDACTED]"}),
        Some(b"original\n"),
    )]);
    let result = run(&[file("a", None, Some(b"original\n"))], &[&session], &[]);
    assert_eq!(result.files[0].class, ReportedOnly);
    assert_eq!(
        result.files[0].line_level,
        LineLevel::Unavailable(Reason::ReconstructionMismatch)
    );
    // A later claim without a hash invalidates an earlier matching claim.
    let session = session_with_unknown_last_write();
    let result = run(&[file("a", None, Some(b"first\n"))], &[&session], &[]);
    assert_eq!(result.files[0].class, Unexplained);
    assert_eq!(lines(&result.files[0]), [(1, Unexplained)]);
}
fn session_with_unknown_last_write() -> Session {
    session(vec![
        write("a", "first\n"),
        call("Write", "a", json!({"content":"first\n"}), None),
    ])
}

#[test]
fn latest_observation_wins_but_partial_lines_keep_their_evidence() {
    let session = session(vec![write("a", "one\ntwo\n")]);
    let old = observation("a", Some(b"one\ntwo\n"), 1);
    let latest = observation("a", Some(b"one\nother\n"), 2);
    let result = run(
        &[file("a", None, Some(b"one\ntwo\n"))],
        &[&session],
        &[latest, old.clone()],
    );
    assert_eq!(result.files[0].class, Unexplained);
    assert_eq!(result.files[0].last_observed, Some(old.observed));
    assert_eq!(
        lines(&result.files[0]),
        [(1, ExplainedFsOnly), (2, Unexplained)]
    );
    assert_eq!(result.explained_lines, 1);
    let result = run(
        &[file("a", None, Some(b"one\nHUMAN\n"))],
        &[&session],
        &[observation("a", Some(b"one\ntwo\n"), 1)],
    );
    assert_eq!(lines(&result.files[0]), [(1, Explained), (2, Unexplained)]);
    assert_eq!(result.explained_lines, 1);
}

#[test]
fn observation_without_content_uses_matching_claim_or_reports_unavailable() {
    let session = session(vec![write("a", "one\ntwo\n")]);
    let mut observed = observation("a", Some(b"one\ntwo\n"), 1);
    observed.content = None;
    let changed = [file("a", None, Some(b"one\nHUMAN\n"))];
    let result = run(&changed, &[&session], &[observed.clone()]);
    assert_eq!(lines(&result.files[0]), [(1, Explained), (2, Unexplained)]);
    let result = run(&changed, &[], &[observed.clone()]);
    assert_eq!(
        result.files[0].line_level,
        LineLevel::Unavailable(Reason::MissingContent)
    );
    observed.content = Some(b"false".to_vec());
    let result = run(&changed, &[&session], &[observed]);
    assert_eq!(
        result.files[0].line_level,
        LineLevel::Unavailable(Reason::ReconstructionMismatch)
    );
}

#[test]
fn large_files_and_expanding_edits_are_bounded() {
    let large = vec![b'x'; LINE_LEVEL_LIMIT + 1];
    let mut changed = file("a", Some(b"x"), Some(&large));
    changed.added_lines = 1;
    let result = run(&[changed], &[], &[observation("a", Some(&large), 0)]);
    assert_eq!(
        result.files[0].line_level,
        LineLevel::Unavailable(Reason::TooLarge)
    );
    assert_eq!((result.total_changed_lines, result.explained_lines), (1, 1));
    let session = session(vec![call(
        "Edit",
        "a",
        json!({"old_string":"x", "new_string":"y".repeat(1024), "replace_all":true}),
        Some(b"target"),
    )]);
    let base = vec![b'x'; 4096];
    let result = run(&[file("a", Some(&base), Some(b"target"))], &[&session], &[]);
    assert_eq!(
        result.files[0].line_level,
        LineLevel::Unavailable(Reason::TooLarge)
    );
}

#[test]
fn line_endings_and_non_utf8_are_not_lossily_compared() {
    let session = session(vec![write("a", "one\r\ntwo\n")]);
    let result = run(
        &[file("a", Some(b"before\n"), Some(b"one\ntwo"))],
        &[&session],
        &[],
    );
    assert_eq!(
        lines(&result.files[0]),
        [(1, Unexplained), (2, Unexplained)]
    );
    let result = run(
        &[file("a", Some(b"before\n"), Some(b"one\r\n\xff\n"))],
        &[&session],
        &[],
    );
    assert_eq!(
        lines(&result.files[0]),
        [(1, ReportedOnly), (2, Unexplained)]
    );
}

#[test]
fn reconcile_is_deterministic() {
    // Exhaustive permutation property over interleaved sessions, including
    // equal instants expressed with different offsets and fractional precision.
    let mut sessions: Vec<_> = (0..4)
        .map(|i| session(vec![write("a", &format!("{i}\n"))]))
        .collect();
    for (s, at) in sessions.iter_mut().zip([
        "2026-10-02T10:00:00.1Z",
        "2026-10-02T12:00:00.10+02:00",
        "2026-10-02T10:00:00.2Z",
        "2026-10-02T11:00:00.3+01:00",
    ]) {
        s.turns[0].at = Some(at.into());
    }
    let changed = [file("a", None, Some(b"3\n"))];
    let expected = run(&changed, &sessions.iter().collect::<Vec<_>>(), &[]);
    assert_eq!(expected.files[0].class, ReportedOnly);
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let mut indices = vec![a, b, c, d];
                    indices.sort();
                    indices.dedup();
                    if indices.len() == 4 {
                        assert_eq!(
                            run(
                                &changed,
                                &[&sessions[a], &sessions[b], &sessions[c], &sessions[d]],
                                &[]
                            ),
                            expected
                        );
                    }
                }
            }
        }
    }
    // Reversing observations is likewise immaterial.
    let observations = [
        observation("a", Some(b"2\n"), 1),
        observation("a", Some(b"3\n"), 2),
    ];
    assert_eq!(
        run(&changed, &[], &observations),
        run(
            &changed,
            &[],
            &[observations[1].clone(), observations[0].clone()]
        )
    );
}

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
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
    output.stdout
}
fn snapshot(path: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(path, path, &mut out);
    out
}

#[test]
fn reconcile_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    let redacted = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_session(session(vec![write("a", "one\ntwo\n")]))
        .unwrap();
    let store = InRepoStore::open(dir.path()).unwrap();
    let id = store.put(&redacted).unwrap().id();
    std::fs::write(dir.path().join("a"), "one\ntwo\n").unwrap();
    git(dir.path(), &["add", "a"]);
    git(
        dir.path(),
        &[
            "commit",
            "-qm",
            &format!("fixture\n\nMinds-Session-Id: {id}"),
        ],
    );
    let repo = Repo::open(dir.path()).unwrap();
    let commit = repo.head().unwrap().commit().unwrap();
    // The working tree is deliberately different: only committed blobs count.
    std::fs::write(dir.path().join("a"), "uncommitted human edit\n").unwrap();
    let before_refs = git(dir.path(), &["show-ref"]);
    let before = snapshot(dir.path());
    let index = Index::build(&repo, &store).unwrap();
    let blob = repo
        .read_blob(repo.tree_of(commit).unwrap(), "a")
        .unwrap()
        .unwrap();
    let changes = [file("a", None, Some(&blob))];
    let result = index.reconcile(commit, None, &changes, &[]);
    assert_eq!(result.files[0].class, ReportedOnly);
    assert_eq!(result, index.reconcile(commit, None, &changes, &[]));
    assert_eq!(before_refs, git(dir.path(), &["show-ref"]));
    assert_eq!(
        before,
        snapshot(dir.path()),
        "no file or Git object may be written"
    );
    // The pure diff uses the same changed lines as the real repository diff.
    let diff = repo.diff_commit(commit).unwrap();
    let added: Vec<_> = diff.files[0]
        .lines
        .iter()
        .filter(|l| l.kind == DiffKind::Added)
        .map(|l| l.new.unwrap())
        .collect();
    assert_eq!(
        lines(&result.files[0])
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>(),
        added
    );
}

#[test]
fn index_only_uses_sessions_linked_to_the_commit() {
    let s = session(vec![write("a", "text\n")]);
    let id = SessionId::of(&s).unwrap();
    let index = Index::from_parts(BTreeMap::from([(id, s)]), BTreeMap::new());
    assert_eq!(
        index
            .reconcile(commit(), None, &[file("a", None, Some(b"text\n"))], &[])
            .files[0]
            .class,
        Unexplained
    );
}

#[test]
fn interleaved_sessions_are_merged_by_turn_time() {
    let mut first = session(vec![write("a", "one\n")]);
    let mut second = session(vec![write("a", "two\n")]);
    second.turns[0].at = Some("2026-10-02T10:00:01Z".into());
    first.turns.push(Turn {
        at: Some("2026-10-02T10:00:02Z".into()),
        parent: Some(0),
        role: Role::Assistant,
        text: String::new(),
        tool_calls: vec![call(
            "Edit",
            "a",
            json!({"old_string":"two", "new_string":"three"}),
            Some(b"three\n"),
        )],
    });
    let changed = [file("a", None, Some(b"three\n"))];
    let result = run(&changed, &[&first, &second], &[]);
    assert_eq!(result.files[0].class, ReportedOnly);
    assert_eq!(lines(&result.files[0]), [(1, ReportedOnly)]);
    assert_eq!(result, run(&changed, &[&second, &first], &[]));
}

#[test]
fn matching_old_claim_explains_observation_but_not_an_unobserved_commit() {
    let session = session(vec![write("a", "old\n"), write("a", "new\n")]);
    let changed = [file("a", None, Some(b"old\n"))];
    assert_eq!(run(&changed, &[&session], &[]).files[0].class, Unexplained);
    assert_eq!(
        run(
            &changed,
            &[&session],
            &[observation("a", Some(b"old\n"), 1)]
        )
        .files[0]
            .class,
        Explained
    );
}
