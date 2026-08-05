//! Reviews als Git-Objekte, gegen das echte Binary (Schicht 3, R1).
//!
//! Zwei Zusagen stehen hier, und beide sind der Grund, warum das Verdict an der
//! **Change-Id** hängt und nicht am Commit:
//!
//! 1. Ein Rebase schreibt jeden Commit-Hash um. Das Verdict muss ihn überleben.
//! 2. Signiert ist es ein Nachweis, nicht eine Behauptung — und eine
//!    manipulierte Zusammenfassung muss die Prüfung reißen lassen.
//!
//! Braucht `git`; der Signaturteil zusätzlich `ssh-keygen`. Fehlt eines,
//! überspringt sich der jeweilige Test, statt falsch-rot zu werden.

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

/// Ein Repo mit Minds-Hooks und einem Commit auf `main`.
///
/// Der Branchname steht **explizit** da. Ohne `-b` entscheidet ihn
/// `init.defaultBranch`: Git liefert `master`, macOS' Command Line Tools setzen
/// per System-Config `main`. Der Test spricht `main` später direkt an
/// (`checkout`, `rebase`) und wäre sonst auf der einen Maschine grün und auf der
/// anderen rot — ohne dass sich an der geprüften Zusage etwas ändert.
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
    // Der prepare-commit-msg-Hook setzt die Change-Id; ohne `enable` gäbe es
    // keine, und dann prüfte dieser Test das Falsche.
    minds(dir.path(), &["enable"]);
    std::fs::write(dir.path().join("a.txt"), "eins\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "erster Commit"]);
    Some(dir)
}

/// Die `Minds-Change-Id` aus der Message von HEAD.
fn change_id_of_head(dir: &Path) -> String {
    let message = String::from_utf8_lossy(&git(dir, &["show", "-s", "--format=%B", "HEAD"]).stdout)
        .into_owned();
    message
        .lines()
        .find_map(|line| line.strip_prefix("Minds-Change-Id: "))
        .map(str::trim)
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("kein Change-Id-Trailer an HEAD:\n{message}"))
}

#[test]
fn a_verdict_survives_a_rebase() {
    // Der Grund für die Change-Id: Nach einem Rebase heißt der Commit anders.
    // Ein Verdict am Commit-Hash wäre danach verwaist; eines an der Change-Id
    // ist es nicht.
    let Some(dir) = repo() else { return };
    let dir = dir.path();

    git(dir, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(dir.join("b.txt"), "zwei\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "feat: b"]);

    let change = change_id_of_head(dir);
    let before = String::from_utf8_lossy(&git(dir, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();

    let out = minds(
        dir,
        &["review", &change, "--approve", "--summary", "geprüft"],
    );
    assert!(out.status.success(), "{}", text(&out));

    // main bewegt sich, topic wird darauf rebased — jeder Hash ändert sich.
    git(dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("c.txt"), "drei\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "feat: c"]);
    git(dir, &["checkout", "-q", "topic"]);
    let rebase = git(dir, &["rebase", "main"]);
    assert!(rebase.status.success(), "{}", text(&rebase));

    let after = String::from_utf8_lossy(&git(dir, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();
    assert_ne!(before, after, "ohne neuen Hash prüft dieser Test nichts");
    assert_eq!(
        change_id_of_head(dir),
        change,
        "die Change-Id muss den Rebase überleben"
    );

    // Und das Verdict hängt weiterhin daran.
    let found = minds(dir, &["reviews", &change]);
    assert!(found.status.success(), "{}", text(&found));
    assert!(text(&found).contains("approve"), "{}", text(&found));
    assert!(text(&found).contains("geprüft"), "{}", text(&found));
}

#[test]
fn a_signed_verdict_verifies_and_tampering_breaks_it() {
    let Some(dir) = repo() else { return };
    let dir = dir.path();

    // Ein Schlüssel für den Test. Ohne ssh-keygen: überspringen.
    let key = dir.join("id");
    let generated = Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "anna@example.org",
            "-q",
            "-f",
        ])
        .arg(&key)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !generated {
        return;
    }
    let pubkey = std::fs::read_to_string(dir.join("id.pub")).unwrap();
    let signers = dir.join("allowed_signers");
    std::fs::write(&signers, format!("anna@example.org {}", pubkey.trim())).unwrap();

    let change = change_id_of_head(dir);
    let out = minds(
        dir,
        &[
            "review",
            &change,
            "--approve",
            "--summary",
            "Backoff ist jetzt korrekt",
            "--sign",
            "--key",
            key.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("signiert"), "{}", text(&out));

    // Ohne --signers wird nicht geprüft, sondern nur gemeldet — die beiden
    // dürfen nicht gleich aussehen.
    let unchecked = minds(dir, &["reviews", &change]);
    assert!(
        text(&unchecked).contains("ungeprüft"),
        "{}",
        text(&unchecked)
    );

    // Mit --signers wird geprüft.
    let checked = minds(
        dir,
        &["reviews", &change, "--signers", signers.to_str().unwrap()],
    );
    assert!(
        text(&checked).contains("Signatur gültig"),
        "{}",
        text(&checked)
    );

    // Und die Manipulation: Ein zweites Verdict mit derselben Signatur ist
    // nicht herstellbar, weil die Signatur über den Hash geht — ein geänderter
    // Text ergibt einen anderen Hash und damit ein anderes Review, das schlicht
    // keine Signatur hat.
    let tampered = minds(
        dir,
        &["review", &change, "--approve", "--summary", "etwas anderes"],
    );
    assert!(tampered.status.success(), "{}", text(&tampered));
    let both = minds(
        dir,
        &["reviews", &change, "--signers", signers.to_str().unwrap()],
    );
    let listing = text(&both);
    assert!(listing.contains("Signatur gültig"), "{listing}");
    assert!(
        listing.contains("nicht signiert"),
        "das untergeschobene Verdict darf nicht als signiert durchgehen: {listing}"
    );
}
