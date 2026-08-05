//! Der Kern-Loop, gegen das echte Binary und ein echtes Git-Repo.
//!
//! Das ist der Test, der zählt: Er startet `minds` als Prozess — so, wie ein
//! Agent und ein Nutzer es täten — und geht den ganzen Weg aus der Definition
//! of Done: einrichten, Hook-Events aufzeichnen, committen, einchecken, und die
//! Session sowohl über den Commit (`show`) als auch über eine einzelne Zeile
//! (`why`) wiederfinden. `fsck` bestätigt am Ende, dass nichts verwaist ist.
//!
//! Braucht `git` im Pfad. Fehlt es, überspringt der Test sich selbst, statt
//! falsch-rot zu werden.

use std::path::Path;
use std::process::{Command, Output};

/// Das unter Test stehende Binary — von Cargo bereitgestellt.
const MINDS: &str = env!("CARGO_BIN_EXE_minds");

/// Ein frisch initialisiertes Repo, oder `None`, wenn kein `git` da ist.
fn scratch_repo() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    let ok = git(dir.path(), &["init", "-q"]).status.success();
    if !ok {
        return None;
    }
    git(dir.path(), &["config", "user.email", "test@minds.invalid"]);
    git(dir.path(), &["config", "user.name", "Minds Test"]);
    Some(dir)
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.env("PATH", path_with_minds());
    without_user_config(&mut cmd).output().expect("git läuft")
}

/// Schneidet die Git-Config des Entwicklers ab.
///
/// Ein **global** gesetztes `core.hooksPath` — husky, lefthook, pre-commit —
/// verschiebt das Hook-Verzeichnis auch in einem frisch angelegten Testrepo.
/// Seit `enable` diesem Pfad folgt (#9), hinge sonst jede Aussage über
/// `.git/hooks` an der Maschine, auf der der Test läuft: hier grün, dort rot,
/// und im schlimmeren Fall grün aus dem falschen Grund. Dasselbe gilt für
/// `commit.gpgsign` und Konsorten.
///
/// `/dev/null` als Config-Datei ist der dokumentierte Weg, eine Config-Ebene
/// abzuschalten. Die von Git gestarteten Hooks erben die Variablen und rufen
/// `minds` damit ebenfalls ohne fremde Config auf.
fn without_user_config(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
}

/// Der `PATH` für Git-Aufrufe: vorneweg das Verzeichnis des Test-Binaries.
///
/// Die von `minds enable` installierten Hooks rufen `minds` **ohne Pfad** auf.
/// Ohne diesen Eintrag greift der Aufruf ins Leere und `|| true` schluckt ihn;
/// mit einer global installierten `minds` im Pfad liefe statt des Test-Binaries
/// eine **fremde Version** — beides macht den Lauf von der Maschine abhängig.
/// Das Verzeichnis steht deshalb vorn und beschattet jede globale Installation.
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

/// Ruft `minds` im Repo auf, optional mit stdin (für `hook`).
fn minds(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    use std::io::Write;
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir).args(args);
    without_user_config(&mut cmd);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("minds startet");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().expect("minds endet")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Ein Hook-Event, wie es der Agent auf stdin schickt.
fn event(dir: &Path, body: &str) {
    let payload = format!(
        r#"{{"session_id":"sess-e2e","cwd":"{}",{body}}}"#,
        dir.display()
    );
    let out = minds(dir, &["hook", "--agent", "claude-code"], Some(&payload));
    assert!(out.status.success(), "hook endet immer mit 0");
}

