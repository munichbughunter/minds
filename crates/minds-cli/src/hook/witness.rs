//! Die Weitergabe an den Witness (EA-07) — der Socket-Teil von `minds hook`.
//!
//! Ist `MINDS_WITNESS_SOCKET` gesetzt, geht das Event als `Hook`-Frame an den
//! Witness statt ins lokale Journal. Die drei Regeln aus [`super`] gelten
//! unverändert, und daraus folgt die Bauform:
//!
//! - **Entdeckung nur über die Umgebung.** Kein Repository, keine
//!   Konfiguration: Die Agent-Seite kennt den Socket-Pfad und sonst nichts
//!   (W1, W3).
//! - **Begrenzte Zeit.** Verbinden höchstens [`CONNECT_TIMEOUT`], Schreiben
//!   höchstens [`WRITE_TIMEOUT`] insgesamt — nicht je Systemaufruf. Ein
//!   hängender Witness kostet den Agenten damit eine feste Obergrenze.
//! - **Feuern und vergessen.** Keine Wiederholung, kein Warten auf Antwort
//!   (`witness_proto`: Hooks werden nie quittiert). Scheitert irgendetwas,
//!   meldet [`forward`] eine Fehlerart aus festem Wortschatz, und der Aufrufer
//!   schreibt wie bisher ins lokale Journal.
//!
//! # Was eine Weitergabe garantiert und was nicht
//!
//! `Ok` heißt: Der vollständige Frame liegt im Socket-Puffer des Witness.
//! Stirbt der Witness, bevor er ihn liest, ist das Event verloren — der Preis
//! von „keine Antwort abwarten", den das Protokoll bewusst zahlt. Was dagegen
//! **nicht** passieren kann, ist eine doppelte Aufzeichnung: Ein Frame, der nur
//! teilweise geschrieben wurde, verwirft der Witness als unvollständig, und das
//! Event landet allein im Rückfall-Journal.
//!
//! Ebenso still bleibt ein falscher Empfänger: Lauscht am Pfad etwas anderes
//! als ein Witness, oder verwirft der Witness den Frame, sieht der Hook ein
//! `Ok` und schreibt keine Logzeile. Sichtbar wird das erst auf der Seite des
//! Witness (`log/witness.log`) oder als Lücke beim Checkpoint.
//!
//! # Der eine Rückkanal: [`request`]
//!
//! `minds checkpoint` (EA-06d) braucht eine Antwort — ob der Witness seine
//! Sessions versiegelt hat. Dafür gibt es [`request`]: derselbe Pfad, dieselbe
//! Pfadprüfung, dieselben festen Fehlerarten, aber mit einer Frist, die das
//! Lesen der Antwort einschließt, und einer Obergrenze für deren Größe. Der
//! Hook selbst benutzt ihn nie.
//!
//! # Wem der Socket-Pfad vertraut wird
//!
//! Der Hook prüft nicht, wer am anderen Ende lauscht: In den Profilen
//! `container` und `user` läuft der Witness bewusst unter einer anderen UID
//! als der Agent (Zugang über die Socket-Gruppe), ein Vergleich der Peer-UID
//! schlösse genau diese Fälle aus. Die Vertrauensgrenze ist deshalb der Pfad
//! selbst: Er muss in einem Verzeichnis liegen, in dem kein Fremder einen
//! Socket anlegen kann (`run/` im Witness-Home, 0700/0750 — Sache von
//! `minds enable`, EA-10). Durchgesetzt wird davon, was der Hook billig prüfen
//! kann: Der Pfad muss absolut sein, kein Symlink, und sein Verzeichnis darf
//! nicht für alle beschreibbar sein (`/tmp`) — sonst `untrusted socket dir`
//! und Rückfall. Die Grenzen dieser Prüfung stehen an [`trusted_dir`].
//!
//! # SIGPIPE
//!
//! Schließt der Witness die Verbindung mitten im Schreiben, liefert `write`
//! `EPIPE` statt eines Signals: Die Rust-Laufzeit ignoriert SIGPIPE in jedem
//! Binary, und `minds` stellt das nirgends zurück.

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::Duration;

