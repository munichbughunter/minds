//! Spec S3-MVP §6.2 (Commit 6): der pre-push-Orchestrator mit echtem
//! `git push` — s3_17 (stdin einmal, Verkettung fremder Hooks, Installation),
//! s3_18 (keine Rekursion, Hintergrund-Sync läuft), s3_19 (Übertragung:
//! inhaltsgleich übersprungen, Ablehnung, Frist, nicht erreichbar).
//!
//! Die Evidence ist hier von Hand angelegt (Bindung, Seal, Observations in
//! Server-Form, unsigniert): Der Remote ist ein schlichtes Bare-Repo; was der
//! Server anrechnet, prüfen die Witness-Tests (s3_19 dort).

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

struct World {
    dir: tempfile::TempDir,
    work: PathBuf,
    remote: PathBuf,
    key: String,
}

fn home(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    home
}

fn env<'a>(cmd: &'a mut Command, dir: &Path) -> &'a mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("HOME", home(dir))
        .env("XDG_CONFIG_HOME", dir.join("xdg-config"))
        .env("XDG_STATE_HOME", dir.join("xdg-state"))
        .env("MINDS_WITNESS", "off")
        .env_remove("MINDS_WITNESS_HOME")
        .env_remove("MINDS_WITNESS_SOCKET")
        .env_remove("MINDS_SYNCING")
        .env("GIT_TERMINAL_PROMPT", "0")
}

fn run(w: &World, dir: &Path, program: &str, args: &[&str]) -> Output {
    let mut cmd = Command::new(program);
    env(&mut cmd, w.dir.path()).current_dir(dir).args(args);
    cmd.output().unwrap()
}