#[test]
fn the_core_loop_closes() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();

    // 1. Einrichten.
    let enable = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(enable.status.success(), "{}", stdout(&enable));
    assert!(dir.join(".claude/settings.json").exists());
    // `.git/hooks` ist hier das effektive Hook-Verzeichnis, weil
    // `without_user_config` ein globales `core.hooksPath` abschneidet — den
    // verschobenen Fall prüft `a_moved_hookspath_still_captures_on_commit`.
    assert!(dir.join(".git/hooks/post-commit").exists());

    // 2. Eine Session aufzeichnen — Prompt, ein Write-Tool, Stop.
    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Schreibe eine Grußfunktion""#,
    );
    event(
        dir,
        r#""hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"greet.rs"}"#,
    );
    event(dir, r#""hook_event_name":"Stop""#);

    // 3. Der Commit, den die Session hervorbrachte.
    std::fs::write(
        dir.join("greet.rs"),
        "fn greet() {\n    println!(\"hallo\");\n}\n",
    )
    .unwrap();
    git(dir, &["add", "greet.rs"]);
    // Der post-commit-Hook checkpointet hier bereits selbst — `path_with_minds`
    // stellt ihm das Test-Binary in den Pfad. Schritt 4 ruft `checkpoint`
    // trotzdem noch einmal von Hand auf: Der Weg muss auch der sein, den ein
    // Nutzer ohne Hook geht, und ein zweiter Lauf über dieselbe Session ist ein
    // No-op (der Ref-Name *ist* der Inhalts-Hash).
    assert!(
        git(dir, &["commit", "-q", "-m", "feat: Grußfunktion"])
            .status
            .success()
    );
    let head = String::from_utf8_lossy(&git(dir, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();

    // 4. Einchecken: Journal → redact → Store → Trailer.
    let checkpoint = minds(dir, &["checkpoint", "--commit", &head], None);
    assert!(checkpoint.status.success(), "{}", stdout(&checkpoint));

    // Der Trailer steht am (nachgerüsteten) HEAD …
    let message = stdout(&git(dir, &["log", "-1", "--format=%B"]));
    let session_id = message
        .lines()
        .find_map(|line| line.strip_prefix("Minds-Session-Id: "))
        .map(str::trim)
        .unwrap_or_else(|| panic!("kein Trailer:\n{message}"));
    assert!(session_id.starts_with("b3-"), "{session_id}");

    // … und die Nutzlast liegt im Store, unter genau dem Ref, den der Trailer
    // benennt. Seit v0.2 trägt **ein Ref je Session** die Nutzlast
    // (`refs/minds/store/<voller Hash>`); der Trailer schreibt denselben Hash
    // mit `b3-`-Präfix. Damit prüft das hier die Kette Commit → Store, nicht nur
    // „irgendwo liegt irgendeine Session".
    let hex = session_id.strip_prefix("b3-").expect("b3-Präfix");
    let session_ref = format!("refs/minds/store/{hex}");
    let ls = stdout(&git(dir, &["ls-tree", "-r", "--name-only", &session_ref]));
    assert!(
        ls.contains("session.json"),
        "keine Nutzlast unter {session_ref}:\n{ls}"
    );

    // 5. Über den Commit zurück zur Absicht.
    let show = minds(dir, &["show"], None);
    assert!(show.status.success());
    assert!(
        stdout(&show).contains("Schreibe eine Grußfunktion"),
        "show zeigt den Prompt nicht:\n{}",
        stdout(&show)
    );

    // 6. Der Magic Moment: über eine einzelne Zeile zurück zur Absicht.
    let why = minds(dir, &["why", "greet.rs:2"], None);
    assert!(why.status.success());
    let why_out = stdout(&why);
    assert!(why_out.contains("greet.rs:2 → commit"), "{why_out}");
    assert!(
        why_out.contains("Schreibe eine Grußfunktion"),
        "why zeigt den Prompt nicht:\n{why_out}"
    );

    // 7. Alles auflösbar.
    let fsck = minds(dir, &["fsck"], None);
    assert!(fsck.status.success(), "fsck rot:\n{}", stdout(&fsck));
    assert!(stdout(&fsck).contains("in Ordnung"));
}

/// Das Feedback aus dem Dogfooding: Eine Session, die *zwei* Dateien anfasst,
/// muss auf ihrer Seite beide zeigen — samt Änderungen, aufklappbar. Und der
/// Store muss `index.json` tragen (Commit → Session), damit der Kontext auch
/// allein (im Child-Repo, in GitLab) selbsttragend ist.
#[test]
fn a_session_page_shows_all_changed_files_and_the_store_has_an_index() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();

    minds(dir, &["enable", "--agent", "claude-code"], None);
    // Hier soll genau *ein* Checkpoint laufen, und zwar der unten von Hand
    // aufgerufene — der Test misst, was dabei im Store landet. Der Hook würde
    // schon beim Commit einchecken; wir nehmen ihn deshalb weg. Das `unwrap`
    // ist Absicht: Schlüge das Entfernen fehl (weil der Hook woanders liegt),
    // liefe der Test still an seiner eigenen Voraussetzung vorbei.
    std::fs::remove_file(dir.join(".git/hooks/post-commit"))
        .expect("der Hook lag da, wo enable ihn hinschrieb");

    // Eine Session, die zwei Dateien anlegt.
    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Lege zwei Dateien an""#,
    );
    event(
        dir,
        r#""hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"a.txt"}"#,
    );
    event(
        dir,
        r#""hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"b.txt"}"#,
    );
    event(dir, r#""hook_event_name":"Stop""#);

    std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(dir.join("b.txt"), "beta\n").unwrap();
    git(dir, &["add", "a.txt", "b.txt"]);
    git(dir, &["commit", "-q", "-m", "feat: zwei Dateien"]);
    let head = String::from_utf8_lossy(&git(dir, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();

    let checkpoint = minds(dir, &["checkpoint", "--commit", &head], None);
    assert!(checkpoint.status.success(), "{}", stdout(&checkpoint));

    // Punkt 1: Die Kante Commit → Session liegt als Daten im Store — seit
    // ADR-0010 bei ihrer Session, nicht in einer gemeinsamen index.json.
    let refs = stdout(&git(
        dir,
        &["for-each-ref", "--format=%(refname)", "refs/minds/store/"],
    ));
    let session_ref = refs.lines().next().expect("ein Session-Ref").to_owned();
    let ls = stdout(&git(dir, &["ls-tree", "-r", "--name-only", &session_ref]));
    assert!(ls.contains("links.json"), "keine Kanten im Store:\n{ls}");
    assert!(
        ls.contains("session.json"),
        "keine Nutzlast im Store:\n{ls}"
    );
    // Der Commit, auf den die Kante zeigt, ist der *nachgerüstete* — das Anhängen
    // der Trailer schreibt HEAD um, und die Kante soll auf das zeigen, was
    // danach dasteht.
    let amended = String::from_utf8_lossy(&git(dir, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();
    let links = stdout(&git(dir, &["show", &format!("{session_ref}:links.json")]));
    assert!(
        links.contains(&amended),
        "die Kante nennt den nachgerüsteten Commit nicht: {links}"
    );

    // Rendern und die Session-Seite einlesen.
    let out = dir.join("site");
    let render = minds(dir, &["render", "--out", out.to_str().unwrap()], None);
    assert!(render.status.success(), "{}", stdout(&render));

    let session_html = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session-"))
        })
        .map(|p| std::fs::read_to_string(p).unwrap())
        .expect("keine Session-Seite erzeugt");

    // Punkt 3: beide Dateien, ihre Änderungen, und je ein aufklappbarer Block.
    assert!(
        session_html.contains("a.txt"),
        "a.txt fehlt:\n{session_html}"
    );
    assert!(session_html.contains("b.txt"), "b.txt fehlt");
    assert!(session_html.contains("alpha"), "Änderung in a.txt fehlt");
    assert!(session_html.contains("beta"), "Änderung in b.txt fehlt");
    assert_eq!(
        session_html.matches(r#"<details class="diff""#).count(),
        2,
        "beide Dateien sollen je einen aufklappbaren Diff haben"
    );
}

/// Die Regression aus #9, Ende zu Ende: In einem Repo mit `core.hooksPath`
/// (husky, lefthook, pre-commit) schrieb `enable` nach `.git/hooks` — ein
/// Verzeichnis, das Git dann **nie** liest. `enable` meldete Erfolg, und kein
/// Commit erzeugte je einen Checkpoint.
///
/// Der Test geht deshalb nicht nur nach der Datei, sondern nach der Wirkung:
/// commit → der post-commit-Hook feuert → die Session liegt im Store.
#[test]
fn a_moved_hookspath_still_captures_on_commit() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    git(dir, &["config", "core.hooksPath", ".husky"]);

    let enable = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(enable.status.success(), "{}", stdout(&enable));
    assert!(
        dir.join(".husky/post-commit").exists(),
        "der Hook gehört ins effektive Verzeichnis"
    );
    assert!(
        !dir.join(".git/hooks/post-commit").exists(),
        "und nicht in das, aus dem Git nichts liest"
    );

    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Schreibe eine Grußfunktion""#,
    );
    event(
        dir,
        r#""hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"greet.rs"}"#,
    );
    event(dir, r#""hook_event_name":"Stop""#);

    std::fs::write(dir.join("greet.rs"), "fn greet() {}\n").unwrap();
    git(dir, &["add", "greet.rs"]);
    assert!(
        git(dir, &["commit", "-q", "-m", "feat: Grußfunktion"])
            .status
            .success()
    );

    // Kein manuelles `checkpoint`: Was hier ankommt, kann nur der Hook getan
    // haben.
    let refs = stdout(&git(
        dir,
        &["for-each-ref", "--format=%(refname)", "refs/minds/store/"],
    ));
    assert!(
        !refs.trim().is_empty(),
        "der post-commit-Hook hat nichts eingecheckt"
    );

    let show = minds(dir, &["show"], None);
    assert!(
        stdout(&show).contains("Schreibe eine Grußfunktion"),
        "die Session ist nicht über den Commit auffindbar:\n{}",
        stdout(&show)
    );
}

/// Ein **gesetztes, aber leeres** `core.hooksPath` schaltet die Hooks in Git
/// ganz ab. `enable` hat dann keinen Ort — und darf sich keinen ausdenken:
/// `rev-parse --git-path hooks` antwortet in dem Fall `./`, wer dem folgt, legt
/// ausführbare Dateien in die Arbeitskopie des Nutzers.
#[test]
fn an_empty_hookspath_aborts_instead_of_littering_the_worktree() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    git(dir, &["config", "core.hooksPath", ""]);

    let enable = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(
        !enable.status.success(),
        "enable darf hier keinen Erfolg melden:\n{}",
        stdout(&enable)
    );

    for name in ["post-commit", "prepare-commit-msg", "pre-push"] {
        assert!(
            !dir.join(name).exists(),
            "{name} liegt in der Arbeitskopie statt in einem Hook-Verzeichnis"
        );
    }
    // Fail-closed heißt hier auch: kein halb eingerichtetes Repo.
    assert!(
        !dir.join(".claude/settings.json").exists(),
        "der Abbruch muss vor dem ersten Schreibzugriff kommen"
    );

    // Und `fsck` sagt dasselbe, statt ein leeres Verzeichnis zu melden.
    let fsck = minds(dir, &["fsck"], None);
    assert!(
        stdout(&fsck).contains("core.hooksPath ist leer"),
        "fsck benennt den Fall nicht:\n{}",
        stdout(&fsck)
    );
}

/// Die Zusage „nichts halb Eingerichtetes" gilt für **jede** Schranke, nicht nur
/// für die erste. Hier scheitert der *letzte* Hook der Reihe (`pre-push`) — und
/// zwar an etwas, das erst beim Schreiben aufgefallen wäre, wenn die Vorprüfung
/// ihn nicht vorher ansähe. Vor dem Fix blieben `.claude/settings.json` und ein
/// halber Hook-Satz zurück, ohne Store-Config: Der Agent journalierte, und
/// nichts checkte je ein.
#[cfg(unix)]
#[test]
fn a_broken_hook_leaves_nothing_behind_at_all() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    git(dir, &["config", "core.hooksPath", ".husky"]);

    let victim = dir.join("opfer.txt");
    std::fs::write(&victim, "PRIVAT\n").unwrap();
    std::fs::create_dir_all(dir.join(".husky")).unwrap();
    std::os::unix::fs::symlink(&victim, dir.join(".husky/pre-push")).unwrap();

    let enable = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(
        !enable.status.success(),
        "enable darf hier keinen Erfolg melden:\n{}",
        stdout(&enable)
    );

    assert!(
        !dir.join(".claude/settings.json").exists(),
        "der Abbruch muss vor der ersten geschriebenen Datei kommen"
    );
    for name in ["post-commit", "prepare-commit-msg"] {
        assert!(
            !dir.join(".husky").join(name).exists(),
            "{name} wurde geschrieben, obwohl pre-push nicht geht"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "PRIVAT\n",
        "die verlinkte Datei bleibt unangetastet"
    );
}

/// Dasselbe für ein Hook-Verzeichnis, in das sich nicht schreiben lässt: Auch
/// dann darf keine Agent-Konfiguration zurückbleiben.
#[cfg(unix)]
#[test]
fn an_unwritable_hooks_directory_leaves_nothing_behind() {
    use std::os::unix::fs::PermissionsExt;

    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    let locked = dir.join("gesperrt");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    git(
        dir,
        &[
            "config",
            "core.hooksPath",
            &locked.join("hooks").to_string_lossy(),
        ],
    );

    let enable = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(!enable.status.success(), "{}", stdout(&enable));
    assert!(
        !dir.join(".claude/settings.json").exists(),
        "der Abbruch muss vor der ersten geschriebenen Datei kommen"
    );

    // Damit das Tempdir wieder aufräumbar ist.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn checkpoint_without_a_session_is_a_clean_no_op() {
    let Some(repo) = scratch_repo() else {
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "chore: ohne Session"]);
    let before = stdout(&git(dir, &["rev-parse", "HEAD"]));

    // Kein Journal-Inhalt → nichts zu speichern, kein Trailer, HEAD unverändert.
    let checkpoint = minds(dir, &["checkpoint", "--commit", before.trim()], None);
    assert!(checkpoint.status.success());
    let after = stdout(&git(dir, &["rev-parse", "HEAD"]));
    assert_eq!(before, after, "ohne Session darf HEAD nicht wandern");

    let fsck = minds(dir, &["fsck"], None);
    assert!(fsck.status.success());
}
