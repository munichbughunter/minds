//! `minds hook` mit Witness (EA-07): weitergeben, wenn es geht; lokal
//! schreiben, wenn nicht — und in jedem Pfad Exit 0 ohne ein Byte auf stdout.

#![cfg(unix)]

#[path = "support/hook_witness.rs"]
mod support;

use std::io::Read;
use std::os::unix::net::UnixListener;
use std::time::Duration;

use minds_capture::witness_proto::{self, Frame};
use support::*;

/// Die Datei aus den Mauer-Tests in `secretwall.rs`.
const DOTENV_READ: &str = r#","tool_name":"Read","tool_input":{"file_path":".env"},"tool_response":"DB_PASSWORD=hunter2""#;

#[test]
fn hook_forwards_to_witness() {
    let f = Fixture::new();
    let _daemon = f.start();
    let stdin = f.payload("fwd", "UserPromptSubmit", r#","prompt":"sortiere stabil""#);

    let (out, _) = f.hook(Some(&f.socket()), stdin.as_bytes());

    assert_silent_success(&out);
    let events = wait_for_events(&f.witness_journal(), "fwd", 1);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].payload.get(),
        stdin,
        "Rohbytes kommen unverändert an"
    );
    assert_eq!(events[0].raw_kind, "UserPromptSubmit");
    assert!(
        !f.local_journal_dir().exists(),
        "mit Witness entsteht kein lokales Journal"
    );
    assert_eq!(f.hook_log(), None, "eine gelungene Weitergabe loggt nichts");
}

