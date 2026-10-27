//! Ein Witness-Dienst je Rechner (`minds witness serve`).
//!
//! Der geschützte Witness läuft als **ein** Systemdienst (`dev.minds.witness`
//! unter macOS, `minds-witness.service` unter Linux) unter dem
//! Witness-Nutzer. Je Repository hält er einen eigenen Witness — `minds
//! witness run --home <base>/<repo-id>` als Kindprozess desselben Binaries —,
//! denn ein Witness besitzt genau ein Repository und genau einen Schreiber
//! ([`super`]). Der Dienst selbst schreibt kein Journal; er nimmt über den
//! Steuer-Socket des Rechners (`<base>/run/control.sock`, erreichbar für die
//! Gruppe des Witness) Registrierungen an, richtet das Home ein
//! ([`init_config`], `keygen`), startet den Witness des Repos und hält ihn
//! am Laufen. So braucht `minds enable` auf jedem weiteren Repo keine
//! Erhöhung: Das Repo für die Gruppe teilen kann der Entwickler selbst, den
//! Rest tut der Dienst als Witness-Nutzer.
//!
//! Was der Socket nicht hergibt: einen Witness **abmelden** darf nur der
//! Witness-Nutzer selbst (`minds disable` ruft das mit der einen Erhöhung
//! auf) — sonst könnte der Agent, der die Kennung des Entwicklers teilt, das
//! Journal eines Repos vor dem Seal löschen. Intents aktiviert weiterhin
//! nur der Steuer-Socket des jeweiligen Repos.
//!
//! Homes, die ein anderer anlegt (die Einrichtung mit `sudo` legt das erste
//! Home als Witness-Nutzer selbst an), nimmt der Dienst beim nächsten
//! Durchgang auf ([`RESCAN`]).

use super::socket::{STOP, Signals, Socket, peer_uid};
use super::*;
use minds_capture::witness_proto::{self, Frame, ProtoError};
use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::process::Child;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Der Steuer-Socket des Rechners, relativ zur Basis.
pub(crate) const HOST_SOCKET: &str = "run/control.sock";

/// Wie oft die Basis nach neuen Homes durchgesehen wird.
const RESCAN: Duration = Duration::from_secs(5);
/// Höchstens so lange wartet der Dienst auf einen Witness, der antwortet.
const START_WAIT: Duration = Duration::from_secs(10);
/// So lange darf ein Kind nach `SIGTERM` brauchen, bevor es `SIGKILL` bekommt.
const STOP_WAIT: Duration = Duration::from_secs(5);
/// Ein Kind, das länger lief, hatte keinen Startfehler — der Rückstau beginnt
/// von vorn.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// Längste Pause zwischen zwei Neustarts eines scheiternden Kindes.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Höchstens so viele Bytes nimmt der Dienst von einer Verbindung an.
const MAX_REQUEST: usize = 64 * 1024;
/// Frist je Verbindung — eine Registrierung ist ein kurzer Austausch.
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// Trennzeichen der Teile einer `Ack`-Statuszeile (die Zeile ist einzeilig).
pub(crate) const PART: &str = " · ";

/// Ein Witness, den der Dienst hält.
struct Served {
    home: PathBuf,
    child: Option<Child>,
    started: Option<Instant>,
    /// Aufeinanderfolgende kurze Läufe — bestimmt den Rückstau.
    failures: u32,
}

impl Served {
    fn new(home: PathBuf) -> Self {
        Self {
            home,
            child: None,
            started: None,
            failures: 0,
        }
    }

    fn running(&mut self) -> bool {
        matches!(self.child.as_mut().map(Child::try_wait), Some(Ok(None)))
    }
}

