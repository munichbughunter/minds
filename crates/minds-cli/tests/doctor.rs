//! EA-10: `minds doctor` — Agent-Seite mit Isolationsprobe, Host-Seite.
#![cfg(unix)]

use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Fixture {
    /// Ein eingerichtetes Repo (Claude-Code-Hooks, Git-Hooks, Store-Config)
    /// und ein Witness-Home außerhalb davon.
    fn new() -> Self {
        // Kurze Pfade: sockaddr_un ist auch auf macOS auf 104 Bytes begrenzt.
        let dir = tempfile::Builder::new()
            .prefix("mdr-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = dir.path().join("repo");
        fs::create_dir(&root).unwrap();
        fs::create_dir(dir.path().join("user-home")).unwrap();
        git(&root, &["init", "-q", "--template="]);
        git(&root, &["config", "user.name", "Doctor Test"]);
        git(&root, &["config", "user.email", "doctor@example.invalid"]);
        let home = dir.path().join("home");
        let f = Self { dir, root, home };
        let out = f.minds(&["enable", "--agent", "claude-code"], &[]);
        assert!(out.status.success(), "{}", text(&out));
        for args in [vec!["witness", "init", "--repo"], vec!["witness", "keygen"]] {
            let mut command = f.command(&[]);
            command.args(&args);
            if args[1] == "init" {
                command.arg(&f.root);
            }
            let out = command.arg("--home").arg(&f.home).output().unwrap();
            assert!(out.status.success(), "{}", text(&out));
        }
        f
    }

    fn command(&self, env: &[(&str, &Path)]) -> Command {
        let mut command = Command::new(MINDS);
        command
            .current_dir(&self.root)
            .env("HOME", self.dir.path().join("user-home"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }

    fn minds(&self, args: &[&str], env: &[(&str, &Path)]) -> Output {
        self.command(env).args(args).output().unwrap()
    }

    fn socket(&self) -> PathBuf {
        self.home.join("run/witness.sock")
    }

    fn start(&self) -> Daemon {
        let child = self
            .command(&[])
            .args(["witness", "run", "--home"])
            .arg(&self.home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let daemon = Daemon(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        // Bereit ist der Witness erst, wenn er antwortet — verbinden lässt
        // sich schon, bevor seine Eventloop läuft.
        while !pong(&self.socket()) {
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        daemon
    }
}

fn pong(socket: &Path) -> bool {
    use minds_capture::witness_proto::{self, Frame};
    use std::io::{Read, Write};
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if stream
        .write_all(&witness_proto::encode(&Frame::Ping).unwrap())
        .is_err()
    {
        return false;
    }
    let mut bytes = Vec::new();
    let mut chunk = [0; 256];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
        }
        match witness_proto::decode(&bytes) {
            Ok((Frame::Ack { status, .. }, _)) => return status == "pong",
            Err(witness_proto::ProtoError::Incomplete) => {}
            _ => return false,
        }
    }
}

fn line<'a>(out: &'a str, name: &str) -> &'a str {
    out.lines()
        .find(|line| line[6.min(line.len())..].starts_with(&format!("{name}:")))
        .unwrap_or_else(|| panic!("no {name} line in:\n{out}"))
}

/// AC: Auf der (simulierten) Agent-Seite meldet die Isolationsprobe `ok`,
/// solange das Home nicht erreichbar ist; ein lesbares Home ist `fail` mit
/// Exit 1.
#[test]
fn doctor_isolation_probe_ok_and_fail() {
    let f = Fixture::new();
    let _daemon = f.start();
    let socket = f.socket();
    let agent = [("MINDS_WITNESS_SOCKET", socket.as_path())];

    // Die simulierte Agent-Seite erreicht das Home nicht: Es liegt da, aber
    // ohne Rechte für diese Kennung (wie im `user`-Profil). Das ist überall
    // ein Nachweis — anders als „fehlt", das ohne Mount-Tabelle (macOS)
    // nichts über einen anderen Mount-Namensraum beweist.
    // SAFETY: geteuid hat keine Vorbedingungen.
    let root_user = unsafe { libc::geteuid() } == 0;
    let closed = f.dir.path().join("closed-home");
    fs::create_dir(&closed).unwrap();
    fs::write(closed.join("witness.json"), "{}").unwrap();
    fs::set_permissions(&closed, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let out = f.minds(
        &["doctor", "--probe-home", closed.to_str().unwrap()],
        &agent,
    );
    fs::set_permissions(&closed, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if root_user {
        // root öffnet jedes Verzeichnis; der ok-Nachweis gilt der
        // Agent-Kennung und lässt sich als root nicht zeigen.
        eprintln!("skipped: the EACCES ok-proof cannot be shown when running as root");
    } else {
        assert!(out.status.success(), "{}", text(&out));
        assert!(line(&stdout, "isolation").starts_with("ok "), "{stdout}");
        assert!(
            line(&stdout, "isolation").contains("permission denied"),
            "{stdout}"
        );
    }

    // Im Container gibt es das Home schlicht nicht. Das ist kein Nachweis —
    // die Mount-Tabelle ist nur eine Heuristik —, also ehrlich warn, nie
    // fail und nie ein falsches ok.
    let absent = f.dir.path().join("not-mounted");
    let out = f.minds(
        &["doctor", "--probe-home", absent.to_str().unwrap()],
        &agent,
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{}", text(&out));
    assert!(line(&stdout, "isolation").starts_with("warn"), "{stdout}");
    assert!(
        line(&stdout, "witness socket").starts_with("ok "),
        "{stdout}"
    );
    assert!(line(&stdout, "witness").starts_with("ok "), "{stdout}");
    assert!(line(&stdout, "agent hooks").starts_with("ok "), "{stdout}");
    assert!(line(&stdout, "git hooks").starts_with("ok "), "{stdout}");
    assert!(line(&stdout, "store").starts_with("ok "), "{stdout}");

    // Dieselbe Kennung wie der Witness: Das Home ist lesbar — fail.
    let out = f.minds(
        &["doctor", "--probe-home", f.home.to_str().unwrap()],
        &agent,
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(line(&stdout, "isolation").starts_with("fail"), "{stdout}");
    assert!(
        line(&stdout, "isolation").contains("agent can reach the witness home"),
        "{stdout}"
    );
}

/// Agent-Seite ohne laufenden Witness: Der Socket antwortet nicht — fail.
#[test]
fn doctor_fails_when_the_witness_does_not_answer() {
    let f = Fixture::new();
    let socket = f.socket();
    let absent = f.dir.path().join("not-mounted");
    let out = f.minds(
        &["doctor", "--probe-home", absent.to_str().unwrap()],
        &[("MINDS_WITNESS_SOCKET", socket.as_path())],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(line(&stdout, "witness").starts_with("fail"), "{stdout}");
    // Der Exit-Code kommt allein vom Ping: Die Probe auf ein fehlendes Home
    // ist warn — kein Nachweis, aber auch kein Fehler.
    assert!(line(&stdout, "isolation").starts_with("warn"), "{stdout}");
}

/// `--probe-home` ohne Socket: Die Agent-Seite ist nicht verdrahtet.
#[test]
fn doctor_fails_without_the_socket_variable_on_the_agent_side() {
    let f = Fixture::new();
    let absent = f.dir.path().join("not-mounted");
    let out = f.minds(&["doctor", "--probe-home", absent.to_str().unwrap()], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        line(&stdout, "witness socket").contains("MINDS_WITNESS_SOCKET is not set"),
        "{stdout}"
    );
}

/// Die Host-Seite: Profil aus `witness.json`, läuft der Witness, Schlüssel
/// mit 0600.
#[test]
fn doctor_reports_the_host_side() {
    let f = Fixture::new();
    let home = [("MINDS_WITNESS_HOME", f.home.as_path())];

    let out = f.minds(&["doctor"], &home);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        line(&stdout, "witness profile").starts_with("ok "),
        "{stdout}"
    );
    assert!(
        line(&stdout, "witness profile").contains("user"),
        "{stdout}"
    );
    assert!(
        line(&stdout, "witness").starts_with("fail"),
        "not running: {stdout}"
    );
    assert!(
        line(&stdout, "witness key").starts_with("ok    witness key: present, 0600"),
        "{stdout}"
    );

    let _daemon = f.start();
    let out = f.minds(&["doctor"], &home);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{}", text(&out));
    assert!(line(&stdout, "witness").starts_with("ok "), "{stdout}");
}

/// Ohne Witness: ein Hinweis, kein Fehler.
#[test]
fn doctor_without_a_witness_warns() {
    let f = Fixture::new();
    let out = f.minds(&["doctor"], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{}", text(&out));
    assert!(line(&stdout, "witness").starts_with("warn"), "{stdout}");
}

/// Fehlende Git-Hooks sind `fail`.
#[test]
fn doctor_fails_on_missing_git_hooks() {
    let f = Fixture::new();
    fs::remove_file(f.root.join(".git/hooks/post-commit")).unwrap();
    let out = f.minds(&["doctor"], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(line(&stdout, "git hooks").starts_with("fail"), "{stdout}");
    assert!(
        line(&stdout, "git hooks").contains("post-commit"),
        "{stdout}"
    );
}
