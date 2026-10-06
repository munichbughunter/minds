//! Der Witness-Checkpoint als eigener Prozess (EA-10).
//!
//! Seit EA-06d kann die Agent-Seite den Checkpoint jederzeit auslösen, und
//! der Witness arbeitet dann auf dem Host in einem `.git`, das der Agent
//! kontrolliert. Was dort schiefgehen kann, ohne dass eine Vorprüfung es
//! verhindert, trifft seit EA-10 einen **Kindprozess**, nicht den einzigen
//! Schreiber:
//!
//! - ein Include oder Ref, das zwischen Vorprüfung und Zugriff zum FIFO wird
//!   und gix oder `git` blockiert — die Frist ([`DEADLINE`]) beendet den Lauf;
//! - `SIGBUS`, wenn der Agent eine gemappte `packed-refs`- oder Pack-Datei
//!   mitten im Lauf kürzt — es stirbt das Kind, nicht der Witness;
//! - die Endlosschleife in gix-ref 0.65 (`store/file/transaction/prepare.rs`),
//!   wenn ein Lock zwischen Probe und Übernahme auftaucht — die Frist;
//! - Rechen- und Hash-Volumen eines Laufs — CPU-Grenze und Frist.
//!
//! ```text
//!   witness (Eltern) ──stdin: {"commit","stream"}──► minds witness __checkpoint
//!        │            ◄──stdout: L <log> … D <json> | E <grund>──
//!        └── schließt stdin, sobald der Anfragende gegangen ist
//! ```
//!
//! Das Kind schreibt nichts ins eigene Log: Seine Diagnosen gehen als `L`-Zeilen
//! an den Elternprozess, der sie dedupliziert und bereinigt ablegt (URL-Zugangsdaten
//! entfernt, Steuerzeichen entschärft, gekürzt — wie `hook.log`; kein volles
//! Secret-Scanning). Es hält
//! keinen Lock — den hält der Elternprozess, der während des Laufs wartet und
//! nichts anhängt.
//!
//! Die Live-Folds leitet das Kind **nicht** aus dem Journal neu ab: Es lädt den
//! Stand, den der Elternprozess nach jedem Append persistiert hat. So prüft
//! `validate` weiterhin, dass das Journal genau das enthält, was der Witness
//! gefaltet hat — ein angehängter Schwanz fiele auf.

use super::*;
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Längste Laufzeit eines Checkpoints, danach wird die Prozessgruppe des
/// Kinds beendet. Die Agent-Seite wartet ohnehin höchstens 15 s; was
/// danach versiegelt wird, trailert der Witness nicht mehr.
pub(super) const DEADLINE: Duration = Duration::from_secs(60);

/// CPU-Sekunden des Kinds — eine zweite, von der Wanduhr unabhängige Grenze.
const CPU_SECONDS: libc::rlim_t = 60;
/// Größte Datei, die das Kind schreiben darf (Store-Objekte, Packs).
const FILE_BYTES: libc::rlim_t = 1024 * 1024 * 1024;
/// Offene Deskriptoren des Kinds.
const OPEN_FILES: libc::rlim_t = 512;
/// Datensegment des Kinds — ein riesiger Objekt-Header oder ein aufgeblähter
/// Split-Index fordert sonst beliebig viel Speicher an. Gemappte Pack-Dateien
/// zählen nicht dazu.
#[cfg(target_os = "linux")]
const DATA_BYTES: libc::rlim_t = 4 * 1024 * 1024 * 1024;

/// So viel liest der Elternprozess höchstens von der Ausgabe des Kinds.
const MAX_OUTPUT: usize = 1024 * 1024;
/// Längste Anfragezeile, die das Kind liest.
const MAX_REQUEST: u64 = 4096;

/// Ob dieser Prozess der Worker ist: Dann gehen Logzeilen an den
/// Elternprozess statt in die Datei.
static WORKER: AtomicBool = AtomicBool::new(false);

pub(super) fn is_worker() -> bool {
    WORKER.load(Ordering::Relaxed)
}

