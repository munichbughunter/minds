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
        outcome: None,
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
        opaque: false,
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
        roots: &[],
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
    let result = index.reconcile(commit, None, &changes, &[], &[]);
    assert_eq!(result.files[0].class, ReportedOnly);
    assert_eq!(result, index.reconcile(commit, None, &changes, &[], &[]));
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
            .reconcile(
                commit(),
                None,
                &[file("a", None, Some(b"text\n"))],
                &[],
                &[]
            )
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

#[test]
fn unexplained_lines_count_line_and_file_level_gaps() {
    let session = session(vec![write("a", "one\ntwo\nthree\n"), write("bin", "x\0y")]);
    let changed = [
        file("a", None, Some(b"one\nHUMAN\nthree\n")),
        file("bin", None, Some(b"x\0y")),
        file("human.bin", None, Some(b"h\0h")),
        file("gone", Some(b"old\n"), None),
    ];
    let result = run(&changed, &[&session], &[]);
    let per_file: Vec<_> = result
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.changed_lines, f.unexplained_lines()))
        .collect();
    // Line level: one human line. Binary + ReportedOnly: no gap. Binary +
    // Unexplained: the whole file counts. A deletion changes no lines.
    assert_eq!(
        per_file,
        [
            ("a", 3, 1),
            ("bin", 1, 0),
            ("gone", 0, 0),
            ("human.bin", 1, 1)
        ]
    );
    assert_eq!(result.unexplained_lines(), 2);
    assert_eq!(result.total_changed_lines, 5);
    assert_eq!(
        result.files.iter().map(|f| f.changed_lines).sum::<u64>(),
        result.total_changed_lines
    );
}

#[test]
fn absolute_claims_match_only_under_a_root() {
    let root = Path::new("/work/repo");
    assert_eq!(
        repo_path("/work/repo/src/a.rs", &[root]).as_deref(),
        Some("src/a.rs")
    );
    assert_eq!(repo_path("./src/a.rs", &[]).as_deref(), Some("src/a.rs"));
    for outside in [
        "/work/other/src/a.rs",
        "/work/repo/../repo/src/a.rs",
        "/work/repo",
        "~/repo/a.rs",
        "$HOME/a.rs",
        "src\\a.rs",
        "../a.rs",
    ] {
        assert_eq!(repo_path(outside, &[root]), None, "{outside}");
    }

    // Claude Code nennt Pfade absolut: Ohne Wurzel erklärt der Claim nichts,
    // mit Wurzel genau die Datei darunter — und keine gleichnamige draußen.
    let session = session(vec![write("/work/repo/a", "text\n")]);
    let changed = [file("a", None, Some(b"text\n"))];
    let with = |roots: &[&Path]| {
        reconcile(&ReconInput {
            commit: commit(),
            base: None,
            changed: &changed,
            sessions: &[&session],
            observations: &[],
            roots,
        })
        .files[0]
            .class
    };
    assert_eq!(with(&[]), Unexplained);
    assert_eq!(with(&[root]), ReportedOnly);
    assert_eq!(with(&[Path::new("/work/other")]), Unexplained);
}

fn with_cwd(mut session: Session, cwd: &str) -> Session {
    session.lineage = Some(minds_core::Lineage {
        local_id: "capture".into(),
        started_at: None,
        ended_at: None,
        cwd: Some(cwd.into()),
        closed: false,
    });
    session
}

#[test]
fn claims_match_under_the_sessions_own_capture_root() {
    // Captured on a developer machine, verified in CI under another path: the
    // session's recorded cwd spells the capture-time root.
    let captured = with_cwd(
        session(vec![
            write("/Users/dev/proj/src/a.rs", "agent\n"),
            write("rel.rs", "relative\n"),
        ]),
        "/Users/dev/proj/src",
    );
    let changed = [
        file("src/a.rs", None, Some(b"agent\n")),
        file("src/rel.rs", None, Some(b"relative\n")),
        file("rel.rs", None, Some(b"relative\n")),
    ];
    let ci = Path::new("/home/runner/work/proj");
    let result = reconcile(&ReconInput {
        commit: commit(),
        base: None,
        changed: &changed,
        sessions: &[&captured],
        observations: &[],
        roots: &[ci],
    });
    let classes: Vec<_> = result
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.class))
        .collect();
    // a.rs maps to exactly one changed path. The relative claim resolves to
    // …/src/rel.rs, which could be `src/rel.rs` (proj is the root) or
    // `rel.rs` (src is the root): ambiguous, so it explains neither.
    assert_eq!(
        classes,
        [
            ("rel.rs", Unexplained),
            ("src/a.rs", ReportedOnly),
            ("src/rel.rs", Unexplained)
        ]
    );
}

#[test]
fn capture_root_fallback_never_aliases() {
    let known = |paths: &'static [&'static str]| move |p: &str| paths.contains(&p);
    // Monorepo session in a subdirectory: its delete may not explain a
    // top-level deletion — deletions carry no hash, so no fallback at all.
    let session_delete = with_cwd(
        session(vec![call("Delete", "/r/pkg/auth.rs", json!({}), None)]),
        "/r/pkg",
    );
    let changed = [file("auth.rs", Some(b"check\n"), None)];
    assert_eq!(
        run(&changed, &[&session_delete], &[]).files[0].class,
        Unexplained
    );
    // Two tree paths fit one claim: ambiguous, no claim.
    assert_eq!(
        claim_path(
            "/r/packages/foo/src/index.ts",
            Some("/r/packages/foo"),
            &[],
            &known(&["src/index.ts", "packages/foo/src/index.ts"]),
            false,
        ),
        None
    );
    assert_eq!(
        claim_path(
            "/r/packages/foo/src/index.ts",
            Some("/r/packages/foo"),
            &[],
            &known(&["packages/foo/src/index.ts"]),
            false,
        )
        .as_deref(),
        Some("packages/foo/src/index.ts")
    );
    // Outside the session's cwd nothing is inferred.
    assert_eq!(
        claim_path(
            "/home/alice/src/x.rs",
            Some("/home/alice/r"),
            &[],
            &known(&["src/x.rs"]),
            false,
        ),
        None
    );
    // `/` is never a root; `..` never resolves.
    assert_eq!(
        claim_path("/x.rs", Some("/"), &[], &known(&["x.rs"]), false),
        None
    );
    assert_eq!(
        claim_path("../x.rs", Some("/r/sub"), &[], &known(&["x.rs"]), false),
        None
    );
    assert_eq!(
        claim_path("./x.rs", None, &[], &known(&[]), false).as_deref(),
        Some("x.rs")
    );
    // The verifying checkout's root is exact, for deletions too.
    assert_eq!(
        claim_path(
            "/ci/repo/a.rs",
            Some("/elsewhere"),
            &[Path::new("/ci/repo")],
            &known(&[]),
            true
        )
        .as_deref(),
        Some("a.rs")
    );
}

