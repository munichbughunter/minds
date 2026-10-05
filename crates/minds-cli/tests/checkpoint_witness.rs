//! `minds checkpoint` delegiert an den Witness (EA-06d): Ein echter
//! `git commit` löst über den post-commit-Hook den Checkpoint der
//! Agent-Seite aus; der Witness läuft als eigener Prozess mit eigenem Home und
//! ist nur über seinen Socket erreichbar — die Container-Simulation.

#![cfg(unix)]

#[path = "support/hook_witness.rs"]
mod support;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use minds_capture::witness_proto::{self, Frame};
use support::*;

/// Der Satz aus der Spezifikation, Wort für Wort.
const UNAVAILABLE: &str =
    "witness unavailable — witnessed sessions stay open, will be sealed at the next checkpoint";

/// Der post-commit-Hook der Agent-Seite: ruft den Checkpoint mit dem gerade
/// entstandenen Commit als Wächter — wie `minds enable` ihn schreibt, nur
/// ohne Umleitung nach `/dev/null`, damit der Test sieht, was er sagt.
fn install_post_commit(f: &Fixture) {
    let hooks = f.dir.path().join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    let script = hooks.join("post-commit");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nexec \"{}\" checkpoint --commit \"$(git rev-parse HEAD)\"\n",
            env!("CARGO_BIN_EXE_minds")
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.root,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    // Der Witness schreibt Store-Commits unter refs/minds/ und braucht dafür
    // eine Identität; auf dem Host kommt sie aus der Nutzerkonfiguration,
    // hier — mit leerer globaler Konfiguration — aus dem Repo.
    git(&f.root, &["config", "user.name", "Witness"]);
    git(
        &f.root,
        &["config", "user.email", "witness@example.invalid"],
    );
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = git_command(root).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn git_command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Agent")
        .env("GIT_AUTHOR_EMAIL", "agent@example.invalid")
        .env("GIT_COMMITTER_NAME", "Agent")
        .env("GIT_COMMITTER_EMAIL", "agent@example.invalid")
        .env_remove(SOCKET_ENV)
        .env_remove("MINDS_WITNESS_TIMEOUT_MS");
    cmd
}

/// Committet eine Datei; `env` landet beim post-commit-Hook. Git reicht die
/// Ausgabe des Hooks auf seinem stderr weiter.
fn commit(f: &Fixture, file: &str, env: &[(&str, &Path)]) -> Output {
    fs::write(f.root.join(file), format!("// {file}\n")).unwrap();
    git(&f.root, &["add", file]);
    let mut cmd = git_command(&f.root);
    cmd.args(["commit", "-q", "-m", &format!("feat: {file}")]);
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.output().unwrap()
}

fn head_trailers(f: &Fixture) -> Vec<String> {
    git(&f.root, &["log", "-1", "--format=%B"])
        .lines()
        .filter_map(|line| line.strip_prefix("Minds-Session-Id: "))
        .map(str::to_owned)
        .collect()
}

/// Die Session-Ids aus den „SESSION SEALED"-Zeilen des Witness.
fn witnessed_ids(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("SESSION SEALED "))
        .map(|rest| rest.split(' ').next().unwrap().to_owned())
        .collect()
}

