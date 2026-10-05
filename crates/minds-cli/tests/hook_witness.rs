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

#[test]
fn hook_falls_back_when_witness_down() {
    let f = Fixture::repo_only();
    let missing = f.dir.path().join("missing.sock");
    let stale = f.dir.path().join("stale.sock");
    // Eine verwaiste Socket-Datei: Verbinden scheitert mit ECONNREFUSED.
    drop(UnixListener::bind(&stale).unwrap());

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