#[derive(Serialize, Deserialize)]
struct Request {
    commit: String,
    /// Die lokale Id des eigenen Stroms (`agent = "witness"`), dessen
    /// Beobachtungs-Epoche dieser Checkpoint schließt.
    stream: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Done {
    status: String,
    retrofitted: Option<String>,
}

/// Was ein Lauf im Kind ergab, aus Sicht des Elternprozesses.
pub(super) enum Failure {
    /// Das Kind hat den Lauf mit diesem Grund beendet — wie ein `Err` aus
    /// dem Lauf im eigenen Prozess.
    Failed(String),
    /// Das Kind starb, überschritt die Frist oder antwortete nicht lesbar.
    Aborted(&'static str),
}

/// Startet den Worker und wartet auf ihn — höchstens [`DEADLINE`].
///
/// `requester_alive` wird während des Wartens befragt; ist der Anfragende
/// gegangen, schließt der Elternprozess die Standardeingabe des Kinds, und
/// das Kind trailert nicht mehr.
pub(super) fn run(
    exe: &Path,
    home: &Path,
    commit: &str,
    stream: Option<&SessionKey>,
    requester_alive: &dyn Fn() -> bool,
) -> Result<Ran, Failure> {
    run_within(exe, home, commit, stream, requester_alive, DEADLINE)
}

fn run_within(
    exe: &Path,
    home: &Path,
    commit: &str,
    stream: Option<&SessionKey>,
    requester_alive: &dyn Fn() -> bool,
    deadline: Duration,
) -> Result<Ran, Failure> {
    let request = serde_json::to_string(&Request {
        commit: commit.to_owned(),
        stream: stream.map(|key| key.local_id().to_owned()),
    })
    .map_err(|_| Failure::Aborted("worker request not encodable"))?;
    let mut command = Command::new(exe);
    command
        .args(["witness", crate::witness_cmd::WORKER_COMMAND, "--home"])
        .arg(home)
        .env_remove("MINDS_WITNESS_SOCKET")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: zwischen fork und exec laufen nur async-signal-sichere
    // Systemaufrufe ohne Allokation.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // `RLIMIT_DATA` nur unter Linux: macOS setzt es nicht durch und
            // lehnt das Setzen ab.
            #[cfg(target_os = "linux")]
            let data = [(libc::RLIMIT_DATA, DATA_BYTES)];
            #[cfg(not(target_os = "linux"))]
            let data: [(libc::c_int, libc::rlim_t); 0] = [];
            for (resource, limit) in [
                (libc::RLIMIT_CPU, CPU_SECONDS),
                (libc::RLIMIT_FSIZE, FILE_BYTES),
                (libc::RLIMIT_NOFILE, OPEN_FILES),
                (libc::RLIMIT_CORE, 0),
            ]
            .into_iter()
            .chain(data)
            {
                let mut current = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::getrlimit(resource, &mut current) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Nie über das hinaus, was der Witness selbst darf.
                let wanted = libc::rlimit {
                    rlim_cur: limit.min(current.rlim_max),
                    rlim_max: limit.min(current.rlim_max),
                };
                if libc::setrlimit(resource, &wanted) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            // Stirbt der Witness, stirbt der Lauf mit ihm (nur Linux kennt das).
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| Failure::Aborted("checkpoint worker did not start"))?;
    let group = child.id() as libc::pid_t;
    // Nur solange das Kind nicht eingesammelt ist: Bis dahin hält es seine
    // Prozess-Id und damit die Gruppe — ein später Kill träfe sonst womöglich
    // eine fremde, inzwischen gleich nummerierte Gruppe.
    let signal_group = |signal: libc::c_int| {
        // SAFETY: negative pid adressiert die eigene Prozessgruppe des Kinds.
        unsafe {
            libc::kill(-group, signal);
        }
    };
    // An der Frist erst `SIGTERM`: Das Kind räumt die Lock-Dateien weg, die
    // gix gerade hält (`HEAD.lock`, `refs/minds/….lock`). Wer nicht in der
    // Gnadenfrist endet, bekommt `SIGKILL` — immer vor dem Einsammeln.
    let abort = |child: &mut std::process::Child| {
        signal_group(libc::SIGTERM);
        let grace = Instant::now();
        while grace.elapsed() < GRACE {
            // `try_wait` sammelt ein; die Gruppe ist dann nur noch Enkel —
            // deren Kill nach dem Einsammeln wäre der riskante Fall.
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        signal_group(libc::SIGKILL);
        let _ = child.wait();
    };
    let mut stdin = child.stdin.take();
    if let Some(pipe) = stdin.as_mut() {
        if writeln!(pipe, "{request}").is_err() {
            abort(&mut child);
            return Err(Failure::Aborted("checkpoint worker did not start"));
        }
    }
    let mut stdout = child.stdout.take().expect("stdout ist eine Pipe");
    let started = Instant::now();
    let mut output = Output::default();
    let mut eof = false;
    while !eof {
        let left = deadline.saturating_sub(started.elapsed());
        if left.is_zero() {
            abort(&mut child);
            return Err(Failure::Aborted("checkpoint worker exceeded its deadline"));
        }
        let mut poll = libc::pollfd {
            fd: stdout.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let wait = left.min(Duration::from_millis(50)).as_millis() as libc::c_int;
        // SAFETY: genau ein gültiges `pollfd`.
        let ready = unsafe { libc::poll(&raw mut poll, 1, wait) };
        if ready > 0 {
            let mut chunk = [0u8; 8192];
            match stdout.read(&mut chunk) {
                Ok(0) => eof = true,
                Ok(n) => output.feed(&chunk[..n], home),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => eof = true,
            }
        }
        if stdin.is_some() && !requester_alive() {
            // Geschlossene Eingabe heißt für das Kind: nicht mehr trailern.
            stdin = None;
        }
    }
    drop(stdin);
    // Die Ausgabe ist zu, der Prozess endet gleich — aber nicht später als
    // die Frist. Ein Enkel, der die Ausgabe noch offen hielte, endet mit der
    // Gruppe an der Frist; nach einem Ende aus eigenem Antrieb hat das Kind
    // seine Kindprozesse (ssh-keygen) selbst eingesammelt.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                abort(&mut child);
                return Err(Failure::Aborted("checkpoint worker exceeded its deadline"));
            }
        }
    };
    output.finish(home);
    match output.answer {
        _ if output.answers > 1 => Err(Failure::Aborted("checkpoint worker answered twice")),
        Some(Ok(done)) if status.success() => Ok(Ran {
            status: done.status,
            retrofitted: done.retrofitted,
        }),
        Some(Err(reason)) if status.success() => Err(Failure::Failed(reason)),
        _ if status.code().is_none() => Err(Failure::Aborted("checkpoint worker was killed")),
        _ => Err(Failure::Aborted("checkpoint worker failed")),
    }
}

/// Gnadenfrist zwischen `SIGTERM` und `SIGKILL` an der Frist.
const GRACE: Duration = Duration::from_secs(2);
/// Längste Zeile, die der Witness vom Kind annimmt; was länger ist, wird
/// verworfen — nie abgeschnitten weitergegeben.
const MAX_LINE: usize = 64 * 1024;

/// Die Ausgabe des Kinds, zeilenweise ausgewertet, während sie ankommt.
///
/// - `L`-Zeilen gehen ins eigene Log, bis [`MAX_OUTPUT`] Bytes erreicht
///   sind; danach nur noch eine feste Zeile. Eine Zeile wird nie
///   abgeschnitten weitergegeben: Ein halbiertes Geheimnis fiele unter die
///   Muster der Redaction.
/// - Die Antwort (`D`/`E`) wird immer ausgewertet, auch nach einer Flut von
///   Logzeilen. Mehr als eine Antwort ist ein Fehler, keine zählt doppelt.
#[derive(Default)]
struct Output {
    pending: Vec<u8>,
    overlong: bool,
    logged: usize,
    dropped: usize,
    answer: Option<Result<Done, String>>,
    answers: usize,
}

impl Output {
    fn feed(&mut self, mut bytes: &[u8], home: &Path) {
        while let Some(end) = bytes.iter().position(|b| *b == b'\n') {
            if self.pending.len() + end > MAX_LINE {
                self.overlong = true;
            }
            if !self.overlong {
                self.pending.extend_from_slice(&bytes[..end]);
                let line = std::mem::take(&mut self.pending);
                self.line(&line, home);
            } else {
                self.dropped += 1;
            }
            self.overlong = false;
            self.pending.clear();
            bytes = &bytes[end + 1..];
        }
        if !self.overlong {
            self.pending.extend_from_slice(bytes);
            if self.pending.len() > MAX_LINE {
                self.overlong = true;
                self.pending = Vec::new();
            }
        }
    }

