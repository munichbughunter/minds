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
/// Die Hooks lösen seit #25 zuerst den bei `enable` gemerkten Ort auf
/// (`minds.binary`) — der zeigt hier aufs Test-Binary. Dieser Eintrag hält die
/// **Rückfallebene** deterministisch: Fiele die Auflösung aus, liefe sonst eine
/// global installierte, womöglich veraltete `minds` — grün aus dem falschen
/// Grund. Dass die Auflösung auch ganz ohne dieses Netz trägt, prüft
/// `a_commit_without_minds_in_the_path_still_checkpoints`.
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
    // Der post-commit-Hook checkpointet hier bereits selbst — er löst das
    // Test-Binary über `minds.binary` auf. Schritt 4 ruft `checkpoint`
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

    // 7. Alles auflösbar — und der ganze Durchlauf hatte nichts zu melden.
    let fsck = minds(dir, &["fsck"], None);
    assert!(fsck.status.success(), "fsck rot:\n{}", stdout(&fsck));
    assert!(stdout(&fsck).contains("in Ordnung"));
    assert!(
        !stdout(&fsck).contains("Log:"),
        "der Kern-Loop schreibt keinen Log-Eintrag:\n{}",
        stdout(&fsck)
    );
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

/// Der `PATH` eines GUI-Clients: `git` ist da, `minds` nicht.
///
/// Hartkodiertes `/usr/bin:/bin` (wie im Repro von #25) wäre nicht überall
/// wahr — Homebrew-git, CI-Images. Entscheidend ist nur, dass **kein**
/// Verzeichnis mit einer `minds` darin auftaucht: genau das Verzeichnis, in
/// dem `git` liegt, und sonst nichts.
fn path_without_minds() -> std::ffi::OsString {
    // Nicht das *erste* git-Verzeichnis, sondern das erste **ohne** minds
    // daneben: Auf einer Maschine mit Homebrew-git und dorthin verlinkter
    // minds wäre der Test sonst hart rot, obwohl `/usr/bin` als Kandidat taugt.
    let git_dir = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .find(|dir| dir.join("git").is_file() && !dir.join("minds").exists())
        .expect("kein PATH-Verzeichnis mit git, aber ohne minds — dieser PATH sagt nichts aus");
    git_dir.into_os_string()
}

/// `git` mit dem PATH eines GUI-Clients — im Unterschied zu [`git`], das das
/// Test-Binary absichtlich in den Pfad stellt.
fn git_without_minds(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.env("PATH", path_without_minds());
    without_user_config(&mut cmd).output().expect("git läuft")
}

/// #25, Akzeptanzkriterium 1: Der Commit aus einem GUI-Client — `minds` ist
/// nirgends im `PATH` — erzeugt trotzdem einen Checkpoint. Der Hook löst den
/// bei `enable` gemerkten Ort auf, statt den `PATH` zu durchsuchen.
#[test]
fn a_commit_without_minds_in_the_path_still_checkpoints() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();

    // `enable` läuft wie beim Nutzer: aus einer Shell, in der minds liegt. Es
    // ist der **Hook**, der später ohne diese Shell auskommen muss.
    let enable = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(enable.status.success(), "{}", stdout(&enable));

    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Commit aus dem GUI-Client""#,
    );
    event(dir, r#""hook_event_name":"Stop""#);

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git_without_minds(dir, &["add", "a.txt"]);
    assert!(
        git_without_minds(dir, &["commit", "-q", "-m", "feat: aus dem GUI"])
            .status
            .success(),
        "der Commit selbst darf nie scheitern"
    );

    // Kein manuelles `checkpoint`: Was hier ankommt, kann nur der post-commit-
    // Hook getan haben — mit minds in keinem PATH-Verzeichnis.
    let refs = stdout(&git(
        dir,
        &["for-each-ref", "--format=%(refname)", "refs/minds/store/"],
    ));
    assert!(
        !refs.trim().is_empty(),
        "kein Checkpoint ohne minds im PATH — der Hook hat den gemerkten Ort nicht aufgelöst"
    );
}

/// #25, Akzeptanzkriterium 2: Ohne `minds` im `PATH` — und mit einem gemerkten
/// Ort, an dem nichts mehr liegt — schreibt der pre-push-Hook **nichts** auf
/// stderr und lässt den Push nicht scheitern. Kein „command not found"
/// zwischen den Zeilen von `git push`.
#[test]
fn pushing_without_minds_anywhere_stays_silent_and_green() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    // Der Worst Case aus #25: Binary umgezogen *und* PATH ohne minds.
    git(dir, &["config", "minds.binary", "/umgezogen/minds"]);

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "chore: etwas zum Pushen"]);

    // `/bin/sh` absolut — der minimale PATH kennt nur das git-Verzeichnis, und
    // genau so (über den Shebang, nicht über den PATH) startet Git den Hook.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(dir.join(".git/hooks/pre-push"))
        .args(["origin", "https://beispiel.invalid/x.git"])
        .current_dir(dir)
        .env("PATH", path_without_minds());
    let hook = without_user_config(&mut cmd)
        .output()
        .expect("der Hook läuft");

    assert!(
        hook.status.success(),
        "der pre-push-Hook darf einen Push nie scheitern lassen"
    );
    let stderr = String::from_utf8_lossy(&hook.stderr);
    assert!(
        stderr.is_empty(),
        "nichts davon gehört in den Push-Output:\n{stderr}"
    );
}

