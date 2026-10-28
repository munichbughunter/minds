//! Isolations-Job (Spec S3-MVP §9): Tests, die root-eigene Dateien
//! brauchen. In der normalen CI als `ignored` ausgewiesen, nie still
//! übersprungen; ausgeführt mit
//! `cargo test -p minds-cli --test witness_isolation -- --ignored` auf einem
//! frischen Runner mit `sudo` (oder als root).
//!
//! Commit 7: s3_28, Teil `geschützt` — die root-eigene Kette des
//! Vertrauensankers und jeder Fall, der sie bricht.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

/// Ein Befehl als root: direkt, wenn der Test als root läuft, sonst mit
/// `sudo -n` (der Isolations-Job hat es; fehlt es, scheitert der Test laut).
fn as_root(args: &[&str]) {
    let status = if is_root() {
        Command::new(args[0]).args(&args[1..]).status()
    } else {
        Command::new("sudo").arg("-n").args(args).status()
    }
    .unwrap();
    assert!(status.success(), "als root: {args:?}");
}

/// Ein root-eigener Baum unter `/opt`, beim Verlassen entfernt.
struct RootTree(PathBuf);

impl RootTree {
    fn new(tag: &str) -> Self {
        let dir = PathBuf::from(format!("/opt/minds-{tag}-{}", std::process::id()));
        as_root(&["rm", "-rf", dir.to_str().unwrap()]);
        as_root(&[
            "install",
            "-d",
            "-m",
            "0755",
            "-o",
            "root",
            "-g",
            "root",
            dir.to_str().unwrap(),
        ]);
        Self(dir)
    }

    /// Eine root-eigene Datei `name` (Modus 0644) mit `text`.
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let src = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(src.path(), text).unwrap();
        let dst = self.0.join(name);
        if let Some(parent) = dst.parent() {
            as_root(&[
                "install",
                "-d",
                "-m",
                "0755",
                "-o",
                "root",
                "-g",
                "root",
                parent.to_str().unwrap(),
            ]);
        }
        as_root(&[
            "install",
            "-m",
            "0644",
            "-o",
            "root",
            "-g",
            "root",
            src.path().to_str().unwrap(),
            dst.to_str().unwrap(),
        ]);
        dst
    }
}

impl Drop for RootTree {
    fn drop(&mut self) {
        let _ = if is_root() {
            Command::new("rm").args(["-rf"]).arg(&self.0).status()
        } else {
            Command::new("sudo")
                .args(["-n", "rm", "-rf"])
                .arg(&self.0)
                .status()
        };
    }
}

fn key(n: u8) -> String {
    format!("ed25519:{}", format!("{n:02x}").repeat(32))
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@x.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ],
    ] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(&args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    dir
}

fn verify(repo: &Path, file: &Path, protected: bool) -> Output {
    let mut args = vec![
        "verify",
        "--receipt",
        "--receipt-key-file",
        file.to_str().unwrap(),
    ];
    if protected {
        args.push("--require-protected-anchor");
    }
    Command::new(MINDS)
        .current_dir(repo)
        .args(&args)
        .env("MINDS_WITNESS", "off")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[ignore = "isolation"]
fn s3_28_receipt_anchor_protected_chain() {
    let repo = repo();
    let tree = RootTree::new("s3-28");
    let line = format!("{}\n", key(1));

    // Root-eigene Kette ohne Symlinks: geschützt; erfüllt
    // `--require-protected-anchor` (danach: keine Quittung, Exit 3).
    let good = tree.file("etc/receipt-keys", &line);
    let out = verify(repo.path(), &good, true);
    assert!(
        text(&out).contains(&format!("Vertrauensanker: geschützt ({})", good.display())),
        "{}",
        text(&out)
    );
    assert!(
        !text(&out).contains("verlangt einen geschützten"),
        "{}",
        text(&out)
    );
    assert_eq!(out.status.code(), Some(3), "{}", text(&out));

    let unprotected = |path: &Path, case: &str| {
        let out = verify(repo.path(), path, false);
        assert!(
            text(&out).contains("Vertrauensanker: ungeschützt (")
                && text(&out).contains("– nur für den Pilotbetrieb"),
            "{case}: {}",
            text(&out)
        );
        let out = verify(repo.path(), path, true);
        assert_eq!(out.status.code(), Some(3), "{case}: {}", text(&out));
        assert!(
            text(&out).contains("verlangt einen geschützten"),
            "{case}: {}",
            text(&out)
        );
    };

    // Fremder Eigentümer (nicht root) in root-eigenem Verzeichnis.
    let foreign = tree.file("foreign/receipt-keys", &line);
    as_root(&["chown", "65534", foreign.to_str().unwrap()]);
    unprotected(&foreign, "fremder Eigentümer");

    // Gruppenbeschreibbares Elternverzeichnis.
    let grouped = tree.file("group/receipt-keys", &line);
    as_root(&["chmod", "0775", grouped.parent().unwrap().to_str().unwrap()]);
    unprotected(&grouped, "gruppenbeschreibbares Elternverzeichnis");

    // Die Datei selbst gruppen- bzw. für alle beschreibbar.
    let group_file = tree.file("gfile/receipt-keys", &line);
    as_root(&["chmod", "0664", group_file.to_str().unwrap()]);
    unprotected(&group_file, "gruppenbeschreibbare Datei");
    let world_file = tree.file("wfile/receipt-keys", &line);
    as_root(&["chmod", "0646", world_file.to_str().unwrap()]);
    unprotected(&world_file, "für alle beschreibbare Datei");

    // Elternverzeichnis für alle beschreibbar, auch mit Sticky-Bit (wie /tmp).
    let world_dir = tree.file("wdir/receipt-keys", &line);
    as_root(&[
        "chmod",
        "1777",
        world_dir.parent().unwrap().to_str().unwrap(),
    ]);
    unprotected(
        &world_dir,
        "für alle beschreibbares Elternverzeichnis mit Sticky-Bit",
    );

    // Fremde Gruppe am Elternverzeichnis, ohne Schreibrecht: geschützt
    // bleibt, was niemand außer root ändern kann.
    let other_group = tree.file("ogroup/receipt-keys", &line);
    as_root(&[
        "chgrp",
        "65534",
        other_group.parent().unwrap().to_str().unwrap(),
    ]);
    let out = verify(repo.path(), &other_group, true);
    assert!(
        text(&out).contains("Vertrauensanker: geschützt ("),
        "{}",
        text(&out)
    );

    // Elternverzeichnis gehört nicht root (fremder Eigentümer).
    let foreign_dir = tree.file("fdir/receipt-keys", &line);
    as_root(&[
        "chown",
        "65534",
        foreign_dir.parent().unwrap().to_str().unwrap(),
    ]);
    unprotected(&foreign_dir, "Elternverzeichnis mit fremdem Eigentümer");

    // Symlink (root-eigen) auf die geschützte Datei.
    let link = tree.0.join("link-keys");
    as_root(&["ln", "-s", good.to_str().unwrap(), link.to_str().unwrap()]);
    unprotected(&link, "Symlink auf eine geschützte Datei");

    // Symlink-Verzeichnis in der Kette.
    let linked_dir = tree.0.join("linked-etc");
    as_root(&[
        "ln",
        "-s",
        good.parent().unwrap().to_str().unwrap(),
        linked_dir.to_str().unwrap(),
    ]);
    unprotected(
        &linked_dir.join("receipt-keys"),
        "Symlink-Verzeichnis in der Kette",
    );
}