/// Ohne `MINDS_WITNESS_SOCKET` findet der Hook den Witness dieses Repos an
/// seinem Standardpfad (`run/witness.sock` im Witness-Home) — so, wie
/// `minds enable` ihn einrichtet.
#[test]
fn hook_finds_the_witness_without_the_variable() {
    let f = Fixture::with_profile("local");
    let _daemon = f.start();
    let stdin = f.payload("found", "UserPromptSubmit", r#","prompt":"ohne Variable""#);

    let mut cmd = minds();
    cmd.current_dir(&f.root)
        .args(["hook", "--agent", "claude-code"])
        .env("MINDS_WITNESS_HOME", &f.home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();

    assert_silent_success(&out);
    let events = wait_for_events(&f.witness_journal(), "found", 1);
    assert_eq!(events.len(), 1);
    assert!(!f.local_journal_dir().exists(), "kein lokales Journal");
}

/// Über den Standardpfad gefunden, aber das Event gehört zu einem anderen
/// Repo (das `cwd` im Payload): Es geht nicht an diesen Witness, sondern ins
/// Journal seines Repos.
#[test]
fn hook_keeps_events_of_another_repository_away_from_the_witness() {
    let f = Fixture::with_profile("local");
    let _daemon = f.start();
    let other = Fixture::repo_only();
    let stdin = other.payload(
        "elsewhere",
        "UserPromptSubmit",
        r#","prompt":"anderes Repo""#,
    );

    let mut cmd = minds();
    cmd.current_dir(&f.root)
        .args(["hook", "--agent", "claude-code"])
        .env("MINDS_WITNESS_HOME", &f.home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();

    assert_silent_success(&out);
    assert_eq!(events(&other.local_journal(), "elsewhere").len(), 1);
    std::thread::sleep(Duration::from_millis(300));
    assert!(events(&f.witness_journal(), "elsewhere").is_empty());
}

/// Am echten Standardpfad (`$XDG_STATE_HOME/minds-witness/<repo-id>`), ganz
/// ohne Variable — so, wie `minds enable` ihn einrichtet.
#[test]
fn hook_finds_the_witness_at_the_default_path() {
    let f = Fixture::local_default();
    let _daemon = f.start();
    let stdin = f.payload("default", "UserPromptSubmit", r#","prompt":"Standardpfad""#);

    let out = run_hook(&f, &stdin, &[("XDG_STATE_HOME", f.xdg_state().as_os_str())]);

    assert_silent_success(&out);
    assert_eq!(wait_for_events(&f.witness_journal(), "default", 1).len(), 1);
    assert!(!f.local_journal_dir().exists(), "kein lokales Journal");
}

/// Ein Witness eines anderen Profils (hier `user`) bezeugt eine Trennung —
/// ein Hook außerhalb davon findet ihn nicht über den Standardpfad, nur über
/// die ausdrückliche `MINDS_WITNESS_SOCKET`. Ebenso ein Home, das ein
/// anderes Repo pinnt.
#[test]
fn hook_discovers_only_a_local_witness_of_this_repository() {
    let f = Fixture::new();
    let _daemon = f.start();
    let stdin = f.payload("separated", "UserPromptSubmit", r#","prompt":"getrennt""#);
    let out = run_hook(&f, &stdin, &[("MINDS_WITNESS_HOME", f.home.as_os_str())]);
    assert_silent_success(&out);
    assert_eq!(events(&f.local_journal(), "separated").len(), 1);

    let other = Fixture::with_profile("local");
    let _other_daemon = other.start();
    let stdin = f.payload("foreign", "UserPromptSubmit", r#","prompt":"fremdes Home""#);
    let out = run_hook(
        &f,
        &stdin,
        &[("MINDS_WITNESS_HOME", other.home.as_os_str())],
    );
    assert_silent_success(&out);
    assert_eq!(events(&f.local_journal(), "foreign").len(), 1);

    std::thread::sleep(Duration::from_millis(300));
    assert!(events(&f.witness_journal(), "separated").is_empty());
    assert!(events(&other.witness_journal(), "foreign").is_empty());
}

/// Ein Home, das andere beschreiben können (hier `witness.json` mit 0666),
/// könnte ein anderer Nutzer angelegt haben: keine Entdeckung, lokales
/// Journal.
#[test]
fn hook_ignores_a_witness_home_others_can_write() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::with_profile("local");
    let _daemon = f.start();
    let config = f.home.join("witness.json");
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o666)).unwrap();
    let stdin = f.payload("writable", "UserPromptSubmit", r#","prompt":"offen""#);
    let out = run_hook(&f, &stdin, &[("MINDS_WITNESS_HOME", f.home.as_os_str())]);
    assert_silent_success(&out);
    assert_eq!(events(&f.local_journal(), "writable").len(), 1);
    std::thread::sleep(Duration::from_millis(300));
    assert!(events(&f.witness_journal(), "writable").is_empty());
}

/// `minds hook` im Repo von `f`, mit `stdin` und weiteren Variablen.
fn run_hook(f: &Fixture, stdin: &str, vars: &[(&str, &std::ffi::OsStr)]) -> std::process::Output {
    let mut cmd = minds();
    cmd.current_dir(&f.root)
        .args(["hook", "--agent", "claude-code"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in vars {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn hook_unset_env_is_unchanged() {
    let f = Fixture::repo_only();
    let stdin = f.payload("plain", "UserPromptSubmit", r#","prompt":"wie bisher""#);

    let (unset, _) = f.hook(None, stdin.as_bytes());
    // Leer zählt als nicht gesetzt — sonst wäre `MINDS_WITNESS_SOCKET=` ein
    // Rückfall mit Logzeile bei jedem Event.
    let (empty, _) = f.hook(Some("".as_ref()), stdin.as_bytes());

    assert_silent_success(&unset);
    assert_silent_success(&empty);
    let events = events(&f.local_journal(), "plain");
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(
            event.payload.get(),
            stdin,
            "Payload Byte für Byte wie heute"
        );
        assert_eq!(event.raw_kind, "UserPromptSubmit");
        assert_eq!(event.cwd.as_deref(), f.root.to_str());
    }
    assert_eq!(events[0].kind, events[1].kind);
    assert_eq!(f.hook_log(), None, "ohne Witness kein Logeintrag");
}

/// Eine verwaiste Socket-Datei, an der Verbinden mit ECONNREFUSED scheitert
/// — geprüft, nicht angenommen.
///
/// macOS kennt kein atomares `SOCK_CLOEXEC`: Zwischen `socket()` und dem
/// Setzen von `FD_CLOEXEC` kann ein parallel gestarteter Prozess (die Tests
/// hier starten Witness-Daemons) den Socket erben. Dann lauscht er nach
/// `drop` weiter, der Hook verbindet sich erfolgreich, und das Event käme
/// nie im Journal an. Unter Last ist das Fenster breiter — daher der Test
/// selbst: verwaist erst, wenn Verbinden abgewiesen wird; sonst ein neuer
/// Pfad.
fn stale_socket(dir: &std::path::Path) -> std::path::PathBuf {
    for attempt in 0..20 {
        let stale = dir.join(format!("stale-{attempt}.sock"));
        drop(UnixListener::bind(&stale).unwrap());
        match std::os::unix::net::UnixStream::connect(&stale) {
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => return stale,
            // Geerbt und noch offen: nicht verwaist — nächster Pfad.
            _ => continue,
        }
    }
    panic!("no stale socket after 20 attempts — something keeps inheriting them");
}

#[test]
fn hook_falls_back_when_witness_down() {
    let f = Fixture::repo_only();
    let missing = f.dir.path().join("missing.sock");
    // Eine verwaiste Socket-Datei: Verbinden scheitert mit ECONNREFUSED.
    let stale = stale_socket(f.dir.path());

    for (n, (socket, kind)) in [(&missing, "socket missing"), (&stale, "connection refused")]
        .into_iter()
        .enumerate()
    {
        let session = format!("down-{n}");
        let stdin = f.payload(
            &session,
            "UserPromptSubmit",
            r#","prompt":"PAYLOAD_SENTINEL""#,
        );

        let (out, took) = f.hook(Some(socket), stdin.as_bytes());

        assert_silent_success(&out);
        assert!(took < Duration::from_secs(5), "{kind}: {took:?}");
        let events = events(&f.local_journal(), &session);
        assert_eq!(events.len(), 1, "{kind}: das Event geht ins lokale Journal");
        assert_eq!(events[0].payload.get(), stdin);
        let log = f.hook_log().expect("der Rückfall steht im Log");
        assert!(
            log.contains(&format!("witness unreachable: {kind}")),
            "{log}"
        );
        assert!(
            !log.contains("PAYLOAD_SENTINEL"),
            "kein Payload im Log: {log}"
        );
        assert!(!log.contains(".sock"), "kein Socket-Pfad im Log: {log}");
    }
    let log = f.hook_log().unwrap();
    assert_eq!(log.lines().count(), 2, "eine Zeile je Rückfall: {log}");
}

#[test]
fn hook_falls_back_when_witness_hangs() {
    let f = Fixture::repo_only();
    let socket = f.dir.path().join("hang.sock");
    // Gebunden, aber nie `accept`, nie gelesen: Die Verbindung gelingt, das
    // Schreiben steht, sobald der Socket-Puffer voll ist. Deshalb ein großes
    // Tool-Ergebnis — ein kleines Event passte in den Puffer und gälte, wie
    // im Protokoll vorgesehen, als abgegeben.
    let _listener = UnixListener::bind(&socket).unwrap();
    let big = "x".repeat(2 * 1024 * 1024);
    let stdin = f.payload(
        "hang",
        "PostToolUse",
        &format!(
            r#","tool_name":"Bash","tool_input":{{"command":"cat big"}},"tool_response":"{big}""#
        ),
    );

    let (out, took) = f.hook(Some(&socket), stdin.as_bytes());

    assert_silent_success(&out);
    // Die Fristen sind 50 ms + 100 ms; der Rest ist Prozessstart, Parsen und
    // das Rückfall-Journal im Debug-Build.
    assert!(took < Duration::from_secs(3), "der Hook hing: {took:?}");
    let events = events(&f.local_journal(), "hang");
    assert_eq!(events.len(), 1, "das Event geht ins lokale Journal");
    assert_eq!(events[0].payload.get(), stdin);
    let log = f.hook_log().expect("der Rückfall steht im Log");
    assert!(log.contains("witness unreachable: timeout"), "{log}");
}

/// K1/M3: Ein `transcript_path`, der eine FIFO ohne Schreiber ist, hält den
/// Hook nicht fest — Exit 0, Event im Journal, nur ohne Modell.
#[test]
fn hook_survives_a_transcript_path_that_is_a_fifo() {
    let f = Fixture::repo_only();
    let fifo = f.dir.path().join("t.jsonl");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: gültiger, NUL-terminierter Pfad.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let stdin = f.payload(
        "fifo",
        "Stop",
        &format!(r#","transcript_path":"{}""#, fifo.display()),
    );

    let (out, took) = f.hook(None, stdin.as_bytes());

    assert_silent_success(&out);
    assert!(took < Duration::from_secs(3), "der Hook hing: {took:?}");
    let events = events(&f.local_journal(), "fifo");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].raw_kind, "Stop");
    assert_eq!(events[0].model, None);
}

#[test]
fn hook_secret_wall_runs_before_forwarding() {
    let f = Fixture::new();
    let stdin = f.payload("wall", "PostToolUse", DOTENV_READ);

    // 1. Was über den Socket geht: ein Lauscher statt des Witness, denn der
    //    Witness wallt selbst noch einmal und verdeckte damit einen Fehler hier.
    let spy = f.dir.path().join("spy.sock");
    let listener = UnixListener::bind(&spy).unwrap();
    let reader = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let (out, _) = f.hook(Some(&spy), stdin.as_bytes());
    assert_silent_success(&out);
    let wire = reader.join().unwrap();
    assert!(
        !String::from_utf8_lossy(&wire).contains("hunter2"),
        "das Geheimnis überquert den Socket nicht"
    );
    let Frame::Hook { stdin: sent, .. } = witness_proto::decode(&wire).unwrap().0 else {
        panic!("kein Hook-Frame");
    };
    let sent = String::from_utf8(sent).unwrap();
    assert!(!sent.contains("tool_response"), "{sent}");
    assert!(sent.contains("[omitted:secret-file]"), "{sent}");
    assert!(sent.contains(".env"), "der Pfad bleibt: {sent}");

    // 2. Was der Witness daraus macht, ist dasselbe Event wie im lokalen
    //    Journal ohne Witness — Byte für Byte im Payload.
    let (out, _) = f.hook(None, stdin.as_bytes());
    assert_silent_success(&out);
    let local = events(&f.local_journal(), "wall");
    assert_eq!(local.len(), 1);

    let _daemon = f.start();
    let (out, _) = f.hook(Some(&f.socket()), stdin.as_bytes());
    assert_silent_success(&out);
    let witnessed = wait_for_events(&f.witness_journal(), "wall", 1);
    assert_eq!(witnessed[0].payload.get(), local[0].payload.get());
    assert_eq!(witnessed[0].raw_kind, local[0].raw_kind);
    assert_eq!(witnessed[0].kind, local[0].kind);
    assert_eq!(witnessed[0].cwd, local[0].cwd);
    assert!(!witnessed[0].payload.get().contains("hunter2"));
    assert_eq!(f.hook_log(), None);
}

#[test]
fn hook_partial_frame_is_never_recorded_twice() {
    // Die Zusage aus `hook/witness.rs`: Ein abgebrochener Frame landet allein
    // im Rückfall-Journal, nie zusätzlich beim Witness. Gegen den echten
    // Daemon, angehalten statt ersetzt — eine spätere Änderung, die
    // Teil-Frames beim EOF „rettet", würde hier rot.
    let f = Fixture::new();
    let daemon = f.start();
    let pid = daemon.0.id() as i32;
    // SAFETY: Der Test signalisiert ausschließlich seinen eigenen Kindprozess.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);

    let big = "x".repeat(2 * 1024 * 1024);
    let stdin = f.payload(
        "partial",
        "PostToolUse",
        &format!(
            r#","tool_name":"Bash","tool_input":{{"command":"cat big"}},"tool_response":"{big}""#
        ),
    );
    let (out, _) = f.hook(Some(&f.socket()), stdin.as_bytes());

    // SAFETY: wie oben.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
    assert_silent_success(&out);
    let hook_log = f.hook_log().unwrap_or_default();
    assert!(
        hook_log.contains("witness unreachable: timeout"),
        "{hook_log}"
    );
    assert_eq!(events(&f.local_journal(), "partial").len(), 1);

    // Der Daemon liest den Rest, sieht EOF mitten im Frame und verwirft ihn.
    let log = f.home.join("log/witness.log");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(&log)
        .unwrap_or_default()
        .contains("incomplete frame dropped")
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Teil-Frame nie verworfen"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        events(&f.witness_journal(), "partial").is_empty(),
        "das Event steht doppelt — lokal und beim Witness"
    );
}

/// Ein Lauscher, der jede Verbindung zählt, aber nie liest.
fn silent_spy(f: &Fixture) -> (std::path::PathBuf, UnixListener) {
    let path = f.dir.path().join("spy.sock");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    (path, listener)
}

#[test]
fn hook_keeps_unforwardable_events_local() {
    let f = Fixture::repo_only();
    let (spy, listener) = silent_spy(&f);

    // 1. Größer als ein Frame: bleibt lokal, ohne den Socket zu berühren.
    let big = "x".repeat(witness_proto::MAX_FRAME as usize);
    let oversized = f.payload("big", "UserPromptSubmit", &format!(r#","prompt":"{big}""#));
    // 2. Ein Tool-Event, das kein JSON ist: Die Mauer konnte nicht prüfen,
    //    also geht es nicht über den Socket.
    let truncated = format!(
        r#"{{"session_id":"cut","cwd":"{}","hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{{"file_path":".env"}},"tool_response":"DB_PASSWORD=hunter2"#,
        f.root.display()
    );
    //    Auch ohne erkennbaren Eventnamen — dann ist die Art nur geraten
    //    (`Other`), und die Mauer sähe gar kein Tool-Event.
    let nameless = format!(
        r#"{{"session_id":"nameless","cwd":"{}","tool_name":"Read","tool_input":{{"file_path":".env"}},"tool_response":"DB_PASSWORD=hunter2"#,
        f.root.display()
    );
    for (agent, session, stdin, kind) in [
        ("claude-code", "big", oversized, "frame too large"),
        ("claude-code", "cut", truncated, "unforwardable payload"),
        ("claude-code", "nameless", nameless, "unforwardable payload"),
        // 3. Der reservierte Name: Der Witness verwürfe ihn still.
        (
            "witness",
            "own",
            f.payload("own", "Stop", ""),
            "reserved agent",
        ),
    ] {
        let (out, _) = f.hook_as(agent, Some(&spy), stdin.as_bytes());
        assert_silent_success(&out);
        let key = minds_capture::SessionKey::new(agent, session).unwrap();
        let local = f.local_journal().read(&key).unwrap().events;
        assert_eq!(local.len(), 1, "{kind}: das Event bleibt lokal");
        let log = f.hook_log().unwrap_or_default();
        assert!(
            log.contains(&format!("witness unreachable: {kind}")),
            "{log}"
        );
    }
    assert!(listener.accept().is_err(), "kein Byte über den Socket");
}

#[test]
fn hook_forwards_a_walled_event_whose_raw_payload_was_too_large() {
    // Der Rohpayload passt nicht in einen Frame, der gewallte Neubau schon:
    // Die Mauer verkleinert das Event, bevor die Größe zählt.
    let f = Fixture::new();
    let _daemon = f.start();
    let secret = "x".repeat(witness_proto::MAX_FRAME as usize);
    let stdin = f.payload(
        "huge-env",
        "PostToolUse",
        &format!(
            r#","tool_name":"Read","tool_input":{{"file_path":".env"}},"tool_response":"{secret}""#
        ),
    );

    let (out, _) = f.hook(Some(&f.socket()), stdin.as_bytes());

    assert_silent_success(&out);
    let witnessed = wait_for_events(&f.witness_journal(), "huge-env", 1);
    assert!(witnessed[0].payload.get().contains("[omitted:secret-file]"));
    assert!(witnessed[0].payload.get().len() < 1024);
    assert!(!f.local_journal_dir().exists());
    assert_eq!(f.hook_log(), None);
}

/// Der `SessionEnd`-Hook liest das Transkript einmal ganz und meldet dem
/// Witness Version, Token-Summen und die redigierten Texte je Zug — als
/// Frames hinter dem Event. Im Journal des Witness hängt der Report am
/// `SessionEnd`; der Witness hat die Datei nie geöffnet (sie liegt außerhalb
/// seiner Zuständigkeit und könnte überall liegen).
#[test]
fn hook_reports_version_tokens_and_texts_at_session_end() {
    let f = Fixture::new();
    let _daemon = f.start();
    let transcript = f.dir.path().join("t.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"erster Auftrag"}}"#,
            "\n",
            r#"{"type":"assistant","version":"2.0.76","message":{"model":"claude-opus-4-1","content":[{"type":"text","text":"Ich schaue nach."}],"usage":{"input_tokens":100,"output_tokens":40}}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"zweiter Auftrag"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-1","content":[{"type":"text","text":"Token AKIAIOSFODNN7EXAMPLE, fertig."}],"usage":{"input_tokens":20,"output_tokens":10}}}"#,
            "\n",
        ),
    )
    .unwrap();
    let stdin = f.payload(
        "report",
        "SessionEnd",
        &format!(r#","transcript_path":"{}""#, transcript.display()),
    );

    let (out, _) = f.hook(Some(&f.socket()), stdin.as_bytes());

    assert_silent_success(&out);
    let events = wait_for_events(&f.witness_journal(), "report", 1);
    assert_eq!(events[0].raw_kind, "SessionEnd");
    assert_eq!(events[0].model.as_deref(), Some("claude-opus-4-1"));
    let report = events[0].report.as_ref().expect("der Report reist mit");
    assert_eq!(report.agent_version.as_deref(), Some("2.0.76"));
    let usage = report.usage.expect("Summen gemeldet");
    assert_eq!((usage.input_tokens, usage.output_tokens), (120, 50));
    assert_eq!(report.said.len(), 2);
    assert_eq!(report.said[0], "Ich schaue nach.");
    assert!(
        !report.said[1].contains("AKIAIOSFODNN7EXAMPLE"),
        "redigiert, bevor es den Socket überquert: {}",
        report.said[1]
    );
    assert_eq!(f.hook_log(), None, "eine gelungene Weitergabe loggt nichts");
}