#[test]
fn removals_are_flagged() {
    let flags = |base: Option<&[u8]>, committed: Option<&[u8]>| {
        run(&[file("a", base, committed)], &[], &[]).files[0].removes
    };
    assert!(!flags(None, Some(b"new\n")));
    assert!(flags(Some(b"old\n"), None));
    // A guard removed, a comment added elsewhere: the removal is no line.
    assert!(flags(Some(b"a\ncheck\nb\n"), Some(b"a\nb\ncomment\n")));
    // A replacement shows as an unexplained line instead.
    assert!(!flags(Some(b"a\ncheck\nb\n"), Some(b"a\nCHANGED\nb\n")));
    assert!(!flags(Some(b"a\n"), Some(b"a\nmore\n")));
    assert!(flags(Some(b"x\0y"), Some(b"x\0z")));
}

#[test]
fn absorbing_file_wise_results_equals_one_pass() {
    let session = session(vec![write("a", "one\n"), write("b", "two\n")]);
    let a = [file("a", None, Some(b"one\n"))];
    let b = [file("b", None, Some(b"HUMAN\n"))];
    let mut merged = run(&b, &[&session], &[]);
    merged.absorb(run(&a, &[&session], &[]));
    let both = [a[0].clone(), b[0].clone()];
    assert_eq!(merged, run(&both, &[&session], &[]));
}

#[test]
fn claimed_lines_missing_from_the_commit_count_as_removal() {
    // The agent wrote a guard; someone removed it and added a comment.
    let session = session(vec![write("a", "fn a(){}\nif !ok { return }\nfn b(){}\n")]);
    let changed = [file("a", None, Some(b"fn a(){}\nfn b(){}\n// note\n"))];
    let result = run(&changed, &[&session], &[]);
    assert_eq!(result.files[0].class, Unexplained);
    assert_eq!(result.files[0].unexplained_lines(), 1);
    assert!(result.files[0].removes);
    // A pure insertion keeps every claimed line: no removal.
    let changed = [file(
        "a",
        None,
        Some(b"fn a(){}\nif !ok { return }\n// note\nfn b(){}\n"),
    )];
    assert!(!run(&changed, &[&session], &[]).files[0].removes);
}

#[test]
fn backed_oversized_files_weigh_one_line() {
    let big = "x\n".repeat(LINE_LEVEL_LIMIT);
    let session = session(vec![write("big", &big)]);
    let mut backed = file("big", None, Some(big.as_bytes()));
    backed.added_lines = 1_000_000;
    let result = run(&[backed.clone()], &[&session], &[]);
    assert_eq!(result.files[0].class, ReportedOnly);
    assert_eq!((result.total_changed_lines, result.backed_lines()), (1, 1));
    // Unexplained, the upper bound applies in full.
    let result = run(&[backed], &[], &[]);
    assert_eq!(result.total_changed_lines, 1_000_000);
    assert_eq!(result.unexplained_lines(), 1_000_000);
}

#[test]
fn removals_made_by_the_agent_are_not_reported_as_unobserved() {
    // The agent removed the guard (claimed), a human appended a line.
    let session = session(vec![call(
        "Edit",
        "a",
        json!({"old_string": "guard\n", "new_string": ""}),
        Some(b"a\nb\n"),
    )]);
    let changed = [file("a", Some(b"a\nguard\nb\n"), Some(b"a\nb\nc\n"))];
    let result = run(&changed, &[&session], &[]);
    assert_eq!(result.files[0].class, Unexplained);
    assert_eq!(lines(&result.files[0]), [(3, Unexplained)]);
    assert!(!result.files[0].removes);
}

/// EA-03: der I/O-Pfad des Readers über einen echten Commit — Abgleich,
/// Datei- und Session-Seite — strikt lesend.
#[test]
fn artifact_and_render_mark_the_human_line_read_only() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let redacted = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_session(session(vec![
            write("a", "one\ntwo\nthree\n"),
            write("b", "three\n"),
        ]))
        .unwrap();
    let store = InRepoStore::open(dir.path()).unwrap();
    let id = store.put(&redacted).unwrap().id();
    std::fs::write(dir.path().join("a"), "one\nHUMAN\nthree\n").unwrap();
    std::fs::write(dir.path().join("b"), "three\n").unwrap();
    git(dir.path(), &["add", "a", "b"]);
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
    // Eine Store-Index-Kante, die sich selbst `Observed` nennt und den
    // menschlichen Stand „erklären" würde: Der Trailer entscheidet, nicht der
    // geteilte Ref.
    let forged = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_session(session(vec![write("a", "one\nHUMAN\nthree\n")]))
        .unwrap();
    let forged = store.put(&forged).unwrap().id();
    store
        .link(
            forged,
            &commit.to_string(),
            minds_core::EvidenceMark::of(minds_core::EvidenceSource::Observed),
        )
        .unwrap();
    let before_refs = git(dir.path(), &["show-ref"]);
    let before = snapshot(dir.path());

    let index = Index::build(&repo, &store).unwrap();
    assert_eq!(index.sessions_of(commit).len(), 2);
    let (claimants, inferred) = index.claimants(commit);
    assert_eq!(claimants.len(), 1);
    assert!(!inferred);
    assert_eq!(SessionId::of(claimants[0]).unwrap(), id);
    // Die nur per Index verknüpfte Session nimmt am Abgleich nicht teil.
    assert!(index.claimed_commits(forged).is_empty());
    assert_eq!(index.claimed_commits(id), [commit]);
    let spellings = minds_reader::artifact::roots_of(&repo);
    let roots: Vec<&Path> = spellings.iter().map(|p| p.as_path()).collect();
    let artifact = index.artifact(&repo, &roots, commit);
    assert_eq!(artifact.subject.as_deref(), Some("fixture"));
    let minds_reader::artifact::ArtifactState::Assessed(assessed) = &artifact.state else {
        panic!("{artifact:?}");
    };
    assert_eq!(
        minds_reader::artifact::summary(&assessed.recon),
        "artifact 3/4 lines explained"
    );
    assert_eq!(assessed.recon.files[0].unexplained_ranges(), [(2, 2)]);
    assert_eq!(
        assessed.recon.files[0].note(),
        "line 2 not observed in the session"
    );
    assert_eq!(assessed.recon.files[1].class, ReportedOnly);
    assert_eq!(artifact, index.artifact(&repo, &roots, commit));

    // Die Seite: außerhalb des Repos geschrieben.
    let out = tempfile::tempdir().unwrap();
    minds_reader::render(&repo, &store, out.path()).unwrap();
    let page = std::fs::read_to_string(out.path().join("a.html")).unwrap();
    let marked: Vec<&str> = page
        .lines()
        .filter(|l| l.contains(" unexplained\""))
        .collect();
    assert_eq!(marked.len(), 1, "{page}");
    assert!(marked[0].contains("</span>2</span><code>HUMAN"), "{page}");
    assert!(page.contains("class=\"recon-legend\""));
    // b ist voll belegt: keine Markierung, keine Legende.
    let page = std::fs::read_to_string(out.path().join("b.html")).unwrap();
    assert!(!page.contains(" unexplained\""), "{page}");
    assert!(!page.contains("class=\"recon-legend\""), "{page}");
    // Die Seite der Trailer-Session trägt den Abgleich; die nur per Index
    // verknüpfte nicht.
    let page_of = |id: SessionId| {
        let short: String = id
            .to_string()
            .trim_start_matches("b3-")
            .chars()
            .take(12)
            .collect();
        std::fs::read_to_string(out.path().join(format!("session-{short}.html"))).unwrap()
    };
    assert!(!page_of(forged).contains("class=\"artifact\""));
    let session_page = page_of(id);
    assert!(
        session_page.contains("artifact 3/4 lines explained"),
        "{session_page}"
    );
    assert!(session_page.contains("<tr class=\"add unexplained\">"));

    assert_eq!(before_refs, git(dir.path(), &["show-ref"]));
    assert_eq!(
        before,
        snapshot(dir.path()),
        "no file or Git object may be written"
    );
}

