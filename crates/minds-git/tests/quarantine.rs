//! Spec S2 §7.1 bis §7.4: `minds_git::quarantine` gegen Repositories, die
//! echtes `git` erzeugt hat (offline, isoliert, im Temp-Verzeichnis).

use std::path::{Path, PathBuf};
use std::process::Command;

use minds_git::quarantine::{Limits, ReceiveEnv, ReceiveError, RefTarget, ServerRepo};

fn git(dir: &Path, args: &[&str]) -> String {
    let empty = dir.join(".no-config");
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", &empty)
        .env("GIT_CONFIG_SYSTEM", &empty)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "2026-10-26T10:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-10-26T10:00:00Z")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn blob(dir: &Path, bytes: &[u8]) -> String {
    let file = dir.join(".blob");
    std::fs::write(&file, bytes).unwrap();
    git(dir, &["hash-object", "-w", file.to_str().unwrap()])
}

fn tree(dir: &Path, spec: &str) -> String {
    let empty = dir.join(".no-config");
    let mut child = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", &empty)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["mktree"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(spec.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn bare() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("server.git");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "--bare", "-b", "main"]);
    (dir, repo)
}

fn env(repo: &Path) -> ReceiveEnv {
    ReceiveEnv {
        git_dir: repo.to_owned(),
        quarantine: None,
        namespace: false,
    }
}

#[test]
fn reads_commits_trees_blobs_diffs_and_refs() {
    let (_tmp, repo) = bare();
    let a = blob(&repo, b"a0");
    let b = blob(&repo, b"b0");
    let sub = tree(&repo, &format!("100644 blob {b}\tx.rs\n"));
    let t1 = tree(
        &repo,
        &format!("100644 blob {a}\ta.rs\n040000 tree {sub}\tsrc\n"),
    );
    let c1 = git(&repo, &["commit-tree", &t1, "-m", "one"]);
    let a2 = blob(&repo, b"a1");
    let t2 = tree(&repo, &format!("100755 blob {a2}\ta.rs\n"));
    let c2 = git(&repo, &["commit-tree", &t2, "-p", &c1, "-m", "two"]);
    git(&repo, &["update-ref", "refs/heads/main", &c2]);
    git(&repo, &["update-ref", "refs/minds/bindings/x/1", &c1]);
    git(&repo, &["update-ref", "refs/minds/bindings/x/2", &c2]);
    git(
        &repo,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );

    let s = ServerRepo::open_for_receive(&env(&repo), Limits::default()).unwrap();
    let c = s.commit_parents(&c2).unwrap();
    assert_eq!(c.tree, t2);
    assert_eq!(c.parents, vec![c1.clone()]);
    let diff = s.tree_diff(Some(&t1), Some(&t2)).unwrap();
    let paths: Vec<String> = diff
        .iter()
        .map(|d| String::from_utf8(d.path.clone()).unwrap())
        .collect();
    assert_eq!(paths, vec!["a.rs", "src/x.rs"]);
    assert_eq!(s.blob_bytes(&a2, 100).unwrap().unwrap(), b"a1");
    assert_eq!(s.blob_bytes(&a2, 1).unwrap(), None);
    assert_eq!(s.lookup(&t1, &[b"src", b"x.rs"]).unwrap().unwrap().oid, b);
    assert_eq!(
        s.read_ref("refs/heads/main").unwrap(),
        Some(RefTarget::Direct(c2.clone()))
    );
    assert_eq!(
        s.read_ref("refs/heads/alias").unwrap(),
        Some(RefTarget::Symbolic("refs/heads/main".to_owned()))
    );
    assert_eq!(s.read_ref("refs/heads/none").unwrap(), None);
    let listed = s.read_prefix("refs/minds/bindings/x/").unwrap();
    assert_eq!(listed.len(), 2);
    // Memoisiert: Eine spätere Änderung wirkt im selben Aufruf nicht.
    git(&repo, &["update-ref", "-d", "refs/heads/main"]);
    git(&repo, &["update-ref", "refs/minds/bindings/x/3", &c2]);
    assert_eq!(
        s.read_ref("refs/heads/main").unwrap(),
        Some(RefTarget::Direct(c2.clone()))
    );
    assert_eq!(s.read_prefix("refs/minds/bindings/x/").unwrap(), listed);

    let rows = s
        .rev_list(
            Path::new("git"),
            &c2,
            &[],
            10,
            std::time::Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(rows.len(), 2);
    let rows = s
        .rev_list(
            Path::new("git"),
            &c2,
            std::slice::from_ref(&c1),
            10,
            std::time::Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(rows, vec![(c2.clone(), vec![c1.clone()])]);
}

#[test]
fn tampered_loose_object_and_preconditions() {
    let (_tmp, repo) = bare();
    let a = blob(&repo, b"original");
    // Ein loses Objekt durch ein anderes ersetzen (gültig komprimiert,
    // falscher Inhalt für seine Id).
    let other = blob(&repo, b"tampered");
    let path = |id: &str| repo.join("objects").join(&id[..2]).join(&id[2..]);
    let bytes = std::fs::read(path(&other)).unwrap();
    let target = path(&a);
    let mut perm = std::fs::metadata(&target).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&target, perm).unwrap();
    std::fs::write(&target, bytes).unwrap();
    let s = ServerRepo::open_for_receive(&env(&repo), Limits::default()).unwrap();
    assert_eq!(s.blob_bytes(&a, 100), Err(ReceiveError::Verify));

    let mut e = env(&repo);
    e.namespace = true;
    assert!(matches!(
        ServerRepo::open_for_receive(&e, Limits::default()),
        Err(ReceiveError::Precondition(_))
    ));
    git(&repo, &["config", "core.ignoreCase", "true"]);
    assert!(matches!(
        ServerRepo::open_for_receive(&env(&repo), Limits::default()),
        Err(ReceiveError::Precondition(_))
    ));
}