    fn line(&mut self, line: &[u8], home: &Path) {
        let line = String::from_utf8_lossy(line);
        if let Some(message) = line.strip_prefix("L ") {
            if self.logged + message.len() <= MAX_OUTPUT {
                self.logged += message.len();
                log(home, message);
            } else {
                self.dropped += 1;
            }
        } else if let Some(json) = line.strip_prefix("D ") {
            self.answers += 1;
            if self.answer.is_none() {
                self.answer = serde_json::from_str::<Done>(json).ok().map(Ok);
            }
        } else if let Some(reason) = line.strip_prefix("E ") {
            self.answers += 1;
            if self.answer.is_none() {
                self.answer = Some(Err(reason.to_owned()));
            }
        }
    }

    /// Eine unvollständige letzte Zeile zählt nicht; verworfene Zeilen
    /// erscheinen als eine feste Zeile im Log.
    fn finish(&mut self, home: &Path) {
        if !self.pending.is_empty() || self.overlong {
            self.dropped += 1;
        }
        if self.dropped > 0 {
            log(
                home,
                &format!("worker output truncated: {} line(s) dropped", self.dropped),
            );
        }
    }
}

/// Der Kindprozess: `minds witness __checkpoint --home <home>`.
///
/// Gibt nur dann `Err` zurück, wenn die Anfrage selbst unbrauchbar ist; das
/// Ergebnis des Laufs steht auf stdout.
pub(crate) fn worker(home: &Path) -> Fallible<()> {
    WORKER.store(true, Ordering::Relaxed);
    let home: PathBuf = home.components().collect();
    install_panic_hook(&home);
    install_term_handler()?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .take(MAX_REQUEST)
        .read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line)?;
    let config = load(&home)?;
    let mut writer = Writer::for_worker(&home, config)?;
    writer.stream = request
        .stream
        .map(|id| SessionKey::new("witness", id))
        .transpose()?;
    let alive = stdin_open;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        writer.checkpoint_sessions(Some(&request.commit), &alive)
    }));
    let mut out = std::io::stdout().lock();
    match result {
        Ok(Ok(ran)) => writeln!(
            out,
            "D {}",
            serde_json::to_string(&Done {
                status: ran.status,
                retrofitted: ran.retrofitted,
            })?
        )?,
        Ok(Err(err)) => writeln!(out, "E {}", one_line(&err.to_string()))?,
        Err(_) => writeln!(out, "E checkpoint panicked")?,
    }
    out.flush()?;
    Ok(())
}