/// Ohne jede Kante gibt es keine Claimants — und nichts ist „vermutet".
#[test]
fn no_links_means_no_claimants() {
    let s = session(vec![write("a", "text\n")]);
    let id = SessionId::of(&s).unwrap();
    let index = Index::from_parts(BTreeMap::from([(id, s)]), BTreeMap::new());
    let (claimants, inferred) = index.claimants(commit());
    assert!(claimants.is_empty() && !inferred);
}

fn stored(store: &InRepoStore, calls: Vec<ToolCall>) -> SessionId {
    let redacted = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_session(session(calls))
        .unwrap();
    store.put(&redacted).unwrap().id()
}

/// Der Trailer entscheidet auch für Commits, die HEAD nicht erreicht: Eine
/// Store-Kante (geteilter Ref, frei beschreibbar) auf einen Seitenzweig-Commit
/// mit fremdem Trailer erklärt keine Zeile. Nur ein Commit **ohne** Trailer
/// fällt auf Store-Kanten zurück — und heißt dann vermutet.
#[test]
fn store_edges_never_override_a_trailer_outside_head() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    // A schrieb genau die Bytes beider Commits; B nicht.
    let a = stored(&store, vec![write("y", "y\n"), write("x", "x\n")]);
    let b = stored(&store, vec![write("x", "other\n")]);

    // Y auf dem Hauptzweig, ohne Trailer.
    std::fs::write(dir.path().join("y"), "y\n").unwrap();
    git(dir.path(), &["add", "y"]);
    git(dir.path(), &["commit", "-qm", "no trailer"]);
    let main = String::from_utf8(git(dir.path(), &["branch", "--show-current"])).unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    let y = repo.head().unwrap().commit().unwrap();

    // X auf einem Seitenzweig, Trailer nennt B; HEAD kehrt zurück.
    git(dir.path(), &["switch", "-qc", "side"]);
    std::fs::write(dir.path().join("x"), "x\n").unwrap();
    git(dir.path(), &["add", "x"]);
    git(
        dir.path(),
        &["commit", "-qm", &format!("side\n\nMinds-Session-Id: {b}")],
    );
    let x = Repo::open(dir.path())
        .unwrap()
        .head()
        .unwrap()
        .commit()
        .unwrap();
    git(dir.path(), &["switch", "-q", main.trim()]);

    let observed = minds_core::EvidenceMark::of(minds_core::EvidenceSource::Observed);
    let heuristic = minds_core::EvidenceMark::of(minds_core::EvidenceSource::Heuristic);
    store.link(a, &x.to_string(), observed).unwrap();
    store.link(a, &y.to_string(), heuristic).unwrap();

    let repo = Repo::open(dir.path()).unwrap();
    let index = Index::build(&repo, &store).unwrap();
    assert_eq!(index.position(x), None, "X liegt außerhalb von HEAD");
    assert_eq!(index.trailer_ids(x), [b]);
    assert_eq!(index.subject_of(x), Some("side"));

    // X: nur B, nicht vermutet — A erklärt dort nichts.
    let (claimants, inferred) = index.claimants(x);
    assert_eq!(
        claimants
            .iter()
            .map(|s| SessionId::of(s).unwrap())
            .collect::<Vec<_>>(),
        [b]
    );
    assert!(!inferred);
    assert_eq!(index.claimed_commits(a), [y]);
    // Die vom Trailer genannte Session sieht ihren Commit.
    assert_eq!(index.claimed_commits(b), [x]);
    assert_eq!(
        index.evidence_of(x, b).map(|m| m.source),
        Some(minds_core::EvidenceSource::Observed)
    );
    let spellings = minds_reader::artifact::roots_of(&repo);
    let roots: Vec<&Path> = spellings.iter().map(|p| p.as_path()).collect();
    let artifact = index.artifact(&repo, &roots, x);
    let minds_reader::artifact::ArtifactState::Assessed(assessed) = &artifact.state else {
        panic!("{artifact:?}");
    };
    assert_eq!(
        minds_reader::artifact::summary(&assessed.recon),
        "artifact 0/1 lines explained"
    );
    assert!(minds_reader::artifact::provenance_note(&artifact).is_empty());

    // Y: ohne Trailer zählt die Store-Kante — als vermutet benannt.
    let (claimants, inferred) = index.claimants(y);
    assert_eq!(claimants.len(), 1);
    assert!(inferred);
    let artifact = index.artifact(&repo, &roots, y);
    assert!(artifact.inferred);
    assert_eq!(
        minds_reader::artifact::provenance_note(&artifact),
        minds_reader::artifact::INFERRED_NOTE
    );
    let minds_reader::artifact::ArtifactState::Assessed(assessed) = &artifact.state else {
        panic!("{artifact:?}");
    };
    assert_eq!(
        minds_reader::artifact::summary(&assessed.recon),
        "artifact 1/1 lines explained"
    );
}

