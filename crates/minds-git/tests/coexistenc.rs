//! „Wer Minds nicht nutzt, merkt nichts" — Punkt 8 der Definition of Done.
//!
//! Die Zusagen aus `refs.rs` und `amend.rs` sind einzeln getestet; hier stehen
//! sie zusammen gegen ein Repository, an dem Minds gearbeitet hat, und werden
//! mit echtem `git` überprüft statt mit unseren eigenen Lesefunktionen. Das ist
//! der Punkt: Ob ein Ref „unsichtbar" ist, entscheidet `git branch`, nicht wir.

mod support;

use minds_git::{DEFAULT_CONTEXT_REF, Repo};
use support::{TempRepo, session_id, store_session};

/// Ein Repository, in dem Minds gearbeitet hat: eine Session im Kontext-Ref,
/// ein Trailer am Production-Commit.
fn repo_after_minds() -> (TempRepo, Repo) {
    let fixture = TempRepo::init();
    fixture.write_file("src/retry.rs", "fn retry() {}\n");
    fixture.commit("feat: Retry");

    let repo = Repo::open(fixture.path()).unwrap();
    let session = session_id('a');
    store_session(&repo, session, r#"{"intent":{"request":"Retry"}}"#);
    repo.amend_head_with_sessions(&[session]).unwrap();

    (fixture, repo)
}

#[test]
fn no_new_branch_shows_up() {
    let (fixture, _repo) = repo_after_minds();

    let branches = fixture.git(&["branch", "--list"]);
    assert!(!branches.contains("minds"), "sichtbar geworden: {branches}");

    // Auffindbar ist der Ref trotzdem — er versteckt sich nicht, er steht nur
    // nicht im Weg.
    let refs = fixture.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"]);
    assert_eq!(refs.trim(), DEFAULT_CONTEXT_REF);
}

#[test]
fn the_working_tree_and_the_index_stay_clean() {
    // Der Amend rührt weder Index noch Arbeitsverzeichnis an; nach getaner
    // Arbeit steht das Repo da wie vorher.
    let (fixture, _repo) = repo_after_minds();

    let status = fixture.git(&["status", "--porcelain"]);
    assert!(status.trim().is_empty(), "nicht sauber:\n{status}");
}

#[test]
fn the_code_history_is_one_commit_long_and_has_no_context_in_it() {
    // Der Kontext hängt an keinem Produktions-Commit. Ein `git log` sieht
    // deshalb genau das, was der Mensch committet hat.
    let (fixture, _repo) = repo_after_minds();

    let log = fixture.git(&["log", "--format=%s"]);
    assert_eq!(log.trim(), "feat: Retry");

    let reachable = fixture.git(&["rev-list", "--count", "HEAD"]);
    assert_eq!(reachable.trim(), "1");
}

#[test]
fn git_fsck_stays_quiet() {
    // Der Amend schreibt ein Commit-Objekt von Hand, der Kontext-Ref eine
    // eigene Orphan-Historie. Beides muss Gits eigener Strukturprüfung
    // standhalten — sonst merkt es niemand, bis jemand pusht.
    //
    // `--no-dangling`, weil der ersetzte Commit nach dem Amend erwartungsgemäß
    // unerreichbar herumliegt, bis `git gc` ihn einsammelt.
    let (fixture, _repo) = repo_after_minds();

    let report = fixture.git(&["fsck", "--no-dangling"]);
    assert!(report.trim().is_empty(), "fsck meldet:\n{report}");
}

#[test]
fn fetching_only_the_context_ref_brings_no_source_code() {
    // Die Zusage des Orphan-Refs aus `refs.rs`: Wer nur die Sessions braucht —
    // der Reader aus M7, ein Auditor —, zieht kein Byte Quellcode mit. Das
    // lässt sich nur mit zwei Repositories prüfen, also erst hier.
    let (source, _repo) = repo_after_minds();
    let code_blob = source.hash("HEAD:src/retry.rs");

    let reader = TempRepo::init();
    let remote = source.path().to_str().expect("Temp-Pfad ist UTF-8");
    reader.git(&[
        "fetch",
        "--quiet",
        remote,
        &format!("{DEFAULT_CONTEXT_REF}:{DEFAULT_CONTEXT_REF}"),
    ]);

    assert!(
        !reader.git_ok(&["cat-file", "-e", &code_blob]),
        "der Code-Blob ist mitgekommen"
    );

    let files = fixture_tree(&reader);
    assert!(
        files.iter().any(|path| path.starts_with("sessions/b3/")),
        "die Sessions fehlen: {files:?}"
    );
}

/// Die Dateien im geholten Kontext-Baum, gelesen mit `git` statt mit unseren
/// eigenen Funktionen — sonst prüfte der Test sich selbst.
fn fixture_tree(repo: &TempRepo) -> Vec<String> {
    repo.git(&["ls-tree", "-r", "--name-only", DEFAULT_CONTEXT_REF])
        .lines()
        .map(str::to_owned)
        .collect()
}
