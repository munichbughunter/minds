//! `minds bind` gegen einen echten Witness-Prozess (Spec S3-MVP §6.1):
//! Anfrage über den Socket, `bind` im Worker, feste Antworten. Die Auswahl
//! mit anrechenbarer Evidence prüfen die Witness-Tests (s3_11 bis s3_15);
//! hier gibt es nur Observations eines nicht getrennten Witness.

#![cfg(unix)]

#[path = "support/hook_witness.rs"]
mod support;

use std::process::{Command, Output};

use support::*;

const PROJECT: &str = "git.test/project-42";

fn git(f: &Fixture, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(&f.root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn bind(f: &Fixture, args: &[&str]) -> Output {
    minds()
        .arg("bind")
        .args(args)
        .current_dir(&f.root)
        .env(SOCKET_ENV, f.socket())
        .output()
        .unwrap()
}

fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Ein Repo mit zwei Commits und einem Witness mit festgehaltenem Projekt.
fn setup() -> Fixture {
    let f = Fixture::repo_only();
    for (k, v) in [
        ("user.name", "t"),
        ("user.email", "t@t"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
        ("minds.backend", "in-repo"),
    ] {
        git(&f, &["config", k, v]);
    }
    for file in ["a", "b"] {
        std::fs::write(f.root.join(file), file).unwrap();
        git(&f, &["add", file]);
        git(&f, &["commit", "-qm", file]);
    }
    for args in [
        vec![
            "witness",
            "init",
            "--repo",
            f.root.to_str().unwrap(),
            "--project",
            PROJECT,
        ],
        vec!["witness", "keygen"],
    ] {
        let out = minds()
            .args(&args)
            .arg("--home")
            .arg(&f.home)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out).1);
        if args[1] == "init" {
            assert!(
                text(&out).0.contains(&format!("pinned project: {PROJECT}")),
                "{}",
                text(&out).0
            );
        }
    }
    f
}

#[test]
fn bind_asks_the_witness_with_the_expected_project_only() {
    let f = setup();
    let _daemon = f.start();
    let head = git(&f, &["rev-parse", "HEAD"]);

    // Ohne `minds.project`: kein Aufruf, klare Meldung.
    let out = bind(&f, &["HEAD~1..HEAD"]);
    assert!(!out.status.success());
    assert!(text(&out).1.contains("no project set"), "{}", text(&out).1);

    // Ungültige Form.
    git(&f, &["config", "minds.project", "git.test/group/repo"]);
    let out = bind(&f, &["HEAD~1..HEAD"]);
    assert!(!out.status.success());
    assert!(
        text(&out).1.contains("must have the form"),
        "{}",
        text(&out).1
    );

    // Anderes Projekt: der Witness lehnt ab, nichts gebunden.
    git(&f, &["config", "minds.project", "git.test/project-7"]);
    let out = bind(&f, &["HEAD~1..HEAD"]);
    assert!(!out.status.success());
    assert!(
        text(&out)
            .0
            .contains(&format!("minds: {} not bound:", &head[..12]))
            && text(&out).0.contains("(project_mismatch)"),
        "{}",
        text(&out).0
    );
    assert_eq!(git(&f, &["for-each-ref", "refs/minds/bindings/"]), "");

    // Richtiges Projekt: Antwort je Commit; ohne getrennte Evidence keine
    // Bindung (Lauf im Worker-Prozess des Witness).
    git(&f, &["config", "minds.project", PROJECT]);
    let out = bind(&f, &["HEAD~1..HEAD"]);
    assert!(out.status.success(), "{:?}", text(&out));
    assert_eq!(
        text(&out).0.trim(),
        format!("minds: {} no evidence found, no binding", &head[..12])
    );
    // Ein einzelner Rev: alle erreichbaren Commits, Eltern zuerst.
    let out = bind(&f, &["--dry-run", "HEAD"]);
    assert!(out.status.success(), "{:?}", text(&out));
    assert_eq!(text(&out).0.lines().count(), 2, "{}", text(&out).0);
    assert!(text(&out).0.lines().last().unwrap().contains(&head[..12]));
    assert_eq!(git(&f, &["for-each-ref", "refs/minds/bindings/"]), "");
}

#[test]
fn bind_without_a_witness_says_so() {
    let f = setup();
    git(&f, &["config", "minds.project", PROJECT]);
    // Kein laufender Witness am Socket.
    let out = bind(&f, &["HEAD~1..HEAD"]);
    assert!(!out.status.success());
    assert!(
        text(&out).0.contains("not bound: witness unavailable"),
        "{:?}",
        text(&out)
    );
    // Ohne Socket-Variable und ohne Witness am Standardort.
    let out = minds()
        .args(["bind", "HEAD~1..HEAD"])
        .current_dir(&f.root)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        text(&out).1.contains("no witness found"),
        "{}",
        text(&out).1
    );
}
