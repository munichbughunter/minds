//! Ein Witness-Dienst je Rechner: `minds witness serve` hält je angemeldetem
//! Repo einen Witness und nimmt Registrierungen über den Steuer-Socket an.
#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
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
    /// `SIGTERM`, nicht `kill`: Der Dienst beendet dann seine Witnesses —
    /// nach `SIGKILL` liefen sie als Waisen weiter.
    fn drop(&mut self) {
        if let Ok(pid) = libc::pid_t::try_from(self.0.id()) {
            // SAFETY: kill mit einer Kind-Pid und einem Standardsignal.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.0.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    base: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Kurze Pfade: sockaddr_un ist auch auf macOS auf 104 Bytes begrenzt.
        let dir = tempfile::Builder::new()
            .prefix("mh-")
            .tempdir_in("/tmp")
            .unwrap();
        let base = dir.path().join("base");
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir, base }
    }

    /// Ein Repo mit einem Commit — der Witness pinnt die Policy aus HEAD.
    fn repo(&self, name: &str) -> PathBuf {
        let root = self.dir.path().join(name);
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "--template="]);
        git(&root, &["config", "user.name", "Host Test"]);
        git(&root, &["config", "user.email", "host@example.invalid"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        git(&root, &["commit", "-qm", "initial", "--allow-empty"]);
        root.canonicalize().unwrap()
    }

    fn command(&self) -> Command {
        let mut minds = Command::new(MINDS);
        minds
            .env("MINDS_WITNESS", "off")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET")
            // Nie der echte Ort geschützter Witnesses auf diesem Rechner.
            .env("MINDS_PROTECTED_BASE", &self.base)
            .env("HOME", self.dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null");
        minds
    }

    fn serve(&self) -> Daemon {
        let child = self
            .command()
            .args(["witness", "serve", "--base"])
            .arg(&self.base)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut daemon = Daemon(child);
        let socket = self.base.join("run/control.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pong(&socket) {
            if let Some(status) = daemon.0.try_wait().unwrap() {
                let mut error = String::new();
                daemon
                    .0
                    .stderr
                    .as_mut()
                    .unwrap()
                    .read_to_string(&mut error)
                    .unwrap();
                panic!("host daemon exited {status}: {error}");
            }
            assert!(Instant::now() < deadline, "host daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    fn repo_id(root: &Path) -> String {
        blake3::hash(root.as_os_str().as_encoded_bytes()).to_hex()[..16].to_owned()
    }
}

/// `Ping` → `Ack pong` über den Steuer-Socket.
fn pong(socket: &Path) -> bool {
    use minds_capture::witness_proto::{self, Frame};
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(&witness_proto::encode(&Frame::Ping).unwrap())
        .is_err()
    {
        return false;
    }
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
        }
        match witness_proto::decode(&bytes) {
            Ok((Frame::Ack { status, .. }, _)) => return status == "pong",
            Ok(_) => return false,
            Err(witness_proto::ProtoError::Incomplete) => {}
            Err(_) => return false,
        }
    }
}

/// Anmelden richtet Home und Schlüssel ein und startet den Witness des
/// Repos; ein zweites Anmelden ist idempotent (`running`); Abmelden hält
/// ihn an und entfernt das Home. Ein weiteres Repo bekommt seinen eigenen
/// Witness neben dem ersten.
#[test]
fn the_host_daemon_registers_runs_and_unregisters_repositories() {
    let f = Fixture::new();
    let _daemon = f.serve();
    let a = f.repo("a");
    let id = Fixture::repo_id(&a);
    let out = f
        .command()
        .args(["witness", "register", "--repo"])
        .arg(&a)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        stdout.starts_with(&format!("registered {id}\n")),
        "{stdout}"
    );
    assert!(
        stdout.contains("signers minds-witness@")
            && stdout.contains("namespaces=\"minds-witness\""),
        "{stdout}"
    );
    assert!(stdout.contains("answering yes"), "{stdout}");
    let home = f.base.join(&id);
    assert!(home.join("witness.json").is_file());
    assert!(home.join("key/witness_ed25519").is_file());
    let socket = home.join("run/witness.sock");
    assert!(pong(&socket), "the repository's witness answers");
    // Idempotent: derselbe Witness läuft weiter, dieselbe Signer-Zeile.
    let again = f
        .command()
        .args(["witness", "register", "--repo"])
        .arg(&a)
        .output()
        .unwrap();
    let again_out = String::from_utf8_lossy(&again.stdout);
    assert!(again.status.success(), "{}", text(&again));
    assert!(
        again_out.starts_with(&format!("running {id}\n")),
        "{again_out}"
    );
    let signers = |text: &str| {
        text.lines()
            .find(|line| line.starts_with("signers "))
            .map(str::to_owned)
    };
    assert_eq!(signers(&stdout), signers(&again_out));
    // Ein zweites Repo, ein zweiter Witness.
    let b = f.repo("b");
    let out = f
        .command()
        .args(["witness", "register", "--repo"])
        .arg(&b)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(pong(
        &f.base.join(Fixture::repo_id(&b)).join("run/witness.sock")
    ));
    assert!(pong(&socket), "the first witness keeps running");
    // Kein Repo: abgelehnt, nichts angelegt.
    let out = f
        .command()
        .args(["witness", "register", "--repo"])
        .arg(f.dir.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    // Abmelden (als derselbe Nutzer, unter dem der Dienst läuft).
    let out = f
        .command()
        .args(["witness", "unregister", "--repo"])
        .arg(&a)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        stdout.starts_with(&format!("unregistered {id}\n")),
        "{stdout}"
    );
    assert!(!home.exists(), "home removed");
    let deadline = Instant::now() + Duration::from_secs(5);
    while pong(&socket) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!pong(&socket), "the witness of a is stopped");
    assert!(pong(
        &f.base.join(Fixture::repo_id(&b)).join("run/witness.sock")
    ));
}

/// Ein Home, das jemand anderes anlegt (die Einrichtung mit `sudo`), nimmt
/// der Dienst beim nächsten Durchgang auf; `minds doctor` nennt den Dienst.
#[test]
fn the_host_daemon_adopts_homes_and_doctor_names_it() {
    let f = Fixture::new();
    let a = f.repo("a");
    let id = Fixture::repo_id(&a);
    let home = f.base.join(&id);
    for args in [vec!["witness", "init", "--repo"], vec!["witness", "keygen"]] {
        let mut command = f.command();
        command.args(&args);
        if args[1] == "init" {
            command.arg(&a);
        }
        let out = command.arg("--home").arg(&home).output().unwrap();
        assert!(out.status.success(), "{}", text(&out));
    }
    let _daemon = f.serve();
    let socket = home.join("run/witness.sock");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !pong(&socket) {
        assert!(Instant::now() < deadline, "home not adopted");
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = f
        .command()
        .current_dir(&a)
        .args(["doctor"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|line| line.contains("witness daemon:"))
        .unwrap_or_else(|| panic!("no witness daemon line:\n{stdout}"));
    assert!(line.starts_with("ok"), "{line}");
    assert!(
        line.contains("serving 1 repositories (1 running)"),
        "{line}"
    );
}
