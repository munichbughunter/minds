//! Begrenztes, nichtblockierendes Multiplexing; allein der Eventloop schreibt.

use super::*;
use minds_capture::witness_proto::{self, Frame, ProtoError};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static STOP: AtomicBool = AtomicBool::new(false);
const MAX_CLIENTS: usize = 128;
const MAX_BUFFERED: usize = 64 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(5);
/// Fairness: Ein Client mit vielen kleinen Frames hält die anderen höchstens
/// so viele Appends lang auf.
const FRAMES_PER_STEP: usize = 16;

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

struct Signals(Vec<(libc::c_int, libc::sigaction)>);
impl Signals {
    fn install() -> Fallible<Self> {
        STOP.store(false, Ordering::Relaxed);
        let mut guard = Self(Vec::new());
        for signal in [libc::SIGTERM, libc::SIGINT] {
            // SAFETY: sigaction ist eine C-Struktur; sigemptyset initialisiert
            // die Maske, der Handler benutzt ausschließlich eine AtomicBool.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                let mut previous: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = stop as *const () as usize;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, &mut previous) != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                guard.0.push((signal, previous));
            }
        }
        Ok(guard)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for (signal, previous) in &self.0 {
            // SAFETY: während install gespeicherte gültige Signalaktionen.
            unsafe {
                libc::sigaction(*signal, previous, std::ptr::null_mut());
            }
        }
    }
}