/// Zieht das Binary um, greift die PATH-Suche — und `minds fsck` sagt, dass
/// ein `minds enable` den Eintrag erneuert. Die Rückfallebene aus #25.
#[test]
fn a_stale_recorded_binary_falls_back_to_the_path_and_fsck_says_so() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    // Das Binary „zieht um": Der gemerkte Ort stimmt nicht mehr. `git` läuft
    // hier mit [`path_with_minds`] — die Rückfallebene findet das Test-Binary.
    git(dir, &["config", "minds.binary", "/umgezogen/minds"]);

    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Nach dem Umzug""#,
    );
    event(dir, r#""hook_event_name":"Stop""#);

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    assert!(
        git(dir, &["commit", "-q", "-m", "feat: nach dem Umzug"])
            .status
            .success()
    );

    let refs = stdout(&git(
        dir,
        &["for-each-ref", "--format=%(refname)", "refs/minds/store/"],
    ));
    assert!(
        !refs.trim().is_empty(),
        "die PATH-Rückfallebene hat nicht gegriffen"
    );

    // Still weiterlaufen genügt nicht — der Zustand muss sichtbar sein, sonst
    // hängt die Erfassung wieder unbemerkt am PATH (genau das Problem aus #25).
    let fsck = minds(dir, &["fsck"], None);
    assert!(fsck.status.success(), "{}", stdout(&fsck));
    assert!(
        stdout(&fsck).contains("minds.binary"),
        "fsck verschweigt den verwaisten Eintrag:\n{}",
        stdout(&fsck)
    );

    // Der Clone-Fall: Die versionierten Hook-Rümpfe reisen mit, die lokale
    // `.git/config` nie. Hooks aktuell, Schlüssel weg — auch das muss `fsck`
    // sagen, sonst attestiert es Gesundheit, während die Erfassung am PATH hängt.
    git(dir, &["config", "--unset", "minds.binary"]);
    let fsck = minds(dir, &["fsck"], None);
    assert!(fsck.status.success(), "{}", stdout(&fsck));
    assert!(
        stdout(&fsck).contains("minds.binary ist nicht gesetzt"),
        "fsck verschweigt den fehlenden Eintrag:\n{}",
        stdout(&fsck)
    );
}

/// `--local` in der Hook-Prelude: Ein `git -c minds.binary=…` (Git vererbt es
/// über `GIT_CONFIG_PARAMETERS` in den Hook-Prozess) darf die Auflösung nicht
/// umlenken — der Ort ist repo- und maschinenlokal, nichts, was ein Aufrufer
/// von außen stellt.
#[cfg(unix)]
#[test]
fn a_config_override_on_the_command_line_cannot_redirect_the_hooks() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    let enable = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(enable.status.success(), "{}", stdout(&enable));

    // Ein „minds", das nichts tut und Erfolg meldet: Würde der Hook hierhin
    // auflösen, entstünde kein Checkpoint — und der Test würde rot.
    let fake = dir.join("fake-minds");
    std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Override-Versuch""#,
    );
    event(dir, r#""hook_event_name":"Stop""#);

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    let override_arg = format!("minds.binary={}", fake.display());
    assert!(
        git(
            dir,
            &[
                "-c",
                &override_arg,
                "commit",
                "-q",
                "-m",
                "feat: mit Override"
            ]
        )
        .status
        .success()
    );

    let refs = stdout(&git(
        dir,
        &["for-each-ref", "--format=%(refname)", "refs/minds/store/"],
    ));
    assert!(
        !refs.trim().is_empty(),
        "der Hook hat den -c-Override ausgeführt statt des lokalen Eintrags"
    );
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// #11, Akzeptanzkriterium 1: Ein Tippfehler im Gate-Flag endet mit Fehler,
/// nicht mit Exit 0 — sonst ist das CI-Policy-Gate lautlos abgeschaltet und
/// die Pipeline grün.
#[test]
fn a_flag_typo_fails_loudly_instead_of_disarming_the_gate() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "chore: Grundstein"]);

    // Der Tippfehler (Plural statt Singular) — bisher Exit 0, Gate aus.
    let typo = minds(dir, &["fsck", "--require-reviews"], None);
    assert!(
        !typo.status.success(),
        "der Tippfehler muss das Gate rot machen, nicht abschalten"
    );
    assert!(
        stderr(&typo).contains("unbekanntes Flag"),
        "die Meldung muss den Fehler benennen:\n{}",
        stderr(&typo)
    );

    // Die Gegenprobe: das richtige Flag läuft weiter.
    let ok = minds(dir, &["fsck", "--require-review"], None);
    assert!(ok.status.success(), "{}", stdout(&ok));

    // Die Hintertür, die das Review gefunden hat: Ein nachgestelltes `--help`
    // darf den Tippfehler nicht in Exit 0 verwandeln.
    let backdoor = minds(dir, &["fsck", "--require-reviews", "--help"], None);
    assert!(
        !backdoor.status.success(),
        "--help hinter dem Tippfehler darf das Gate nicht entschärfen"
    );

    // Und die Variante ohne Bindestriche ist derselbe Fehler, nur positional.
    let bare = minds(dir, &["fsck", "require-review"], None);
    assert!(
        !bare.status.success(),
        "vergessene Bindestriche dürfen das Gate nicht abschalten"
    );
    assert!(
        stderr(&bare).contains("unerwartetes Argument"),
        "{}",
        stderr(&bare)
    );
}

/// Die Rekorder-Regel überlebt den strikten Parser: `minds hook` mit fremdem
/// Flag endet mit 0, schreibt kein Byte auf stdout (Steuerkanal des Agenten)
/// — und verliert das Event nicht.
#[test]
fn the_hook_swallows_a_foreign_flag_without_losing_the_event() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    let payload = format!(
        r#"{{"session_id":"sess-e2e","cwd":"{}","hook_event_name":"UserPromptSubmit","prompt":"Trotz Fremd-Flag"}}"#,
        dir.display()
    );
    let out = minds(
        dir,
        &["hook", "--agent", "claude-code", "--help"],
        Some(&payload),
    );
    assert!(out.status.success(), "hook endet immer mit 0");
    assert!(
        stdout(&out).is_empty(),
        "hook darf kein Byte auf stdout schreiben:\n{}",
        stdout(&out)
    );

    // Das Event ist trotzdem im Journal gelandet — sichtbar daran, dass der
    // nächste Checkpoint es eincheckt.
    let stop = format!(
        r#"{{"session_id":"sess-e2e","cwd":"{}","hook_event_name":"Stop"}}"#,
        dir.display()
    );
    minds(dir, &["hook", "--agent", "claude-code"], Some(&stop));
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "feat: trotz Fremd-Flag"]);
    let refs = stdout(&git(
        dir,
        &["for-each-ref", "--format=%(refname)", "refs/minds/store/"],
    ));
    assert!(
        !refs.trim().is_empty(),
        "das Event vor dem Fremd-Flag ist verloren gegangen"
    );
}

