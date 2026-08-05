//! `minds stack` und die Kontinuität über einen Force-Push (Schicht 3, R3).
//!
//! Die Zusage: Ein Stapel aus drei aufeinander aufbauenden Changes wird
//! **einzeln** reviewt, und ein Force-Push — der jeden Commit-Hash umschreibt —
//! lässt jedes Verdict an seinem Change stehen. Das ist der Unterschied zwischen
//! „Review am Commit" und „Review an der Change-Id", und er ist der Grund, warum
//! Schicht 2 die Change-Id vor Schicht 3 gebracht hat.
//!
//! Braucht `git`; fehlt es, überspringen sich die Tests selbst.

use std::path::Path;
use std::process::{Command, Output};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.env("PATH", path_with_minds());
    without_user_config(&mut cmd).output().expect("git läuft")
}

/// Schneidet die Git-Config des Entwicklers ab: Ein global gesetztes
/// `core.hooksPath` (husky, lefthook) verschiebt seit #9 auch hier das
/// Hook-Verzeichnis, `commit.gpgsign` verlangt eine Signatur. Beides machte den
/// Lauf von der Maschine abhängig. `/dev/null` schaltet die Config-Ebene ab.
fn without_user_config(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
}

/// Der `PATH` für Git-Aufrufe: vorneweg das Verzeichnis des Test-Binaries.
///
/// Die von `minds enable` installierten Hooks rufen `minds` **ohne Pfad** auf.
/// Ohne diesen Eintrag greift der Aufruf ins Leere, `|| true` schluckt ihn, und
/// der Commit bekäme keine Change-Id — der Test wäre rot aus einem Grund, der
/// nichts mit der Zusage zu tun hat. Das Verzeichnis steht **vorn**, damit auch
/// eine global installierte `minds` den Lauf nicht verfälscht.
fn path_with_minds() -> std::ffi::OsString {
    let bin_dir = Path::new(MINDS)
        .parent()
        .expect("Binary hat ein Verzeichnis");
    let mut dirs = vec![bin_dir.to_path_buf()];
    dirs.extend(
        std::env::var_os("PATH")
            .iter()
            .flat_map(std::env::split_paths),
    );
    std::env::join_paths(dirs).expect("PATH lässt sich zusammensetzen")
}

fn minds(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir).args(args);
    without_user_config(&mut cmd).output().expect("minds läuft")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Ein Repo mit Hooks (für die Change-Id) und einem Commit auf `main`.
fn repo() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    if !git(dir.path(), &["init", "-q", "-b", "main"])
        .status
        .success()
    {
        return None;
    }
    git(dir.path(), &["config", "user.email", "anna@example.org"]);
    git(dir.path(), &["config", "user.name", "Anna"]);
    minds(dir.path(), &["enable"]);
    std::fs::write(dir.path().join("a.txt"), "eins\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "Basis"]);
    Some(dir)
}

/// Legt einen Commit an und gibt dessen Change-Id zurück.
fn commit_with_change(dir: &Path, file: &str, message: &str) -> String {
    std::fs::write(dir.join(file), format!("{message}\n")).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", message]);
    let body = String::from_utf8_lossy(&git(dir, &["show", "-s", "--format=%B", "HEAD"]).stdout)
        .into_owned();
    body.lines()
        .find_map(|line| line.strip_prefix("Minds-Change-Id: "))
        .map(|id| id.trim().to_owned())
        .unwrap_or_else(|| panic!("kein Change-Id-Trailer:\n{body}"))
}

#[test]
fn the_stack_shows_each_change_with_its_own_verdict() {
    let Some(dir) = repo() else { return };
    let dir = dir.path();

    git(dir, &["checkout", "-q", "-b", "topic"]);
    let first = commit_with_change(dir, "b.txt", "feat: b");
    let second = commit_with_change(dir, "c.txt", "feat: c");
    let _third = commit_with_change(dir, "d.txt", "feat: d");

    minds(dir, &["review", &first, "--approve"]);
    minds(
        dir,
        &["review", &second, "--needs-work", "--summary", "Backoff"],
    );
    minds(
        dir,
        &[
            "comment",
            &second,
            "--on",
            "c.txt:1",
            "hier bitte nachziehen",
        ],
    );

    let out = minds(dir, &["stack", "--base", "main"]);
    assert!(out.status.success(), "{}", text(&out));
    let listing = text(&out);

    // Drei Changes, jeder mit seinem eigenen Stand.
    assert!(listing.contains("3 Change(s)"), "{listing}");
    assert!(listing.contains("approve"), "{listing}");
    assert!(listing.contains("needs-work"), "{listing}");
    assert!(listing.contains("kein Verdict"), "{listing}");
    assert!(listing.contains("1 Kommentar"), "{listing}");
    assert!(listing.contains("1 von 3 approbiert"), "{listing}");
}

#[test]
fn a_force_push_of_the_stack_keeps_every_verdict() {
    let Some(dir) = repo() else { return };
    let dir = dir.path();

    // Ein Remote, damit „Force-Push" hier wirklich einer ist.
    let bare = dir.join("remote.git");
    assert!(
        Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&bare)
            .status()
            .expect("git läuft")
            .success()
    );
    git(dir, &["remote", "add", "origin", &bare.to_string_lossy()]);

    git(dir, &["checkout", "-q", "-b", "topic"]);
    let first = commit_with_change(dir, "b.txt", "feat: b");
    let second = commit_with_change(dir, "c.txt", "feat: c");

    minds(
        dir,
        &["review", &first, "--approve", "--summary", "erster geprüft"],
    );
    minds(
        dir,
        &[
            "review",
            &second,
            "--needs-work",
            "--summary",
            "zweiter offen",
        ],
    );

    let hashes_before = String::from_utf8_lossy(&git(dir, &["rev-list", "main..HEAD"]).stdout)
        .trim()
        .to_owned();
    git(dir, &["push", "-q", "origin", "topic"]);

    // Der Stapel wird überarbeitet: beide Commits neu geschrieben, dann
    // force-gepusht. Genau der Vorgang, an dem ein commit-gebundener Review
    // stirbt.
    git(dir, &["rebase", "--quiet", "main"]);
    std::fs::write(dir.join("c.txt"), "feat: c — überarbeitet\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "--amend", "--no-edit"]);
    let force = git(dir, &["push", "-q", "--force", "origin", "topic"]);
    assert!(force.status.success(), "{}", text(&force));

    let hashes_after = String::from_utf8_lossy(&git(dir, &["rev-list", "main..HEAD"]).stdout)
        .trim()
        .to_owned();
    assert_ne!(
        hashes_before, hashes_after,
        "ohne neue Hashes prüft dieser Test nichts"
    );

    // Und beide Verdicts stehen weiterhin an ihrem Change.
    let listing = text(&minds(dir, &["stack", "--base", "main"]));
    assert!(listing.contains("approve"), "{listing}");
    assert!(listing.contains("needs-work"), "{listing}");
    assert!(
        !listing.contains("kein Verdict"),
        "ein Verdict ist beim Force-Push verloren gegangen:\n{listing}"
    );

    for (change, summary) in [(&first, "erster geprüft"), (&second, "zweiter offen")] {
        let found = text(&minds(dir, &["reviews", change]));
        assert!(found.contains(summary), "{found}");
    }
}