use minds_capture::witness_proto::{self, Frame, ProtoError};

/// Die einzige Stelle, an der die Agent-Seite vom Witness erfährt (W3).
pub(super) const SOCKET_ENV: &str = "MINDS_WITNESS_SOCKET";

/// Obergrenze für den Verbindungsaufbau.
#[cfg(unix)]
const CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Obergrenze für das Schreiben des ganzen Frames, ab erfolgreicher Verbindung.
#[cfg(unix)]
const WRITE_TIMEOUT: Duration = Duration::from_millis(100);

/// Der Socket-Pfad aus der Umgebung. Eine leere Variable zählt als nicht
/// gesetzt — sonst wäre `MINDS_WITNESS_SOCKET=` ein Dauer-Rückfall mit
/// Logzeile bei jedem Event.
pub(crate) fn socket_path() -> Option<PathBuf> {
    std::env::var_os(SOCKET_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Der Agent-Name, unter dem der Witness seinen eigenen Strom führt. Ein
/// Hook-Frame mit diesem Namen verwirft er ("reserved agent rejected") — ohne
/// dass der Hook davon erführe. Deshalb bleibt ein solches Event lokal.
const RESERVED_AGENT: &str = "witness";

/// Was ein `Hook`-Frame außer Agent, Override und `stdin` gegen `MAX_FRAME`
/// zählt: Art (1) und zwei Längenpräfixe (2 + 2). Der 12-Byte-Header zählt
/// dort nicht mit.
const HOOK_OVERHEAD: usize = 5;

/// Ob ein Event dieser Größe überhaupt weitergegeben werden kann — geprüft,
/// **bevor** der Aufrufer die Rohbytes kopiert.
///
/// Ein Payload über `MAX_FRAME` (8 MiB) bleibt lokal: `minds hook` liest bis
/// 32 MiB, das Protokoll trägt ein Viertel davon. Ein solches Event ist damit
/// absichtlich ein Event im lokalen Journal, nicht im Witness — die Logzeile
/// `witness unreachable: frame too large` macht den Fall sichtbar.
pub(super) fn admissible(
    agent: &str,
    event_override: Option<&str>,
    stdin_len: usize,
) -> Result<(), &'static str> {
    if agent == RESERVED_AGENT {
        return Err("reserved agent");
    }
    let len = HOOK_OVERHEAD + agent.len() + event_override.map_or(0, str::len);
    if len.saturating_add(stdin_len) > witness_proto::MAX_FRAME as usize {
        return Err("frame too large");
    }
    Ok(())
}

/// Schickt das Event als `Hook`-Frame an den Witness.
///
/// Der Fehler ist eine kurze, feste Bezeichnung für `hook.log`
/// (`witness unreachable: <kind>`) — nie fremder Text, nie Payload, nie der
/// Socket-Pfad.
pub(super) fn forward(
    socket: &Path,
    agent: &str,
    event_override: Option<&str>,
    stdin: Vec<u8>,
) -> Result<(), &'static str> {
    admissible(agent, event_override, stdin.len())?;
    let frame = witness_proto::encode(&Frame::Hook {
        agent: agent.to_owned(),
        event_override: event_override.map(str::to_owned),
        stdin,
    })
    .map_err(|err| match err {
        ProtoError::TooLarge(_) => "frame too large",
        _ => "invalid frame",
    })?;
    send(socket, &frame).map_err(|err| kind(&err))
}

/// Obergrenze für eine Antwort des Witness: Kopf plus `request_id` plus eine
/// Statuszeile von höchstens 4 KiB (`witness_proto`), mit etwas Luft. Mehr
/// liest der Client nie — ein Witness, der mehr schickt, ist kein Witness.
#[cfg(unix)]
const MAX_RESPONSE: usize = 8 * 1024;