/// #66/#64: Ein Hook-Verzeichnis außerhalb des Repos bekommt keine Hooks ohne
/// Zustimmung — nicht-interaktiv heißt das: Abbruch mit Hinweis auf das Flag,
/// und **nichts** ist halb eingerichtet.
#[test]
fn enable_refuses_an_outside_hooks_dir_without_confirmation() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    let outside = tempfile::tempdir().unwrap();
    git(
        dir,
        &["config", "core.hooksPath", outside.path().to_str().unwrap()],
    );

    // stdin ist im Test eine Pipe, kein Terminal — der nicht-interaktive Fall.
    let out = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(
        !out.status.success(),
        "enable darf ohne Zustimmung nicht nach draußen schreiben"
    );
    assert!(
        stderr(&out).contains("--global-hooks"),
        "die Meldung muss den Ausweg nennen:\n{}",
        stderr(&out)
    );

    assert!(
        !outside.path().join("post-commit").exists(),
        "draußen darf nichts entstanden sein"
    );
    assert!(
        !dir.join(".claude/settings.json").exists(),
        "nichts halb Eingerichtetes: auch die Agent-Konfiguration bleibt aus"
    );
}

/// #64: Mit `--global-hooks` liegt die Zustimmung vor — die Hooks entstehen
/// draußen, und `minds fsck` benennt den besonderen Ort, statt ihn wie einen
/// gewöhnlichen zu behandeln.
#[test]
fn the_global_hooks_flag_confirms_an_outside_dir_and_fsck_names_it() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    let outside = tempfile::tempdir().unwrap();
    git(
        dir,
        &["config", "core.hooksPath", outside.path().to_str().unwrap()],
    );

    let out = minds(
        dir,
        &["enable", "--agent", "claude-code", "--global-hooks"],
        None,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        outside.path().join("post-commit").exists(),
        "mit Zustimmung entstehen die Hooks draußen"
    );

    let fsck = minds(dir, &["fsck"], None);
    assert!(fsck.status.success(), "{}", stdout(&fsck));
    assert!(
        stdout(&fsck).contains("außerhalb des Repos"),
        "fsck muss den Ort benennen:\n{}",
        stdout(&fsck)
    );
}

/// #66, der Leitfall: Ein eingecheckter Symlink lenkt das Hook-Verzeichnis um
/// (`core.hooksPath = ".husky/_"`, `.husky` ist ein Link nach draußen). Am
/// fremden Ziel darf nichts entstehen — egal, dass kein Glied des
/// konfigurierten Pfads selbst ein Link ist.
#[cfg(unix)]
#[test]
fn a_checked_in_symlink_cannot_redirect_enable_outside() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.join(".husky")).unwrap();
    git(dir, &["config", "core.hooksPath", ".husky/_"]);

    let out = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(
        !out.status.success(),
        "der Symlink darf enable nicht nach draußen lenken"
    );
    assert!(
        !outside.path().join("_").exists(),
        "am fremden Ziel darf nichts entstanden sein"
    );
}

/// Die Gegenprobe zu #66 (Umgehung 4, das Falsch-Rot): Ein symlinktes `.git`
/// ist ein von Git unterstütztes Setup — dorthin kann ein Checkout nichts
/// legen, und `enable` muss ohne Rückfrage durchlaufen.
#[cfg(unix)]
#[test]
fn a_symlinked_git_dir_still_enables_without_questions() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    let store = tempfile::tempdir().unwrap();
    let gitstore = store.path().join("gitstore");
    std::fs::rename(dir.join(".git"), &gitstore).unwrap();
    std::os::unix::fs::symlink(&gitstore, dir.join(".git")).unwrap();

    let out = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        gitstore.join("hooks/post-commit").exists(),
        "die Hooks gehören ins (symlinkte) Git-Verzeichnis"
    );
}

/// #54: Ein Panic im heißen Pfad bleibt **vollständig** still. `catch_unwind`
/// allein genügte nicht — der Standard-Handler hatte `thread 'main' panicked
/// at …` samt Backtrace-Hinweis vorher schon auf stderr geschrieben, und
/// stderr des Hooks gibt Claude Code dem Modell zurück.
#[test]
fn a_panic_in_the_hook_reaches_neither_stdout_nor_stderr() {
    use std::io::Write;

    if !cfg!(debug_assertions) {
        eprintln!("Release-Build — der Panic-Haken existiert dort nicht, Test übersprungen");
        return;
    }
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    assert!(
        minds(dir, &["enable", "--agent", "claude-code"], None)
            .status
            .success()
    );

    let payload = format!(
        r#"{{"session_id":"sess-panic","cwd":"{}","hook_event_name":"Stop"}}"#,
        dir.display()
    );
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir)
        .args(["hook", "--agent", "claude-code"])
        .env("MINDS_PANIC_FOR_TEST", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    without_user_config(&mut cmd);
    let mut child = cmd.spawn().expect("minds startet");
    // Kein `unwrap`: Beendet sich das Kind vor dem Schreiben, ist EPIPE hier
    // kein Testfehler — was zählt, steht unten.
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("minds endet");

    // Regel 1: immer 0 — Exit 2 hieße bei Claude Code „blockiere diese Aktion".
    assert!(out.status.success(), "der Hook endet immer mit 0");
    // Regel 2: kein Byte auf stdout, und seit #54 auch keines auf stderr.
    assert!(out.stdout.is_empty(), "stdout: {:?}", stdout(&out));
    assert!(
        out.stderr.is_empty(),
        "stderr trägt den Panic in die Agent-Sitzung:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Verschwunden ist er trotzdem nicht — samt Ort, sonst wüsste niemand, wo
    // nachzusehen ist.
    let log = std::fs::read_to_string(dir.join(".git/minds/hook.log")).expect("das Log existiert");
    assert!(log.contains("Panic"), "{log}");
    assert!(
        log.contains("hook.rs"),
        "der Ort des Panics fehlt im Log:\n{log}"
    );
}

/// #68: Ein scheiterndes `minds brief --hook` verschwindet nicht mehr. Die
/// registrierte Zeile lautet `minds brief --hook 2>/dev/null || true` — stderr
/// ging ins Nichts, der Rückgabewert wurde verschluckt, und die Sitzung
/// startete ohne den Kontext, den minds ihr mitgeben wollte.
#[test]
fn a_failing_brief_hook_lands_in_the_log_instead_of_nowhere() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    assert!(
        minds(dir, &["enable", "--agent", "claude-code"], None)
            .status
            .success()
    );

    // Ein Store, der sich nicht öffnen lässt: Die Konfiguration zeigt auf ein
    // Child-Repo, das es nicht gibt.
    git(dir, &["config", "minds.backend", "child-repo"]);
    git(dir, &["config", "minds.childPath", "../gibt-es-nicht"]);

    let out = minds(dir, &["brief", "--hook"], None);
    assert!(
        !out.status.success(),
        "der Fehler soll sich im Rückgabewert zeigen — der Hook fängt ihn mit `|| true`"
    );
    assert!(
        out.stdout.is_empty(),
        "stdout trägt den injizierten Kontext, keine Diagnose:\n{}",
        stdout(&out)
    );

    let log = std::fs::read_to_string(dir.join(".git/minds/hook.log")).expect("das Log existiert");
    assert!(
        log.contains("brief:"),
        "der Eintrag nennt seinen Pfad nicht:\n{log}"
    );
}

