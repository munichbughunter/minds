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