/// Schickt eine Anfrage, die eine Antwort erwartet (EA-06d: der Checkpoint),
/// und wartet bis `deadline` auf genau einen `Ack` oder `Nack` mit derselben
/// `request_id`.
///
/// Derselbe Weg wie [`forward`] — Pfadprüfung, nichtblockierendes Verbinden,
/// Fristen —, aber mit Rückkanal. Die Frist gilt für alles zusammen:
/// Verbinden, Schreiben, Lesen. Der Fehler ist wie bei [`forward`] eine feste
/// Bezeichnung, nie fremder Text.
pub(crate) fn request(
    socket: &Path,
    frame: &Frame,
    deadline: std::time::Instant,
) -> Result<Frame, &'static str> {
    let request_id = match frame {
        Frame::CheckpointRequest { request_id, .. } | Frame::IntentActivate { request_id, .. } => {
            *request_id
        }
        _ => return Err("invalid frame"),
    };
    let bytes = witness_proto::encode(frame).map_err(|_| "invalid frame")?;
    let response = exchange(socket, &bytes, deadline).map_err(|err| {
        // Nur hier gibt es eine Antwort, die unpassend sein kann.
        if err.kind() == std::io::ErrorKind::InvalidData {
            "unexpected response"
        } else {
            kind(&err)
        }
    })?;
    let (answer, _) = witness_proto::decode(&response).map_err(|_| "unexpected response")?;
    let answered = match &answer {
        Frame::Ack { request_id: id, .. } | Frame::Nack { request_id: id, .. } => *id == request_id,
        _ => false,
    };
    if answered {
        Ok(answer)
    } else {
        Err("unexpected response")
    }
}

#[cfg(not(unix))]
fn exchange(_: &Path, _: &[u8], _: std::time::Instant) -> std::io::Result<Vec<u8>> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Schreibt `request` und liest genau einen vollständigen Antwort-Frame —
/// beides bis `deadline`.
#[cfg(unix)]
fn exchange(
    socket: &Path,
    request: &[u8],
    deadline: std::time::Instant,
) -> std::io::Result<Vec<u8>> {
    use std::io::{ErrorKind, Read, Write};
    use std::time::Instant;

    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or(std::io::Error::from(ErrorKind::TimedOut))
    };
    // macOS meldet `EINVAL` beim Setzen einer Frist, sobald die Gegenseite
    // die Verbindung geschlossen hat — auch wenn ihre Antwort noch im Puffer
    // liegt. Dann blockiert weder `read` (liefert Puffer, danach 0) noch
    // `write` (liefert `EPIPE`); weiterlesen ist richtig, abbrechen wäre es
    // nicht.
    let tolerate_closed_peer = |result: std::io::Result<()>| match result {
        Err(err) if err.kind() == ErrorKind::InvalidInput => Ok(()),
        other => other,
    };
    let mut stream = connect(socket, deadline)?;
    let mut rest = request;
    while !rest.is_empty() {
        tolerate_closed_peer(stream.set_write_timeout(Some(remaining()?)))?;
        match stream.write(rest) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => rest = &rest[n..],
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match witness_proto::decode(&bytes) {
            Ok((_, used)) => {
                bytes.truncate(used);
                return Ok(bytes);
            }
            Err(ProtoError::Incomplete) if bytes.len() < MAX_RESPONSE => {}
            Err(_) => return Err(ErrorKind::InvalidData.into()),
        }
        tolerate_closed_peer(stream.set_read_timeout(Some(remaining()?)))?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// Die Fehlerart „Frist abgelaufen" — eigens benannt, weil die
/// Checkpoint-Delegation sie von den anderen unterscheidet.
pub(crate) const TIMEOUT: &str = "timeout";

/// Die Fehlerart einer I/O-Störung, in Worten, die in einer Logzeile taugen.
fn kind(err: &std::io::Error) -> &'static str {
    use std::io::ErrorKind as K;
    if err
        .get_ref()
        .is_some_and(|inner| inner.is::<UntrustedDir>())
    {
        return "untrusted socket dir";
    }
    match err.kind() {
        K::NotFound => "socket missing",
        K::ConnectionRefused => "connection refused",
        K::TimedOut | K::WouldBlock => TIMEOUT,
        K::PermissionDenied => "permission denied",
        K::BrokenPipe
        | K::ConnectionReset
        | K::ConnectionAborted
        | K::WriteZero
        | K::UnexpectedEof => "connection closed",
        K::InvalidInput => "invalid socket path",
        K::Unsupported => "unsupported platform",
        _ => "io error",
    }
}