/// Und ohne `--hook` bleibt es beim alten Weg: Dort steht ein Mensch davor,
/// der Fehler gehört auf stderr — und **nicht** ins Log, das sonst bei jedem
/// Terminal-Aufruf mitwüchse.
#[test]
fn a_failing_brief_without_the_hook_flag_stays_on_stderr() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    git(dir, &["config", "minds.backend", "child-repo"]);
    git(dir, &["config", "minds.childPath", "../gibt-es-nicht"]);

    let out = minds(dir, &["brief"], None);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("minds brief:"),
        "der Fehler gehört im Terminal auf stderr"
    );
    assert!(
        !dir.join(".git/minds/hook.log").exists(),
        "der Terminal-Aufruf soll das Log nicht füllen"
    );
}

/// #68: Ein Panic im `--hook`-Pfad erreicht die Sitzung nicht — weder über
/// stdout (dort steht der injizierte Kontext) noch über stderr —, verschwindet
/// aber auch nicht: Der **Ort** steht im Log. Die Meldung nicht, denn `brief`
/// hält redigierte Sessions im Speicher.
#[test]
fn a_panic_in_brief_hook_reaches_neither_channel_but_leaves_its_place() {
    if !cfg!(debug_assertions) {
        eprintln!("Release-Build — der Panic-Haken existiert dort nicht, Test übersprungen");
        return;
    }
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    assert!(
        minds(dir, &["enable", "--agent", "claude-code"], None)
            .status
            .success()
    );

    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir)
        .args(["brief", "--hook"])
        .env("MINDS_BRIEF_PANIC_FOR_TEST", "1")
        .env("RUST_BACKTRACE", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    without_user_config(&mut cmd);
    let out = cmd.output().expect("minds endet");

    assert!(out.stdout.is_empty(), "stdout: {:?}", stdout(&out));
    assert!(
        out.stderr.is_empty(),
        "stderr trägt den Panic in die Sitzung:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let log = std::fs::read_to_string(dir.join(".git/minds/hook.log")).expect("das Log existiert");
    assert!(log.contains("brief: Panic"), "{log}");
    assert!(
        log.contains("brief_cmd.rs:"),
        "der Ort des Panics fehlt:\n{log}"
    );
    assert!(
        !log.contains("absichtlicher Panic"),
        "die Meldung gehört nicht ins Log — sie könnte Nutzlast tragen:\n{log}"
    );
}

/// #54, die Kehrseite: Der Panic-Text ist ein **neuer** Kanal ins Log (vorher
/// stand dort eine Konstante). Die Zusage des Moduls — keine Nutzlast in
/// `hook.log`, die Datei geht in Bug-Reports mit — muss auch für ihn gelten.
/// Deshalb landet vom heißen Pfad nur der **Ort** dort, nicht die Meldung.
#[test]
fn a_panic_message_never_carries_the_payload_into_the_log() {
    use std::io::Write;

    if !cfg!(debug_assertions) {
        eprintln!("Release-Build — der Panic-Haken existiert dort nicht, Test übersprungen");
        return;
    }
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();

    let secret = "glpat-AAAAAAAAAAAAAAAAAAAA";
    let payload = format!(
        r#"{{"session_id":"sess-x","cwd":"{}","hook_event_name":"UserPromptSubmit","prompt":"Deploy mit {secret}"}}"#,
        dir.display()
    );
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir)
        .args(["hook", "--agent", "claude-code"])
        .env("MINDS_PANIC_FOR_TEST", "payload")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    without_user_config(&mut cmd);
    let mut child = cmd.spawn().expect("minds startet");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("minds endet");
    assert!(out.status.success());

    let log = std::fs::read_to_string(dir.join(".git/minds/hook.log")).expect("das Log existiert");
    assert!(
        !log.contains(secret),
        "der Panic-Kanal trägt Payload ins Log:\n{log}"
    );
    assert!(
        !log.contains("Deploy mit"),
        "der Panic-Kanal trägt Payload ins Log:\n{log}"
    );
    // Der Ort bleibt — sonst wäre der Gewinn aus #54 gleich wieder weg.
    assert!(log.contains("hook.rs:"), "der Ort fehlt:\n{log}");
}