/// Der Dienst: hält je Home unter `base` einen Witness und bedient den
/// Steuer-Socket des Rechners, bis `SIGTERM`/`SIGINT` kommt.
pub fn serve(base: &Path) -> Fallible<()> {
    let base = base.canonicalize()?;
    let _signals = Signals::install()?;
    // SAFETY: geteuid hat keine Vorbedingungen.
    let euid = unsafe { libc::geteuid() };
    // Die Gruppe nur, wenn dieser Prozess der Witness-Nutzer ist — sonst
    // (ein Test, ein Lauf unter eigener Kennung) bleibt der Socket privat.
    let group = witness_group().filter(|_| crate::enable_protected::witness_uid() == Some(euid));
    let run = base.join("run");
    match fs::symlink_metadata(&run) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err("the run directory of the witness base is not a directory".into()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&run)?,
        Err(err) => return Err(err.into()),
    }
    let mode = if group.is_some() { 0o710 } else { 0o700 };
    fs::set_permissions(&run, fs::Permissions::from_mode(mode))?;
    if let Some(group) = group {
        let name = std::ffi::CString::new(run.as_os_str().as_encoded_bytes())?;
        // SAFETY: nul-terminierter Pfad; uid=-1 lässt den Eigentümer stehen.
        if unsafe { libc::chown(name.as_ptr(), !0, group) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let socket = Socket::bind_at(
        base.join(HOST_SOCKET),
        group,
        if group.is_some() { 0o660 } else { 0o600 },
    )?;
    eprintln!("witness host: serving from {}", base.display());
    let mut served: BTreeMap<String, Served> = BTreeMap::new();
    let mut last_scan: Option<Instant> = None;
    while !STOP.load(Ordering::Relaxed) {
        if last_scan.is_none_or(|at| at.elapsed() >= RESCAN) {
            adopt(&base, &mut served);
            last_scan = Some(Instant::now());
        }
        supervise(&mut served);
        match socket.listener.accept() {
            Ok((stream, _)) => handle(stream, &base, &mut served, euid),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => eprintln!("witness host: accept failed: {err}"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("witness host: stopping {} witness(es)", served.len());
    for entry in served.values_mut() {
        stop_child(entry);
    }
    Ok(())
}

/// Die Gruppe des Witness-Nutzers, falls es sie gibt.
fn witness_group() -> Option<u32> {
    let name = std::ffi::CString::new(crate::enable_protected::WITNESS_USER).ok()?;
    // SAFETY: `name` ist NUL-terminiert; getgrnam liefert NULL oder einen
    // Zeiger auf eine statische Struktur, aus der sofort gelesen wird.
    unsafe {
        let entry = libc::getgrnam(name.as_ptr());
        (!entry.is_null()).then(|| std::ptr::read_unaligned(&raw const (*entry).gr_gid))
    }
}

/// Nimmt Homes auf, die unter `base` liegen und der Dienst noch nicht hält
/// — ein `witness.json` in einem Verzeichnis, dessen Name eine Repo-Kennung
/// ist (16 Hex-Zeichen).
fn adopt(base: &Path, served: &mut BTreeMap<String, Served>) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str() else {
            continue;
        };
        if !is_repo_id(id) || served.contains_key(id) {
            continue;
        }
        let home = base.join(id);
        if fs::symlink_metadata(home.join("witness.json")).is_ok_and(|meta| meta.is_file()) {
            eprintln!("witness host: adopting {id}");
            served.insert(id.to_owned(), Served::new(home));
        }
    }
}

fn is_repo_id(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Startet jeden Witness, der nicht läuft — nach einem kurzen Lauf erst nach
/// dem Rückstau.
fn supervise(served: &mut BTreeMap<String, Served>) {
    for (id, entry) in served.iter_mut() {
        if let Some(child) = entry.child.as_mut() {
            match child.try_wait() {
                Ok(None) => continue,
                Ok(Some(status)) => {
                    let short = entry.started.is_some_and(|at| at.elapsed() < STABLE_AFTER);
                    entry.failures = if short { entry.failures + 1 } else { 0 };
                    eprintln!("witness host: witness {id} exited ({status})");
                    entry.child = None;
                }
                Err(err) => {
                    eprintln!("witness host: witness {id} cannot be waited for: {err}");
                    entry.child = None;
                }
            }
        }
        let backoff = Duration::from_secs(1u64 << entry.failures.min(6)).min(MAX_BACKOFF);
        if entry.failures > 0 && entry.started.is_some_and(|at| at.elapsed() < backoff) {
            continue;
        }
        match spawn(&entry.home) {
            Ok(child) => {
                eprintln!("witness host: started witness {id}");
                entry.child = Some(child);
                entry.started = Some(Instant::now());
            }
            Err(err) => {
                eprintln!("witness host: witness {id} not started: {err}");
                entry.failures += 1;
                entry.started = Some(Instant::now());
            }
        }
    }
}

/// Startet `minds witness run --home <home>` aus dem laufenden Binary — mit
/// absolutem Pfad als `argv[0]` (die Trennungsfakten prüfen ihn) und dem
/// stderr des Witness in seiner Log-Datei.
fn spawn(home: &Path) -> std::io::Result<Child> {
    let exe = std::env::current_exe()?;
    let log = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(home.join("log").join(STDERR_LOG))?;
    Command::new(&exe)
        .args(["witness", "run", "--home"])
        .arg(home)
        .env_remove("MINDS_WITNESS_SOCKET")
        .env_remove("MINDS_WITNESS_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
}

/// Beendet das Kind: `SIGTERM`, dann bis [`STOP_WAIT`] warten, dann `SIGKILL`.
fn stop_child(entry: &mut Served) {
    let Some(mut child) = entry.child.take() else {
        return;
    };
    let Ok(pid) = libc::pid_t::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    // SAFETY: kill mit einer Kind-Pid und einem Standardsignal.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + STOP_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

/// Eine Verbindung: genau eine Anfrage, genau eine Antwort.
fn handle(mut stream: UnixStream, base: &Path, served: &mut BTreeMap<String, Served>, euid: u32) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let Some(frame) = read_frame(&mut stream) else {
        return;
    };
    let answer = match frame {
        Frame::Ping => Frame::Ack {
            request_id: [0; 16],
            status: "pong".into(),
        },
        Frame::Status => {
            let mut running = 0usize;
            for entry in served.values_mut() {
                if entry.running() {
                    running += 1;
                }
            }
            Frame::Ack {
                request_id: [0; 16],
                status: format!("serving {} repositories ({running} running)", served.len()),
            }
        }
        Frame::Register {
            repo,
            intent_key,
            request_id,
        } => match register_here(base, served, &repo, intent_key.as_deref()) {
            Ok(status) => Frame::Ack { request_id, status },
            Err(reason) => Frame::Nack { request_id, reason },
        },
        Frame::Unregister { repo, request_id } => {
            if peer_uid(&stream) != Some(euid) {
                Frame::Nack {
                    request_id,
                    reason: "only the witness user may unregister a repository".into(),
                }
            } else {
                match unregister_here(base, served, &repo) {
                    Ok(status) => Frame::Ack { request_id, status },
                    Err(reason) => Frame::Nack { request_id, reason },
                }
            }
        }
        _ => Frame::Nack {
            request_id: [0; 16],
            reason: "the host socket accepts ping, status, register and unregister".into(),
        },
    };
    if let Ok(bytes) = witness_proto::encode(&answer) {
        let _ = stream.write_all(&bytes);
    }
}

/// Liest genau einen Frame — bis [`MAX_REQUEST`] Bytes, bis zur Frist.
fn read_frame(stream: &mut UnixStream) -> Option<Frame> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    while bytes.len() < MAX_REQUEST {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
        }
        match witness_proto::decode(&bytes) {
            Ok((frame, _)) => return Some(frame),
            Err(ProtoError::Incomplete) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Die Registrierung als Witness-Nutzer: Repo prüfen, Home einrichten
/// (idempotent), Schlüssel, Witness starten, Signer-Zeile zurück.
fn register_here(
    base: &Path,
    served: &mut BTreeMap<String, Served>,
    repo: &str,
    intent_key: Option<&str>,
) -> Result<String, String> {
    let root = Path::new(repo)
        .canonicalize()
        .map_err(|err| format!("repository not reachable: {err}"))?;
    if super::super::repo_root_of(&root).as_deref() != Some(root.as_path()) {
        return Err("the path is not the root of a Git worktree".into());
    }
    if !writable_for_witness(&root) {
        return Err(format!(
            "the witness cannot write {}/.git — the repository is not shared with the witness \
             group (minds enable does that)",
            crate::text::sanitize(&root.display().to_string())
        ));
    }
    let id = crate::enable_witness::repo_id(&root);
    let home = base.join(&id);
    let root_text = root
        .to_str()
        .ok_or_else(|| "the repository path is not valid UTF-8".to_owned())?;
    // Die Gruppe nur als Witness-Nutzer — sonst gibt es keine, die das Home
    // durchqueren dürfte.
    // SAFETY: geteuid hat keine Vorbedingungen.
    let euid = unsafe { libc::geteuid() };
    let socket_group = (crate::enable_protected::witness_uid() == Some(euid))
        .then_some(crate::enable_protected::WITNESS_USER);
    let initialized = init_config(
        &home,
        &InitRequest {
            repo: root_text,
            mapping: None,
            profile: Some("user"),
            socket_group,
            child_repo: None,
            policy_rev: None,
            intent_key,
            replace_intent_key: false,
            project: None,
        },
    )
    .map_err(|err| crate::text::sanitize(&err.to_string()))?;
    let fresh = matches!(initialized, Initialized::Created(_));
    let (Initialized::Created(pins) | Initialized::Unchanged(pins) | Initialized::Pinned(pins)) =
        &initialized;
    let signers = match super::super::existing_signers_line(&home) {
        Ok(Some(line)) => line,
        Ok(None) => {
            super::super::keygen(&home).map_err(|err| crate::text::sanitize(&err.to_string()))?
        }
        Err(err) => return Err(crate::text::sanitize(&err.to_string())),
    };
    let entry = served
        .entry(id.clone())
        .or_insert_with(|| Served::new(home.clone()));
    let was_running = entry.running();
    if !was_running {
        entry.failures = 0;
        entry.started = None;
        let mut single = BTreeMap::new();
        single.insert(
            id.clone(),
            std::mem::replace(entry, Served::new(home.clone())),
        );
        supervise(&mut single);
        if let Some(restarted) = single.remove(&id) {
            *entry = restarted;
        }
    }
    let socket = home.join("run").join("witness.sock");
    let deadline = Instant::now() + START_WAIT;
    let answering = loop {
        if super::socket::ping(&socket) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut parts = vec![
        format!(
            "{} {id}",
            if fresh {
                "registered"
            } else if was_running {
                "running"
            } else {
                "restarted"
            }
        ),
        format!("signers {signers}"),
        format!("socket {}", socket.display()),
        format!("answering {}", if answering { "yes" } else { "no" }),
    ];
    for line in pins.lines() {
        if line.starts_with("intent key:") {
            parts.push(line);
        }
    }
    let status = parts.join(PART);
    if status.chars().any(char::is_control) || status.len() > 4000 {
        return Err("the registration report is not a single line".into());
    }
    Ok(status)
}

/// Ob der Witness-Nutzer schreibt, was er schreiben muss (die Einrichtung
/// teilt diese Pfade mit seiner Gruppe), und HEAD liest.
fn writable_for_witness(root: &Path) -> bool {
    let access = |path: PathBuf, mode: libc::c_int| {
        let Ok(name) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
            return false;
        };
        // SAFETY: nul-terminierter Pfad; access hat keine weiteren
        // Vorbedingungen.
        unsafe { libc::access(name.as_ptr(), mode) == 0 }
    };
    let git = root.join(".git");
    [git.clone(), git.join("objects"), git.join("refs")]
        .into_iter()
        .all(|dir| access(dir, libc::W_OK | libc::X_OK))
        && access(git.join("HEAD"), libc::R_OK)
}

/// Meldet das Repo ab: Witness anhalten, Home entfernen.
fn unregister_here(
    base: &Path,
    served: &mut BTreeMap<String, Served>,
    repo: &str,
) -> Result<String, String> {
    // Auch ein Repo, das es nicht mehr gibt: Die Kennung folgt aus dem Pfad.
    let root = Path::new(repo)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(repo));
    let id = crate::enable_witness::repo_id(&root);
    let home = base.join(&id);
    if let Some(mut entry) = served.remove(&id) {
        stop_child(&mut entry);
    }
    match fs::symlink_metadata(&home) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            fs::remove_dir_all(&home).map_err(|err| format!("witness home not removed: {err}"))?;
            Ok(format!(
                "unregistered {id}{PART}home {} removed",
                home.display()
            ))
        }
        Ok(_) => Err("the witness home is not a directory".into()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(format!("not registered {id}"))
        }
        Err(err) => Err(format!("witness home not readable: {err}")),
    }
}

// ---------------------------------------------------------------------------
// Die Gegenseite: `minds enable` und `minds witness register|unregister`
// ---------------------------------------------------------------------------

/// Was eine Registrierung ergab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registered {
    pub(crate) id: String,
    /// `registered`, `running` oder `restarted`.
    pub(crate) state: String,
    pub(crate) signers: Option<String>,
    pub(crate) socket: Option<PathBuf>,
    pub(crate) answering: bool,
    /// Die Zeilen zum Freigabe-Schlüssel.
    pub(crate) intent_key: Vec<String>,
    /// Alle Teile, wie der Dienst sie nannte.
    pub(crate) parts: Vec<String>,
}

/// Liest die Statuszeile eines `Ack` auf eine Registrierung.
pub(crate) fn parse_registered(status: &str) -> Option<Registered> {
    let parts: Vec<String> = status.split(PART).map(str::to_owned).collect();
    let (state, id) = parts.first()?.split_once(' ')?;
    if !matches!(state, "registered" | "running" | "restarted") || !is_repo_id(id) {
        return None;
    }
    let mut registered = Registered {
        id: id.to_owned(),
        state: state.to_owned(),
        signers: None,
        socket: None,
        answering: false,
        intent_key: Vec::new(),
        parts: parts.clone(),
    };
    for part in &parts[1..] {
        if let Some(line) = part.strip_prefix("signers ") {
            registered.signers = Some(line.to_owned());
        } else if let Some(path) = part.strip_prefix("socket ") {
            registered.socket = Some(PathBuf::from(path));
        } else if let Some(answer) = part.strip_prefix("answering ") {
            registered.answering = answer == "yes";
        } else if let Some(key) = part.strip_prefix("intent key:") {
            registered.intent_key.push(key.trim().to_owned());
        }
    }
    Some(registered)
}

/// Eine Anfrage-Kennung für den Steuer-Socket des Rechners.
fn request_id() -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"minds witness host");
    hasher.update(&std::process::id().to_le_bytes());
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.update(&now.as_nanos().to_le_bytes());
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    id
}