#[cfg(not(unix))]
fn send(_: &Path, _: &[u8]) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(unix)]
fn send(socket: &Path, frame: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::time::Instant;

    let mut stream = connect(socket, Instant::now() + CONNECT_TIMEOUT)?;
    let deadline = Instant::now() + WRITE_TIMEOUT;
    let mut rest = frame;
    while !rest.is_empty() {
        // `SO_SNDTIMEO` gilt je Aufruf; die Restzeit macht daraus eine
        // Grenze für den ganzen Frame.
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or(std::io::ErrorKind::TimedOut)?;
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(rest) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => rest = &rest[n..],
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Verbindet nichtblockierend, mit Frist.
///
/// `UnixStream::connect` blockiert ohne Obergrenze, sobald der Backlog des
/// Witness voll ist (Linux) — deshalb hier von Hand: Socket anlegen, auf
/// nichtblockierend stellen, verbinden. `EINPROGRESS` wird per `poll`
/// abgewartet; `EAGAIN` (voller Backlog unter Linux) heißt „gleich nochmal",
/// aber nur bis zur Frist.
///
/// macOS meldet einen vollen Backlog dagegen sofort als `ECONNREFUSED`. Ein
/// Schwall von mehr Hooks, als der Witness-Backlog fasst, erscheint dort im
/// Log deshalb als `connection refused`, obwohl der Witness lebt — der
/// Rückfall ist trotzdem richtig, nur die Diagnose ist unscharf.
#[cfg(unix)]
fn connect(
    path: &Path,
    deadline: std::time::Instant,
) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::io::{Error, ErrorKind};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::time::Instant;

    let bytes = path.as_os_str().as_bytes();
    // SAFETY: `sockaddr_un` ist eine C-Struktur, für die Nullen gültig sind.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    // Relativ hieße: aufgelöst gegen das `cwd` des Hooks, also meist gegen
    // das Repository — ein Ort, den jeder Checkout mitbestimmt.
    if !path.is_absolute() || bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        return Err(ErrorKind::InvalidInput.into());
    }
    trusted_dir(path)?;
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
        *dst = *src as libc::c_char;
    }
    let path_offset = std::mem::offset_of!(libc::sockaddr_un, sun_path);
    let len = (path_offset + bytes.len() + 1) as libc::socklen_t;

    // SAFETY: einfacher Systemaufruf; das Ergebnis wird sofort geprüft und an
    // `OwnedFd` übergeben, das ihn genau einmal schließt.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(Error::last_os_error());
    }
    // SAFETY: `raw` ist ein frisch geöffneter, uns allein gehörender Deskriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: fcntl auf einem gültigen, eigenen Deskriptor.
    unsafe {
        if libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
            return Err(Error::last_os_error());
        }
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) != 0 {
            return Err(Error::last_os_error());
        }
    }

    loop {
        // SAFETY: `addr` ist vollständig initialisiert, `len` deckt Familie,
        // Pfad und abschließendes NUL ab und liegt innerhalb der Struktur.
        let rc = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&raw const addr).cast::<libc::sockaddr>(),
                len,
            )
        };
        if rc == 0 {
            break;
        }
        let err = Error::last_os_error();
        match err.raw_os_error() {
            // Ein unterbrochenes `connect` läuft im Kern weiter; ein zweiter
            // Aufruf meldete `EALREADY`. Also abwarten wie bei `EINPROGRESS`.
            Some(libc::EINTR | libc::EINPROGRESS) => {
                wait_writable(&fd, deadline)?;
                break;
            }
            Some(libc::EAGAIN) => {
                if Instant::now() >= deadline {
                    return Err(ErrorKind::TimedOut.into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            _ => return Err(err),
        }
    }

    let stream = std::os::unix::net::UnixStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

/// Prüft den Ort des Sockets: Ist sein Verzeichnis für alle beschreibbar
/// (`/tmp`, auch mit Sticky-Bit), könnte dort jeder lokale Nutzer einen
/// eigenen Socket anlegen, solange kein Witness läuft — und bekäme die
/// Payloads. Eigentümer und Gruppe prüfen wir bewusst nicht: In den Profilen
/// `container` und `user` gehört `run/` legitim einem anderen Nutzer, und
/// Gruppenmitglieder gelten als vertraut.
///
/// Symlinks **im Verzeichnispfad** wird gefolgt (`metadata`): Maßgeblich ist
/// das Verzeichnis, in dem `connect` am Ende landet. Ein Symlink **am Socket
/// selbst** wird abgelehnt — er könnte aus einem geschützten `run/` heraus in
/// ein offenes Verzeichnis zeigen, das diese Prüfung nie sähe.
///
/// Ehrliche Grenzen: Geprüft wird nur das unmittelbare Verzeichnis, nicht
/// seine Vorfahren; zwischen Prüfung und `connect` liegt ein Zeitfenster; und
/// ACLs sieht der Modus-Test nicht. Das schließt den häufigen Fehler (Variable
/// zeigt nach `/tmp`) aus, ersetzt aber nicht ein richtig angelegtes
/// Witness-Home.
#[cfg(unix)]
fn trusted_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let parent = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let mode = std::fs::metadata(parent)?.permissions().mode();
    if mode & 0o002 != 0 {
        return Err(std::io::Error::other(UntrustedDir));
    }
    // Fehlt der Socket, entscheidet `connect` (`socket missing`).
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(std::io::Error::other(UntrustedDir));
    }
    Ok(())
}