/// #54, das Fenster **vor** der Klammer: Ein Panic im Argument-Parsing ging
/// mit Exit 101 und vollem Backtrace an den Agenten — und die
/// Claude-Registrierung ruft `minds hook` ohne `2>/dev/null` auf. Seit der
/// Prozess sich beim Dispatch als Hook-Pfad erklärt, gelten die Regeln ab
/// der ersten Zeile.
#[cfg(unix)]
#[test]
fn a_panic_before_the_guard_still_obeys_the_hook_rules() {
    use std::os::unix::ffi::OsStrExt;

    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();

    // Ungültiges UTF-8 als Argument: `std::env::args()` panickt daran, noch
    // bevor irgendein minds-Code läuft.
    let ugly = std::ffi::OsStr::from_bytes(b"\xff\xfe-kaputt-\xff");
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir)
        .arg("hook")
        .arg("--agent")
        .arg(ugly)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    without_user_config(&mut cmd);
    let out = cmd.output().expect("minds endet");

    assert!(
        out.stderr.is_empty(),
        "stderr trägt den Panic in die Agent-Sitzung:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "stdout: {:?}", stdout(&out));
    assert!(
        out.status.success(),
        "Regel 1: der Hook endet immer mit 0, auch hier"
    );
}

/// #52, end-to-end: Ein Hook ohne Execute-Bit erfasst nichts — Git
/// überspringt ihn wortlos. `minds fsck` benennt das, und `minds enable`
/// repariert es, obwohl der Inhalt unverändert stimmt.
#[cfg(unix)]
#[test]
fn a_hook_without_its_execute_bit_is_named_by_fsck_and_repaired_by_enable() {
    use std::os::unix::fs::PermissionsExt;

    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    assert!(
        minds(dir, &["enable", "--agent", "claude-code"], None)
            .status
            .success()
    );

    let hook = dir.join(".git/hooks/post-commit");
    let before = std::fs::read_to_string(&hook).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).unwrap();

    // Der Beweis, dass es nicht nur um Bits geht: So entsteht kein Checkpoint.
    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Ohne Execute-Bit""#,
    );
    event(dir, r#""hook_event_name":"Stop""#);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "feat: ohne Bit"]);
    let refs = stdout(&git(
        dir,
        &["for-each-ref", "--format=%(refname)", "refs/minds/store/"],
    ));
    assert!(
        refs.trim().is_empty(),
        "Voraussetzung verfehlt: der Hook lief trotz fehlendem Bit"
    );

    // fsck sagt es — vorher galt der Hook als „installiert".
    let fsck = minds(dir, &["fsck"], None);
    assert!(
        stdout(&fsck).contains("nicht ausführbar"),
        "fsck verschweigt den toten Hook:\n{}",
        stdout(&fsck)
    );

    // Und enable repariert, ohne den Inhalt anzufassen.
    assert!(
        minds(dir, &["enable", "--agent", "claude-code"], None)
            .status
            .success()
    );
    assert_eq!(std::fs::read_to_string(&hook).unwrap(), before);
    assert!(
        std::fs::metadata(&hook).unwrap().permissions().mode() & 0o111 != 0,
        "das Execute-Bit wurde nicht wiederhergestellt"
    );
    assert!(!stdout(&minds(dir, &["fsck"], None)).contains("nicht ausführbar"));
}

/// #65, end-to-end: Ein Symlink auf **eine** Agent-Konfiguration bricht
/// `enable` ab, **bevor** irgendetwas geschrieben ist — nicht mitten in der
/// Reihe. Sonst bliebe ein Repo zurück, dessen Agents journalieren, während
/// kein Hook je etwas eincheckt.
#[cfg(unix)]
#[test]
fn a_symlinked_agent_config_stops_enable_before_anything_is_written() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();

    let victim = tempfile::tempdir().unwrap();
    let target = victim.path().join("fremd.json");
    std::fs::write(&target, "{\"fremd\":true}\n").unwrap();
    std::fs::create_dir_all(dir.join(".gemini")).unwrap();
    std::os::unix::fs::symlink(&target, dir.join(".gemini/settings.json")).unwrap();

    // Ohne --agent: Claude, Codex und Cursor kämen vor Gemini an die Reihe.
    let out = minds(dir, &["enable"], None);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("Symlink"),
        "die Meldung muss den Grund nennen:\n{}",
        stderr(&out)
    );

    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "{\"fremd\":true}\n",
        "die fremde Datei wurde angefasst"
    );
    for untouched in [
        ".claude/settings.json",
        ".codex/hooks.json",
        ".cursor/hooks.json",
        ".git/hooks/post-commit",
    ] {
        assert!(
            !dir.join(untouched).exists(),
            "nichts halb Eingerichtetes — {untouched} ist entstanden"
        );
    }
}