/// Ein Commit, den der Klon (noch) nicht hat, ist nicht „ohne Trailer":
/// Sein Trailer ist unbekannt, eine Store-Kante wählt keine Claimants.
#[test]
fn a_store_edge_to_a_missing_commit_chooses_no_claimants() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    let a = stored(&store, vec![write("x", "x\n")]);
    std::fs::write(dir.path().join("x"), "x\n").unwrap();
    git(dir.path(), &["add", "x"]);
    git(dir.path(), &["commit", "-qm", "base"]);
    let missing: CommitId = "0123456789abcdef0123456789abcdef01234567".parse().unwrap();
    let observed = minds_core::EvidenceMark::of(minds_core::EvidenceSource::Observed);
    store.link(a, &missing.to_string(), observed).unwrap();
    // Auch ein Schlüssel, der auf einen Blob statt auf einen Commit zeigt.
    let blob: CommitId = String::from_utf8(git(dir.path(), &["rev-parse", "HEAD:x"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    store.link(a, &blob.to_string(), observed).unwrap();

    let repo = Repo::open(dir.path()).unwrap();
    let index = Index::build(&repo, &store).unwrap();
    for commit in [missing, blob] {
        assert!(index.trailer_unknown(commit), "{commit}");
        let (claimants, inferred) = index.claimants(commit);
        assert!(claimants.is_empty() && !inferred, "{commit}");
    }
    assert!(index.claimed_commits(a).is_empty());
}

/// Ein Ersatzobjekt (`git replace`) ohne Trailer verdeckt den Trailer eines
/// Commits außerhalb von HEAD nicht: Er bleibt unbekannt.
#[test]
fn a_replaced_commit_outside_head_has_an_unknown_trailer() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    let a = stored(&store, vec![write("x", "x\n")]);
    let b = stored(&store, vec![write("x", "other\n")]);
    std::fs::write(dir.path().join("y"), "y\n").unwrap();
    git(dir.path(), &["add", "y"]);
    git(dir.path(), &["commit", "-qm", "base"]);
    let main = String::from_utf8(git(dir.path(), &["branch", "--show-current"])).unwrap();
    git(dir.path(), &["switch", "-qc", "side"]);
    std::fs::write(dir.path().join("x"), "x\n").unwrap();
    git(dir.path(), &["add", "x"]);
    git(
        dir.path(),
        &["commit", "-qm", &format!("side\n\nMinds-Session-Id: {b}")],
    );
    let x = String::from_utf8(git(dir.path(), &["rev-parse", "HEAD"])).unwrap();
    git(dir.path(), &["commit", "-q", "--amend", "-m", "no trailer"]);
    let replacement = String::from_utf8(git(dir.path(), &["rev-parse", "HEAD"])).unwrap();
    git(dir.path(), &["replace", x.trim(), replacement.trim()]);
    git(dir.path(), &["switch", "-q", main.trim()]);
    let x: CommitId = x.trim().parse().unwrap();
    let observed = minds_core::EvidenceMark::of(minds_core::EvidenceSource::Observed);
    store.link(a, &x.to_string(), observed).unwrap();

    let repo = Repo::open(dir.path()).unwrap();
    let index = Index::build(&repo, &store).unwrap();
    // Entweder liest die Git-Schicht das Original (Trailer B) oder sie
    // verwirft das Ersatzobjekt (Trailer unbekannt) — nie gilt X als
    // trailerlos, nie wählt die Store-Kante A.
    let (claimants, inferred) = index.claimants(x);
    assert!(!inferred);
    assert!(
        claimants.iter().all(|s| SessionId::of(s).unwrap() == b),
        "{claimants:?}"
    );
    assert!(index.trailer_unknown(x) || index.trailer_ids(x) == [b]);
    assert!(index.claimed_commits(a).is_empty());
}

/// Nennt der Trailer nur Sessions, die der Index nicht hält, erklärt
/// niemand — und die Oberfläche sagt warum, statt still „0/N" zu zeigen.
#[test]
fn a_trailer_without_readable_sessions_says_so() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    std::fs::write(dir.path().join("a"), "a\n").unwrap();
    git(dir.path(), &["add", "a"]);
    let ghost = format!("b3-{}", "e".repeat(64));
    git(
        dir.path(),
        &[
            "commit",
            "-qm",
            &format!("ghost\n\nMinds-Session-Id: {ghost}"),
        ],
    );
    let repo = Repo::open(dir.path()).unwrap();
    let commit = repo.head().unwrap().commit().unwrap();
    let index = Index::build(&repo, &store).unwrap();
    assert_eq!(index.trailer_ids(commit).len(), 1);
    let artifact = index.artifact(&repo, &[], commit);
    assert_eq!(artifact.claimants, 0);
    assert!(!artifact.inferred);
    assert_eq!(
        minds_reader::artifact::provenance_note(&artifact),
        minds_reader::artifact::NO_CLAIMANT_NOTE
    );
}

/// Eine Zeile: (Zeile, Klasse, (Turn, Aufruf) der Herkunft).
type Sourced = (u32, ReconClass, Option<(usize, usize)>);

/// Je Zeile: Nummer, Klasse und Herkunft.
fn sources(file: &FileRecon) -> Vec<Sourced> {
    match &file.line_level {
        LineLevel::Available(lines) => lines
            .iter()
            .map(|l| (l.line, l.class, l.source.as_ref().map(|s| (s.turn, s.call))))
            .collect(),
        other => panic!("expected lines, got {other:?}"),
    }
}

/// Die Herkunft wandert mit: Ein Edit gibt nur den Zeilen, die er einführt,
/// seinen Aufruf; was er stehen lässt, behält den Write davor. Eine Zeile,
/// die kein Aufruf schrieb, hat keine Herkunft.
#[test]
fn every_backed_line_knows_the_call_that_introduced_it() {
    let edit = call(
        "Edit",
        "f",
        json!({"old_string": "b\n", "new_string": "B\nB2\n"}),
        Some(b"a\nB\nB2\nc\n"),
    );
    let session = session(vec![write("f", "a\nb\nc\n"), edit]);
    let id = SessionId::of(&session).unwrap();
    let result = run_sourced(
        &[file("f", None, Some(b"a\nB\nB2\nc\nhuman\n"))],
        &[&session],
        &[],
    );
    assert_eq!(
        sources(&result.files[0]),
        [
            (1, ReportedOnly, Some((0, 0))),
            (2, ReportedOnly, Some((0, 1))),
            (3, ReportedOnly, Some((0, 1))),
            (4, ReportedOnly, Some((0, 0))),
            (5, Unexplained, None),
        ]
    );
    let LineLevel::Available(lines) = &result.files[0].line_level else {
        unreachable!()
    };
    assert!(
        lines
            .iter()
            .flat_map(|l| l.source.as_deref())
            .all(|s| s.session == id)
    );
}

