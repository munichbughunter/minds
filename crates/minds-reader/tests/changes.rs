//! Der Changes-Tab ab Repository: Diff je Datei, Klasse und Herkunft je
//! `+`-Zeile, die Begründung einer Zeile — gegen ein echtes Repo und einen
//! echten Store.

use std::path::Path;
use std::process::Command;

use minds_core::{Agent, Effect, EffectKind, Intent, Model, Role, Session, ToolCall, Turn};
use minds_git::{DiffKind, Repo};
use minds_reader::Inspection;
use minds_reader::reconcile::ReconClass;
use minds_store::{ContextStore, InRepoStore};
use serde_json::json;

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

/// Eine Diff-Zeile zum Vergleich: Art, alte und neue Nummer, Text, Klasse,
/// ob eine Herkunft da ist.
type Row<'a> = (
    DiffKind,
    Option<u32>,
    Option<u32>,
    &'a str,
    Option<ReconClass>,
    bool,
);

fn write(path: &str, text: &str) -> ToolCall {
    ToolCall {
        outcome: None,
        name: "Write".into(),
        arguments: json!({ "content": text }).to_string(),
        capture: None,
        effect: Some(Effect {
            kind: EffectKind::Write,
            path: Some(path.into()),
            content: None,
            written: Some(minds_core::ContentHash::from_bytes(
                *blake3::hash(text.as_bytes()).as_bytes(),
            )),
            written_unavailable: None,
        }),
    }
}

/// Ein Repo mit Basis-Commit, einer Session, die `a` umschreibt, und einem
/// Commit mit Trailer, in dem ein Mensch eine Zeile dazugeschrieben hat.
fn fixture() -> (tempfile::TempDir, Repo, InRepoStore) {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(dir.path().join("a"), "keep\nold\n").unwrap();
    git(dir.path(), &["add", "a"]);
    git(dir.path(), &["commit", "-qm", "base"]);

    let mut session = Session::new(
        Agent {
            name: "claude-code".into(),
            version: "test".into(),
        },
        Model {
            provider: "test".into(),
            id: "opus".into(),
        },
        Intent {
            request: "Ersetze old durch new".into(),
            ..Intent::default()
        },
    );
    session.turns.push(Turn {
        role: Role::Assistant,
        text: "AC2 verlangt Stabilität,\ndeshalb ersetze ich old.".into(),
        tool_calls: vec![write("a", "keep\nnew\n")],
        parent: None,
        at: Some("2026-10-09T10:00:00Z".into()),
    });
    let redacted = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_session(session)
        .unwrap();
    let store = InRepoStore::open(dir.path()).unwrap();
    let id = store.put(&redacted).unwrap().id();
    std::fs::write(dir.path().join("a"), "keep\nnew\n\thuman\n").unwrap();
    std::fs::write(dir.path().join("bin"), [0u8, 1, 2]).unwrap();
    git(dir.path(), &["add", "a", "bin"]);
    git(
        dir.path(),
        &[
            "commit",
            "-qm",
            &format!("feat: neu\n\nMinds-Session-Id: {id}"),
        ],
    );
    let repo = Repo::open(dir.path()).unwrap();
    (dir, repo, store)
}