/// #21, end-to-end: `minds enable` in einem Linked Worktree richtet ein — und
/// ein Commit **dort** erzeugt einen Checkpoint. Vorher meldete `enable` „kein
/// Git-Repository gefunden", weil es die `.git`-Datei nicht auflöste.
#[test]
fn enable_works_in_a_linked_worktree_and_captures_there() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let main = repo.path();
    std::fs::write(main.join("a.txt"), "a\n").unwrap();
    git(main, &["add", "a.txt"]);
    git(main, &["commit", "-q", "-m", "chore: Grundstein"]);

    let outside = tempfile::tempdir().unwrap();
    let linked = outside.path().join("zweig");
    if !git(main, &["worktree", "add", "-q", linked.to_str().unwrap()])
        .status
        .success()
    {
        eprintln!("git worktree add scheitert hier — Test übersprungen");
        return;
    }

    // Einrichten **im Worktree** — der Fall, der vorher abbrach.
    let enable = minds(&linked, &["enable", "--agent", "claude-code"], None);
    assert!(enable.status.success(), "{}", stderr(&enable));
    assert!(
        stdout(&enable).contains("verlinkter Worktree"),
        "der Worktree-Fall gehört benannt:\n{}",
        stdout(&enable)
    );

    // Die Hooks liegen im gemeinsamen Verzeichnis — Git führt für alle
    // Arbeitsbäume dieselben aus.
    assert!(
        main.join(".git/hooks/post-commit").exists(),
        "der Hook gehört ins gemeinsame Git-Verzeichnis"
    );

    // Und die Wirkung: eine Session im Worktree, ein Commit im Worktree, ein
    // Checkpoint. Ohne manuelles `checkpoint` — das kann nur der Hook getan
    // haben.
    event(
        &linked,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Im Worktree""#,
    );
    event(&linked, r#""hook_event_name":"Stop""#);
    std::fs::write(linked.join("b.txt"), "b\n").unwrap();
    git(&linked, &["add", "b.txt"]);
    assert!(
        git(&linked, &["commit", "-q", "-m", "feat: im Worktree"])
            .status
            .success()
    );

    let refs = stdout(&git(
        &linked,
        &["for-each-ref", "--format=%(refname)", "refs/minds/store/"],
    ));
    assert!(
        !refs.trim().is_empty(),
        "kein Checkpoint aus dem Worktree — der Hook hat nicht gefeuert"
    );

    // Und die Kette Commit → Store steht: Der Trailer am Commit des Worktrees
    // nennt genau den Ref, der entstanden ist.
    let message = stdout(&git(&linked, &["log", "-1", "--format=%B"]));
    let session = message
        .lines()
        .find_map(|line| line.strip_prefix("Minds-Session-Id: "))
        .map(str::trim)
        .unwrap_or_else(|| panic!("kein Trailer am Worktree-Commit:\n{message}"));
    let hex = session.strip_prefix("b3-").expect("b3-Präfix");
    assert!(
        refs.contains(hex),
        "der Trailer nennt einen anderen Ref als den entstandenen:\n{refs}"
    );

    // `minds fsck` bestätigt, dass nichts verwaist ist — der Lese-Weg über
    // `show`/`why` ist im Worktree noch nicht richtig, weil `repo_root()`
    // dort `…/.git/worktrees` ergibt. Das ist #20 („Repo::work_dir fehlt"),
    // ein workspace-weites Problem mit elf Fundstellen, und bewusst nicht
    // Teil von #21.
    let fsck = minds(&linked, &["fsck"], None);
    assert!(fsck.status.success(), "{}", stdout(&fsck));
    assert!(
        stdout(&fsck).contains("0 verwaist"),
        "fsck im Worktree meldet Waisen:\n{}",
        stdout(&fsck)
    );
}

/// Die zweite Gegenprobe zu #66: In einem Linked Worktree liegt das effektive
/// Hook-Verzeichnis im common dir des Haupt-Repos — von Git verwaltet, kein
/// fremder Ort. `fsck` darf dort kein „außerhalb des Repos" behaupten.
#[test]
fn fsck_in_a_linked_worktree_claims_no_outside() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "chore: Grundstein"]);

    let wt = tempfile::tempdir().unwrap();
    let linked = wt.path().join("zweig");
    let added = git(dir, &["worktree", "add", "-q", linked.to_str().unwrap()]);
    if !added.status.success() {
        eprintln!("git worktree add scheitert hier — Test übersprungen");
        return;
    }

    let fsck = minds(&linked, &["fsck"], None);
    assert!(fsck.status.success(), "{}", stdout(&fsck));
    assert!(
        !stdout(&fsck).contains("außerhalb des Repos"),
        "das common dir ist kein fremder Ort:\n{}",
        stdout(&fsck)
    );
}

/// #11, Akzeptanzkriterium 3: `--summary --sign` erzeugt kein unsigniertes
/// Review mit der Zusammenfassung „--sign" — es ist ein Fehler mit Meldung.
#[test]
fn a_swallowed_flag_value_is_an_error_not_an_unsigned_review() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();

    let out = minds(
        dir,
        &["review", "b3-0000", "--approve", "--summary", "--sign"],
        None,
    );
    assert!(
        !out.status.success(),
        "ein verschlucktes Flag darf kein Review anlegen"
    );
    assert!(
        stderr(&out).contains("braucht einen Wert"),
        "die Meldung muss den Wert einfordern:\n{}",
        stderr(&out)
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

    // Der Gutfall schreibt nichts. Ohne diese Zusage bekäme jeder Nutzer
    // dauerhaft einen `fsck`-Hinweis, sobald eine der best-effort-Stellen
    // (etwa `put_session_branch`) auf dem Default-Backend anfinge zu melden.
    assert!(
        hook_log(dir).is_none(),
        "ohne Fehler entsteht kein Log: {:?}",
        hook_log(dir)
    );
}

// ---------------------------------------------------------------------------
// Der Hook-Pfad hat ein Log (#10)
// ---------------------------------------------------------------------------

/// Das Log, das die Hook-Pfade bei einem Fehler beschreiben.
fn hook_log(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(".git/minds/hook.log")).ok()
}

/// Ruft einen installierten Hook so auf, wie Git es täte: über `sh`, aus der
/// Repo-Wurzel, mit `minds` im Pfad und ohne fremde Git-Config.
fn run_hook(dir: &Path, name: &str, args: &[&str]) -> Output {
    let mut cmd = Command::new("sh");
    cmd.arg(dir.join(".git/hooks").join(name))
        .args(args)
        .current_dir(dir)
        .env("PATH", path_with_minds());
    without_user_config(&mut cmd)
        .output()
        .expect("der Hook läuft")
}

#[test]
fn a_broken_redaction_policy_lands_in_the_log_instead_of_nowhere() {
    let Some(repo) = scratch_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = repo.path();
    let enable = minds(dir, &["enable", "--agent", "claude-code"], None);
    assert!(enable.status.success(), "{}", stdout(&enable));

    // Der Auslöser aus #10: ein Tippfehler in der Redaction-Policy. `checkpoint`
    // bricht daran *fail-closed* ab — bewusst, denn lieber nichts einchecken als
    // etwas Unredigiertes.
    std::fs::create_dir_all(dir.join(".minds")).unwrap();
    std::fs::write(dir.join(".minds/redact.json"), "{ das ist kein json").unwrap();

    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Schreibe etwas""#,
    );
    event(dir, r#""hook_event_name":"Stop""#);

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    let commit = git(dir, &["commit", "-q", "-m", "feat: etwas"]);

    // 1. Der Commit gelingt trotzdem. Ein Rekorder darf ihn nie scheitern lassen.
    assert!(
        commit.status.success(),
        "der Commit muss durchgehen: {}",
        String::from_utf8_lossy(&commit.stderr)
    );

    // 2. Und der Grund steht in der Datei, die die Doku verspricht — vor diesem
    //    Commit war der post-commit-Pfad der einzige ohne Log.
    let log = hook_log(dir).expect(".git/minds/hook.log muss entstanden sein");
    assert!(
        log.contains("checkpoint:"),
        "der Eintrag nennt seinen Ursprung:\n{log}"
    );
    assert!(
        log.contains("redact.json"),
        "der Eintrag nennt die kaputte Datei:\n{log}"
    );
    assert_eq!(log.lines().count(), 1, "genau ein Eintrag:\n{log}");

    // 3. Die Session ist vertagt, nicht verloren: Das Journal bleibt liegen, bis
    //    die Policy repariert ist.
    let fsck = stdout(&minds(dir, &["fsck"], None));
    assert!(
        fsck.contains("Journal: 1 Session(s) noch nicht eingecheckt"),
        "{fsck}"
    );
}

