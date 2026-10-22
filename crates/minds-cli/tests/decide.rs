//! `minds decide` und `minds decisions` gegen das echte Binary.
//!
//! Drei Zusagen:
//!
//! 1. Ein Record geht als JSON hinein und kommt als Git-Objekt heraus — die
//!    erste Ausgabezeile ist `decision <id>`, und die Id ist die der
//!    abgelegten (redigierten) Bytes.
//! 2. `minds decisions` rechnet den Stand gegen den Index: Fällt eine
//!    tragende Annahme hinter einer Verwerfung, kommt genau diese
//!    Alternative zurück auf den Tisch — und `--check` sagt es mit Exit 2.
//! 3. Was die Policy entfernen würde, erreicht den Store im Freitext nur
//!    redigiert und in einer Probe gar nicht.
//!
//! Braucht `git`; fehlt es, überspringt sich der Test.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    cmd.output().expect("git läuft")
}

fn minds_with_stdin(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir)
        .args(args)
        .env("MINDS_WITNESS", "off")
        .env(
            "XDG_CONFIG_HOME",
            std::env::temp_dir().join("minds-tests-no-config"),
        )
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("MINDS_WITNESS_HOME")
        .env_remove("MINDS_WITNESS_SOCKET")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("minds läuft");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn minds(dir: &Path, args: &[&str]) -> Output {
    minds_with_stdin(dir, args, "")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Ein Repo mit einem Commit: `src/daemon.rs` forkt, `crates/a/Cargo.toml`
/// kennt kein Windows.
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
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::create_dir_all(dir.path().join("crates/a")).unwrap();
    std::fs::write(
        dir.path().join("src/daemon.rs"),
        "fn main() { unsafe { fork() }; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("crates/a/Cargo.toml"),
        "[dependencies]\nserde = \"1\"\n",
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "code"]);
    Some(dir)
}

const RECORD: &str = r#"{
  "topic": "lock for the witness socket on macOS",
  "chosen": {"approach": "flock on a lock file",
             "rests_on": [{"assumption": "local-fs", "weight": "load-bearing"}]},
  "alternatives": [
    {"approach": "Unix socket as the lock",
     "rejected_because": [{"assumption": "fork", "weight": "load-bearing"},
                          {"assumption": "no-windows", "weight": "marginal"}]}
  ],
  "assumptions": [
    {"id": "local-fs", "statement": "the lock file lives on a local file system"},
    {"id": "fork", "statement": "daemon.rs forks",
     "probe": {"type": "pattern", "path": "src/daemon.rs", "must_match": "\\bfork\\("}},
    {"id": "no-windows", "statement": "no Windows support is needed",
     "probe": {"type": "pattern", "glob": "crates/*/Cargo.toml", "must_not_match": "windows-sys"}}
  ],
  "scope": ["src/**"]
}"#;