/// Ein Event im lokalen Rückfall-Journal: ein Hook ohne Witness-Variable.
fn local_event(f: &Fixture, session: &str) {
    let stdin = f.payload(session, "UserPromptSubmit", r#","prompt":"lokal""#);
    let (out, _) = f.hook(None, stdin.as_bytes());
    assert_silent_success(&out);
    assert_eq!(events(&f.local_journal(), session).len(), 1);
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn checkpoint_delegates_to_witness() {
    let f = Fixture::new();
    install_post_commit(&f);
    let _daemon = f.start();
    let socket = f.socket();

    // Dieselbe Agent-Session, zwei Schreiber: zwei Events über den Witness,
    // eines (Witness kurz weg) im lokalen Rückfall-Journal.
    for event in ["UserPromptSubmit", "Stop"] {
        let stdin = f.payload("run", event, r#","prompt":"sortiere stabil""#);
        assert_silent_success(&f.hook(Some(&socket), stdin.as_bytes()).0);
    }
    wait_for_events(&f.witness_journal(), "run", 2);
    local_event(&f, "run");

    let out = commit(&f, "sort.rs", &[(SOCKET_ENV, &socket)]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");

    // Der Witness hat versiegelt und es über den Socket gesagt …
    assert!(
        said.contains("witness: 1 range(s) sealed"),
        "{said}\nwitness.log: {}",
        fs::read_to_string(f.home.join("log/witness.log")).unwrap_or_default()
    );
    assert!(said.contains("· scope witness/v1 ·"), "{said}");
    assert!(!said.contains(UNAVAILABLE), "{said}");
    // … und danach lief der lokale Pfad unverändert.
    assert!(said.contains("Scope      agent-hooks/v1"), "{said}");
    let witnessed = witnessed_ids(&said);
    assert_eq!(witnessed.len(), 1, "{said}");

    // Beide Bereiche hängen am Commit — zwei Ids, nichts zusammengeführt.
    let trailers = head_trailers(&f);
    assert_eq!(trailers.len(), 2, "{trailers:?}");
    assert_eq!(trailers[0], witnessed[0], "der Witness trailert zuerst");
    assert!(events(&f.local_journal(), "run").is_empty());
    assert!(
        f.witness_journal()
            .read(&minds_capture::SessionKey::new("claude-code", "run").unwrap())
            .map_or(true, |read| read.events.is_empty())
    );
    let ledger = fs::read_to_string(f.home.join("ledger")).unwrap();
    assert_eq!(ledger.lines().count(), 1, "{ledger}");
    assert!(ledger.contains(" witness/v1 "), "{ledger}");

    // Auch der Store allein kennt beide am Commit des Branches — nicht am
    // Zwischen-Commit, den der zweite Amend verwaist hat.
    {
        use minds_store::ContextStore;
        let head = git(&f.root, &["rev-parse", "HEAD"]);
        let index = minds_store::InRepoStore::open(&f.root)
            .unwrap()
            .index()
            .unwrap();
        let mut linked: Vec<String> = index
            .links_of(head.trim())
            .iter()
            .map(|link| link.session.to_string())
            .collect();
        linked.sort();
        let mut expected = trailers.clone();
        expected.sort();
        assert_eq!(linked, expected);
    }

    // `minds verify` findet die bezeugte Session am Commit.
    let verify = minds().current_dir(&f.root).arg("verify").output().unwrap();
    let report = text(&verify);
    assert!(
        report.contains(&format!("Session        {}", witnessed[0])),
        "{report}"
    );
    assert!(
        report.contains(&format!("Session        {}", trailers[1])),
        "{report}"
    );
    assert!(!report.contains("TAMPERED"), "{report}");
    assert!(matches!(verify.status.code(), Some(0 | 2)), "{report}");

    // Ein zweiter Checkpoint auf denselben Commit: beide Schreiber laufen,
    // keiner trailert doppelt, HEAD bleibt stehen.
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let again = minds()
        .current_dir(&f.root)
        .env(SOCKET_ENV, &socket)
        .args(["checkpoint", "--commit", head.trim()])
        .output()
        .unwrap();
    assert!(again.status.success(), "{}", text(&again));
    assert!(text(&again).contains("witness: nothing to seal"));
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), head);
    assert_eq!(head_trailers(&f), trailers);
}

#[test]
fn checkpoint_witness_down_falls_back() {
    let f = Fixture::new();
    install_post_commit(&f);

    // Witness gar nicht gestartet: Der Socket fehlt.
    local_event(&f, "down");
    let out = commit(&f, "a.rs", &[(SOCKET_ENV, &f.socket())]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains(UNAVAILABLE), "{said}");
    assert!(said.contains("Scope      agent-hooks/v1"), "{said}");
    assert!(!said.contains("witness/v1"), "{said}");
    assert_eq!(head_trailers(&f).len(), 1);
    assert!(
        events(&f.local_journal(), "down").is_empty(),
        "als A1 versiegelt"
    );
    let log = f.hook_log().unwrap_or_default();
    assert!(log.contains("witness unavailable: socket missing"), "{log}");

    // Ein Witness, der annimmt, aber nie antwortet: Die Frist hält.
    let run = f.dir.path().join("hang");
    fs::create_dir(&run).unwrap();
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
    let hanging = run.join("w.sock");
    let _listener = UnixListener::bind(&hanging).unwrap();
    local_event(&f, "hang");
    let started = Instant::now();
    let mut cmd = git_command(&f.root);
    fs::write(f.root.join("b.rs"), "// b\n").unwrap();
    git(&f.root, &["add", "b.rs"]);
    let out = cmd
        .args(["commit", "-q", "-m", "feat: b"])
        .env(SOCKET_ENV, &hanging)
        .env("MINDS_WITNESS_TIMEOUT_MS", "300")
        .output()
        .unwrap();
    let took = started.elapsed();
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains(UNAVAILABLE), "{said}");
    assert!(said.contains("Scope      agent-hooks/v1"), "{said}");
    assert!(took < Duration::from_secs(10), "zu lange: {took:?}");
    assert_eq!(head_trailers(&f).len(), 1);
    let log = f.hook_log().unwrap_or_default();
    assert!(log.contains("witness unavailable: timeout"), "{log}");
}

#[test]
fn checkpoint_witness_refusal_keeps_sessions_open() {
    // Der Witness lebt, kann aber nicht versiegeln (Schlüssel weg): `Nack`.
    // Die Agent-Seite fällt zurück; der Witness bleibt stehen, seine Session
    // bleibt offen und wird beim nächsten Checkpoint versiegelt.
    let f = Fixture::new();
    install_post_commit(&f);
    let mut daemon = f.start();
    let socket = f.socket();
    let stdin = f.payload("open", "UserPromptSubmit", r#","prompt":"x""#);
    assert_silent_success(&f.hook(Some(&socket), stdin.as_bytes()).0);
    wait_for_events(&f.witness_journal(), "open", 1);
    let key = f.home.join("key/witness_ed25519");
    let parked = f.dir.path().join("parked_key");
    fs::rename(&key, &parked).unwrap();

    let out = commit(&f, "a.rs", &[(SOCKET_ENV, &socket)]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains(UNAVAILABLE), "{said}");
    assert!(
        daemon.0.try_wait().unwrap().is_none(),
        "der Witness läuft weiter"
    );
    assert_eq!(wait_for_events(&f.witness_journal(), "open", 1).len(), 1);
    let log = f.hook_log().unwrap_or_default();
    assert!(log.contains("witness unavailable: refused"), "{log}");

    // Schlüssel zurück: Der nächste Checkpoint holt die Session nach — nach
    // dem Mindestabstand, den auch ein gescheiterter Lauf setzt.
    fs::rename(&parked, &key).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let out = minds()
        .current_dir(&f.root)
        .env(SOCKET_ENV, &socket)
        .args(["checkpoint", "--commit", head.trim()])
        .output()
        .unwrap();
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert_eq!(witnessed_ids(&said).len(), 1, "{said}");
    assert_eq!(head_trailers(&f), witnessed_ids(&said));
}

/// Stellt die Rechte eines gesperrten Verzeichnisses wieder her — auch wenn
/// der Test scheitert, sonst könnte `TempDir` nicht aufräumen.
struct Locked(PathBuf);
impl Locked {
    fn new(path: &Path) -> Self {
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
        Self(path.to_owned())
    }
}
impl Drop for Locked {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

/// Ein Ersatz-Witness auf einem Socket in einem eigenen 0700-Verzeichnis: Er
/// nimmt `connections` Verbindungen nacheinander an, liest je einen Frame und
/// beantwortet einen `CheckpointRequest` mit `status` — nachdem `before_ack`
/// gelaufen ist.
fn stand_in_witness(
    f: &Fixture,
    connections: usize,
    status: &'static str,
    before_ack: impl Fn() + Send + 'static,
) -> (PathBuf, std::thread::JoinHandle<Vec<Frame>>) {
    let run = f.dir.path().join("agent-run");
    fs::create_dir(&run).unwrap();
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = run.join("w.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let stub = std::thread::spawn(move || {
        let mut frames = Vec::new();
        for _ in 0..connections {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            let frame = loop {
                if let Ok((frame, _)) = witness_proto::decode(&bytes) {
                    break frame;
                }
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0, "Verbindung ohne vollständigen Frame");
                bytes.extend_from_slice(&chunk[..n]);
            };
            if let Frame::CheckpointRequest { request_id, .. } = &frame {
                before_ack();
                let ack = Frame::Ack {
                    request_id: *request_id,
                    status: status.into(),
                };
                stream
                    .write_all(&witness_proto::encode(&ack).unwrap())
                    .unwrap();
            }
            frames.push(frame);
        }
        frames
    });
    (socket, stub)
}

/// Namen, Größen und Änderungszeiten unter `dir` — um zu sehen, ob dort
/// jemand geschrieben hat.
fn snapshot(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            }
            out.push((entry.path(), meta.len(), meta.modified().unwrap()));
        }
    }
    out.sort();
    out
}