/// Die Annahme, auf der die Sicherheitsüberlegung zu `hook.log` steht: Keine
/// Fehlermeldung auf dem Hook-Pfad trägt Rohmaterial mit sich.
///
/// Sie stimmt heute, weil keine `Display`-Implementierung in dieser Kette den
/// Nutzlast-Text einbettet (`RedactionError` nennt Feldpfad und Anzahl,
/// `StoreError` Ids und Hashes). Das ist aber eine Eigenschaft, die ein
/// künftiges `#[error("… {0}")]` unbemerkt aufgäbe — deshalb hier als Test und
/// nicht nur als Kommentar. Der Ort ist zwar derselbe wie der des rohen
/// Journals, aber der Unterschied zählt: Das Journal wird redigiert, bevor
/// irgendetwas es verlässt; eine Log-Zeile wird das nie.
#[test]
fn a_hook_error_never_carries_the_raw_transcript() {
    let Some(repo) = scratch_repo() else {
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    // Synthetische Token — keine echten, aber in der Form, die die Redaction
    // erkennt.
    const TOKEN: &str = "glpat-AAAAAAAAAAAAAAAAAAAA";
    const KEY: &str = "AKIAIOSFODNN7EXAMPLE";

    event(
        dir,
        &format!(r#""hook_event_name":"UserPromptSubmit","prompt":"Deploy mit {TOKEN} und {KEY}""#),
    );
    event(dir, r#""hook_event_name":"Stop""#);

    // **Eine gültige Policy, die trotzdem scheitert.** Das ist der Punkt: Eine
    // kaputte `redact.json` bräche schon beim Laden ab — also *bevor* das
    // Journal gelesen ist, und dann hätte die Fehlermeldung die Nutzlast nie in
    // der Hand. Der Test wäre grün, ohne etwas zu prüfen.
    //
    // Ohne einen einzigen Detektor lädt die Policy, und `redact_session`
    // scheitert erst an der gebauten Session (`RedactionError::NoDetectors`) —
    // im `übersprungen`-Zweig, mit den Token im Envelope.
    std::fs::create_dir_all(dir.join(".minds")).unwrap();
    std::fs::write(
        dir.join(".minds/redact.json"),
        r#"{"known_tokens":false,"email":false,"keyed_values":false,
            "url_credentials":false,"high_entropy":{"enabled":false}}"#,
    )
    .unwrap();

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "feat: etwas"]);

    let log = hook_log(dir).expect("der Fehler steht im Log");
    assert!(
        log.contains("übersprungen"),
        "der Fehler muss aus dem Session-Pfad kommen, sonst prüft der Test nichts:\n{log}"
    );
    assert!(!log.contains(TOKEN), "Token im Log:\n{log}");
    assert!(!log.contains(KEY), "Schlüssel im Log:\n{log}");
    assert!(!log.contains("Deploy mit"), "Prompt-Text im Log:\n{log}");
}