fn git(w: &World, args: &[&str]) -> String {
    let out = run(w, &w.work, "git", args);
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn remote_git(w: &World, args: &[&str]) -> String {
    let out = run(w, &w.remote, "git", args);
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn minds(w: &World, args: &[&str]) -> Output {
    run(w, &w.work, MINDS, args)
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn world_with(enable: bool) -> World {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let remote = dir.path().join("remote.git");
    fs::create_dir_all(&work).unwrap();
    let mut w = World {
        dir,
        work,
        remote,
        key: String::new(),
    };
    let out = run(
        &w,
        w.dir.path(),
        "git",
        &["init", "-q", "--bare", "remote.git"],
    );
    assert!(out.status.success());
    git(&w, &["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.email", "t@minds.invalid"),
        ("user.name", "Minds Test"),
        ("commit.gpgsign", "false"),
    ] {
        git(&w, &["config", k, v]);
    }
    git(&w, &["remote", "add", "origin", w.remote.to_str().unwrap()]);
    fs::write(w.work.join("a.txt"), "a\n").unwrap();
    git(&w, &["add", "."]);
    git(&w, &["commit", "-qm", "first"]);
    let keyfile = w.dir.path().join("key");
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&keyfile)
        .output()
        .unwrap();
    assert!(out.status.success());
    let public = fs::read_to_string(keyfile.with_extension("pub")).unwrap();
    let mut parts = public.split_whitespace();
    w.key = format!("{} {}", parts.next().unwrap(), parts.next().unwrap());
    if enable {
        let out = minds(&w, &["enable", "--agent", "claude-code"]);
        assert!(out.status.success(), "{}", text(&out));
    }
    w
}

fn world() -> World {
    world_with(true)
}

fn hooks(w: &World) -> PathBuf {
    w.work.join(".git/hooks")
}

fn write_exec(path: &Path, content: &str, mode: u32) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Schreibt ein Objekt und gibt seine Id zurück.
fn blob(w: &World, bytes: &[u8]) -> String {
    let mut child = Command::new("git");
    env(&mut child, w.dir.path())
        .arg("-C")
        .arg(&w.work)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = child.spawn().unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Ein wurzelloser Commit mit genau diesen Dateien unter `name`.
fn put(w: &World, name: &str, files: &[(&str, &[u8])]) -> String {
    let mut listing = String::new();
    for (file, bytes) in files {
        listing.push_str(&format!("100644 blob {}\t{file}\n", blob(w, bytes)));
    }
    let mut child = Command::new("git");
    env(&mut child, w.dir.path())
        .arg("-C")
        .arg(&w.work)
        .arg("mktree")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = child.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(listing.as_bytes())
        .unwrap();
    let tree = String::from_utf8(child.wait_with_output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned();
    let commit = git(w, &["commit-tree", &tree, "-m", "minds evidence"]);
    git(w, &["update-ref", name, &commit]);
    commit
}

/// Evidence in Server-Form für `commit`: Observations, Seal, Bindung.
/// Liefert die drei Ref-Namen in Übertragungsreihenfolge.
fn evidence_for(w: &World, commit: &str) -> Vec<String> {
    use minds_core::evidence::{SCOPE_WITNESS_FS_V1, Seal, SealOutcome};
    let obs = format!("{{\"commit\":\"{commit}\"}}");
    let obs_id = format!("b3-{}", blake3::hash(obs.as_bytes()).to_hex());
    let obs_ref = format!("refs/minds/observations/{}", &obs_id[3..]);
    put(w, &obs_ref, &[("observations.json", obs.as_bytes())]);
    let text = Seal {
        root: minds_core::ContentHash::from_bytes(*blake3::hash(commit.as_bytes()).as_bytes()),
        agent: "witness".into(),
        scope: SCOPE_WITNESS_FS_V1.into(),
        first_seq: 1,
        last_seq: 1,
        events: 1,
        gaps: 0,
        pre_chain: 0,
        outcome: SealOutcome::ObservationsStored {
            observations: obs_id,
        },
        previous: None,
        last_event_at: "2026-10-27T10:00:00Z".into(),
    }
    .to_text()
    .unwrap();
    let seal_id = Seal::id_of_text(&text).as_str().to_owned();
    let seal_ref = format!("refs/minds/evidence/{}", &seal_id[3..]);
    put(w, &seal_ref, &[("seal", text.as_bytes())]);
    let binding = minds_witness_git::binding::CommitBinding::new(
        commit,
        &w.key,
        "git.test/project-42",
        vec![seal_id],
        32,
    )
    .unwrap();
    put(w, &binding.ref_name(), &[("binding", &binding.to_bytes())]);
    vec![obs_ref, seal_ref, binding.ref_name()]
}

fn new_commit(w: &World, branch: &str, file: &str) -> String {
    git(w, &["checkout", "-q", "-B", branch, "main"]);
    fs::write(w.work.join(file), file).unwrap();
    git(w, &["add", file]);
    git(w, &["commit", "-qm", file]);
    let c = git(w, &["rev-parse", "HEAD"]);
    git(w, &["checkout", "-q", "main"]);
    c
}

fn push(w: &World, args: &[&str]) -> Output {
    let mut all = vec!["push"];
    all.extend_from_slice(args);
    run(w, &w.work, "git", &all)
}

fn on_remote(w: &World, name: &str) -> bool {
    !remote_git(w, &["for-each-ref", "--format=%(refname)", name]).is_empty()
}

#[test]
fn s3_17_orchestrator_reads_stdin_once() {
    let w = world();
    // Ein fremder Hook, der stdin vollständig liest (wie `git lfs`), mit
    // einem alten minds-Block darin; `enable` verkettet ihn.
    let seen = w.dir.path().join("chained-stdin");
    let foreign = format!(
        "#!/bin/sh\ncat > '{}'\nexit 0\n# >>> minds >>>\nold minds block\n# <<< minds <<<\n",
        seen.display()
    );
    let pre_push = hooks(&w).join("pre-push");
    write_exec(&pre_push, &foreign, 0o755);
    let out = minds(&w, &["enable", "--agent", "claude-code"]);
    assert!(out.status.success(), "{}", text(&out));
    let chained = hooks(&w).join("pre-push.minds-chained");
    let moved = fs::read_to_string(&chained).unwrap();
    assert!(moved.contains("cat >"), "{moved}");
    assert!(
        !moved.contains("old minds block"),
        "alter Block entfernt: {moved}"
    );
    assert!(
        fs::metadata(&chained).unwrap().permissions().mode() & 0o111 != 0,
        "Modus bleibt"
    );
    let ours = fs::read_to_string(&pre_push).unwrap();
    assert!(ours.contains("hook pre-push"), "{ours}");

    // Drei Ref-Zeilen in einem Push.
    let commits: Vec<String> = ["b1", "b2", "b3"]
        .iter()
        .map(|b| new_commit(&w, b, &format!("{b}.txt")))
        .collect();
    let wanted: Vec<Vec<String>> = commits.iter().map(|c| evidence_for(&w, c)).collect();
    let out = push(&w, &["origin", "b1", "b2", "b3"]);
    assert!(out.status.success(), "{}", text(&out));
    let stdin = fs::read_to_string(&seen).unwrap();
    assert_eq!(
        stdin.lines().count(),
        3,
        "der fremde Hook sah alle drei: {stdin}"
    );
    for refs in &wanted {
        for name in refs {
            assert!(on_remote(&w, name), "{name} fehlt: {}", text(&out));
        }
    }
    assert!(
        text(&out).contains("minds: evidence transferred (9 refs)"),
        "{}",
        text(&out)
    );

    // Fremder Hook mit Exit 3: Push abgebrochen, keine Evidence.
    write_exec(&chained, "#!/bin/sh\ncat >/dev/null\nexit 3\n", 0o755);
    let c4 = new_commit(&w, "b4", "b4.txt");
    let refs4 = evidence_for(&w, &c4);
    let out = push(&w, &["origin", "b4"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(!on_remote(&w, "refs/heads/b4"));
    assert!(refs4.iter().all(|n| !on_remote(&w, n)));

    // Nicht ausführbar: übersprungen, minds läuft.
    let marker = w.dir.path().join("chained-ran");
    write_exec(
        &chained,
        &format!("#!/bin/sh\ntouch '{}'\nexit 3\n", marker.display()),
        0o644,
    );
    let out = push(&w, &["origin", "b4"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!marker.exists());
    assert!(refs4.iter().all(|n| on_remote(&w, n)), "{}", text(&out));

    // Ohne Shebang: die Shell führt ihn aus.
    write_exec(
        &chained,
        &format!("cat >/dev/null\ntouch '{}'\n", marker.display()),
        0o755,
    );
    new_commit(&w, "b5", "b5.txt");
    let out = push(&w, &["origin", "b5"]);
    assert!(out.status.success() && marker.exists(), "{}", text(&out));

    // Ein Binary ohne `hook pre-push` (Exit 2) bzw. ein nicht startbares:
    // Der Push geht durch, der verkettete Hook lief.
    for (content, mode) in [
        ("#!/bin/sh\necho 'usage' >&2\nexit 2\n", 0o755),
        ("#!/nonexistent/interpreter\n", 0o755),
    ] {
        let fake = w.dir.path().join("fake-minds");
        write_exec(&fake, content, mode);
        git(&w, &["config", "minds.binary", fake.to_str().unwrap()]);
        fs::remove_file(&marker).ok();
        let b = format!("x{mode}{}", content.len());
        new_commit(&w, &b, &format!("{b}.txt"));
        let out = push(&w, &["origin", &b]);
        assert!(out.status.success() && marker.exists(), "{}", text(&out));
    }
    // Binary nicht auffindbar (kein `minds.binary`, keins im PATH): Push
    // geht durch, der verkettete Hook bekommt das volle stdin.
    git(&w, &["config", "--unset", "minds.binary"]);
    write_exec(
        &chained,
        &format!("#!/bin/sh\ncat > '{}'\n", seen.display()),
        0o755,
    );
    new_commit(&w, "b6", "b6.txt");
    new_commit(&w, "b7", "b7.txt");
    let out = push(&w, &["origin", "b6", "b7"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(&seen).unwrap().lines().count(), 2);

    // Ein `TMPDIR` mit Leerzeichen: geht. Eines, das es nicht gibt
    // (`mktemp` scheitert): Nur der verkettete Hook läuft, mit dem echten
    // stdin; minds überträgt nichts.
    let spaced = w.dir.path().join("tmp dir");
    fs::create_dir_all(&spaced).unwrap();
    let c8 = new_commit(&w, "b8", "b8.txt");
    let refs8 = evidence_for(&w, &c8);
    git(&w, &["config", "minds.binary", MINDS]);
    let mut cmd = Command::new("git");
    env(&mut cmd, w.dir.path())
        .current_dir(&w.work)
        .env("TMPDIR", &spaced)
        .args(["push", "origin", "b8"]);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(refs8.iter().all(|n| on_remote(&w, n)), "{}", text(&out));
    let c9 = new_commit(&w, "b9", "b9.txt");
    let refs9 = evidence_for(&w, &c9);
    let mut cmd = Command::new("git");
    env(&mut cmd, w.dir.path())
        .current_dir(&w.work)
        .env("TMPDIR", w.dir.path().join("does-not-exist"))
        .args(["push", "origin", "b9"]);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(&seen).unwrap().lines().count(), 1);
    assert!(refs9.iter().all(|n| !on_remote(&w, n)));

    // `disable` stellt den fremden Hook wieder her.
    let out = minds(&w, &["disable"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!chained.exists());
    let restored = fs::read_to_string(&pre_push).unwrap();
    assert!(restored.contains(&seen.display().to_string()), "{restored}");
}

/// Ein fremder pre-push in einem gemeinsamen Hook-Verzeichnis (hier im
/// Worktree, wie husky): nichts verschoben, kein Hook geschrieben, Exit 2.
#[test]
fn s3_17_shared_hooks_dir_with_a_foreign_pre_push_is_left_alone() {
    let w = world_with(false);
    let shared = w.work.join(".husky");
    fs::create_dir_all(&shared).unwrap();
    git(&w, &["config", "core.hooksPath", ".husky"]);
    write_exec(&shared.join("pre-push"), "#!/bin/sh\nnpm test\n", 0o755);
    let out = minds(&w, &["enable", "--agent", "claude-code"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(
        text(&out).contains("pre-push.minds-chained"),
        "{}",
        text(&out)
    );
    let names: Vec<String> = fs::read_dir(&shared)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["pre-push"],
        "kein Hook geschrieben, nichts verschoben"
    );
    assert_eq!(
        fs::read_to_string(shared.join("pre-push")).unwrap(),
        "#!/bin/sh\nnpm test\n"
    );
}

#[test]
fn s3_18_internal_push_never_recurses() {
    let w = world();
    let count = w.dir.path().join("count");
    write_exec(
        &hooks(&w).join("pre-push.minds-chained"),
        &format!(
            "#!/bin/sh\ncat >/dev/null\necho x >> '{}'\n",
            count.display()
        ),
        0o755,
    );
    let c = new_commit(&w, "f", "f.txt");
    let refs = evidence_for(&w, &c);
    // Ein lokaler minds-Ref, den nur der Hintergrund-Sync überträgt.
    let head = git(&w, &["rev-parse", "HEAD"]);
    git(&w, &["update-ref", "refs/minds/context", &head]);
    let out = push(&w, &["origin", "f"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(refs.iter().all(|n| on_remote(&w, n)), "{}", text(&out));
    // Der Hintergrund-Sync läuft tatsächlich — und löst wie der interne
    // Evidence-Push keinen Orchestrator aus.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !on_remote(&w, "refs/minds/context") {
        assert!(Instant::now() < deadline, "Hintergrund-Sync kam nie an");
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 1);

    // Mit `MINDS_SYNCING`: sofort Ende, nichts gelesen, nichts übertragen.
    let c2 = new_commit(&w, "g", "g.txt");
    let refs2 = evidence_for(&w, &c2);
    let mut cmd = Command::new(MINDS);
    env(&mut cmd, w.dir.path())
        .current_dir(&w.work)
        .env("MINDS_SYNCING", "1")
        .args(["hook", "pre-push", "origin", w.remote.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let line = format!("refs/heads/g {c2} refs/heads/g {}\n", "0".repeat(40));
    let _ = child.stdin.take().unwrap().write_all(line.as_bytes());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "{}",
        text(&out)
    );
    assert!(refs2.iter().all(|n| !on_remote(&w, n)));
}

#[test]
fn s3_19_evidence_transfer() {
    let w = world();
    let c = new_commit(&w, "e", "e.txt");
    let refs = evidence_for(&w, &c);
    let out = push(&w, &["origin", "e"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(refs.iter().all(|n| on_remote(&w, n)));
    // Inhaltsgleich vorhanden: nicht erneut übertragen.
    let out = minds(&w, &["sync", "--evidence", &format!("{c}~1..{c}")]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("minds: no evidence to transfer"),
        "{}",
        text(&out)
    );

    // Der Remote lehnt minds-Refs ab: feste Warnung, der Code geht durch.
    write_exec(
        &w.remote.join("hooks/pre-receive"),
        "#!/bin/sh\nwhile read old new ref; do case \"$ref\" in refs/minds/*) \
         echo 'minds-witness: rejected for the test' >&2; exit 1;; esac; done\nexit 0\n",
        0o755,
    );
    let c2 = new_commit(&w, "r", "r.txt");
    let refs2 = evidence_for(&w, &c2);
    let out = push(&w, &["origin", "r"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains(
            "minds: warning: evidence not transferred (rejected); the code push continues"
        ),
        "{}",
        text(&out)
    );
    assert!(
        text(&out).contains("rejected for the test"),
        "Server-Meldung sichtbar"
    );
    assert!(on_remote(&w, "refs/heads/r"));
    assert!(refs2.iter().all(|n| !on_remote(&w, n)));
    fs::remove_file(w.remote.join("hooks/pre-receive")).unwrap();

    // Frist: ein Remote, der nicht antwortet.
    git(&w, &["remote", "add", "slow", w.remote.to_str().unwrap()]);
    git(
        &w,
        &[
            "config",
            "remote.slow.uploadpack",
            "sleep 20; git-upload-pack",
        ],
    );
    let mut cmd = Command::new(MINDS);
    env(&mut cmd, w.dir.path())
        .current_dir(&w.work)
        .env("MINDS_EVIDENCE_TIMEOUT_MS", "1000")
        .args([
            "sync",
            "--evidence",
            "--remote",
            "slow",
            &format!("{c2}~1..{c2}"),
        ]);
    let started = Instant::now();
    let out = cmd.output().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "Frist gehalten"
    );
    assert!(!out.status.success());
    assert!(
        text(&out).contains("evidence not transferred (timeout)"),
        "{}",
        text(&out)
    );
    // Frist auch in der Übertragung selbst (Abgleich schnell, Push hängt) —
    // mit Evidence, die sicher noch nicht am Remote liegt (der
    // Hintergrund-Sync früherer Pushes kann die übrigen schon gebracht haben).
    let c3 = new_commit(&w, "t", "t.txt");
    evidence_for(&w, &c3);
    git(
        &w,
        &["remote", "add", "slowpush", w.remote.to_str().unwrap()],
    );
    git(
        &w,
        &[
            "config",
            "remote.slowpush.receivepack",
            "sleep 20; git-receive-pack",
        ],
    );
    let mut cmd = Command::new(MINDS);
    env(&mut cmd, w.dir.path())
        .current_dir(&w.work)
        .env("MINDS_EVIDENCE_TIMEOUT_MS", "1500")
        // Der abgebrochene Transport lässt `sleep` als Enkel zurück, der das
        // geerbte stderr offen hält; im Hook wartet niemand darauf (Git wartet
        // auf den Hook-Prozess, nicht auf das Ende der Ausgabe), hier schon.
        .stderr(Stdio::null())
        .args([
            "sync",
            "--evidence",
            "--remote",
            "slowpush",
            &format!("{c3}~1..{c3}"),
        ]);
    let started = Instant::now();
    let out = cmd.output().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "Frist gehalten"
    );
    assert!(
        text(&out).contains("evidence not transferred (timeout)"),
        "{}",
        text(&out)
    );
    // Nicht erreichbar.
    git(&w, &["remote", "add", "gone", "/nonexistent/remote.git"]);
    let out = minds(
        &w,
        &[
            "sync",
            "--evidence",
            "--remote",
            "gone",
            &format!("{c2}~1..{c2}"),
        ],
    );
    assert!(
        text(&out).contains("evidence not transferred (unreachable)"),
        "{}",
        text(&out)
    );
}

/// Der Hook bindet mit einem echten Witness (`minds.project` gesetzt): eine
/// Zusammenfassung statt einer Zeile je Commit; ist der Witness weg, eine
/// sichtbare Warnung — der Push geht in beiden Fällen durch.
#[test]
fn the_hook_binds_through_the_witness_and_never_blocks() {
    let w = world();
    let home = w.dir.path().join("wh");
    for args in [
        vec![
            "witness",
            "init",
            "--repo",
            w.work.to_str().unwrap(),
            "--project",
            "git.test/project-42",
        ],
        vec!["witness", "keygen"],
    ] {
        let mut all = args.clone();
        all.extend(["--home", home.to_str().unwrap()]);
        let out = minds(&w, &all);
        assert!(out.status.success(), "{}", text(&out));
    }
    let mut daemon = Command::new(MINDS);
    env(&mut daemon, w.dir.path())
        .args(["witness", "run", "--home", home.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut daemon = daemon.spawn().unwrap();
    let socket = home.join("run/witness.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "witness did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    git(&w, &["config", "minds.project", "git.test/project-42"]);
    new_commit(&w, "h", "h.txt");
    let mut cmd = Command::new("git");
    env(&mut cmd, w.dir.path())
        .current_dir(&w.work)
        .env("MINDS_WITNESS_SOCKET", &socket)
        .args(["push", "origin", "h"]);
    let out = cmd.output().unwrap();
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("minds: 0 commit(s) bound, 0 already bound, 2 without evidence"),
        "{}",
        text(&out)
    );
    // Der Witness ist weg: sichtbare Warnung, der Push geht durch.
    new_commit(&w, "i", "i.txt");
    let mut cmd = Command::new("git");
    env(&mut cmd, w.dir.path())
        .current_dir(&w.work)
        .env("MINDS_WITNESS_SOCKET", &socket)
        .args(["push", "origin", "i"]);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("witness unavailable")
            && text(&out).contains("the code push continues"),
        "{}",
        text(&out)
    );
    assert!(on_remote(&w, "refs/heads/i"));
}
