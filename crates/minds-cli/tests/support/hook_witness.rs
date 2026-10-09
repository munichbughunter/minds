//! Gemeinsames Gerüst für die Tests von `minds hook` mit Witness (EA-07):
//! ein Repo, ein Witness-Home, ein laufender Daemon und ein Hook-Aufruf, der
//! so startet, wie ein Agent ihn startet.

#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use minds_capture::witness_proto::{self, Frame};
use minds_capture::{Journal, JournalEvent, SessionKey};

pub const SOCKET_ENV: &str = "MINDS_WITNESS_SOCKET";

pub fn minds() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_minds"));
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        // Ein leeres Home: `minds verify` liest ohne `--signers`
        // `~/.ssh/allowed_signers` — die Datei des Entwicklers darf das
        // Ergebnis eines Tests nicht bestimmen (in CI gibt es sie nie).
        .env("HOME", isolated_home())
        .env_remove(SOCKET_ENV);
    cmd
}

/// Ein leeres, für alle Tests gemeinsames Home ohne `.ssh`.
fn isolated_home() -> PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("isolated-home");
    fs::create_dir_all(&home).unwrap();
    home
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub home: PathBuf,
    pub root: PathBuf,
}

pub struct Daemon(pub Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Fixture {
    /// Ein Repo ohne Witness-Home — für die Fälle, die keinen Daemon brauchen.
    pub fn repo_only() -> Self {
        // Kurze Pfade: sockaddr_un ist auch auf macOS auf 104 Bytes begrenzt.
        let dir = tempfile::Builder::new()
            .prefix("mh-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = dir.path().join("repo");
        fs::create_dir(&root).unwrap();
        let status = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q", "--template="])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
        let home = dir.path().join("home");
        Self { dir, home, root }
    }

    /// Repo plus initialisiertes Witness-Home mit Schlüssel.
    pub fn new() -> Self {
        let f = Self::repo_only();
        for args in [
            vec!["witness", "init", "--repo", f.root.to_str().unwrap()],
            vec!["witness", "keygen"],
        ] {
            let out = minds()
                .args(&args)
                .arg("--home")
                .arg(&f.home)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        f
    }

    pub fn socket(&self) -> PathBuf {
        self.home.join("run/witness.sock")
    }

    pub fn start(&self) -> Daemon {
        let child = minds()
            .args(["witness", "run", "--home"])
            .arg(&self.home)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut daemon = Daemon(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(mut stream) = UnixStream::connect(self.socket()) {
                stream
                    .set_read_timeout(Some(Duration::from_millis(200)))
                    .unwrap();
                if ping(&mut stream).is_ok() {
                    break;
                }
            }
            if let Some(status) = daemon.0.try_wait().unwrap() {
                let mut error = String::new();
                let _ = daemon.0.stderr.as_mut().unwrap().read_to_string(&mut error);
                panic!("daemon exited {status}: {error}");
            }
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        daemon
    }

    pub fn witness_journal(&self) -> Journal {
        Journal::at(self.home.join("journal"))
    }

    pub fn local_journal_dir(&self) -> PathBuf {
        self.root.join(".git/minds/journal")
    }

    pub fn local_journal(&self) -> Journal {
        Journal::at(self.local_journal_dir())
    }

    pub fn hook_log(&self) -> Option<String> {
        fs::read_to_string(self.root.join(".git/minds/hook.log")).ok()
    }

    /// Ein Payload, wie Claude Code ihn schickt, mit `cwd` auf dieses Repo.
    pub fn payload(&self, session: &str, event: &str, extra: &str) -> String {
        format!(
            r#"{{"session_id":"{session}","cwd":"{}","hook_event_name":"{event}"{extra}}}"#,
            self.root.display()
        )
    }

    /// Startet `minds hook` im Repo, schreibt `stdin` und wartet auf das Ende.
    /// `socket = None` heißt: Variable nicht gesetzt.
    pub fn hook(&self, socket: Option<&Path>, stdin: &[u8]) -> (Output, Duration) {
        self.hook_as("claude-code", socket, stdin)
    }

    /// Wie [`Self::hook`], mit frei gewähltem `--agent`.
    pub fn hook_as(&self, agent: &str, socket: Option<&Path>, stdin: &[u8]) -> (Output, Duration) {
        let mut cmd = minds();
        cmd.current_dir(&self.root)
            .args(["hook", "--agent", agent])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(socket) = socket {
            cmd.env(SOCKET_ENV, socket);
        }
        let started = Instant::now();
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        (out, started.elapsed())
    }
}

/// Die Zusage des Hooks in jedem Pfad: Exit 0, kein Byte auf stdout oder stderr.
pub fn assert_silent_success(out: &Output) {
    assert!(out.status.success(), "der Hook endet immer mit 0");
    assert!(
        out.stdout.is_empty(),
        "stdout: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn ping(stream: &mut UnixStream) -> std::io::Result<()> {
    stream.write_all(&witness_proto::encode(&Frame::Ping).unwrap())?;
    let mut header = [0; 12];
    stream.read_exact(&mut header)?;
    let size = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    assert!(size < 4096);
    let mut bytes = header.to_vec();
    bytes.resize(12 + size, 0);
    stream.read_exact(&mut bytes[12..])?;
    assert!(matches!(
        witness_proto::decode(&bytes).unwrap().0,
        Frame::Ack { .. }
    ));
    Ok(())
}

/// Wartet, bis `journal` für `session` mindestens `count` Events trägt. Der
/// Hook wartet auf keine Antwort — der Test muss es tun.
pub fn wait_for_events(journal: &Journal, session: &str, count: usize) -> Vec<JournalEvent> {
    let key = SessionKey::new("claude-code", session).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(read) = journal.read(&key) {
            if read.events.len() >= count {
                return read.events;
            }
        }
        assert!(
            Instant::now() < deadline,
            "event never arrived at {session}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Die Events einer Session im lokalen Journal, oder keine.
pub fn events(journal: &Journal, session: &str) -> Vec<JournalEvent> {
    let key = SessionKey::new("claude-code", session).unwrap();
    journal
        .read(&key)
        .map(|read| read.events)
        .unwrap_or_default()
}