/// Mit Witness: dieselbe Herkunft für bestätigte Zeilen; ohne passenden
/// Claim (nur im Dateisystem gesehen) keine.
#[test]
fn witnessed_lines_keep_their_source_and_fs_only_lines_have_none() {
    let text = b"one\ntwo\n";
    let session = session(vec![write("a", "one\ntwo\n")]);
    let result = run_sourced(
        &[file("a", None, Some(text)), file("b", None, Some(text))],
        &[&session],
        &[
            observation("a", Some(text), 1),
            observation("b", Some(text), 2),
        ],
    );
    assert_eq!(
        sources(&result.files[0]),
        [(1, Explained, Some((0, 0))), (2, Explained, Some((0, 0)))]
    );
    assert_eq!(
        sources(&result.files[1]),
        [(1, ExplainedFsOnly, None), (2, ExplainedFsOnly, None)]
    );
}

/// Wie [`run`], samt Herkunft je Zeile.
fn run_sourced(
    changed: &[ChangedFile<'_>],
    sessions: &[&Session],
    observations: &[FsObservation],
) -> Reconciliation {
    reconcile_with_sources(&ReconInput {
        commit: commit(),
        base: None,
        changed,
        sessions,
        observations,
        roots: &[],
    })
}

/// Ohne Anfrage keine Herkunft — `verify` zahlt nicht für die Anzeige —, und
/// die Klassen sind mit und ohne dieselben.
#[test]
fn sources_are_computed_only_on_request_and_never_change_the_classes() {
    let session = session(vec![write("f", "a\nb\n")]);
    let changed = [file("f", None, Some(b"a\nb\nc\n"))];
    let plain = run(&changed, &[&session], &[]);
    let sourced = run_sourced(&changed, &[&session], &[]);
    assert!(sources(&plain.files[0]).iter().all(|(_, _, s)| s.is_none()));
    assert!(
        sources(&sourced.files[0])
            .iter()
            .any(|(_, _, s)| s.is_some())
    );
    assert_eq!(lines(&plain.files[0]), lines(&sourced.files[0]));
    assert_eq!(
        (plain.explained_lines, plain.total_changed_lines),
        (sourced.explained_lines, sourced.total_changed_lines)
    );
}

/// Ein Delete löscht die Herkunft: Was danach geschrieben wird, gehört ganz
/// dem neuen Aufruf. Ebenso nach einer Wiedergabe, die nicht zum Hash passt.
#[test]
fn delete_and_failed_replays_restart_the_sources() {
    let delete = call("Delete", "f", json!({}), None);
    let deleted = session(vec![write("f", "a\nb\n"), delete, write("f", "a\nb\n")]);
    let result = run_sourced(&[file("f", None, Some(b"a\nb\n"))], &[&deleted], &[]);
    assert_eq!(
        sources(&result.files[0]),
        [
            (1, ReportedOnly, Some((0, 2))),
            (2, ReportedOnly, Some((0, 2)))
        ]
    );

    let broken = call(
        "Edit",
        "f",
        json!({"old_string": "missing", "new_string": "x"}),
        Some(b"x\n"),
    );
    let failed = session(vec![write("f", "a\n"), broken, write("f", "a\nz\n")]);
    let result = run_sourced(&[file("f", None, Some(b"a\nz\n"))], &[&failed], &[]);
    assert_eq!(
        sources(&result.files[0]),
        [
            (1, ReportedOnly, Some((0, 2))),
            (2, ReportedOnly, Some((0, 2)))
        ]
    );
}

/// Eine Zeile aus der Basis, die ein Edit stehen lässt, ist keine geänderte
/// Zeile — eine geänderte daneben trägt den Edit.
#[test]
fn an_edit_on_a_base_file_attributes_only_what_it_changed() {
    let edit = call(
        "Edit",
        "f",
        json!({"old_string": "old\n", "new_string": "new\n"}),
        Some(b"keep\nnew\n"),
    );
    let session = session(vec![edit]);
    let result = reconcile_with_sources(&ReconInput {
        commit: commit(),
        base: None,
        changed: &[file("f", Some(b"keep\nold\n"), Some(b"keep\nnew\n"))],
        sessions: &[&session],
        observations: &[],
        roots: &[],
    });
    assert_eq!(sources(&result.files[0]), [(2, ReportedOnly, Some((0, 0)))]);
}

/// Über dem Budget wird die Herkunft nicht weitergetragen — die Klasse
/// bleibt, die Anzeige verliert nur die Quelle.
#[test]
fn sources_stop_beyond_the_budget_but_classes_stay() {
    let line = "x".repeat(99) + "\n";
    let text = line.repeat(1024 * 1024 * 3 / 2 / line.len());
    let writes = (0..12).map(|_| write("f", &text)).collect();
    let session = session(writes);
    let result = run_sourced(&[file("f", None, Some(text.as_bytes()))], &[&session], &[]);
    let lines = sources(&result.files[0]);
    assert!(lines.iter().all(|(_, class, _)| *class == ReportedOnly));
    assert!(lines.iter().all(|(_, _, source)| source.is_none()));
}

/// `verify` hält je geänderter Zeile ein `LineRecon` — die Herkunft (nur
/// auf Anfrage) darf das nicht aufblähen.
#[test]
fn a_line_recon_stays_small() {
    assert!(std::mem::size_of::<LineRecon>() <= 16);
}

fn exec(command: &str) -> ToolCall {
    let mut call = call("Bash", "", json!({ "command": command }), None);
    let effect = call.effect.as_mut().unwrap();
    effect.kind = EffectKind::Exec;
    effect.path = None;
    call
}

fn gap_of(result: &Reconciliation, path: &str) -> Option<Gap> {
    result.files.iter().find(|f| f.path == path).unwrap().gap
}

/// Je unerklärter Datei der Grund: nach dem Agenten geändert, vom Witness
/// anders gesehen, nur von einem Shell-Befehl genannt, oder unberührt.
#[test]
fn every_unexplained_file_names_why() {
    let agent = session(vec![
        write("src/a.rs", "one\ntwo\n"),
        write("src/fmt.rs", "x\n"),
        exec("cargo fmt -- ./src/fmt.rs && echo done"),
        exec(r#"python3 gen.py > "out/gen.txt""#),
        write("ok.rs", "fine\n"),
    ]);
    let result = run(
        &[
            file("src/a.rs", None, Some(b"one\nHUMAN\n")),
            file("src/fmt.rs", None, Some(b"y\n")),
            file("out/gen.txt", None, Some(b"generated\n")),
            file("seen.rs", None, Some(b"new\n")),
            file("hand.rs", None, Some(b"a\nb\n")),
            file("ok.rs", None, Some(b"fine\n")),
        ],
        &[&agent],
        &[observation("seen.rs", Some(b"old\n"), 1)],
    );
    assert_eq!(
        gap_of(&result, "src/a.rs"),
        Some(Gap::AfterAgent { later_shell: None })
    );
    let Some(Gap::AfterAgent {
        later_shell: Some(fmt),
    }) = gap_of(&result, "src/fmt.rs")
    else {
        panic!("a later shell command names src/fmt.rs");
    };
    assert_eq!((fmt.turn, fmt.call), (0, 2));
    let Some(Gap::Shell(generator)) = gap_of(&result, "out/gen.txt") else {
        panic!("a shell command names out/gen.txt");
    };
    assert_eq!(generator.call, 3);
    assert_eq!(gap_of(&result, "seen.rs"), Some(Gap::WitnessOther));
    assert_eq!(
        gap_of(&result, "hand.rs"),
        Some(Gap::Untouched { complete: true })
    );
    assert_eq!(gap_of(&result, "ok.rs"), None, "only unexplained files");
    // Dieselbe Zählung wie `unexplained_lines`, aufgeschlüsselt.
    let counts = result.unexplained_by_gap();
    assert_eq!(
        counts,
        GapCounts {
            after_agent: 2,
            witness_other: 1,
            shell: 1,
            untouched: 2,
            ..GapCounts::default()
        }
    );
    assert_eq!(counts.total(), result.unexplained_lines());
}

/// Die Heuristik nennt nur ganze Pfade oder Dateinamen — kein Präfix — und
/// ein Shell-Befehl vor dem Schreibvorgang des Agenten zählt nicht als
/// „danach".
#[test]
fn shell_mentions_match_whole_names_and_only_after_the_agent() {
    let agent = session(vec![
        exec("cat src/ab.rs src/a.rs.bak"),
        exec("sed -i s/x/y/ src/late.rs"),
        write("src/late.rs", "x\n"),
    ]);
    let result = run(
        &[
            file("src/a.rs", None, Some(b"a\n")),
            file("src/late.rs", None, Some(b"y\n")),
        ],
        &[&agent],
        &[],
    );
    assert_eq!(
        gap_of(&result, "src/a.rs"),
        Some(Gap::Untouched { complete: true })
    );
    assert_eq!(
        gap_of(&result, "src/late.rs"),
        Some(Gap::AfterAgent { later_shell: None })
    );
}

/// Ein Schreibvorgang ohne Hash belegt nicht „danach geändert"; ein nicht
/// zuordenbarer Claim-Pfad nicht „nichts nennt die Datei"; eine nur opake
/// Beobachtung nicht „nie diese Fassung".
#[test]
fn gaps_claim_no_more_than_the_sessions_hold() {
    let mut unhashed = write("u.rs", "u\n");
    unhashed.effect.as_mut().unwrap().written = None;
    let agent = session(vec![unhashed, write("/elsewhere/checkout/far.rs", "f\n")]);
    let mut opaque = observation("dark.rs", None, 1);
    opaque.opaque = true;
    let result = run(
        &[
            file("u.rs", None, Some(b"u\n")),
            file("far.rs", None, Some(b"f\n")),
            file("dark.rs", None, Some(b"d\n")),
        ],
        &[&agent],
        &[opaque],
    );
    assert_eq!(gap_of(&result, "u.rs"), Some(Gap::Unhashed));
    assert_eq!(gap_of(&result, "far.rs"), Some(Gap::Unmapped));
    assert_eq!(gap_of(&result, "dark.rs"), Some(Gap::WitnessOpaque));
    assert_eq!(
        result.unexplained_by_gap().total(),
        result.unexplained_lines()
    );
}

/// Ein Dateiname allein erwähnt eine Datei nur als bloßes Wort ohne `/`;
/// JSON-Escapes (`\n`) trennen Wörter, statt sie zu verkleben.
#[test]
fn mentions_respect_directories_and_json_escapes() {
    let agent = session(vec![
        exec("cat lib/mod.rs"),
        exec("cd src/b && cat util.rs"),
        exec("echo start\nsed -i s/a/b/ src/n.rs"),
    ]);
    let result = run(
        &[
            file("src/mod.rs", None, Some(b"m\n")),
            file("src/b/util.rs", None, Some(b"u\n")),
            file("src/n.rs", None, Some(b"n\n")),
        ],
        &[&agent],
        &[],
    );
    assert_eq!(
        gap_of(&result, "src/mod.rs"),
        Some(Gap::Untouched { complete: true })
    );
    assert!(matches!(gap_of(&result, "src/b/util.rs"), Some(Gap::Shell(s)) if s.call == 1));
    assert!(matches!(gap_of(&result, "src/n.rs"), Some(Gap::Shell(s)) if s.call == 2));
}

/// Agent-gesteuerte Befehlstexte kosten linear: tief verschachtelte
/// Riesenwörter, ein Multibyte-Zeichen an der Schnittgrenze, ein
/// erschöpftes Wort-Budget — danach schweigt die Heuristik.
#[test]
fn mention_heuristic_is_bounded() {
    // Tief verschachtelte Riesenwörter unter 64 KiB: Sie erreichen den
    // Tokenizer und werden dort übersprungen (> 4096 Bytes).
    let mut calls: Vec<ToolCall> = (0..64)
        .map(|i| exec(&format!("cat {}{i}/late.rs", "a/".repeat(16 * 1024))))
        .collect();
    // Mehr verschiedene Wörter, als der Index aufnimmt (256 Ki), je Aufruf
    // unter 64 KiB — nur das Wort-Budget greift.
    for call in 0..50 {
        let words: String = (0..6_000).map(|w| format!("{call}k{w}.i ")).collect();
        assert!(words.len() < 60 * 1024);
        calls.push(exec(&words));
    }
    calls.push(exec("touch late.rs"));
    let agent = session(calls);
    let started = std::time::Instant::now();
    let result = run(&[file("late.rs", None, Some(b"l\n"))], &[&agent], &[]);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    // Das Wort-Budget griff: „nichts gefunden" sagt dazu, dass es nicht
    // alles sah.
    assert_eq!(
        gap_of(&result, "late.rs"),
        Some(Gap::Untouched { complete: false })
    );
}

/// „Danach" folgt der gemischten Turn-Reihenfolge über Sessions, nicht der
/// Reihenfolge der Eingabe.
#[test]
fn later_shell_follows_merged_turn_time() {
    let agent = session(vec![write("x.rs", "x\n")]);
    let at = |time: &str| {
        let mut other = session(vec![exec("rustfmt x.rs")]);
        other.turns[0].at = Some(time.into());
        other
    };
    let (before, after) = (at("2026-10-02T09:00:00Z"), at("2026-10-02T11:00:00Z"));
    let gap = |other: &Session| {
        gap_of(
            &run(&[file("x.rs", None, Some(b"y\n"))], &[other, &agent], &[]),
            "x.rs",
        )
    };
    assert_eq!(gap(&before), Some(Gap::AfterAgent { later_shell: None }));
    assert!(matches!(
        gap(&after),
        Some(Gap::AfterAgent {
            later_shell: Some(_)
        })
    ));
}

/// Der letzte Claim trägt genau die Commit-Fassung, nur der Witness sah
/// zuletzt anderes: nicht „danach geändert".
#[test]
fn a_matching_claim_against_the_witness_is_not_after_the_agent() {
    let agent = session(vec![write("w.rs", "ok\n")]);
    let result = run(
        &[file("w.rs", None, Some(b"ok\n"))],
        &[&agent],
        &[observation("w.rs", Some(b"other\n"), 1)],
    );
    assert_eq!(result.files[0].class, Unexplained);
    assert_eq!(gap_of(&result, "w.rs"), Some(Gap::WitnessOther));
}

/// Nur die letzte lesbare Beobachtung zählt: Sah sie die Commit-Fassung,
/// ist es nicht „der Witness sah zuletzt anderes".
#[test]
fn only_the_latest_readable_observation_contradicts() {
    let mut opaque = observation("o.rs", None, 3);
    opaque.opaque = true;
    let result = run(
        &[file("o.rs", None, Some(b"new\n"))],
        &[],
        &[
            observation("o.rs", Some(b"old\n"), 1),
            observation("o.rs", Some(b"new\n"), 2),
            opaque,
        ],
    );
    // Der Witness sah zuletzt genau diese Fassung lesbar, danach nur opak:
    // nicht „anderes gesehen", nicht „nichts gefunden".
    assert_eq!(gap_of(&result, "o.rs"), Some(Gap::WitnessOpaque));
}

/// Ein nicht zuordenbarer Claim nennt nur seinen eigenen Pfad, nicht jede
/// Datei gleichen Namens.
#[test]
fn unmapped_claims_match_paths_not_names() {
    let agent = session(vec![
        write("/tmp/far.rs", "f\n"),
        write("/elsewhere/checkout/src/near.rs", "n\n"),
    ]);
    let result = run(
        &[
            file("src/far.rs", None, Some(b"f\n")),
            file("src/near.rs", None, Some(b"n\n")),
        ],
        &[&agent],
        &[],
    );
    assert_eq!(
        gap_of(&result, "src/far.rs"),
        Some(Gap::Untouched { complete: true })
    );
    assert_eq!(gap_of(&result, "src/near.rs"), Some(Gap::Unmapped));
    // Keine Komponentengrenze: `/x/ba.rs` endet nicht auf den Pfad `a.rs`.
    let agent = session(vec![write("/x/ba.rs", "a\n")]);
    let result = run(&[file("a.rs", None, Some(b"a\n"))], &[&agent], &[]);
    assert_eq!(
        gap_of(&result, "a.rs"),
        Some(Gap::Untouched { complete: true })
    );
}

/// Die Reihenfolge der Claims zählt: Ein späterer Write mit Hash macht
/// einen früheren ohne Hash bedeutungslos; eine Löschung als letzter Claim
/// bei vorhandener Datei ist „danach geändert".
#[test]
fn the_last_claim_decides_hashed_or_not() {
    let mut early = write("h.rs", "h\n");
    early.effect.as_mut().unwrap().written = None;
    let agent = session(vec![
        early,
        write("h.rs", "h2\n"),
        write("d.rs", "d\n"),
        call("Delete", "d.rs", json!({}), None),
    ]);
    let result = run(
        &[
            file("h.rs", None, Some(b"h3\n")),
            file("d.rs", None, Some(b"back\n")),
        ],
        &[&agent],
        &[],
    );
    assert_eq!(
        gap_of(&result, "h.rs"),
        Some(Gap::AfterAgent { later_shell: None })
    );
    assert_eq!(
        gap_of(&result, "d.rs"),
        Some(Gap::AfterAgent { later_shell: None })
    );
}

/// Ein Windows-Pfad mit maskierten Backslashes wird nicht zum bloßen
/// Dateinamen, der jede Datei des Namens trifft.
#[test]
fn escaped_windows_paths_do_not_match_as_bare_names() {
    let agent = session(vec![exec(r"type C:\proj\src\bar.rs")]);
    let result = run(&[file("lib/bar.rs", None, Some(b"b\n"))], &[&agent], &[]);
    assert_eq!(
        gap_of(&result, "lib/bar.rs"),
        Some(Gap::Untouched { complete: true })
    );
}

/// Ein Windows-Pfad erwähnt auch keine Datei im Wurzelverzeichnis.
#[test]
fn windows_paths_do_not_match_root_files() {
    let agent = session(vec![exec(r"type C:\\proj\\src\\bar.rs")]);
    let result = run(&[file("bar.rs", None, Some(b"b\n"))], &[&agent], &[]);
    assert_eq!(
        gap_of(&result, "bar.rs"),
        Some(Gap::Untouched { complete: true })
    );
}

/// Wird ein Aufruf bei 64 KiB abgeschnitten, zählt das angeschnittene
/// letzte Wort nicht (`src/a.rs.bak` wäre sonst `src/a.rs`) — und „nichts
/// gefunden" sagt, dass die Suche unvollständig war.
#[test]
fn a_cut_word_does_not_count() {
    // `arguments` ist JSON: `{"command":"` (12 Bytes) davor. Der Schnitt
    // fällt genau hinter `src/a.rs`.
    let mut text = "x".repeat(64 * 1024 - 12 - 1 - "src/a.rs".len());
    text.push(' ');
    text.push_str("src/a.rs.bak");
    let agent = session(vec![exec(&text)]);
    let result = run(&[file("src/a.rs", None, Some(b"a\n"))], &[&agent], &[]);
    assert_eq!(
        gap_of(&result, "src/a.rs"),
        Some(Gap::Untouched { complete: false })
    );
}

/// Je Dateiname höchstens 4096 Wörter — danach schweigt die Heuristik für
/// diesen Namen und sagt es.
#[test]
fn mentions_per_name_are_capped() {
    let words: String = (0..5000).map(|i| format!("d{i}/mod.rs ")).collect();
    let agent = session(vec![exec(&words), exec("cat src/mod.rs")]);
    let result = run(&[file("src/mod.rs", None, Some(b"m\n"))], &[&agent], &[]);
    assert_eq!(
        gap_of(&result, "src/mod.rs"),
        Some(Gap::Untouched { complete: false })
    );
}

/// Die Zusammenfassung erbt „unvollständig" — und nennt Heuristik als
/// Heuristik.
#[test]
fn the_summary_keeps_its_qualifiers() {
    let mut counts = GapCounts::default();
    counts.add(Gap::Untouched { complete: true }, 2);
    counts.add(Gap::Untouched { complete: false }, 1);
    counts.add(
        Gap::Shell(LineSource {
            session: SessionId::of(&session(vec![])).unwrap(),
            turn: 0,
            call: 0,
        }),
        1,
    );
    let summary = minds_reader::artifact::gap_summary(&counts).unwrap();
    assert_eq!(
        summary,
        "1 only mentioned by a shell command (heuristic) · 3 with no tool claim or shell mention found (search incomplete)"
    );
}

/// Ein nicht zuordenbarer Claim nach dem letzten zugeordneten: Was die
/// Session zuletzt mit der Datei tat, ist offen — nicht „danach geändert".
#[test]
fn a_later_unmapped_claim_wins_over_an_earlier_mapped_one() {
    let agent = session(vec![
        write("a.rs", "one\n"),
        write("/elsewhere/checkout/a.rs", "two\n"),
    ]);
    let result = run(&[file("a.rs", None, Some(b"two\n"))], &[&agent], &[]);
    assert_eq!(result.files[0].class, Unexplained);
    assert_eq!(gap_of(&result, "a.rs"), Some(Gap::Unmapped));
    // Ein früherer nicht zuordenbarer Claim zählt nach einem zugeordneten
    // nicht.
    let agent = session(vec![
        write("/elsewhere/checkout/b.rs", "two\n"),
        write("b.rs", "one\n"),
    ]);
    let result = run(&[file("b.rs", None, Some(b"two\n"))], &[&agent], &[]);
    assert_eq!(
        gap_of(&result, "b.rs"),
        Some(Gap::AfterAgent { later_shell: None })
    );
}

/// Widerspricht der Witness und trägt der letzte Claim keinen Hash, ist es
/// „ohne Hash", nicht „der Witness sah anderes".
#[test]
fn an_unhashed_claim_against_the_witness_stays_unhashed() {
    let mut unhashed = write("u.rs", "u\n");
    unhashed.effect.as_mut().unwrap().written = None;
    let agent = session(vec![unhashed]);
    let result = run(
        &[file("u.rs", None, Some(b"u\n"))],
        &[&agent],
        &[observation("u.rs", Some(b"other\n"), 1)],
    );
    assert_eq!(gap_of(&result, "u.rs"), Some(Gap::Unhashed));
}

/// Nicht zuordenbare Claims: derselbe Pfad zählt einmal, mit seiner letzten
/// Position; je Name höchstens 4096 — darüber „search incomplete".
#[test]
fn unmapped_claims_are_deduplicated_and_capped() {
    let agent = session(vec![
        write("/elsewhere/a.rs", "x\n"),
        write("a.rs", "one\n"),
        write("/elsewhere/a.rs", "two\n"),
    ]);
    let result = run(&[file("a.rs", None, Some(b"two\n"))], &[&agent], &[]);
    assert_eq!(gap_of(&result, "a.rs"), Some(Gap::Unmapped));

    let calls: Vec<ToolCall> = (0..4100)
        .map(|i| write(&format!("/elsewhere/{i}/m.rs"), "m\n"))
        .collect();
    let agent = session(calls);
    let result = run(&[file("hand.rs", None, Some(b"h\n"))], &[&agent], &[]);
    assert_eq!(
        gap_of(&result, "hand.rs"),
        Some(Gap::Untouched { complete: false })
    );
}

/// Ein Multibyte-Zeichen genau an der 64-KiB-Grenze: kein Panic, und der
/// Schnitt gilt als unvollständig.
#[test]
fn a_cut_inside_a_multibyte_character_does_not_panic() {
    let mut cut = "x".repeat(64 * 1024 - 1);
    cut.push('ä');
    let agent = session(vec![exec(&cut)]);
    let result = run(&[file("z.rs", None, Some(b"z\n"))], &[&agent], &[]);
    assert_eq!(
        gap_of(&result, "z.rs"),
        Some(Gap::Untouched { complete: false })
    );
}

/// Auch ein nicht zuordenbarer Delete ist `Unmapped`; ein Windows-Claim
/// endet auf den verschachtelten Repo-Pfad, nicht auf einen anderen.
#[test]
fn unmapped_deletes_and_windows_claims_match_their_path() {
    let agent = session(vec![
        call("Delete", "/elsewhere/checkout/d.rs", json!({}), None),
        write(r"C:\p\src\a.rs", "a\n"),
    ]);
    let result = run(
        &[
            file("d.rs", Some(b"d\n"), None),
            file("src/a.rs", None, Some(b"a\n")),
            file("b/a.rs", None, Some(b"a\n")),
        ],
        &[&agent],
        &[],
    );
    assert_eq!(gap_of(&result, "d.rs"), Some(Gap::Unmapped));
    assert_eq!(gap_of(&result, "src/a.rs"), Some(Gap::Unmapped));
    assert_eq!(
        gap_of(&result, "b/a.rs"),
        Some(Gap::Untouched { complete: true })
    );
}

/// Ein pfadartiges Wort über 4096 Bytes wird übersprungen — und „nichts
/// gefunden" sagt dann, dass die Suche unvollständig war.
#[test]
fn an_overlong_path_word_marks_the_search_incomplete() {
    let agent = session(vec![exec(&format!("cat {}/src/a.rs", "d".repeat(5000)))]);
    let result = run(&[file("src/a.rs", None, Some(b"a\n"))], &[&agent], &[]);
    assert_eq!(
        gap_of(&result, "src/a.rs"),
        Some(Gap::Untouched { complete: false })
    );
}