struct Socket {
    listener: UnixListener,
    path: PathBuf,
    inode: u64,
    device: u64,
}
impl Socket {
    fn bind(home: &Path, group: Option<u32>) -> Fallible<Self> {
        let path = home.join("run/witness.sock");
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if !meta.file_type().is_socket() {
                    return Err("refusing to remove non-socket witness path".into());
                }
                // Eine antwortende ODER nur verbundene Gegenstelle ist aktiv.
                // Timeout, volle Backlog oder EACCES sind niemals Stale-Beweise.
                if ping(&path) {
                    return Err("witness socket is already active".into());
                }
                match UnixStream::connect(&path) {
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
                    _ => return Err("witness socket may still be active".into()),
                }
                fs::remove_file(&path)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = UnixListener::bind(&path)?;
        let meta = fs::symlink_metadata(&path)?;
        let socket = Self {
            listener,
            path,
            inode: meta.ino(),
            device: meta.dev(),
        };
        if let Some(group) = group {
            let name = std::ffi::CString::new(socket.path.as_os_str().as_encoded_bytes())?;
            // SAFETY: nul-terminierter Pfad; uid=-1 lässt den Eigentümer stehen.
            if unsafe { libc::chown(name.as_ptr(), !0, group) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        fs::set_permissions(&socket.path, fs::Permissions::from_mode(0o660))?;
        socket.listener.set_nonblocking(true)?;
        Ok(socket)
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.ino() == self.inode && m.dev() == self.device)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(super) fn ping(path: &Path) -> bool {
    let request = witness_proto::encode(&Frame::Ping).expect("static ping");
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
    if stream.write_all(&request).is_err() {
        return false;
    }
    let mut bytes = Vec::new();
    let mut chunk = [0; 128];
    while bytes.len() < 4096 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
        }
        match witness_proto::decode(&bytes) {
            Ok((Frame::Ack { request_id, status }, _)) => {
                return request_id == [0; 16] && status == "pong";
            }
            Err(ProtoError::Incomplete) => {}
            _ => return false,
        }
    }
    false
}

pub(super) struct Client {
    pub(super) stream: UnixStream,
    pub(super) bytes: Vec<u8>,
    pub(super) since: Instant,
    pub(super) eof: bool,
    /// Vollständige Frames warten noch im Puffer; bis sie verarbeitet sind,
    /// wird nicht nachgelesen und der Rest staut sich im Socket.
    pub(super) pending: bool,
}

pub(super) fn step(client: &mut Client, writer: &mut Writer) -> Fallible<bool> {
    let mut chunk = [0u8; 64 * 1024];
    if !client.eof && !client.pending {
        match client.stream.read(&mut chunk) {
            Ok(0) => client.eof = true,
            Ok(n) => client.bytes.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return Ok(false),
        }
    }
    // Vollständige Frames werden verarbeitet, nie verdrängt. Im Puffer bleibt
    // danach ein unvollständiger Frame oder ein gedeckelter Rest.
    client.pending = false;
    for _ in 0..FRAMES_PER_STEP {
        match witness_proto::decode(&client.bytes) {
            Ok((frame, used)) => {
                client.bytes.drain(..used);
                client.since = Instant::now();
                if !dispatch(client, frame, writer)? {
                    return Ok(false);
                }
            }
            Err(ProtoError::Incomplete) if !client.eof && client.since.elapsed() < DEADLINE => {
                return Ok(true);
            }
            Err(_) => {
                if !client.bytes.is_empty() {
                    log(&writer.home, "malformed or incomplete frame dropped");
                }
                return Ok(false);
            }
        }
    }
    client.pending = true;
    Ok(true)
}

/// `Ok(false)` schließt die Verbindung; `Err` ist ausschließlich ein Fehler
/// des eigenen Speichers.
fn dispatch(client: &mut Client, frame: Frame, writer: &mut Writer) -> Fallible<bool> {
    match frame {
        Frame::Hook {
            agent,
            event_override,
            stdin,
        } => writer.hook(&agent, event_override.as_deref(), stdin, clock::now())?,
        Frame::Ping => {
            let bytes = witness_proto::encode(&Frame::Ack {
                request_id: [0; 16],
                status: "pong".into(),
            })?;
            if client.stream.write_all(&bytes).is_err() {
                return Ok(false);
            }
        }
        Frame::CheckpointRequest { commit, request_id } => {
            // Ein gescheiterter oder abgewiesener Checkpoint ist kein Grund,
            // den einzigen Schreiber zu beenden: Die Sessions bleiben offen,
            // die Agent-Seite bekommt einen festen Grund.
            let stream = &client.stream;
            let answer =
                match writer.checkpoint_requested(commit.as_deref(), &|| !peer_gone(stream)) {
                    Ok(status) => Frame::Ack { request_id, status },
                    Err(reason) => Frame::Nack {
                        request_id,
                        reason: reason.into(),
                    },
                };
            // Eine nicht kodierbare Statuszeile ist ein Fehler der Antwort,
            // nicht des eigenen Speichers — sie darf den Schreiber nicht
            // beenden.
            let bytes = witness_proto::encode(&answer).or_else(|_| {
                log(&writer.home, "checkpoint status not encodable");
                witness_proto::encode(&Frame::Nack {
                    request_id,
                    reason: "status not encodable".into(),
                })
            })?;
            if client.stream.write_all(&bytes).is_err() {
                return Ok(false);
            }
        }
        Frame::IntentActivate { request_id, .. } => {
            let bytes = witness_proto::encode(&Frame::Nack {
                request_id,
                reason: "not supported".into(),
            })?;
            if client.stream.write_all(&bytes).is_err() {
                return Ok(false);
            }
        }
        _ => {
            log(&writer.home, "unexpected response frame dropped");
            return Ok(false);
        }
    }
    Ok(true)
}

/// Ob die Gegenseite aufgelegt hat — ohne zu warten und ohne ein Byte zu
/// verbrauchen. Die Agent-Seite schickt nach ihrer Anfrage nichts mehr;
/// lesbar wird die Verbindung erst, wenn sie sie schließt (Frist abgelaufen).
fn peer_gone(stream: &UnixStream) -> bool {
    let mut poll = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: genau ein gültiges `pollfd`, Frist 0.
    if unsafe { libc::poll(&raw mut poll, 1, 0) } <= 0 {
        return false;
    }
    if poll.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
        return true;
    }
    let mut byte = 0u8;
    // SAFETY: ein Byte großer, beschreibbarer Puffer; `MSG_PEEK` verbraucht
    // nichts, der Socket ist nichtblockierend.
    let read = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    read == 0
}