/// Ob der Elternprozess die Standardeingabe noch offen hält — er schließt
/// sie, sobald der Anfragende gegangen ist. Nach der Anfragezeile schickt er
/// nichts mehr; lesbar wird sie also erst mit dem Schließen.
fn stdin_open() -> bool {
    let mut poll = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: genau ein gültiges `pollfd`, Frist 0.
    let ready = unsafe { libc::poll(&raw mut poll, 1, 0) };
    ready == 0
}

/// Eine Zeile ohne Steuerzeichen — das Protokoll zum Elternprozess ist
/// zeilenweise.
pub(super) fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Im Witness und seinem Worker gibt ein Panic eine feste Zeile aus, nie die
/// Meldung aus gix — die kann Pfade und Inhalte aus dem Repo des Agenten
/// tragen. `catch_unwind` fängt danach wie bisher.
///
/// Der Hook schreibt ohne die Deduplizierung: Ein Panic, während deren
/// Mutex gehalten wird, nähme ihn sonst auf demselben Thread ein zweites Mal.
pub(super) fn install_panic_hook(home: &Path) {
    let home = home.to_path_buf();
    std::panic::set_hook(Box::new(move |_| {
        let _ = writeln!(std::io::stderr(), "minds witness: internal error");
        if is_worker() {
            println!("L internal error (panic)");
        } else {
            write_log(&home, "internal error (panic)");
        }
    }));
}