#[test]
fn a_decision_is_recorded_listed_and_re_evaluated_when_its_assumption_falls() {
    let Some(dir) = repo() else { return };
    let dir = dir.path();

    // 1. Aufnehmen: Die erste Zeile ist der Vertrag mit dem Checkpoint.
    let out = minds_with_stdin(dir, &["decide"], RECORD);
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let first = stdout.lines().next().unwrap();
    let id = first
        .strip_prefix("decision b3-")
        .unwrap_or_else(|| panic!("erste Zeile ist `decision <id>`: {stdout}"));
    assert_eq!(id.len(), 64);
    assert!(stdout.contains("chosen      flock on a lock file"));
    assert!(stdout.contains("by          agent ·"), "{stdout}");
    assert!(stdout.contains("rejected    Unix socket as the lock"));
    assert!(stdout.contains("assumptions 3 (2 with a probe)"));
    let reference = format!("refs/minds/decisions/{id}");
    assert!(
        git(dir, &["rev-parse", "--verify", &reference])
            .status
            .success()
    );

    // 2. In Kraft, solange die Annahmen gelten.
    let list = minds(dir, &["decisions"]);
    assert!(list.status.success(), "{}", text(&list));
    let listed = text(&list);
    assert!(listed.contains("1 decision · 1 in force"), "{listed}");
    assert!(
        listed.contains("in force · 2 hold · 1 not checkable"),
        "{listed}"
    );
    assert!(listed.contains("probes ran against the index"), "{listed}");
    let check = minds(dir, &["decisions", "--check"]);
    assert_eq!(check.status.code(), Some(0), "{}", text(&check));

    // Mit einem Pfad außerhalb des Scopes: nichts.
    let none = minds(dir, &["decisions", "docs/x.md"]);
    assert!(text(&none).contains("no decision concerns docs/x.md"));

    // 3. Der Daemon forkt nicht mehr — gestagt, denn die Proben sehen den
    // Index: Die tragende Annahme hinter der Verwerfung fällt, der Socket
    // kommt zurück auf den Tisch, --check sagt es mit Exit 2.
    std::fs::write(dir.join("src/daemon.rs"), "fn main() { spawn() }\n").unwrap();
    let before = minds(dir, &["decisions"]);
    assert!(text(&before).contains("in force"), "{}", text(&before));
    git(dir, &["add", "src/daemon.rs"]);
    let after = minds(dir, &["decisions", "--check"]);
    assert_eq!(after.status.code(), Some(2), "{}", text(&after));
    let shown = text(&after);
    assert!(
        shown.contains("re-evaluate: fork no longer holds → reconsider Unix socket as the lock"),
        "{shown}"
    );
    assert!(
        shown.contains("1 to re-evaluate, re-check or unchecked — exit 2"),
        "{shown}"
    );

    // Der Brief und der Recall tragen die Entscheidung mit — zuerst, was
    // neu zu bewerten ist, mit den Annahmen und ihrem Stand.
    let brief = text(&minds(dir, &["brief"]));
    assert!(brief.contains("## Decisions to re-check"), "{brief}");
    assert!(
        brief.contains("re-evaluate: fork no longer holds → reconsider Unix socket as the lock"),
        "{brief}"
    );
    assert!(
        brief.contains("fork daemon.rs forks [no longer holds]"),
        "{brief}"
    );
    assert!(
        brief.contains("→ reconsider: Unix socket as the lock — fork no longer holds"),
        "{brief}"
    );
    // Mit einem Pfad außerhalb des Scopes: kein Abschnitt.
    let brief = text(&minds(dir, &["brief", "docs/x.md"]));
    assert!(!brief.contains("Decisions"), "{brief}");
    let recall = text(&minds(dir, &["recall", "src/daemon.rs"]));
    assert!(recall.contains("Decisions to re-check"), "{recall}");
    assert!(recall.contains("chosen: flock on a lock file"), "{recall}");
    let distill = text(&minds(dir, &["distill"]));
    assert!(distill.contains("Decisions to re-check"), "{distill}");

    // Auch das Detail trägt das Gate.
    let detail_check = minds(dir, &["decisions", &id[..12], "--check"]);
    assert_eq!(
        detail_check.status.code(),
        Some(2),
        "{}",
        text(&detail_check)
    );
    // Eine Id, die nichts trifft, ist ein Fehler — kein Pfadfilter.
    let none = minds(dir, &["decisions", &format!("b3-{}", "0".repeat(64))]);
    assert_eq!(none.status.code(), Some(1), "{}", text(&none));
    assert!(
        text(&none).contains("no decision with that id"),
        "{}",
        text(&none)
    );

    // Das Detail nennt jede Annahme mit Probe und Ergebnis.
    let detail = minds(dir, &["decisions", &id[..12]]);
    let detail = text(&detail);
    assert!(detail.contains(&format!("decision b3-{id}")), "{detail}");
    assert!(
        detail.contains("fork  daemon.rs forks (load-bearing)"),
        "{detail}"
    );
    assert!(detail.contains("✗ no longer holds"), "{detail}");
    assert!(
        detail.contains("local-fs  the lock file lives on a local file system"),
        "{detail}"
    );
    assert!(detail.contains("no probe  · not checkable"), "{detail}");

    // 4. Der Mensch entscheidet neu: Der alte Record ist Historie.
    let revised = RECORD.replace(
        "\"chosen\": {\"approach\": \"flock on a lock file\"",
        "\"chosen\": {\"approach\": \"Unix socket after all\"",
    );
    let out = minds_with_stdin(
        dir,
        &["decide", "--human", "--supersedes", &format!("b3-{id}")],
        &revised,
    );
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains("by          human"), "{stdout}");
    assert!(stdout.contains(&format!("supersedes  b3-{id}")), "{stdout}");
    let list = text(&minds(dir, &["decisions"]));
    assert!(list.contains("1 decision ·"), "{list}");
    assert!(list.contains("1 superseded"), "{list}");
    assert!(!list.contains("flock on a lock file"), "{list}");
    let old = text(&minds(dir, &["decisions", &id[..12]]));
    assert!(old.contains("superseded  by b3-"), "{old}");

    // 5. Die Zeit setzt das Kommando: Derselbe Text noch einmal ist ein
    // zweiter Akt mit eigener Id, nie derselbe Record.
    let again = minds_with_stdin(dir, &["decide"], RECORD);
    assert!(again.status.success(), "{}", text(&again));
    assert_ne!(
        String::from_utf8_lossy(&again.stdout)
            .lines()
            .next()
            .unwrap(),
        first
    );
}