/// Meldet `root` beim Dienst hinter `socket` an. `peer`: die Kennung, die
/// die Gegenstelle haben muss (der Witness-Nutzer), `None` prüft nicht.
pub(crate) fn register(
    socket: &Path,
    peer: Option<u32>,
    root: &Path,
    intent_key: Option<&str>,
) -> Result<Registered, String> {
    let frame = Frame::Register {
        repo: root
            .to_str()
            .ok_or_else(|| "the repository path is not valid UTF-8".to_owned())?
            .to_owned(),
        intent_key: intent_key.map(str::to_owned),
        request_id: request_id(),
    };
    let deadline = Instant::now() + START_WAIT + Duration::from_secs(20);
    match crate::hook::witness::request(socket, peer, &frame, deadline) {
        Ok(Frame::Ack { status, .. }) => parse_registered(&crate::text::sanitize(&status))
            .ok_or_else(|| "the witness host answered with something else".to_owned()),
        Ok(Frame::Nack { reason, .. }) => Err(crate::text::sanitize(&reason)),
        Ok(_) => Err("unexpected response from the witness host".into()),
        Err(kind) => Err(format!("witness host not reachable ({kind})")),
    }
}

/// Meldet `root` beim Dienst ab — als Witness-Nutzer.
pub(crate) fn unregister(socket: &Path, peer: Option<u32>, root: &Path) -> Result<String, String> {
    let frame = Frame::Unregister {
        repo: root
            .to_str()
            .ok_or_else(|| "the repository path is not valid UTF-8".to_owned())?
            .to_owned(),
        request_id: request_id(),
    };
    let deadline = Instant::now() + STOP_WAIT + Duration::from_secs(5);
    match crate::hook::witness::request(socket, peer, &frame, deadline) {
        Ok(Frame::Ack { status, .. }) => Ok(crate::text::sanitize(&status)),
        Ok(Frame::Nack { reason, .. }) => Err(crate::text::sanitize(&reason)),
        Ok(_) => Err("unexpected response from the witness host".into()),
        Err(kind) => Err(format!("witness host not reachable ({kind})")),
    }
}