/// Höchstens eine Zeile pro Sekunde: Ein dauerhafter accept-Fehler (EMFILE)
/// darf das Log nicht im Takt der Eventloop füllen.
fn log_throttled(home: &Path, last: &mut Option<Instant>, message: &str) {
    if last.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
        *last = Some(Instant::now());
        log(home, message);
    }
}

#[derive(Serialize, Deserialize)]
struct Marker {
    local_id: String,
    running: bool,
}

pub fn run(home: &Path, follow: bool) -> Fallible<()> {
    let home: PathBuf = home.components().collect();
    let config = load(&home)?;
    refuse_home_in_repo(&home, &config)?;
    let _lock = lock(&home)?;
    let _signals = Signals::install()?;
    let key_fingerprint = fingerprint(&home)?;
    let socket = Socket::bind(&home, config.socket_group)?;
    let mut writer = Writer::open(&home, config, follow)?;
    let marker_path = home.join("run/lifecycle.json");
    let previous = match fs::read(&marker_path) {
        Ok(bytes) => {
            let marker: Marker = serde_json::from_slice(&bytes)?;
            let key = SessionKey::new("witness", marker.local_id)?;
            let stopped = writer
                .journal
                .read(&key)?
                .events
                .last()
                .is_some_and(|e| e.raw_kind == "witness.stop");
            if stopped { "clean" } else { "unclean" }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "none",
        Err(e) => return Err(e.into()),
    };
    // RFC3339 enthält Doppelpunkte, SessionKey erlaubt sie nicht. Unix-Nanos
    // sind derselbe Startzeitpunkt in einer pfadsicheren Darstellung.
    let local_id = clock::now().1.to_string();
    let key = SessionKey::new("witness", &local_id)?;
    atomic(
        &marker_path,
        &serde_json::to_vec(&Marker {
            local_id: local_id.clone(),
            running: true,
        })?,
    )?;
    let profile = writer.config.profile.name();
    writer.lifecycle(
        &key,
        "witness.start",
        serde_json::json!({"previous_stop": previous, "profile": profile, "key": key_fingerprint}),
    )?;
    // Das zweite Auge (EA-08). Scheitert der Watcher (etwa am inotify-Limit),
    // läuft der Witness weiter — die Blindheit steht dann als `fs.gap` in der
    // Kette, nicht nur im Log.
    // Der eigene Stream gilt auch ohne Beobachter: Jeder Checkpoint
    // schließt seine Epoche mit `fs.checkpoint` ab.
    writer.stream = Some(key.clone());
    let root = writer.config.repo_root.clone();
    // Inhalte prüft dieselbe Policy wie der Checkpoint, nie schwächer als
    // der Standard; ist sie nicht lesbar, gilt der Standard.
    let pipeline = crate::config::load_redaction_untrusted(&root)
        .ok()
        .and_then(|config| config.floored_at_default().pipeline().ok());
    match observer::Observer::watch(&root, Box::new(observer::Blake3)) {
        Ok(observer) => {
            let observer = match pipeline {
                Some(pipeline) => observer.with_pipeline(pipeline),
                None => {
                    log(
                        &home,
                        "redaction policy not loadable; observer uses the default",
                    );
                    observer
                }
            };
            writer.observe_into(key.clone(), observer)
        }
        Err(err) => {
            log(&home, &format!("file observer unavailable: {err}"));
            writer.lifecycle(&key, "fs.gap", serde_json::json!({"reason": "unavailable"}))?;
        }
    }
    let result = serve(&socket.listener, &mut writer);
    if result.is_ok() {
        writer.lifecycle(&key, "witness.stop", serde_json::json!({}))?;
        atomic(
            &marker_path,
            &serde_json::to_vec(&Marker {
                local_id,
                running: false,
            })?,
        )?;
    } else {
        log(
            &home,
            "witness storage failure; daemon stopped without a clean marker",
        );
    }
    result
}

fn serve(listener: &UnixListener, writer: &mut Writer) -> Fallible<()> {
    let mut clients: Vec<Client> = Vec::new();
    let mut accept_logged = None;
    while !STOP.load(Ordering::Relaxed) {
        writer.next_step();
        for _ in clients.len()..MAX_CLIENTS {
            match listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(true).is_err() {
                        // Ohne nonblocking könnte ein Client die Schleife anhalten.
                        log_throttled(
                            &writer.home,
                            &mut accept_logged,
                            "client socket setup failed; connection closed",
                        );
                        continue;
                    }
                    clients.push(Client {
                        stream,
                        bytes: Vec::new(),
                        since: Instant::now(),
                        eof: false,
                        pending: false,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                // ECONNABORTED, EMFILE und Verwandte betreffen eine Verbindung
                // oder sind vorübergehend; der einzige Schreiber bleibt stehen.
                Err(e) => {
                    log_throttled(
                        &writer.home,
                        &mut accept_logged,
                        &format!("accept failed: {e}"),
                    );
                    break;
                }
            }
        }
        let mut idx = 0;
        while idx < clients.len() {
            if step(&mut clients[idx], writer)? {
                idx += 1;
            } else {
                clients.swap_remove(idx);
            }
        }
        writer.observe(false)?;
        if evict_largest(&mut clients, MAX_BUFFERED) {
            log(
                &writer.home,
                "client buffer limit reached; largest incomplete connections closed",
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// Es weichen die größten Puffer ohne wartende vollständige Frames, bis das
/// Limit wieder hält; Clients mit wartenden Frames erst als letztes Mittel.
fn evict_largest(clients: &mut Vec<Client>, limit: usize) -> bool {
    let mut buffered: usize = clients.iter().map(|c| c.bytes.len()).sum();
    let evicted = buffered > limit;
    while buffered > limit {
        let Some((largest, _)) = clients
            .iter()
            .enumerate()
            .max_by_key(|(_, client)| (!client.pending, client.bytes.len()))
        else {
            break;
        };
        buffered -= clients.swap_remove(largest).bytes.len();
    }
    evicted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(stream: UnixStream, bytes: usize) -> Client {
        stream.set_nonblocking(true).unwrap();
        Client {
            stream,
            bytes: vec![0; bytes],
            since: Instant::now(),
            eof: false,
            pending: false,
        }
    }

    #[test]
    fn peer_gone_sees_a_hang_up_but_not_silence_or_data() {
        use std::io::Write;
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        assert!(!peer_gone(&ours), "still wartend ist nicht gegangen");
        theirs.write_all(b"x").unwrap();
        assert!(!peer_gone(&ours), "ein ungelesenes Byte ist kein Auflegen");
        let mut byte = [0u8; 1];
        std::io::Read::read_exact(&mut &ours, &mut byte).unwrap();
        drop(theirs);
        // Das Auflegen wird unter macOS einen Augenblick später sichtbar; im
        // Betrieb ist der Absender da längst Sekunden weg.
        let deadline = Instant::now() + Duration::from_secs(1);
        while !peer_gone(&ours) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(peer_gone(&ours), "aufgelegt");
    }

    #[test]
    fn buffer_limit_evicts_only_the_largest_incomplete_clients() {
        let mut keep = Vec::new();
        let mut clients = Vec::new();
        for size in [10, 400, 20, 300] {
            let (ours, theirs) = UnixStream::pair().unwrap();
            keep.push(theirs);
            clients.push(client(ours, size));
        }
        assert!(!evict_largest(&mut clients, 1000));
        assert_eq!(clients.len(), 4);
        assert!(evict_largest(&mut clients, 100));
        let mut left: Vec<_> = clients.iter().map(|c| c.bytes.len()).collect();
        left.sort();
        assert_eq!(left, vec![10, 20]);
    }
}
