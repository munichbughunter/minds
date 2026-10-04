#![cfg(unix)]

use minds_capture::witness_proto::{self, Frame};
use minds_capture::{Journal, SessionKey};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
}
struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn minds() -> Command {
    Command::new(env!("CARGO_BIN_EXE_minds"))
}
fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap()
            .status
            .success()
    );
}
impl Fixture {
    fn new() -> Self {
        // Kurze Pfade: sockaddr_un ist auch auf macOS auf 104 Bytes begrenzt.
        let dir = tempfile::Builder::new()
            .prefix("mw-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = dir.path().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "--template="]);
        let home = dir.path().join("home");
        let out = minds()
            .args(["witness", "init", "--repo"])
            .arg(&root)
            .arg("--home")
            .arg(&home)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = minds()
            .args(["witness", "keygen", "--home"])
            .arg(&home)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Self {
            _dir: dir,
            home,
            root,
        }
    }
    fn socket(&self) -> PathBuf {
        self.home.join("run/witness.sock")
    }
    fn start(&self) -> Daemon {
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
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                if barrier(&mut stream).is_ok() {
                    break;
                }
            }
            if let Some(status) = daemon.0.try_wait().unwrap() {
                let mut error = String::new();
                daemon
                    .0
                    .stderr
                    .as_mut()
                    .unwrap()
                    .read_to_string(&mut error)
                    .unwrap();
                panic!("daemon exited {status}: {error}");
            }
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        daemon
    }
    fn connect(&self) -> UnixStream {
        let stream = UnixStream::connect(self.socket()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        stream
    }
    fn journal(&self) -> Journal {
        Journal::at(self.home.join("journal"))
    }
}
fn send(stream: &mut UnixStream, frame: &Frame) {
    stream
        .write_all(&witness_proto::encode(frame).unwrap())
        .unwrap();
}
fn barrier(stream: &mut UnixStream) -> std::io::Result<()> {
    stream.write_all(&witness_proto::encode(&Frame::Ping).unwrap())?;
    let mut header = [0; 12];
    stream.read_exact(&mut header)?;
    let size = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    assert!(size < 4096);
    let mut bytes = header.to_vec();
    bytes.resize(12 + size, 0);
    stream.read_exact(&mut bytes[12..])?;
    assert_eq!(
        witness_proto::decode(&bytes).unwrap().0,
        Frame::Ack {
            request_id: [0; 16],
            status: "pong".into()
        }
    );
    Ok(())
}
fn hook(n: usize) -> Frame {
    Frame::Hook { agent: "claude-code".into(), event_override: None, stdin: format!(r#"{{"session_id":"concurrent","hook_event_name":"UserPromptSubmit","prompt":"event-{n}"}}"#).into_bytes() }
}
fn stop(daemon: &mut Daemon) {
    // SAFETY: Der Test signalisiert ausschließlich seinen eigenen Kindprozess.
    assert_eq!(
        unsafe { libc::kill(daemon.0.id() as i32, libc::SIGTERM) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn witness_serializes_concurrent_clients() {
    let f = Fixture::new();
    let mut daemon = f.start();
    let clients: Vec<_> = (0..8)
        .map(|thread| {
            let mut stream = f.connect();
            std::thread::spawn(move || {
                for event in 0..125 {
                    send(&mut stream, &hook(thread * 125 + event));
                }
                barrier(&mut stream).unwrap();
            })
        })
        .collect();
    for client in clients {
        client.join().unwrap();
    }
    let read = f
        .journal()
        .read(&SessionKey::new("claude-code", "concurrent").unwrap())
        .unwrap();
    assert!(read.is_complete());
    assert_eq!(read.events.len(), 1000);
    for (seq, event) in read.events.iter().enumerate() {
        assert_eq!(event.seq, seq as u64);
    }
    let payloads: std::collections::BTreeSet<_> = read
        .events
        .iter()
        .map(|event| event.payload.get())
        .collect();
    assert_eq!(payloads.len(), 1000);
    stop(&mut daemon);
}
#[test]
fn witness_survives_kill_between_events_and_records_unclean_restart() {
    let f = Fixture::new();
    let mut daemon = f.start();
    let mut stream = f.connect();
    send(&mut stream, &hook(0));
    barrier(&mut stream).unwrap();
    daemon.0.kill().unwrap();
    daemon.0.wait().unwrap();
    drop(stream);
    let mut daemon = f.start();
    let mut stream = f.connect();
    send(&mut stream, &hook(1));
    barrier(&mut stream).unwrap();
    let key = SessionKey::new("claude-code", "concurrent").unwrap();
    let read = f.journal().read(&key).unwrap();
    assert_eq!(read.events.len(), 2);
    let epochs = minds_capture::epoch::EpochState::at(f.home.join("evidence/state"));
    let batch = minds_capture::chain::chain_salted(&epochs.salt(&key).unwrap(), &read);
    let digest = blake3::hash(b"claude-code\0concurrent");
    let state = fs::read(
        f.home
            .join("evidence/folders")
            .join(format!("{}.json", digest.to_hex())),
    )
    .unwrap();
    let folder =
        minds_core::evidence::ChainFolder::from_state(serde_json::from_slice(&state).unwrap());
    assert_eq!(folder.snapshot(), batch);
    let starts: Vec<_> = f
        .journal()
        .sessions()
        .unwrap()
        .keys
        .iter()
        .filter(|key| key.agent() == "witness")
        .flat_map(|key| f.journal().read(key).unwrap().events)
        .filter(|event| event.raw_kind == "witness.start")
        .collect();
    assert!(starts.iter().any(|event| {
        event
            .payload
            .get()
            .contains("\"previous_stop\":\"unclean\"")
    }));
    stop(&mut daemon);
    let mut daemon = f.start();
    let starts: Vec<_> = f
        .journal()
        .sessions()
        .unwrap()
        .keys
        .iter()
        .filter(|key| key.agent() == "witness")
        .flat_map(|key| f.journal().read(key).unwrap().events)
        .collect();
    assert!(
        starts
            .iter()
            .any(|event| event.payload.get().contains("\"previous_stop\":\"clean\""))
    );
    stop(&mut daemon);
}
#[test]
fn witness_ignores_malformed_frames_and_idle_clients() {
    let f = Fixture::new();
    let mut daemon = f.start();
    let _idle = f.connect();
    for bytes in [
        b"garbage PAYLOAD_SENTINEL".to_vec(),
        [witness_proto::MAGIC.as_slice(), &u32::MAX.to_le_bytes()].concat(),
        b"MWIT".to_vec(),
    ] {
        let mut stream = f.connect();
        stream.write_all(&bytes).unwrap();
    }
    let mut stream = f.connect();
    send(
        &mut stream,
        &Frame::Hook {
            agent: "claude-code".into(),
            event_override: None,
            stdin: b"PAYLOAD_SENTINEL".to_vec(),
        },
    );
    send(&mut stream, &hook(0));
    barrier(&mut stream).unwrap();
    assert_eq!(
        f.journal()
            .read(&SessionKey::new("claude-code", "concurrent").unwrap())
            .unwrap()
            .events
            .len(),
        1
    );
    assert!(
        !fs::read_to_string(f.home.join("log/witness.log"))
            .unwrap()
            .contains("PAYLOAD_SENTINEL")
    );
    assert!(daemon.0.try_wait().unwrap().is_none());
    stop(&mut daemon);
}
#[test]
fn witness_status_permissions_and_duplicate_daemon() {
    let f = Fixture::new();
    let mut daemon = f.start();
    assert_eq!(
        fs::metadata(f.socket()).unwrap().permissions().mode() & 0o777,
        0o660
    );
    let duplicate = minds()
        .args(["witness", "run", "--home"])
        .arg(&f.home)
        .output()
        .unwrap();
    assert_eq!(duplicate.status.code(), Some(4));
    assert!(f.socket().exists());
    let status = minds()
        .args(["witness", "status", "--home"])
        .arg(&f.home)
        .output()
        .unwrap();
    assert!(status.status.success());
    let text = String::from_utf8(status.stdout).unwrap();
    assert!(text.contains("Profile: user\n"));
    assert!(text.contains("(running)"));
    assert!(text.contains("Open sessions: 0\n"));
    assert!(text.contains("Key: SHA256:"));
    stop(&mut daemon);
    assert!(!f.socket().exists());
    assert!(!f.root.join(".git/minds/journal").exists());
}
#[test]
fn witness_run_refuses_insecure_and_symlink_homes() {
    let f = Fixture::new();
    fs::set_permissions(&f.home, fs::Permissions::from_mode(0o770)).unwrap();
    let output = minds()
        .args(["witness", "run", "--home"])
        .arg(&f.home)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    fs::set_permissions(&f.home, fs::Permissions::from_mode(0o700)).unwrap();
    let link = f._dir.path().join("symlink");
    std::os::unix::fs::symlink(&f.home, &link).unwrap();
    let output = minds()
        .args(["witness", "run", "--home"])
        .arg(format!("{}/", link.display()))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(!f.socket().exists());
}