/// Was der Dienst über sich sagt (`Frame::Status`) — `None`, wenn keiner
/// antwortet.
pub(crate) fn host_status(socket: &Path) -> Option<Result<String, String>> {
    super::socket::observer_status(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registration_report_is_read_back() {
        let status = "registered 0123456789abcdef · signers minds-witness-user@h namespaces=\"minds-witness\" ssh-ed25519 AAAA · socket /b/0123456789abcdef/run/witness.sock · answering yes · intent key: sk-ssh-ed25519 SHA256:x";
        let parsed = parse_registered(status).unwrap();
        assert_eq!(parsed.id, "0123456789abcdef");
        assert_eq!(parsed.state, "registered");
        assert_eq!(
            parsed.signers.as_deref(),
            Some("minds-witness-user@h namespaces=\"minds-witness\" ssh-ed25519 AAAA")
        );
        assert_eq!(
            parsed.socket.as_deref(),
            Some(Path::new("/b/0123456789abcdef/run/witness.sock"))
        );
        assert!(parsed.answering);
        assert_eq!(parsed.intent_key, ["sk-ssh-ed25519 SHA256:x"]);
        assert!(parse_registered("pong").is_none());
        assert!(parse_registered("registered nope").is_none());
        assert!(
            !parse_registered("running 0123456789abcdef · answering no")
                .unwrap()
                .answering
        );
    }

    #[test]
    fn only_repo_ids_are_adopted() {
        assert!(is_repo_id("0123456789abcdef"));
        assert!(!is_repo_id("run"));
        assert!(!is_repo_id("0123456789abcdeg"));
        assert!(!is_repo_id("0123456789abcdef0"));
    }
}