/// Ein Ref im Namensraum ohne lesbaren Record (hier: ein Commit ohne
/// `record`) wird genannt — Id und Grund auf stderr, die Zahl auf stdout —
/// und schließt das Gate, auch wenn jede lesbare Entscheidung in Kraft ist.
#[test]
fn a_record_that_cannot_be_read_is_named_and_closes_the_gate() {
    let Some(dir) = repo() else { return };
    let dir = dir.path();
    let out = minds_with_stdin(dir, &["decide"], RECORD);
    assert!(out.status.success(), "{}", text(&out));
    let fake = format!("refs/minds/decisions/{}", "0".repeat(64));
    assert!(
        git(dir, &["update-ref", &fake, "HEAD"]).status.success(),
        "der Ref zeigt auf einen Commit ohne record"
    );
    let list = minds(dir, &["decisions"]);
    assert!(list.status.success(), "{}", text(&list));
    let stdout = String::from_utf8_lossy(&list.stdout);
    let stderr = String::from_utf8_lossy(&list.stderr);
    assert!(stdout.contains("1 decision · 1 in force"), "{stdout}");
    assert!(
        stdout.contains("1 record not readable — named on stderr"),
        "{stdout}"
    );
    assert!(
        stderr.contains(&format!(
            "minds decisions: decision b3-{} no record under its ref",
            "0".repeat(64)
        )),
        "{stderr}"
    );
    assert!(!text(&list).contains("fsck"), "{}", text(&list));
    let check = minds(dir, &["decisions", "--check"]);
    assert_eq!(check.status.code(), Some(2), "{}", text(&check));
    assert!(
        text(&check).contains("0 to re-evaluate, re-check or unchecked, records missing — exit 2"),
        "{}",
        text(&check)
    );
    // Ein Verzeichnis als Pfad trifft, was eine Datei darunter betrifft.
    let by_dir = minds(dir, &["decisions", "src"]);
    assert!(
        text(&by_dir).contains("1 decision · 1 in force concerning src"),
        "{}",
        text(&by_dir)
    );
    let other = minds(dir, &["decisions", "crates"]);
    assert!(
        text(&other).contains("no decision concerns crates"),
        "{}",
        text(&other)
    );
    // `.` und `./` sind das ganze Repo: dieselbe Liste, derselbe Exit-Code.
    for dot in [".", "./", "././"] {
        let dotted = minds(dir, &["decisions", dot, "--check"]);
        assert_eq!(dotted.status.code(), Some(2), "{dot}: {}", text(&dotted));
        assert!(
            String::from_utf8_lossy(&dotted.stdout).contains("1 decision · 1 in force"),
            "{dot}: {}",
            text(&dotted)
        );
    }
}