/// Dieselbe Frage für den Sync-Pfad, wo die Antwort anders ausfällt: Dort baut
/// `Job::push` seinen Fehler aus dem **rohen stderr** eines fremden Prozesses,
/// und `git` schreibt die Remote-URL hinein. Steht ein Token in der
/// Username-Position, redigiert Git es nicht — minds muss das selbst tun.
#[test]
fn a_sync_error_never_carries_the_remote_credentials() {
    let Some(repo) = scratch_repo() else {
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Etwas""#,
    );
    event(dir, r#""hook_event_name":"Stop""#);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "feat: etwas"]);

    const TOKEN: &str = "glpat-AAAAAAAAAAAAAAAAAAAA";
    git(
        dir,
        &[
            "remote",
            "add",
            "kaputt",
            &format!("https://{TOKEN}@127.0.0.1:1/x.git"),
        ],
    );

    let hook = run_hook(
        dir,
        "pre-push",
        &["kaputt", &format!("https://{TOKEN}@127.0.0.1:1/x.git")],
    );
    assert!(hook.status.success());

    let log = hook_log(dir).expect("der Sync-Fehler steht im Log");
    assert!(log.contains("sync:"), "{log}");
    assert!(!log.contains(TOKEN), "Token im Log:\n{log}");
    // Der Host bleibt — ohne ihn wäre die Diagnose wertlos.
    assert!(log.contains("127.0.0.1"), "Host fehlt:\n{log}");
}

/// Und dasselbe mit `GIT_TRACE` in der Umgebung.
///
/// Der Fall ist tückischer, als er aussieht: Git protokolliert dann seinen
/// ganzen Verkehr auf stderr — samt `Authorization: Basic …`, und das ist keine
/// URL, die sich herausschneiden ließe. Seit dieses stderr in eine Datei geht,
/// genügte ein `GIT_TRACE=1` in der Shell des Entwicklers, um ein Token
/// dauerhaft auf die Platte zu legen. Der Kindprozess bekommt die Schalter
/// deshalb gar nicht erst zu sehen.
#[test]
fn a_traced_git_does_not_dump_its_headers_into_the_log() {
    let Some(repo) = scratch_repo() else {
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    event(dir, r#""hook_event_name":"Stop""#);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "feat: etwas"]);

    const TOKEN: &str = "glpat-CCCCCCCCCCCCCCCCCCCC";
    let url = format!("https://{TOKEN}@127.0.0.1:1/x.git");
    git(dir, &["remote", "add", "kaputt", &url]);

    let mut cmd = Command::new("sh");
    cmd.arg(dir.join(".git/hooks/pre-push"))
        .args(["kaputt", &url])
        .current_dir(dir)
        .env("PATH", path_with_minds())
        // Der Auslöser: gesetzt in der Umgebung, nicht von uns.
        .env("GIT_TRACE", "1")
        .env("GIT_CURL_VERBOSE", "1")
        .env("GIT_TRACE_CURL", "1");
    let hook = without_user_config(&mut cmd)
        .output()
        .expect("der Hook läuft");
    assert!(hook.status.success());

    let log = hook_log(dir).expect("der Sync-Fehler steht im Log");
    assert!(!log.contains(TOKEN), "Token im Log:\n{log}");
    assert!(!log.contains("Authorization"), "Header-Dump im Log:\n{log}");
    // Die Diagnose bleibt trotzdem brauchbar.
    assert!(log.contains("127.0.0.1"), "{log}");
}

/// Ein Push auf eine **URL** statt auf ein benanntes Remote darf keinen
/// Log-Eintrag erzeugen.
///
/// Git ruft den pre-push-Hook dann mit der URL als `$1` auf. Daraus einen
/// Tracking-Ref zu bauen ergibt keinen gültigen Ref-Namen — der Merge scheitert,
/// und ohne diese Sperre entstünde bei *jedem* solchen Push eine Zeile: ein
/// `fsck`-Hinweis, den man nur durch Löschen loswird und der sofort wiederkommt.
#[test]
fn pushing_to_a_url_instead_of_a_remote_writes_no_log_entry() {
    let Some(repo) = scratch_repo() else {
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "chore: etwas"]);

    let url = "https://example.invalid/x.git";
    let hook = run_hook(dir, "pre-push", &[url, url]);

    assert!(hook.status.success());
    assert!(
        hook_log(dir).is_none(),
        "kein Eintrag ohne benanntes Remote: {:?}",
        hook_log(dir)
    );
}

#[test]
fn fsck_points_at_the_log_but_does_not_quote_it() {
    let Some(repo) = scratch_repo() else {
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    // Ein Eintrag mit einem wiedererkennbaren Wortlaut: `prepare-commit-msg` auf
    // eine Datei, die es nicht gibt.
    let out = minds(
        dir,
        &["prepare-commit-msg", "gibt-es-nicht/message.txt"],
        None,
    );
    assert!(!out.status.success(), "der Aufruf scheitert");
    let log = hook_log(dir).expect("der Fehler steht im Log");
    assert!(log.contains("prepare-commit-msg:"), "{log}");

    let fsck = minds(dir, &["fsck"], None);
    let report = stdout(&fsck);

    // Verwiesen wird: auf die Zahl und auf den Pfad.
    assert!(report.contains("Log: 1 Eintrag"), "{report}");
    assert!(report.contains("hook.log"), "{report}");
    // Ein Hinweis, kein Befund — sonst hielte ein alter Eintrag das CI-Gate an.
    assert!(report.contains("Hinweis(e)"), "{report}");
    assert!(fsck.status.success(), "{report}");

    // Zitiert wird nicht: Die Ausgabe von `fsck` landet in CI-Logs, der Wortlaut
    // eines Hook-Fehlers kann einen Ausschnitt aus dem Rohmaterial tragen.
    assert!(
        !report.contains("gibt-es-nicht"),
        "der Wortlaut darf nicht in den Bericht:\n{report}"
    );
}

#[test]
fn the_pre_push_hook_keeps_its_stderr_out_of_the_push_output() {
    let Some(repo) = scratch_repo() else {
        return;
    };
    let dir = repo.path();
    minds(dir, &["enable", "--agent", "claude-code"], None);

    // Eine eingecheckte Session, damit es überhaupt etwas zu syncen gibt.
    event(
        dir,
        r#""hook_event_name":"UserPromptSubmit","prompt":"Etwas""#,
    );
    event(dir, r#""hook_event_name":"Stop""#);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "feat: etwas"]);

    // Ein Remote, das es nicht gibt — der häufigste Sync-Fehler überhaupt.
    git(dir, &["remote", "add", "kaputt", "/gibt/es/nicht.git"]);

    let hook = run_hook(dir, "pre-push", &["kaputt", "/gibt/es/nicht.git"]);

    // 1. Der Push des Nutzers bleibt unbehelligt: kein Rückgabewert ≠ 0 …
    assert!(hook.status.success(), "der Hook darf den Push nie anhalten");
    // … und keine rohe Fehlermeldung mitten im Push-Output. Vor diesem Commit
    // stand hier „minds sync: …" plus die vier Zeilen, die `git push` selbst
    // ausgibt — bei jedem Push, für einen Vorgang, den niemand angestoßen hat.
    assert!(
        hook.stderr.is_empty(),
        "stderr muss leer bleiben:\n{}",
        String::from_utf8_lossy(&hook.stderr)
    );

    // … aber der Fortschritt **überlebt**. Ohne diese Zusage wäre die
    // Umleitung ein Verschweigen: Ein `println!` → `eprintln!` in `sync.rs`
    // machte den Push wortlos, und der Test oben bliebe grün, weil `sh` das
    // stderr ohnehin wegwirft.
    let visible = String::from_utf8_lossy(&hook.stdout);
    assert!(
        visible.contains("Ref(s)"),
        "der Fortschritt fehlt:\n{visible}"
    );
    assert!(
        visible.contains("minds fsck"),
        "der Fehlschlag muss sichtbar bleiben und den Weg nennen:\n{visible}"
    );

    // 2. Verschwunden ist der Fehler deshalb trotzdem nicht.
    let log = hook_log(dir).expect("der Sync-Fehler steht im Log");
    assert!(log.contains("sync:"), "{log}");

    // 3. Die mehrzeilige Meldung von `git push` bleibt *ein* Eintrag — sonst
    //    täuschte sie vier vor.
    assert_eq!(log.lines().count(), 1, "genau ein Eintrag:\n{log}");
    assert!(log.contains("\\n"), "die Umbrüche sind entschärft:\n{log}");
}