#[test]
fn agent_side_never_reads_witness_home() {
    // W1: Die Agent-Seite kennt den Socket, sonst nichts. Dafür sind das
    // Witness-Home und der Ort, an dem ein Default-Home läge
    // (`$XDG_STATE_HOME/minds-witness`), hier `chmod 000` — jeder Zugriff
    // scheiterte mit EACCES. Ein echter Witness kann so nicht laufen (er
    // teilt die UID des Tests und bräuchte sein Home); an seiner Stelle
    // beantwortet ein Ersatz auf einem Socket außerhalb des Homes die Anfrage.
    // Grenze des Tests: Ein Lesefehler, den die Agent-Seite still verschluckte,
    // fiele nur über ein verändertes Verhalten auf — deshalb prüft er alles
    // Beobachtbare: Ausgabe, Log, Trailer, die gesendeten Frames und dass im
    // Home nichts geschrieben wurde.
    // SAFETY: geteuid hat keine Vorbedingungen.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root ignores directory permissions");
        return;
    }
    let f = Fixture::new();
    install_post_commit(&f);
    let (socket, stub) = stand_in_witness(
        &f,
        2,
        "witness: 1 range(s) sealed | SESSION SEALED stub",
        || {},
    );

    let stdin = f.payload("seen", "UserPromptSubmit", r#","prompt":"x""#);
    assert_silent_success(&f.hook(Some(&socket), stdin.as_bytes()).0);
    local_event(&f, "fallback");

    let state = f.dir.path().join("state");
    fs::create_dir_all(state.join("minds-witness")).unwrap();
    let before = snapshot(&f.home);
    let out = {
        let _home = Locked::new(&f.home);
        let _default = Locked::new(&state.join("minds-witness"));
        let mut cmd = git_command(&f.root);
        fs::write(f.root.join("a.rs"), "// a\n").unwrap();
        git(&f.root, &["add", "a.rs"]);
        cmd.args(["commit", "-q", "-m", "feat: a"])
            .env(SOCKET_ENV, &socket)
            .env("MINDS_WITNESS_HOME", &f.home)
            .env("XDG_STATE_HOME", &state)
            .output()
            .unwrap()
    };
    assert_eq!(
        snapshot(&f.home),
        before,
        "im Witness-Home wurde geschrieben"
    );
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(!said.contains(UNAVAILABLE), "{said}");
    assert!(said.contains("SESSION SEALED stub"), "{said}");
    assert!(said.contains("Scope      agent-hooks/v1"), "{said}");
    assert!(!said.contains("ermission denied"), "{said}");
    let log = f.hook_log().unwrap_or_default();
    assert!(!log.contains("ermission denied"), "{log}");
    assert!(!log.contains("witness"), "{log}");

    let frames = stub.join().unwrap();
    assert!(matches!(&frames[0], Frame::Hook { .. }), "{frames:?}");
    assert_eq!(head_trailers(&f).len(), 1, "nur der lokale Bereich");
    // Angefragt wurde der frische Commit — der vor dem lokalen Trailer-Amend.
    let committed = git(&f.root, &["rev-parse", "HEAD@{1}"]);
    match &frames[1] {
        Frame::CheckpointRequest {
            commit: Some(commit),
            ..
        } => assert_eq!(commit, committed.trim()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn checkpoint_does_not_follow_head_to_a_new_commit() {
    // Gegenprobe zum Folgen des Witness-Amends: Steht HEAD nach der Antwort
    // auf einem wirklich neuen Commit (nicht nur mehr Trailer), trailert der
    // lokale Pfad dort nicht — der Wächter greift.
    let f = Fixture::new();
    install_post_commit(&f);
    let root = f.root.clone();
    let (socket, stub) = stand_in_witness(&f, 1, "witness: nothing to seal", move || {
        // Ohne Hooks, sonst liefe der Checkpoint rekursiv.
        let out = git_command(&root)
            .args(["-c", "core.hooksPath=/dev/null", "commit", "-q"])
            .args(["--allow-empty", "-m", "dazwischen"])
            .output()
            .unwrap();
        assert!(out.status.success());
    });
    local_event(&f, "guarded");

    let out = commit(&f, "a.rs", &[(SOCKET_ENV, &socket)]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    stub.join().unwrap();

    assert_eq!(
        git(&f.root, &["log", "-1", "--format=%s"]).trim(),
        "dazwischen"
    );
    assert!(
        head_trailers(&f).is_empty(),
        "kein Trailer am falschen Commit"
    );
    let log = f.hook_log().unwrap_or_default();
    assert!(log.contains("HEAD no longer points at"), "{log}");
}