/// Mit einem Pfad gated `--check` nur, was ihn betrifft; ein Symlink an
/// der Stelle der Probendatei ist `unchecked` und schließt das Gate; ein
/// Pfad, der nicht repo-relativ ist, ist ein Fehler, kein grünes Gate.
#[cfg(unix)]
#[test]
fn the_path_filter_gates_only_what_it_concerns_and_a_symlink_is_unchecked() {
    let Some(dir) = repo() else { return };
    let dir = dir.path();
    let out = minds_with_stdin(dir, &["decide"], RECORD);
    assert!(out.status.success(), "{}", text(&out));
    std::fs::write(dir.join("src/daemon.rs"), "fn main() { spawn() }\n").unwrap();
    git(dir, &["add", "src/daemon.rs"]);
    assert_eq!(minds(dir, &["decisions", "--check"]).status.code(), Some(2));
    // Außerhalb des Scopes: nichts betroffen, Exit 0.
    let outside = minds(dir, &["decisions", "crates", "--check"]);
    assert_eq!(outside.status.code(), Some(0), "{}", text(&outside));
    assert!(text(&outside).contains("no decision concerns crates"));
    for bad in ["../x", "/abs", "src//x", "src/."] {
        let refused = minds(dir, &["decisions", bad, "--check"]);
        assert_eq!(refused.status.code(), Some(1), "{bad}: {}", text(&refused));
        assert!(
            text(&refused).contains("not repo-relative"),
            "{bad}: {}",
            text(&refused)
        );
    }
    // Ein Symlink an der Stelle der Datei: gelistet, unlesbar — unchecked.
    std::fs::remove_file(dir.join("src/daemon.rs")).unwrap();
    std::os::unix::fs::symlink("../Cargo.toml", dir.join("src/daemon.rs")).unwrap();
    git(dir, &["add", "src/daemon.rs"]);
    let unchecked = minds(dir, &["decisions", "--check"]);
    assert_eq!(unchecked.status.code(), Some(2), "{}", text(&unchecked));
    assert!(
        text(&unchecked).contains("1 unchecked"),
        "{}",
        text(&unchecked)
    );
}

/// `--human` ist ein Flag, das jeder setzen kann — und heißt darum überall
/// `human (unsigned)`, auch in der Bestätigung des Schreibers.
#[test]
fn a_human_record_is_marked_unsigned_everywhere() {
    let Some(dir) = repo() else { return };
    let dir = dir.path();
    let human = minds_with_stdin(dir, &["decide", "--human"], RECORD);
    assert!(human.status.success(), "{}", text(&human));
    let stdout = String::from_utf8_lossy(&human.stdout).into_owned();
    assert!(
        stdout.contains("by          human (unsigned) ·"),
        "{stdout}"
    );
    let id = stdout
        .lines()
        .next()
        .unwrap()
        .strip_prefix("decision ")
        .unwrap()
        .to_owned();
    let detail = text(&minds(dir, &["decisions", &id]));
    assert!(
        detail.contains("by          human (unsigned) ·"),
        "{detail}"
    );
    let brief = text(&minds(dir, &["brief"]));
    assert!(brief.contains(", human (unsigned))"), "{brief}");
}

/// Das Gate ist eines gegen unbemerktes Driften, nicht gegen einen
/// Schreiber des Stores: Wer `minds decide --supersedes` ausführen kann,
/// löst eine gefallene Entscheidung ab — dokumentiert, und so erwartet.
#[test]
fn superseding_a_fallen_decision_opens_the_gate_as_documented() {
    let Some(dir) = repo() else { return };
    let dir = dir.path();
    let out = minds_with_stdin(dir, &["decide"], RECORD);
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let id = stdout
        .lines()
        .next()
        .unwrap()
        .strip_prefix("decision ")
        .unwrap()
        .to_owned();
    std::fs::write(dir.join("src/daemon.rs"), "fn main() { spawn() }\n").unwrap();
    git(dir, &["add", "src/daemon.rs"]);
    let fallen = minds(dir, &["decisions", "--check"]);
    assert_eq!(fallen.status.code(), Some(2), "{}", text(&fallen));
    let successor = minds_with_stdin(
        dir,
        &["decide", "--supersedes", &id],
        r#"{"topic": "lock for the witness socket on macOS", "chosen": {"approach": "a global mutex"}}"#,
    );
    assert!(successor.status.success(), "{}", text(&successor));
    let after = minds(dir, &["decisions", "--check"]);
    assert_eq!(after.status.code(), Some(0), "{}", text(&after));
    assert!(text(&after).contains("1 superseded"), "{}", text(&after));
    // Das Detail des abgelösten Records gated auf die Spitze, nicht auf
    // seinen historischen Stand.
    let old = minds(dir, &["decisions", &id, "--check"]);
    assert_eq!(old.status.code(), Some(0), "{}", text(&old));
}