/// Markiert die Fehlerart „Socket in einem für alle beschreibbaren
/// Verzeichnis" — damit [`kind`] sie von anderen I/O-Fehlern trennen kann.
#[derive(Debug)]
struct UntrustedDir;

impl std::fmt::Display for UntrustedDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("untrusted socket dir")
    }
}

impl std::error::Error for UntrustedDir {}

/// Wartet bis zur Frist darauf, dass ein laufendes `connect` fertig wird, und
/// liefert dessen Ergebnis aus `SO_ERROR`.
#[cfg(unix)]
fn wait_writable(fd: &std::os::fd::OwnedFd, deadline: std::time::Instant) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    use std::os::fd::AsRawFd;
    use std::time::Instant;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ErrorKind::TimedOut.into());
        }
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // Aufrunden: Eine Restzeit unter einer Millisekunde darf nicht zu
        // `poll(0)` werden und damit zur Schleife ohne Pause.
        let millis = remaining.as_micros().div_ceil(1000).min(i32::MAX as u128) as libc::c_int;
        // SAFETY: genau ein gültiges `pollfd`.
        match unsafe { libc::poll(&raw mut poll, 1, millis) } {
            0 => continue,
            n if n < 0 => {
                let err = Error::last_os_error();
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            _ => break,
        }
    }
    let mut code: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `code` und `size` sind passend dimensionierte, beschreibbare Werte.
    let rc = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&raw mut code).cast(),
            &raw mut size,
        )
    };
    if rc != 0 {
        return Err(Error::last_os_error());
    }
    if code != 0 {
        return Err(Error::from_raw_os_error(code));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    fn short_dir() -> tempfile::TempDir {
        // sockaddr_un ist auf macOS auf 104 Bytes begrenzt.
        tempfile::Builder::new()
            .prefix("mh-")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[test]
    fn a_frame_reaches_a_listening_witness_unchanged() {
        let dir = short_dir();
        let path = dir.path().join("w.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let reader = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            bytes
        });

        forward(&path, "claude-code", Some("Stop"), b"{\"a\":1}".to_vec()).unwrap();

        let bytes = reader.join().unwrap();
        let (frame, used) = witness_proto::decode(&bytes).unwrap();
        assert_eq!(used, bytes.len(), "genau ein Frame, nichts dahinter");
        assert_eq!(
            frame,
            Frame::Hook {
                agent: "claude-code".into(),
                event_override: Some("Stop".into()),
                stdin: b"{\"a\":1}".to_vec(),
            }
        );
    }

    #[test]
    fn every_failure_has_a_fixed_kind() {
        let dir = short_dir();
        let missing = dir.path().join("missing.sock");
        assert_eq!(forward(&missing, "a", None, vec![]), Err("socket missing"));

        let stale = dir.path().join("stale.sock");
        drop(UnixListener::bind(&stale).unwrap());
        // Ein Kindprozess, den ein parallel laufender Test gerade startet, kann
        // den gebundenen Socket kurz erben (macOS hat kein `SOCK_CLOEXEC`) —
        // solange er lebt, nimmt der Socket noch an. Erst wenn er wirklich tot
        // ist, zählt die Antwort.
        let deadline = Instant::now() + Duration::from_secs(5);
        let stale_result = loop {
            let result = forward(&stale, "a", None, vec![]);
            if result.is_err() || Instant::now() >= deadline {
                break result;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(stale_result, Err("connection refused"));

        let long = Path::new("/tmp").join("x".repeat(200));
        assert_eq!(
            forward(&long, "a", None, vec![]),
            Err("invalid socket path")
        );

        assert_eq!(forward(&stale, "../a", None, vec![]), Err("invalid frame"));
        let huge = vec![b' '; witness_proto::MAX_FRAME as usize];
        assert_eq!(forward(&stale, "a", None, huge), Err("frame too large"));
        assert_eq!(
            forward(&stale, "witness", None, vec![]),
            Err("reserved agent")
        );

        let relative = Path::new("w.sock");
        assert_eq!(
            forward(relative, "a", None, vec![]),
            Err("invalid socket path")
        );

        // Ein für alle beschreibbares Verzeichnis: Dort wird gar nicht erst
        // verbunden — auch nicht zu einem Socket, der dort lauscht.
        use std::os::unix::fs::PermissionsExt;
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let planted = open.join("w.sock");
        let listener = UnixListener::bind(&planted).unwrap();
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            forward(&planted, "a", None, vec![]),
            Err("untrusted socket dir")
        );
        assert!(listener.accept().is_err(), "keine Verbindung zum Fremden");

        // Ein Symlink am Socket aus einem geschützten Verzeichnis heraus in
        // das offene: ebenso abgelehnt, ebenso ohne Verbindung.
        let link = dir.path().join("link.sock");
        std::os::unix::fs::symlink(&planted, &link).unwrap();
        assert_eq!(
            forward(&link, "a", None, vec![]),
            Err("untrusted socket dir")
        );
        assert!(listener.accept().is_err(), "keine Verbindung über den Link");
    }

    /// Ein Witness-Ersatz, der genau einen Frame liest und `reply` zurückgibt.
    fn answering(path: &Path, reply: Vec<u8>) -> std::thread::JoinHandle<Frame> {
        use std::io::Write;
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 256];
            let frame = loop {
                let n = stream.read(&mut chunk).unwrap();
                bytes.extend_from_slice(&chunk[..n]);
                if let Ok((frame, _)) = witness_proto::decode(&bytes) {
                    break frame;
                }
            };
            // Der Client darf mitten in einer zu großen Antwort auflegen.
            let _ = stream.write_all(&reply);
            frame
        })
    }

    fn checkpoint_request(id: u8) -> Frame {
        Frame::CheckpointRequest {
            commit: None,
            request_id: [id; 16],
        }
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn a_request_returns_the_matching_answer() {
        let dir = short_dir();
        let path = dir.path().join("w.sock");
        let ack = Frame::Ack {
            request_id: [7; 16],
            status: "sealed".into(),
        };
        let witness = answering(&path, witness_proto::encode(&ack).unwrap());

        assert_eq!(request(&path, &checkpoint_request(7), soon()), Ok(ack));
        assert_eq!(witness.join().unwrap(), checkpoint_request(7));
    }

    #[test]
    fn a_foreign_or_oversized_answer_is_unexpected() {
        let dir = short_dir();
        let path = dir.path().join("a.sock");
        let other = Frame::Nack {
            request_id: [1; 16],
            reason: "no".into(),
        };
        let witness = answering(&path, witness_proto::encode(&other).unwrap());
        assert_eq!(
            request(&path, &checkpoint_request(2), soon()),
            Err("unexpected response")
        );
        witness.join().unwrap();

        // Ein Kopf, der mehr ankündigt, als der Client je lesen würde.
        let path = dir.path().join("b.sock");
        let mut huge = witness_proto::MAGIC.to_vec();
        huge.extend_from_slice(&(1024 * 1024u32).to_le_bytes());
        huge.extend(std::iter::repeat_n(0x81, 16 * 1024));
        let witness = answering(&path, huge);
        assert_eq!(
            request(&path, &checkpoint_request(2), soon()),
            Err("unexpected response")
        );
        witness.join().unwrap();

        // Nur Anfragen mit `request_id` erwarten eine Antwort.
        assert_eq!(request(&path, &Frame::Ping, soon()), Err("invalid frame"));
    }

    #[test]
    fn a_witness_that_never_answers_costs_the_deadline() {
        let dir = short_dir();
        let path = dir.path().join("mute.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let _held = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(2));
            drop(stream);
        });
        let started = Instant::now();
        let deadline = started + Duration::from_millis(200);

        assert_eq!(
            request(&path, &checkpoint_request(3), deadline),
            Err("timeout")
        );
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(200), "zu früh: {took:?}");
        assert!(took < Duration::from_millis(1500), "zu lange: {took:?}");
    }

    #[test]
    fn admissible_agrees_with_the_encoder_at_the_frame_limit() {
        // Die Vorprüfung entscheidet, ob kopiert wird; sie darf weder einen
        // Frame ablehnen, den `encode` annähme, noch umgekehrt.
        let max = witness_proto::MAX_FRAME as usize;
        for (agent, event) in [("a", None), ("claude-code", Some("PostToolUse"))] {
            let fits = max - HOOK_OVERHEAD - agent.len() - event.map_or(0, str::len);
            for len in [fits, fits + 1] {
                let frame = Frame::Hook {
                    agent: agent.into(),
                    event_override: event.map(Into::into),
                    stdin: vec![b' '; len],
                };
                assert_eq!(
                    admissible(agent, event, len).is_ok(),
                    witness_proto::encode(&frame).is_ok(),
                    "{agent} {event:?} {len}"
                );
            }
        }
    }

    #[test]
    fn a_witness_that_never_reads_costs_a_bounded_time() {
        let dir = short_dir();
        let path = dir.path().join("hang.sock");
        // Gebunden, aber nie `accept` und nie gelesen: Die Verbindung landet im
        // Backlog, der Frame füllt den Puffer, dann steht `write`.
        let _listener = UnixListener::bind(&path).unwrap();
        let started = Instant::now();
        let result = forward(&path, "a", None, vec![b'x'; 4 * 1024 * 1024]);
        let took = started.elapsed();
        assert_eq!(result, Err("timeout"));
        assert!(took < Duration::from_millis(500), "zu lange: {took:?}");
        assert!(
            took >= WRITE_TIMEOUT,
            "die Frist gilt für den ganzen Frame: {took:?}"
        );
    }
}
