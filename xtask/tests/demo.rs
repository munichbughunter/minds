//! Phase D: Die Demo-Fixture ist reproduzierbar und trägt die drei Fälle —
//! sauber, Lücke, Manipulation. Gegen das echte `minds`-Binary; ohne Binary
//! oder Git wird übersprungen (wie die End-to-End-Tests der CLI).

use std::path::PathBuf;
use std::process::Command;

fn minds_binary() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let exe = root.join("target/debug/minds");
    exe.is_file().then_some(exe)
}

fn verify(dir: &std::path::Path, minds: &std::path::Path, rev: &str) -> (Option<i32>, String) {
    let out = xtask::demo::command(minds, dir)
        .args(["verify", rev])
        .output()
        .expect("minds verify runs");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn the_demo_fixture_has_a_clean_a_gap_and_a_tampered_case() {
    let Some(minds) = minds_binary() else {
        eprintln!("kein minds-Binary unter target/debug — Test übersprungen");
        return;
    };
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    }
    let dir = tempfile_dir();
    let demo = xtask::demo::create(&dir, &minds).expect("the fixture builds");
    // Sauber: eine Session mit drei verworfenen Ansätzen, am Commit belegt.
    assert_eq!(demo.discarded, 3);
    let (code, text) = verify(&dir, &minds, &demo.clean_commit);
    assert_ne!(code, Some(1), "not tampered before the tamper step\n{text}");
    assert!(!text.contains("TAMPERED"), "{text}");
    // Lücke: ein Commit mit einer Datei, die keine Session beobachtet hat.
    assert_ne!(demo.gap_commit, demo.clean_commit);
    // Manipulation: per Befehl verändertes Seal-Material → TAMPERED.
    xtask::demo::tamper(&dir).expect("the seal is forged");
    let (code, text) = verify(&dir, &minds, &demo.clean_commit);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("TAMPERED"), "{text}");
    // Nur der saubere Fall ist manipuliert — die Lücke bleibt eine Lücke.
    let (_, text) = verify(&dir, &minds, &demo.gap_commit);
    assert!(!text.contains("TAMPERED"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "minds-demo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