#[test]
fn free_text_is_redacted_but_a_secret_in_a_probe_refuses_the_record() {
    let Some(dir) = repo() else { return };
    let dir = dir.path();
    const TOKEN: &str = "ghp_u8jzPde0IgxLd6GncfBAepfJBd0Kh8oOOL8d";

    let record = RECORD.replace(
        "lock for the witness socket on macOS",
        &format!("rotate {TOKEN} before release"),
    );
    let out = minds_with_stdin(dir, &["decide"], &record);
    assert!(out.status.success(), "{}", text(&out));
    let shown = text(&out);
    assert!(!shown.contains(TOKEN), "{shown}");
    assert!(shown.contains("replaced 1 secret(s)"), "{shown}");
    let id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap()
        .strip_prefix("decision ")
        .unwrap()
        .to_owned();
    let bytes = git(
        dir,
        &["show", &format!("refs/minds/decisions/{}:record", &id[3..])],
    );
    assert!(!String::from_utf8_lossy(&bytes.stdout).contains(TOKEN));

    let record = RECORD.replace("\\\\bfork\\\\(", TOKEN);
    let out = minds_with_stdin(dir, &["decide"], &record);
    assert!(!out.status.success());
    let shown = text(&out);
    assert!(shown.contains("decision.assumptions[1].probe"), "{shown}");
    assert!(!shown.contains(TOKEN), "{shown}");
}

#[test]
fn malformed_input_is_refused_with_a_reason_and_the_template_is_valid() {
    let Some(dir) = repo() else { return };
    let dir = dir.path();

    let out = minds_with_stdin(dir, &["decide"], "");
    assert!(!out.status.success());
    assert!(text(&out).contains("expected the decision as JSON on stdin"));

    let out = minds_with_stdin(dir, &["decide"], "{\"topic\": \"x\"}");
    assert!(!out.status.success());
    assert!(text(&out).contains("either open"), "{}", text(&out));

    // `decided_by` und `supersedes` im JSON zählen nicht — nur die Flags.
    let out = minds_with_stdin(
        dir,
        &["decide"],
        &RECORD.replace(
            "\"topic\":",
            &format!(
                "\"decided_by\": \"human\", \"supersedes\": \"b3-{}\", \"topic\":",
                "0".repeat(64)
            ),
        ),
    );
    assert!(out.status.success(), "{}", text(&out));
    let shown = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(shown.contains("by          agent"), "{shown}");
    assert!(!shown.contains("supersedes"), "{shown}");

    // Eine token-förmige Kennung erscheint in keinem Fehler.
    let token = "ghp_u8jzPde0IgxLd6GncfBAepfJBd0Kh8oOOL8d";
    for broken in [
        RECORD
            .replace("\"id\": \"fork\"", &format!("\"id\": \"{token}\""))
            .replace("\\\\bfork\\\\(", "("),
        RECORD.replace(
            "\"assumption\": \"fork\"",
            &format!("\"assumption\": \"{token}\""),
        ),
    ] {
        let out = minds_with_stdin(dir, &["decide"], &broken);
        assert!(!out.status.success(), "{}", text(&out));
        assert!(!text(&out).contains(token), "{}", text(&out));
    }

    // Ein Pfad außerhalb des Repositories in einer Probe.
    let out = minds_with_stdin(
        dir,
        &["decide"],
        &RECORD.replace("src/daemon.rs", "../../etc/passwd"),
    );
    assert!(!out.status.success());
    assert!(text(&out).contains("not repo-relative"), "{}", text(&out));

    // Ein Muster, das nie geprüft werden könnte.
    let out = minds_with_stdin(dir, &["decide"], &RECORD.replace("\\\\bfork\\\\(", "("));
    assert!(!out.status.success());
    assert!(
        text(&out).contains("would never be checked"),
        "{}",
        text(&out)
    );

    // --supersedes auf etwas, das es nicht gibt.
    let out = minds_with_stdin(
        dir,
        &["decide", "--supersedes", &format!("b3-{}", "0".repeat(64))],
        RECORD,
    );
    assert!(!out.status.success());
    assert!(text(&out).contains("no such decision"), "{}", text(&out));

    // Die Vorlage ist selbst ein gültiger Record.
    let template = minds(dir, &["decide", "--template"]);
    assert!(template.status.success());
    let out = minds_with_stdin(dir, &["decide"], &String::from_utf8_lossy(&template.stdout));
    assert!(out.status.success(), "{}", text(&out));

    // Ohne Records: der Hinweis auf die Vorlage.
    let Some(empty) = repo() else { return };
    let listed = text(&minds(empty.path(), &["decisions"]));
    assert!(listed.contains("no decisions recorded"), "{listed}");
}