/// `SIGTERM` an der Frist: die Lock-Dateien entfernen, die gix gerade hält,
/// und sofort enden.
///
/// Best effort: gix nennt sein Aufräumen signal-sicher, aber das Entfernen
/// eines langen Pfads kann allokieren. Trifft das Signal das Kind mitten in
/// `malloc`, kann der Handler hängen — dann endet es mit `SIGKILL` nach der
/// Gnadenfrist, und der Witness nennt die liegengebliebenen Locks im Log.
fn install_term_handler() -> Fallible<()> {
    extern "C" fn on_term(_: libc::c_int) {
        minds_git::Repo::cleanup_lock_files_signal_safe();
        // SAFETY: `_exit` ist async-signal-sicher und kehrt nicht zurück.
        unsafe { libc::_exit(128 + libc::SIGTERM) }
    }
    // SAFETY: sigaction ist eine C-Struktur; sigemptyset initialisiert die
    // Maske, der Handler ist async-signal-sicher.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_term as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein Stub an Stelle des Binaries: Ein Shell-Skript bekommt dieselbe
    /// Aufrufzeile und dieselbe Anfrage auf stdin.
    fn stub(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("stub-worker");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn home(dir: &Path) -> PathBuf {
        let home = dir.join("home");
        fs::create_dir_all(home.join("log")).unwrap();
        home
    }

    fn run_stub(body: &str, alive: &dyn Fn() -> bool, deadline: Duration) -> Result<Ran, Failure> {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), body);
        run_within(&exe, &home(dir.path()), "abc", None, alive, deadline)
    }

    #[test]
    fn a_worker_answer_and_its_log_lines_reach_the_witness() {
        let dir = tempfile::tempdir().unwrap();
        let home = home(dir.path());
        let exe = stub(
            dir.path(),
            r#"read request
case "$request" in *'"commit":"abc"'*) ;; *) exit 3 ;; esac
echo 'L from the worker'
echo 'D {"status":"witness: 1 range(s) sealed","retrofitted":"def"}'"#,
        );
        let Ok(ran) = run_within(&exe, &home, "abc", None, &|| true, DEADLINE) else {
            panic!("worker failed");
        };
        assert_eq!(ran.status, "witness: 1 range(s) sealed");
        assert_eq!(ran.retrofitted.as_deref(), Some("def"));
        let log = fs::read_to_string(home.join("log/witness.log")).unwrap();
        assert!(log.contains("from the worker"), "{log}");
    }

    #[test]
    fn a_worker_failure_carries_its_reason() {
        match run_stub(
            "read r; echo 'E repository layout is not plain'",
            &|| true,
            DEADLINE,
        ) {
            Err(Failure::Failed(reason)) => assert_eq!(reason, "repository layout is not plain"),
            _ => panic!("expected a failure with reason"),
        }
    }

    /// SIGBUS (etwa über eine gekürzte, gemappte Pack-Datei) trifft das Kind.
    #[test]
    fn a_crashing_worker_does_not_take_the_witness_down() {
        match run_stub("read r; kill -BUS $$", &|| true, DEADLINE) {
            Err(Failure::Aborted(reason)) => assert_eq!(reason, "checkpoint worker was killed"),
            _ => panic!("expected an aborted run"),
        }
    }

    /// Ein blockierter Lauf (FIFO, gix-ref-Schleife) endet an der Frist —
    /// samt seiner Prozessgruppe.
    #[test]
    fn a_hanging_worker_is_killed_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild-alive");
        let body = format!("read r; (sleep 2; touch '{}') & sleep 30", marker.display());
        let started = Instant::now();
        match run_stub(&body, &|| true, Duration::from_millis(500)) {
            Err(Failure::Aborted(reason)) => {
                assert_eq!(reason, "checkpoint worker exceeded its deadline");
            }
            _ => panic!("expected the deadline"),
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_secs(3));
        assert!(!marker.exists(), "der Enkel lief nach der Frist weiter");
    }

    /// Ist der Anfragende gegangen, sieht das Kind seine Eingabe schließen.
    #[test]
    fn a_gone_requester_closes_the_worker_input() {
        let Ok(ran) = run_stub(
            r#"read request
while read more; do :; done
echo 'D {"status":"input closed","retrofitted":null}'"#,
            &|| false,
            Duration::from_secs(10),
        ) else {
            panic!("worker failed");
        };
        assert_eq!(ran.status, "input closed");
    }

    /// Eine überlange Logzeile wird verworfen, nie abgeschnitten weitergegeben
    /// — und die Antwort dahinter zählt trotzdem.
    #[test]
    fn an_overlong_line_is_dropped_and_the_answer_still_counts() {
        let dir = tempfile::tempdir().unwrap();
        let home = home(dir.path());
        let exe = stub(
            dir.path(),
            r#"read request
printf 'L '
head -c 70000 /dev/zero | tr '\0' 'a'
printf 'AKIAIOSFODNN7EXAMPLE\n'
echo 'D {"status":"sealed","retrofitted":null}'"#,
        );
        let Ok(ran) = run_within(&exe, &home, "abc", None, &|| true, DEADLINE) else {
            panic!("worker failed");
        };
        assert_eq!(ran.status, "sealed");
        let log = fs::read_to_string(home.join("log/witness.log")).unwrap();
        assert!(!log.contains("AKIA"), "kein Bruchstück: {log}");
        assert!(
            log.contains("worker output truncated: 1 line(s) dropped"),
            "{log}"
        );
    }

    /// Mehr als eine Antwort ist kein Ergebnis.
    #[test]
    fn two_answers_abort_the_run() {
        match run_stub(
            r#"read r
echo 'E first'
echo 'D {"status":"forged","retrofitted":null}'"#,
            &|| true,
            DEADLINE,
        ) {
            Err(Failure::Aborted(reason)) => assert_eq!(reason, "checkpoint worker answered twice"),
            _ => panic!("expected an aborted run"),
        }
    }

    #[test]
    fn one_line_strips_control_characters() {
        assert_eq!(one_line("a\nb\tc\u{1b}"), "a b c ");
    }
}
