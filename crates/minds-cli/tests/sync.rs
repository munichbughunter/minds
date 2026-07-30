//! `minds sync` gegen ein echtes Remote — die drei Zusagen des Push-Pfades.
//!
//! Der `pre-push`-Hook rief bis v0.2 selbst `git push`. Das kostete auf jedem
//! Push den vollen Verbindungsaufbau (gegen gitlab.com ~1,9 s gemessen), auch
//! wenn es gar nichts Neues gab. Die drei Zusagen, die das ablösen, stehen hier
//! als Tests — gegen ein bares Repo nebenan, ohne Netz:
//!
//! 1. Alle fälligen Refs gehen in **einem** Aufruf mit.
//! 2. Ist nichts neu, passiert **nichts** (kein Push, keine Ausgabe).
//! 3. Divergiert der Review-Log, wird **vereinigt** statt überschrieben.
//!
//! Braucht `git` im Pfad; fehlt es, überspringen sich die Tests selbst.

use std::path::Path;
use std::process::{Command, Output};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

/// Eine Change-Id, wie `minds review` sie als Subjekt annimmt.
fn change_id(fill: &str) -> String {
    format!("I{}", fill.repeat(20))
}

/// Ein Arbeits-Repo mit einem Commit und einem baren Remote daneben.
///
/// Gibt `None` zurück, wenn kein `git` da ist — dann überspringt sich der Test,
/// statt falsch-rot zu werden.
fn repo_with_remote() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let remote = dir.path().join("remote.git");
    std::fs::create_dir_all(&work).unwrap();

    if !Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&remote)
        .status()
        .ok()?
        .success()
    {
        return None;
    }
    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "test@minds.invalid"]);
    git(&work, &["config", "user.name", "Minds Test"]);
    git(
        &work,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    std::fs::write(work.join("a.txt"), "hallo\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "erster Commit"]);

    Some((dir, work))
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git läuft")
}

fn minds(dir: &Path, args: &[&str]) -> Output {
    Command::new(MINDS)
        .current_dir(dir)
        .args(args)
        .output()
        .expect("minds läuft")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Die Refs, die am Remote liegen.
fn remote_refs(work: &Path) -> String {
    String::from_utf8_lossy(&git(work, &["ls-remote", "origin", "refs/minds/*"]).stdout)
        .into_owned()
}

#[test]
fn all_due_refs_travel_in_one_call() {
    let Some((_dir, work)) = repo_with_remote() else {
        return;
    };

    // Zwei Minds-Refs: ein Verdict und ein Kontext-Stand.
    minds(&work, &["review", &change_id("ab"), "--approve"]);
    let head = String::from_utf8_lossy(&git(&work, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git(&work, &["update-ref", "refs/minds/context", &head]);

    let out = minds(&work, &["sync", "--remote", "origin"]);
    assert!(out.status.success(), "{}", text(&out));

    // Ein Aufruf, beide Refs — und die Meldung nennt beide.
    assert!(
        text(&out).contains("2 Ref(s)"),
        "beide Refs müssen in einem Push gehen: {}",
        text(&out)
    );
    let refs = remote_refs(&work);
    assert!(refs.contains("refs/minds/context"), "{refs}");
    assert!(
        refs.contains("refs/minds/reviews"),
        "Reviews müssen am Remote ankommen: {refs}"
    );

    // Und der Erfolg ist lokal vermerkt — das ist die Buchhaltung, die den
    // nächsten Lauf ohne Netz auskommen lässt.
    let tracking =
        String::from_utf8_lossy(&git(&work, &["for-each-ref", "refs/minds/remotes/"]).stdout)
            .into_owned();
    assert!(
        tracking.contains("refs/minds/remotes/origin/context"),
        "{tracking}"
    );
    assert!(
        tracking.contains("refs/minds/remotes/origin/reviews"),
        "{tracking}"
    );
}

#[test]
fn nothing_new_means_nothing_happens() {
    // Die eigentliche Beschleunigung: Der zweite Lauf darf das Remote nicht
    // anfassen. Beobachtbar ist das an der Ausgabe — ohne fällige Refs wird
    // nicht einmal die Fortschrittszeile gedruckt.
    let Some((_dir, work)) = repo_with_remote() else {
        return;
    };
    minds(&work, &["review", &change_id("cd"), "--approve"]);

    let first = minds(&work, &["sync", "--remote", "origin"]);
    assert!(text(&first).contains("Ref(s)"), "{}", text(&first));

    let second = minds(&work, &["sync", "--remote", "origin", "-v"]);
    assert!(second.status.success());
    assert!(
        !text(&second).contains("Ref(s) →"),
        "ohne neue Refs darf kein Push laufen: {}",
        text(&second)
    );
    assert!(text(&second).contains("nichts Neues"), "{}", text(&second));
}

#[test]
fn a_diverged_review_log_is_merged_not_overwritten() {
    // Zwei Reviewer, zwei Maschinen, dasselbe Remote. Der zweite Push wird
    // abgewiesen (non-fast-forward) — und darf den fremden Verdict weder
    // überschreiben noch verlieren.
    let Some((_dir, work)) = repo_with_remote() else {
        return;
    };
    let Some((_dir_b, other)) = repo_with_remote() else {
        return;
    };
    // Beide zeigen auf dasselbe Remote.
    let remote_url = String::from_utf8_lossy(&git(&work, &["remote", "get-url", "origin"]).stdout)
        .trim()
        .to_string();
    git(&other, &["remote", "set-url", "origin", &remote_url]);

    // Maschine A legt ein Verdict ab und pusht.
    minds(
        &work,
        &[
            "review",
            &change_id("ab"),
            "--approve",
            "--summary",
            "von A",
        ],
    );
    assert!(
        minds(&work, &["sync", "--remote", "origin"])
            .status
            .success()
    );

    // Maschine B kennt das nicht, legt ihr eigenes Verdict ab und pusht.
    minds(
        &other,
        &["review", &change_id("cd"), "--reject", "--summary", "von B"],
    );
    let out = minds(&other, &["sync", "--remote", "origin"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("vereinige"),
        "die Divergenz muss über den Merge laufen: {}",
        text(&out)
    );

    // Beide Verdicts sind am Remote — keines wurde überschrieben.
    minds(&work, &["sync", "--remote", "origin"]);
    let fetched = minds(&other, &["reviews", &change_id("ab")]);
    assert!(
        text(&fetched).contains("von A"),
        "das fremde Verdict muss erhalten bleiben: {}",
        text(&fetched)
    );
    let own = minds(&other, &["reviews", &change_id("cd")]);
    assert!(text(&own).contains("von B"), "{}", text(&own));
}