#[test]
fn the_diff_carries_class_and_source_per_added_line() {
    let (_dir, repo, store) = fixture();
    let inspection = Inspection::load(&repo, &store, None, "t").unwrap();
    let head = repo.head().unwrap().commit().unwrap();
    assert_eq!(inspection.head(), Some(head));
    assert_eq!(inspection.change_commits().first(), Some(&head));

    let set = inspection.changes(&repo, head).unwrap();
    assert_eq!(set.subject.as_deref(), Some("feat: neu"));
    assert!(set.unassessed.is_none(), "{:?}", set.unassessed);
    let a = set.files.iter().find(|f| f.path == "a").unwrap();
    assert_eq!((a.added, a.removed), (2, 1));
    let rows: Vec<Row<'_>> = a
        .rows
        .iter()
        .map(|r| {
            (
                r.kind,
                r.old,
                r.new,
                r.text.as_str(),
                r.class,
                r.source.is_some(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (DiffKind::Hunk, None, None, "@@ -1,2 +1,3 @@", None, false),
            (DiffKind::Context, Some(1), Some(1), "keep", None, false),
            (DiffKind::Removed, Some(2), None, "old", None, false),
            (
                DiffKind::Added,
                None,
                Some(2),
                "new",
                Some(ReconClass::ReportedOnly),
                true
            ),
            // Der Tab erscheint als Leerzeichen, nicht als `\t`.
            (
                DiffKind::Added,
                None,
                Some(3),
                "    human",
                Some(ReconClass::Unexplained),
                false
            ),
        ]
    );
    assert_eq!(a.unexplained(), 1);

    let bin = set.files.iter().find(|f| f.path == "bin").unwrap();
    assert_eq!(bin.note.as_deref(), Some("binary"));
    assert!(bin.rows.is_empty());
}

#[test]
fn a_line_knows_why_it_exists() {
    let (_dir, repo, store) = fixture();
    let inspection = Inspection::load(&repo, &store, None, "t").unwrap();
    let head = repo.head().unwrap().commit().unwrap();
    let set = inspection.changes(&repo, head).unwrap();
    let source = set.files[0]
        .rows
        .iter()
        .find_map(|r| r.source)
        .expect("the agent's line has a source");
    let reason = inspection.line_reason(source).unwrap();
    assert_eq!(reason.tool, "Write");
    assert_eq!(reason.request, "Ersetze old durch new");
    // Einzeilig: Der Umbruch im Turn-Text wird ein Leerzeichen.
    assert_eq!(
        reason.said.as_deref(),
        Some("AC2 verlangt Stabilität, deshalb ersetze ich old.")
    );
    assert_eq!(reason.agent, "claude-code · opus");
    assert_eq!((reason.turn, reason.call), (0, 0));
}

#[test]
fn reading_changes_writes_nothing() {
    let (dir, repo, store) = fixture();
    let refs = || {
        Command::new("git")
            .current_dir(dir.path())
            .args(["show-ref"])
            .output()
            .unwrap()
            .stdout
    };
    let before = refs();
    let inspection = Inspection::load(&repo, &store, None, "t").unwrap();
    let head = repo.head().unwrap().commit().unwrap();
    let _ = inspection.changes(&repo, head).unwrap();
    assert_eq!(before, refs());
    let _ = store.list().unwrap();
}

/// Was das Terminal steuern könnte — ANSI und OSC 8, Bidi-Umkehr,
/// Zeilentrenner, `\r`, BEL — erreicht die Anzeige nie roh: nicht über den
/// Pfad, nicht über eine Diff-Zeile, nicht über den Text des Agenten.
#[test]
fn nothing_in_the_changes_can_steer_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "base"]);
    let path = "src\u{202E}txt.rs";
    let text = "x\u{1b}]8;;https://evil\u{7}click\u{1b}]8;;\u{7}\na\u{2028}+fake\nok\rEVIL\n";
    let mut session = Session::new(
        Agent {
            name: "claude-code".into(),
            version: "test".into(),
        },
        Model {
            provider: "test".into(),
            id: "opus".into(),
        },
        Intent {
            request: "do \u{1b}[31mthis".into(),
            ..Intent::default()
        },
    );
    session.turns.push(Turn {
        role: Role::Assistant,
        text: "\u{1b}[2J\u{1b}[Hcleared".into(),
        tool_calls: vec![write(path, text)],
        parent: None,
        at: None,
    });
    let redacted = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_session(session)
        .unwrap();
    let store = InRepoStore::open(dir.path()).unwrap();
    let id = store.put(&redacted).unwrap().id();
    std::fs::write(dir.path().join(path), text).unwrap();
    git(dir.path(), &["add", "."]);
    git(
        dir.path(),
        &["commit", "-qm", &format!("evil\n\nMinds-Session-Id: {id}")],
    );
    let repo = Repo::open(dir.path()).unwrap();
    let inspection = Inspection::load(&repo, &store, None, "t").unwrap();
    let head = repo.head().unwrap().commit().unwrap();
    let set = inspection.changes(&repo, head).unwrap();
    let raw = |s: &str| {
        s.chars()
            .any(|c| matches!(c, '\u{1b}' | '\u{7}' | '\u{202E}' | '\u{2028}' | '\r'))
    };
    let file = &set.files[0];
    assert!(!raw(&file.path), "{:?}", file.path);
    for row in &file.rows {
        assert!(!raw(&row.text), "{:?}", row.text);
    }
    if let Some(source) = file.rows.iter().find_map(|r| r.source) {
        let reason = inspection.line_reason(source).unwrap();
        for shown in [
            Some(&reason.request),
            reason.said.as_ref(),
            Some(&reason.tool),
        ]
        .into_iter()
        .flatten()
        {
            assert!(!raw(shown), "{shown:?}");
        }
    }
}
