//! EA-14: der Intent-Anker als erstes Glied der Kette einer Session.

use super::*;
use minds_core::intent_anchor::{
    INTENT_EVENT_KIND, IntentAnchor, IntentEvent, IntentEventPayload, IntentSource, content_hash,
};

const SIGNATURE: &str = "-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n";

/// Ein Anker, wie EA-15 ihn baut: über `redact_intent`, Datei-Quelle mit
/// der echten Blob-Id des Snapshots, abgelegt im Store des Witness — nur
/// ein solcher ist belegt und wird aktiviert (`intent_proof`).
fn anchor(f: &Fixture, snapshot: &str) -> String {
    let repo = minds_git::Repo::discover(&f.root).unwrap();
    let intent = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_intent(
            IntentSource::File {
                path: "docs/spec.md".into(),
                blob: repo.blob_id_of(snapshot.as_bytes()).unwrap(),
            },
            vec!["src/**".into()],
            snapshot.as_bytes().to_vec(),
        )
        .unwrap();
    minds_store::InRepoStore::open(&f.root)
        .unwrap()
        .put_intent(&intent)
        .unwrap();
    intent.text().to_owned()
}

/// Ein Hook-Event der Test-Session; ein `SessionStart` als echter Anfang
/// (`source: startup`).
fn hook_as(writer: &mut Writer, event: &str, seconds: u64) {
    let source = if event == "SessionStart" {
        r#","source":"startup""#
    } else {
        ""
    };
    hook_raw(
        writer,
        &format!(
            r#"{{"session_id":"test-session","hook_event_name":"{event}","prompt":"Implement sorting"{source}}}"#
        ),
        seconds,
    );
}

fn hook_raw(writer: &mut Writer, payload: &str, seconds: u64) {
    let nanos = at().1 + seconds * 1_000_000_000;
    let stamp = (minds_capture::clock::rfc3339_from_nanos(nanos), nanos);
    writer
        .hook("claude-code", None, payload.as_bytes().to_vec(), stamp)
        .unwrap();
}

/// `(seq, Anker, opens_session)` der Intent-Events im offenen Journal.
fn intent_payloads(writer: &Writer) -> Vec<(u64, String, bool)> {
    writer
        .journal
        .read(&key())
        .unwrap()
        .events
        .iter()
        .filter(|e| e.raw_kind == INTENT_EVENT_KIND)
        .map(|e| {
            let payload: IntentEventPayload = serde_json::from_str(e.payload.get()).unwrap();
            (e.seq, payload.anchor, payload.opens_session)
        })
        .collect()
}

/// Die eine gespeicherte Session nach einem Witness-Checkpoint.
fn stored_session(f: &Fixture) -> minds_core::Session {
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    let mut sessions: Vec<_> = store
        .list_seals()
        .unwrap()
        .iter()
        .filter_map(|id| {
            let seal =
                minds_core::evidence::Seal::parse(&store.seal_text(id).unwrap().unwrap()).unwrap();
            match seal.outcome {
                minds_core::evidence::SealOutcome::Stored { session } => {
                    Some(session.parse::<minds_core::SessionId>().unwrap())
                }
                _ => None,
            }
        })
        .collect();
    sessions.dedup();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    store.get(sessions[0]).unwrap().unwrap()
}

#[test]
fn witness_prepends_intent_to_new_session() {
    let f = Fixture::new();
    let mut writer = f.writer();
    let text = anchor(&f, "Retry exponentiell.\n");
    let status = writer
        .activate_by_control(&text, Some(SIGNATURE.into()))
        .unwrap()
        .unwrap();
    let id = IntentAnchor::id_of_text(&text);
    assert_eq!(status, format!("intent active {id}"));
    // Die Aktivierung selbst schreibt nichts in eine Session.
    assert!(writer.journal.read(&key()).unwrap().events.is_empty());

    hook_as(&mut writer, "SessionStart", 0);
    hook_as(&mut writer, "UserPromptSubmit", 1);

    let events = writer.journal.read(&key()).unwrap().events;
    assert_eq!(events.len(), 3);
    // Das erste Glied der Kette ist der Intent — vor dem ersten Hook-Event.
    assert_eq!(events[0].raw_kind, INTENT_EVENT_KIND);
    assert_eq!(events[0].seq, 0);
    assert_eq!(events[1].raw_kind, "SessionStart");
    assert!(events[0].at_nanos < events[1].at_nanos);
    let payload: IntentEventPayload = serde_json::from_str(events[0].payload.get()).unwrap();
    assert_eq!(payload.anchor_id, id);
    assert_eq!(payload.anchor, text);
    assert!(payload.opens_session);
    assert_eq!(payload.signature.as_deref(), Some(SIGNATURE));
    // Verkettet wie jedes andere Event.
    let salt = writer.epochs.salt(&key()).unwrap();
    let expected = chain::chain_salted(&salt, &writer.journal.read(&key()).unwrap());
    assert_eq!(writer.folders[&key()].snapshot(), expected);
    assert_eq!(expected.coverage.events, 3);

    // Im gespeicherten Envelope: die Beobachtung, versiegelt.
    f.keygen();
    writer.checkpoint_now(None).unwrap();
    let session = stored_session(&f);
    assert_eq!(session.intent_anchor, None);
    assert_eq!(
        session.intent_events,
        vec![IntentEvent {
            seq: 0,
            anchor_id: id,
            opens_session: true,
        }]
    );
    // Der Prompt bleibt der Prompt — der Intent ist kein Turn.
    assert_eq!(session.intent.request, "Implement sorting");

    // Eine schon begonnene Session bekommt in der nächsten Epoche keinen
    // zweiten Anker vorangestellt — auch nicht nach einem Neustart.
    hook_as(&mut writer, "UserPromptSubmit", 2);
    assert!(intent_payloads(&writer).is_empty());
    drop(writer);
    let mut writer = f.writer();
    hook_as(&mut writer, "Stop", 3);
    assert!(intent_payloads(&writer).is_empty());
}

/// Die Witness-Hälfte von `intent_change_mid_session_is_flagged`: Der
/// Wechsel steht als weiteres Ketten-Glied im gespeicherten Envelope. Die
/// Bewertung („changed mid-session") rechnet der Reader — geprüft in
/// `minds-reader/tests/intent.rs`, nicht hier: Ein Schreibpfad liest die
/// Stufe nie (W2, `assurance_is_never_stored`).
#[test]
fn witness_appends_intent_change_mid_session() {
    let f = Fixture::new();
    let mut writer = f.writer();
    // Ohne aktiven Intent: kein Anker.
    hook_as(&mut writer, "UserPromptSubmit", 0);
    assert!(intent_payloads(&writer).is_empty());

    let first = anchor(&f, "Version 1\n");
    writer.activate_by_control(&first, None).unwrap().unwrap();
    hook_as(&mut writer, "Stop", 1);
    let second = anchor(&f, "Version 2\n");
    writer.activate_by_control(&second, None).unwrap().unwrap();
    // Dieselbe Aktivierung noch einmal: nichts Neues.
    assert_eq!(
        writer.activate_by_control(&second, None).unwrap().unwrap(),
        format!("intent unchanged {}", IntentAnchor::id_of_text(&second))
    );
    hook_as(&mut writer, "UserPromptSubmit", 2);
    hook_as(&mut writer, "Stop", 3);

    // Jeder Wechsel genau einmal, unmittelbar vor dem nächsten Hook-Event,
    // keiner eröffnet die Session (sie lief erst ungebunden).
    assert_eq!(
        intent_payloads(&writer),
        vec![(1, first.clone(), false), (3, second.clone(), false)]
    );

    f.keygen();
    writer.checkpoint_now(None).unwrap();
    let session = stored_session(&f);
    assert_eq!(
        session.intent_events,
        vec![
            IntentEvent {
                seq: 1,
                anchor_id: IntentAnchor::id_of_text(&first),
                opens_session: false,
            },
            IntentEvent {
                seq: 3,
                anchor_id: IntentAnchor::id_of_text(&second),
                opens_session: false,
            },
        ]
    );
}

#[test]
fn a_change_reaches_a_session_only_with_its_next_event() {
    let f = Fixture::new();
    let mut writer = f.writer();
    hook_as(&mut writer, "UserPromptSubmit", 0);
    hook_as(&mut writer, "SessionEnd", 1);
    writer
        .activate_by_control(&anchor(&f, "x\n"), None)
        .unwrap()
        .unwrap();
    // Eine beendete (oder abgestürzte) Session bekommt nichts — keine
    // Epoche aus Witness-Events allein.
    assert!(intent_payloads(&writer).is_empty());
    // `--resume`: Mit ihrem nächsten Event erreicht sie der Wechsel.
    hook_as(&mut writer, "UserPromptSubmit", 2);
    assert_eq!(
        intent_payloads(&writer),
        vec![(2, anchor(&f, "x\n"), false)]
    );
}

#[test]
fn the_agent_cannot_forge_an_intent_event() {
    let f = Fixture::new();
    let mut writer = f.writer();
    let mut forged = IntentEventPayload::new(&anchor(&f, "x\n"), None).unwrap();
    forged.opens_session = true;
    let forged = serde_json::to_string(&forged).unwrap();
    for (event, event_override) in [
        (INTENT_EVENT_KIND, None),
        ("MINDS.INTENT", None),
        ("minds.anything", None),
        ("", Some(INTENT_EVENT_KIND)),
    ] {
        let mut payload: serde_json::Value = serde_json::from_str(&forged).unwrap();
        payload["session_id"] = "test-session".into();
        if !event.is_empty() {
            payload["hook_event_name"] = event.into();
        }
        writer
            .hook(
                "claude-code",
                event_override,
                payload.to_string().into_bytes(),
                at(),
            )
            .unwrap();
    }
    assert!(writer.journal.read(&key()).unwrap().events.is_empty());
}

#[test]
fn invalid_activations_are_refused_with_a_fixed_reason() {
    let f = Fixture::new();
    let mut writer = f.writer();
    let good = anchor(&f, "x\n");
    assert_eq!(
        writer
            .activate_by_control("minds-intent-v1\n", None)
            .unwrap(),
        Err("invalid intent anchor")
    );
    assert_eq!(
        writer
            .activate_by_control(&good.replace("src/**", "src/**\u{202E}"), None)
            .unwrap(),
        Err("invalid intent anchor")
    );
    for signature in [
        "sig\u{0}".to_owned(),
        "free text that is no signature".to_owned(),
        format!(
            "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
            "A".repeat(17 * 1024)
        ),
    ] {
        assert_eq!(
            writer.activate_by_control(&good, Some(signature)).unwrap(),
            Err("invalid intent signature")
        );
    }
    // Ein Anker, den `redact_intent` nie gebaut hätte: ein Token im Scope.
    let leaky = IntentAnchor {
        source: IntentSource::Prompt,
        content: content_hash(b"x"),
        scope: vec!["src/ghp_R4nd0mT0k3nV4lu3F0rT3st1ngPurp0s3s00/**".into()],
    }
    .to_text()
    .unwrap();
    assert_eq!(
        writer.activate_by_control(&leaky, None).unwrap(),
        Err("intent anchor refused by the redaction policy")
    );
    assert!(writer.intent.is_none());
    assert!(!f.home.join("evidence/intent.json").exists());
}

/// Hashes, Blob-SHA und Pfade eines echten Ankers sind für die Default-Policy
/// kein Fund — sonst wiese der Witness jeden Anker ab.
#[test]
fn a_real_anchor_survives_the_policy() {
    let f = Fixture::new();
    let writer = f.writer();
    let text = IntentAnchor {
        source: IntentSource::File {
            path: "docs/@team/spec.md".into(),
            blob: "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b".into(),
        },
        content: "b3-9b588b7bb5ea2bb3635b0156f4379aacb8b41fbc5931c4d64bc4210a88fea8e8"
            .parse()
            .unwrap(),
        scope: vec!["src/retry/**".into(), "tests/retry_*.rs".into()],
    }
    .to_text()
    .unwrap();
    assert!(intent::policy_accepts(&writer.config, &text));
    let issue = IntentAnchor {
        source: IntentSource::Issue {
            project: "group/sub/minds".into(),
            iid: 4711,
            updated_at: "2026-10-01T08:15:00.123Z".into(),
        },
        content: content_hash(b"{}"),
        scope: Vec::new(),
    }
    .to_text()
    .unwrap();
    assert!(intent::policy_accepts(&writer.config, &issue));
}

#[test]
fn the_active_intent_survives_a_restart() {
    let f = Fixture::new();
    let text = anchor(&f, "x\n");
    {
        let mut writer = f.writer();
        writer.activate_by_control(&text, None).unwrap().unwrap();
        hook_as(&mut writer, "SessionStart", 0);
    }
    let path = f.home.join("evidence/intent.json");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut writer = f.writer();
    assert_eq!(
        writer.intent.as_ref().map(|p| p.anchor.clone()),
        Some(text.clone())
    );
    // Ein Wechsel nach dem Neustart erreicht die laufende Session.
    writer
        .activate_by_control(&anchor(&f, "y\n"), None)
        .unwrap()
        .unwrap();
    hook_as(&mut writer, "Stop", 1);
    assert_eq!(
        intent_payloads(&writer),
        vec![(0, text, true), (2, anchor(&f, "y\n"), false)]
    );

    // Eine beschädigte Datei heißt „nichts aktiv", nie ein falscher Anker.
    fs::write(&path, b"{").unwrap();
    assert!(f.writer().intent.is_none());
}

/// Der Anfang einer Session braucht einen positiven Beleg: Geht der
/// Epochen-Zustand verloren (er ist best-effort), oder ist der Eintrag der
/// Session unlesbar, wird eine bekannte Session nie wieder „neu".
#[test]
fn a_known_session_never_becomes_new_again() {
    let f = Fixture::new();
    let mut writer = f.writer();
    hook_as(&mut writer, "UserPromptSubmit", 0);
    f.keygen();
    writer.checkpoint_now(None).unwrap();
    drop(writer);
    // Epochen-Zustand weg — der Fold der nächsten Epoche ist ohnehin leer.
    for entry in fs::read_dir(f.home.join("evidence/state/claude-code")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "salt") {
            fs::remove_file(path).unwrap();
        }
    }
    let mut writer = f.writer();
    assert!(writer.epochs.last_seal(&key()).is_none());
    writer
        .activate_by_control(&anchor(&f, "x\n"), None)
        .unwrap()
        .unwrap();
    hook_as(&mut writer, "Stop", 1);
    assert_eq!(
        intent_payloads(&writer),
        vec![(0, anchor(&f, "x\n"), false)]
    );

    // Ein unlesbarer Eintrag: bekannt, ungebunden — der Wechsel kommt als
    // Wechsel, nie als Anfang.
    let other = SessionKey::new("claude-code", "other-session").unwrap();
    fs::write(
        folder_path(&f.home, &other).with_extension("intent"),
        b"kaputt",
    )
    .unwrap();
    writer
        .hook(
            "claude-code",
            None,
            br#"{"session_id":"other-session","hook_event_name":"UserPromptSubmit"}"#.to_vec(),
            at(),
        )
        .unwrap();
    let events = writer.journal.read(&other).unwrap().events;
    let payload: IntentEventPayload = serde_json::from_str(events[0].payload.get()).unwrap();
    assert_eq!(events[0].raw_kind, INTENT_EVENT_KIND);
    assert!(!payload.opens_session);
}

// ---------------------------------------------------------------------------
// Wer aktivieren darf
// ---------------------------------------------------------------------------

/// Ein Client über ein Socket-Paar; `control` wie beim Annehmen gesetzt.
fn connected(control: bool) -> (socket::Client, std::os::unix::net::UnixStream) {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    ours.set_nonblocking(true).unwrap();
    theirs
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let client = socket::Client {
        stream: ours,
        bytes: Vec::new(),
        since: std::time::Instant::now(),
        eof: false,
        pending: false,
        control,
        challenge: None,
        partial_since: None,
    };
    (client, theirs)
}

/// Schickt `frame`, lässt den Witness einen Durchlauf machen und liest die
/// Antwort.
fn ask(
    writer: &mut Writer,
    control: bool,
    frame: minds_capture::witness_proto::Frame,
) -> minds_capture::witness_proto::Frame {
    use minds_capture::witness_proto;
    let (mut client, mut theirs) = connected(control);
    theirs
        .write_all(&witness_proto::encode(&frame).unwrap())
        .unwrap();
    assert!(socket::step(&mut client, writer).unwrap());
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = theirs.read(&mut chunk).unwrap();
        assert!(n > 0, "the witness closed the connection without an answer");
        bytes.extend_from_slice(&chunk[..n]);
        if let Ok((frame, _)) = witness_proto::decode(&bytes) {
            return frame;
        }
    }
}

fn activate(anchor: &str) -> minds_capture::witness_proto::Frame {
    minds_capture::witness_proto::Frame::IntentActivate {
        anchor: anchor.into(),
        signature: (anchor != "-").then(|| SIGNATURE.into()),
        request_id: [7; 16],
    }
}

#[test]
fn intent_activation_is_host_side_only() {
    use minds_capture::witness_proto::Frame;
    let f = Fixture::new();
    let mut writer = f.writer();
    let text = anchor(&f, "x\n");

    // Die Agent-Seite: abgewiesen, nichts aktiv, nichts gespeichert.
    assert_eq!(
        ask(&mut writer, false, activate(&text)),
        Frame::Nack {
            request_id: [7; 16],
            reason: "intent activation is host-side only".into(),
        }
    );
    assert!(writer.intent.is_none());
    assert!(!f.home.join("evidence/intent.json").exists());
    // Auch das Aufheben nicht.
    assert!(matches!(
        ask(&mut writer, false, activate("-")),
        Frame::Nack { .. }
    ));

    // Die Host-Seite: aktiviert …
    assert_eq!(
        ask(&mut writer, true, activate(&text)),
        Frame::Ack {
            request_id: [7; 16],
            status: format!("intent active {}", IntentAnchor::id_of_text(&text)),
        }
    );
    hook_as(&mut writer, "SessionStart", 0);
    assert_eq!(intent_payloads(&writer), vec![(0, text.clone(), true)]);

    // … und hebt auf: Neue Sessions beginnen ungebunden, die gebundene
    // behält ihren Anker (kein Wechsel-Event).
    assert_eq!(
        ask(&mut writer, true, activate("-")),
        Frame::Ack {
            request_id: [7; 16],
            status: "intent cleared".into(),
        }
    );
    assert!(writer.intent.is_none());
    assert!(!f.home.join("evidence/intent.json").exists());
    hook_as(&mut writer, "Stop", 1);
    assert_eq!(intent_payloads(&writer), vec![(0, text, true)]);
    let other = SessionKey::new("claude-code", "other-session").unwrap();
    writer
        .hook(
            "claude-code",
            None,
            br#"{"session_id":"other-session","hook_event_name":"UserPromptSubmit"}"#.to_vec(),
            at(),
        )
        .unwrap();
    assert!(
        writer
            .journal
            .read(&other)
            .unwrap()
            .events
            .iter()
            .all(|e| e.raw_kind != INTENT_EVENT_KIND)
    );
}

#[test]
fn the_control_socket_accepts_only_control_frames() {
    use minds_capture::witness_proto::{self, Frame};
    let f = Fixture::new();
    let mut writer = f.writer();
    assert_eq!(
        ask(&mut writer, true, Frame::Ping),
        Frame::Ack {
            request_id: [0; 16],
            status: "pong".into(),
        }
    );
    let (mut client, mut theirs) = connected(true);
    theirs
        .write_all(
            &witness_proto::encode(&Frame::Hook {
                agent: "claude-code".into(),
                event_override: None,
                stdin: payload(),
            })
            .unwrap(),
        )
        .unwrap();
    // Die Verbindung wird geschlossen, nichts landet im Journal.
    assert!(!socket::step(&mut client, &mut writer).unwrap());
    assert!(writer.journal.read(&key()).unwrap().events.is_empty());
}

/// Volle Plätze der Agent-Seite sperren den Steuer-Socket nicht aus.
#[test]
fn the_agent_side_cannot_starve_the_control_socket() {
    let f = Fixture::new();
    let writer = f.writer();
    let path = f._dir.path().join("control-test.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut keep = Vec::new();
    let mut clients: Vec<socket::Client> = (0..socket::MAX_CLIENTS)
        .map(|_| {
            let (client, theirs) = connected(false);
            keep.push(theirs);
            client
        })
        .collect();
    let _host = std::os::unix::net::UnixStream::connect(&path).unwrap();
    let mut logged = None;
    socket::accept(&listener, true, &mut clients, &writer, &mut logged);
    assert_eq!(clients.len(), socket::MAX_CLIENTS + 1);
    assert!(clients.last().unwrap().control);
}

/// Den Anfang belegt nur ein `SessionStart`: Eine Session, die der Witness
/// zuerst mitten im Lauf sieht (Hooks vorher unterdrückt), bekommt den
/// Intent ohne `opens_session`.
#[test]
fn only_a_session_start_opens_a_session() {
    let f = Fixture::new();
    let mut writer = f.writer();
    writer
        .activate_by_control(&anchor(&f, "x\n"), None)
        .unwrap()
        .unwrap();
    hook_as(&mut writer, "PostToolUse", 0);
    assert_eq!(
        intent_payloads(&writer),
        vec![(0, anchor(&f, "x\n"), false)]
    );
    // Kein zweiter Eintrag mit dem nächsten Event.
    hook_as(&mut writer, "Stop", 1);
    assert_eq!(intent_payloads(&writer).len(), 1);
}

/// Absturz zwischen Intent-Event und Buchführung: Der Eintrag fehlt, das
/// Event steht schon in der Kette. Danach wiederholt sich höchstens derselbe
/// Anker — nie ein zweiter Anfang (die Session ist über ihren Fold bekannt).
#[test]
fn a_crash_between_event_and_record_repeats_the_anchor_without_a_new_start() {
    let f = Fixture::new();
    let text = anchor(&f, "x\n");
    {
        let mut writer = f.writer();
        writer.activate_by_control(&text, None).unwrap().unwrap();
        hook_as(&mut writer, "SessionStart", 0);
    }
    fs::remove_file(folder_path(&f.home, &key()).with_extension("intent")).unwrap();
    let mut writer = f.writer();
    hook_as(&mut writer, "UserPromptSubmit", 1);
    assert_eq!(
        intent_payloads(&writer),
        vec![(0, text.clone(), true), (2, text, false)]
    );
}

/// `resume` und `compact` setzen eine laufende Session fort: Sieht der
/// Witness sie dort zum ersten Mal, ist das kein Anfang — ebenso wenig ein
/// `SessionStart` ohne oder mit unbekanntem `source`.
#[test]
fn resume_or_compact_is_not_a_session_start() {
    for source in [
        r#","source":"resume""#,
        r#","source":"compact""#,
        r#","source":"something-new""#,
        "",
    ] {
        let f = Fixture::new();
        let mut writer = f.writer();
        writer
            .activate_by_control(&anchor(&f, "x\n"), None)
            .unwrap()
            .unwrap();
        hook_raw(
            &mut writer,
            &format!(r#"{{"session_id":"test-session","hook_event_name":"SessionStart"{source}}}"#),
            0,
        );
        assert_eq!(
            intent_payloads(&writer),
            vec![(0, anchor(&f, "x\n"), false)],
            "{source}"
        );
    }
    // `clear` beginnt eine neue Session.
    let f = Fixture::new();
    let mut writer = f.writer();
    writer
        .activate_by_control(&anchor(&f, "x\n"), None)
        .unwrap()
        .unwrap();
    hook_raw(
        &mut writer,
        r#"{"session_id":"test-session","hook_event_name":"SessionStart","source":"clear"}"#,
        0,
    );
    assert_eq!(intent_payloads(&writer), vec![(0, anchor(&f, "x\n"), true)]);
}

/// Der fail-closed-Pfad selbst: Eintrag weg, Epochen-Zustand beschädigt,
/// Fold nach dem Versiegeln leer — ein `SessionStart{startup}` ist dann
/// trotzdem kein Anfang, weil die Session schon versiegelt war.
#[test]
fn a_damaged_epoch_state_never_makes_a_sealed_session_new() {
    let f = Fixture::new();
    let mut writer = f.writer();
    hook_as(&mut writer, "UserPromptSubmit", 0);
    f.keygen();
    writer.checkpoint_now(None).unwrap();
    drop(writer);
    fs::remove_file(folder_path(&f.home, &key()).with_extension("intent")).unwrap();
    for entry in fs::read_dir(f.home.join("evidence/state/claude-code")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "salt") {
            fs::write(path, b"kaputt").unwrap();
        }
    }
    let mut writer = f.writer();
    assert!(writer.epochs.last_seal(&key()).is_none());
    writer
        .activate_by_control(&anchor(&f, "x\n"), None)
        .unwrap()
        .unwrap();
    hook_as(&mut writer, "SessionStart", 1);
    assert_eq!(
        intent_payloads(&writer),
        vec![(0, anchor(&f, "x\n"), false)]
    );
}

/// Nach einem Neustart gilt der aktive Intent nur mit demselben Beleg wie
/// bei der Aktivierung: Fehlt der Anker im Store, ist nichts aktiv.
#[test]
fn a_restart_reproves_the_active_intent() {
    let f = Fixture::new();
    let text = anchor(&f, "x\n");
    f.writer()
        .activate_by_control(&text, None)
        .unwrap()
        .unwrap();
    assert!(f.writer().intent.is_some());
    let id = IntentAnchor::id_of_text(&text);
    git(
        &f.root,
        &[
            "update-ref",
            "-d",
            &format!("refs/minds/intents/{}", id.hex()),
        ],
    );
    let mut writer = f.writer();
    assert!(writer.intent.is_none());
    hook_as(&mut writer, "SessionStart", 0);
    assert!(intent_payloads(&writer).is_empty());
}

// ---------------------------------------------------------------------------
// Freigabe per Berührung (geschützter Witness)
// ---------------------------------------------------------------------------

/// Ein ausgedachter FIDO-Schlüssel und eine Signatur „mit ihm" (Flags
/// `flags`) — die Signaturbytes prüft im Test `verify_activation`.
mod fido {
    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let word = chunk
                .iter()
                .enumerate()
                .fold(0u32, |word, (i, b)| word | u32::from(*b) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(word >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn string(bytes: &[u8]) -> Vec<u8> {
        let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(bytes);
        out
    }

    const SK: &str = "sk-ssh-ed25519@openssh.com";

    fn blob() -> Vec<u8> {
        [string(SK.as_bytes()), string(&[9; 32]), string(b"ssh:")].concat()
    }

    pub(super) fn key() -> String {
        format!("{SK} {}", base64(&blob()))
    }

    /// Ein anderer ausgedachter Schlüssel.
    pub(super) fn other_key() -> String {
        let blob = [string(SK.as_bytes()), string(&[8; 32]), string(b"ssh:")].concat();
        format!("{SK} {}", base64(&blob))
    }

    /// Die Freigabe der Aktivierung: die nächste Signatur nach
    /// [`signature`] (Zähler 2).
    pub(super) fn proof(flags: u8) -> String {
        proof_at(flags, 2)
    }

    /// Die Freigabe des Ankers (Zähler 1, berührt).
    pub(super) fn signature() -> String {
        proof_at(1, 1)
    }

    pub(super) fn proof_at(flags: u8, counter: u32) -> String {
        let mut signature = [string(SK.as_bytes()), string(&[7; 64])].concat();
        signature.push(flags);
        signature.extend_from_slice(&counter.to_be_bytes());
        let body = base64(
            &[
                b"SSHSIG".to_vec(),
                1u32.to_be_bytes().to_vec(),
                string(&blob()),
                string(minds_attest::NS_INTENT_ACTIVATE.as_bytes()),
                string(b""),
                string(b"sha512"),
                string(&signature),
            ]
            .concat(),
        );
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(70)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect();
        format!(
            "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
            lines.join("\n")
        )
    }
}

/// Schickt `frame` auf einer **bestehenden** Verbindung und liest die
/// Antwort.
fn ask_on(
    writer: &mut Writer,
    client: &mut socket::Client,
    theirs: &mut std::os::unix::net::UnixStream,
    frame: minds_capture::witness_proto::Frame,
) -> minds_capture::witness_proto::Frame {
    use minds_capture::witness_proto;
    theirs
        .write_all(&witness_proto::encode(&frame).unwrap())
        .unwrap();
    assert!(socket::step(client, writer).unwrap());
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = theirs.read(&mut chunk).unwrap();
        assert!(n > 0, "the witness closed the connection without an answer");
        bytes.extend_from_slice(&chunk[..n]);
        if let Ok((frame, _)) = witness_proto::decode(&bytes) {
            return frame;
        }
    }
}

fn challenge() -> minds_capture::witness_proto::Frame {
    minds_capture::witness_proto::Frame::IntentChallenge {
        request_id: [5; 16],
    }
}

fn nonce_of(answer: minds_capture::witness_proto::Frame) -> String {
    match answer {
        minds_capture::witness_proto::Frame::Ack { status, .. } => {
            status.strip_prefix("nonce ").unwrap().to_owned()
        }
        other => panic!("no nonce: {other:?}"),
    }
}

fn proven(anchor: &str, nonce: &str, proof: String) -> minds_capture::witness_proto::Frame {
    minds_capture::witness_proto::Frame::IntentActivateProven {
        anchor: anchor.into(),
        // Die Freigabe des Ankers: vom festgehaltenen Schlüssel, berührt.
        signature: Some(fido::signature()),
        nonce: nonce.into(),
        proof,
        request_id: [6; 16],
    }
}

fn refused(reason: &str) -> minds_capture::witness_proto::Frame {
    minds_capture::witness_proto::Frame::Nack {
        request_id: [6; 16],
        reason: reason.into(),
    }
}

thread_local! {
    /// Was der Prüfer (sonst `ssh-keygen`) in diesem Test sah: Text und
    /// Namespace.
    static SEEN: std::cell::RefCell<Vec<(String, String)>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

fn accept_all(text: &str, _: &str, _: &Path, namespace: &str) -> Option<bool> {
    SEEN.with(|seen| {
        seen.borrow_mut()
            .push((text.to_owned(), namespace.to_owned()))
    });
    Some(true)
}

/// Ohne festgehaltenen Schlüssel gibt es keine Nonce — und ohne Nonce
/// keine Freigabe auf der Agent-Seite.
#[test]
fn without_a_pinned_key_the_agent_side_gets_no_challenge() {
    use minds_capture::witness_proto::Frame;
    let f = Fixture::new();
    let mut writer = f.writer();
    assert_eq!(
        ask(&mut writer, false, challenge()),
        Frame::Nack {
            request_id: [5; 16],
            reason: crate::intent_proof::NO_INTENT_KEY.into(),
        }
    );
    let text = anchor(&f, "x\n");
    assert_eq!(
        ask(
            &mut writer,
            false,
            proven(&text, &"0".repeat(64), fido::proof(1))
        ),
        refused("no fresh challenge on this connection")
    );
    assert!(writer.intent.is_none());
}

/// Mit festgehaltenem Schlüssel: Nonce holen, mit Berührung über Anker-Id
/// und Nonce signieren, auf derselben Verbindung aktivieren — einmal. Die
/// neue Session beginnt beim Anker.
#[test]
fn a_fresh_touch_of_the_pinned_key_activates_on_the_agent_side() {
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    writer.verify_activation = accept_all;
    let text = anchor(&f, "x\n");
    let id = IntentAnchor::id_of_text(&text);

    let (mut client, mut theirs) = connected(false);
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    assert_eq!(nonce.len(), 64);
    // Ohne Berührung: abgewiesen — und die Nonce ist verbraucht.
    assert_eq!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &nonce, fido::proof(0))
        ),
        refused("activation proof without a touch of the security key")
    );
    assert_eq!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &nonce, fido::proof(1))
        ),
        refused("no fresh challenge on this connection")
    );
    assert!(writer.intent.is_none());

    // Eine neue Nonce, eine Freigabe mit Berührung: aktiv.
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    assert_eq!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &nonce, fido::proof(1))
        ),
        minds_capture::witness_proto::Frame::Ack {
            request_id: [6; 16],
            status: format!("intent active {id}"),
        }
    );
    assert_eq!(
        SEEN.with(|seen| seen.borrow().clone()),
        [
            (
                crate::intent_proof::activation_message(id.as_str(), &fido::signature(), &nonce),
                minds_attest::NS_INTENT_ACTIVATE.to_owned()
            ),
            (text.clone(), minds_attest::NS_INTENT.to_owned()),
        ]
    );
    // Die folgenden Versuche sollen nicht gebremst werden.
    writer.last_verify = None;
    // Eine Freigabe des Ankers gilt einmal: Mit derselben Anker-Signatur
    // und einer neuen Nonce (eine zweite, vielleicht erschlichene
    // Berührung) bindet nichts erneut.
    let again = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    assert_eq!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &again, fido::proof(1))
        ),
        refused("this intent approval was already used for an activation — sign again")
    );
    writer.last_verify = None;
    hook_as(&mut writer, "SessionStart", 0);
    assert_eq!(intent_payloads(&writer), vec![(0, text.clone(), true)]);

    // Eine Nonce gilt nur auf ihrer Verbindung: Auf einer anderen ist
    // dieselbe Freigabe wertlos (kein Wiedereinspielen).
    let (mut other, mut other_theirs) = connected(false);
    assert_eq!(
        ask_on(
            &mut writer,
            &mut other,
            &mut other_theirs,
            proven(&text, &nonce, fido::proof(1))
        ),
        refused("no fresh challenge on this connection")
    );
    // Eine falsche Nonce verbraucht die richtige.
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    let wrong = if nonce.starts_with('0') { "1" } else { "0" }.repeat(64);
    assert_eq!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &wrong, fido::proof(1))
        ),
        refused("no fresh challenge on this connection")
    );
    // Aufheben bleibt dem Steuer-Socket.
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    let clear = minds_capture::witness_proto::Frame::IntentActivateProven {
        anchor: "-".into(),
        signature: None,
        nonce,
        proof: fido::proof(1),
        request_id: [6; 16],
    };
    assert_eq!(
        ask_on(&mut writer, &mut client, &mut theirs, clear),
        refused("clearing an intent needs the control socket")
    );
    assert!(writer.intent.is_some());
}

/// Eine abgelaufene Nonce gilt nicht mehr, und solange eine aussteht,
/// bleibt die Verbindung über die übliche Frist hinaus offen.
#[test]
fn a_challenge_expires_and_keeps_its_connection_open_until_then() {
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    writer.verify_activation = accept_all;
    let text = anchor(&f, "x\n");
    let (mut client, mut theirs) = connected(false);
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    // Still seit länger als die übliche Frist (5 s): Die Verbindung bleibt.
    client.since = std::time::Instant::now() - std::time::Duration::from_secs(30);
    assert!(socket::step(&mut client, &mut writer).unwrap());
    // Nach Ablauf der Nonce: abgewiesen.
    let issued = std::time::Instant::now() - intent::CHALLENGE_TTL;
    client.challenge = Some((nonce.clone(), issued));
    assert_eq!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &nonce, fido::proof(1))
        ),
        refused("no fresh challenge on this connection")
    );
    // Ohne ausstehende Nonce gilt wieder die übliche Frist.
    client.since = std::time::Instant::now() - std::time::Duration::from_secs(30);
    assert!(!socket::step(&mut client, &mut writer).unwrap());
}

/// Ein von Hand verbogener Schlüssel in `witness.json` gilt nicht: Nur die
/// Form, die `init` schreibt, wird festgehalten — geprüft über die Datei,
/// die der Witness bei jeder Prüfung liest.
#[test]
fn a_hand_edited_intent_key_does_not_count() {
    use minds_capture::witness_proto::Frame;
    let f = Fixture::new();
    let mut writer = f.writer();
    let edit = |line: &str| {
        let path = f.home.join("witness.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        config["intent_key"] = serde_json::Value::String(line.to_owned());
        atomic(&path, &serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    };
    let no_key = Frame::Nack {
        request_id: [5; 16],
        reason: crate::intent_proof::NO_INTENT_KEY.into(),
    };
    for edited in [
        format!("{} comment", fido::key()),
        "ssh-ed25519 AAAA".to_owned(),
    ] {
        edit(&edited);
        assert_eq!(ask(&mut writer, false, challenge()), no_key, "{edited}");
    }
    edit(&fido::key());
    assert!(matches!(
        ask(&mut writer, false, challenge()),
        Frame::Ack { .. }
    ));
}

/// Eine Nonce kostet das Lesen einer Datei, nicht einen Gang durch das
/// ganze Home: Ein unlesbares Verzeichnis im Journal stört sie nicht.
#[test]
fn a_challenge_reads_only_the_witness_configuration() {
    use std::os::unix::fs::PermissionsExt;
    // Als root ließe sich das Verzeichnis trotzdem lesen.
    // SAFETY: geteuid hat keine Vorbedingungen.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    let hidden = f.home.join("journal/unreadable");
    fs::create_dir(&hidden).unwrap();
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(load(&f.home).is_err());
    let answer = ask(&mut writer, false, challenge());
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();
    let _ = nonce_of(answer);
}

/// Gebremst wird, ohne die Nonce zu verbrauchen: Dieselbe Freigabe geht
/// nach der Pause durch — eine Flut erfundener Freigaben entwertet die des
/// Menschen nicht.
#[test]
fn a_throttled_approval_keeps_its_nonce() {
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    writer.verify_activation = accept_all;
    let text = anchor(&f, "x\n");
    let (mut client, mut theirs) = connected(false);
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    // Eben hat jemand anderes eine Prüfung ausgelöst.
    writer.last_verify = Some(std::time::Instant::now());
    assert_eq!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &nonce, fido::proof(1))
        ),
        refused(crate::intent_proof::THROTTLED)
    );
    let earlier = std::time::Instant::now() - intent::VERIFY_INTERVAL;
    writer.last_verify = Some(earlier);
    assert!(matches!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &nonce, fido::proof(1))
        ),
        minds_capture::witness_proto::Frame::Ack { .. }
    ));
    // Eine Prüfung, die `ssh-keygen` erreicht, setzt die Bremse.
    assert!(writer.last_verify.unwrap() > earlier);
}

/// Ein Frame, der lange nach der Nonce in zwei Teilen kommt (größer als der
/// Socket-Puffer), wird nicht verworfen: Die übliche Frist zählt ab seinem
/// ersten Byte.
#[test]
fn a_split_approval_after_a_long_touch_is_read() {
    use minds_capture::witness_proto;
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    writer.verify_activation = accept_all;
    let text = anchor(&f, "x\n");
    let (mut client, mut theirs) = connected(false);
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    // Die Berührung dauerte 30 Sekunden.
    client.since = std::time::Instant::now() - std::time::Duration::from_secs(30);
    let bytes = witness_proto::encode(&proven(&text, &nonce, fido::proof(1))).unwrap();
    let (first, second) = bytes.split_at(bytes.len() / 2);
    theirs.write_all(first).unwrap();
    assert!(socket::step(&mut client, &mut writer).unwrap());
    assert!(!client.bytes.is_empty());
    theirs.write_all(second).unwrap();
    assert!(socket::step(&mut client, &mut writer).unwrap());
    let mut answer = Vec::new();
    let mut chunk = [0u8; 4096];
    let frame = loop {
        let n = theirs.read(&mut chunk).unwrap();
        assert!(n > 0, "the witness closed the connection without an answer");
        answer.extend_from_slice(&chunk[..n]);
        if let Ok((frame, _)) = witness_proto::decode(&answer) {
            break frame;
        }
    };
    assert!(
        matches!(frame, witness_proto::Frame::Ack { .. }),
        "{frame:?}"
    );
    // Ein halber Frame, der länger als die übliche Frist liegt, schließt
    // die Verbindung — auch mit ausstehender Nonce.
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    let bytes = witness_proto::encode(&proven(&text, &nonce, fido::proof(1))).unwrap();
    theirs.write_all(&bytes[..10]).unwrap();
    assert!(socket::step(&mut client, &mut writer).unwrap());
    client.partial_since = Some(std::time::Instant::now() - std::time::Duration::from_secs(6));
    assert!(!socket::step(&mut client, &mut writer).unwrap());
}

/// `init` und der Freigabe-Schlüssel: Wer nur ihn ersetzt, muss die
/// Socket-Gruppe nicht nennen; ein vorhandener, aber ungültiger Eintrag
/// gilt als festgehalten — ersetzt wird er nur ausdrücklich.
#[test]
fn replacing_the_intent_key_needs_neither_the_group_nor_a_valid_old_entry() {
    let f = Fixture::new();
    let home = f._dir.path().join("grouped");
    // SAFETY: getegid hat keine Vorbedingungen.
    let gid = unsafe { libc::getegid() }.to_string();
    let request = |socket_group, intent_key, replace_intent_key| InitRequest {
        repo: f.root.to_str().unwrap(),
        mapping: None,
        profile: None,
        socket_group,
        child_repo: None,
        policy_rev: None,
        intent_key,
        replace_intent_key,
    };
    let key = fido::key();
    let other = fido::other_key();
    init_config(
        &home,
        &request(Some(gid.as_str()), Some(key.as_str()), false),
    )
    .unwrap();
    assert_eq!(load(&home).unwrap().intent_key(), Some(key.as_str()));
    // Ohne Gruppe, ohne Ersetzen: eine abweichende Konfiguration.
    let err = init_config(&home, &request(None, Some(other.as_str()), false)).unwrap_err();
    assert!(err.to_string().contains("socket group"), "{err}");
    // Ohne Gruppe, mit Ersetzen: ersetzt, die Gruppe bleibt.
    let Initialized::Unchanged(pins) =
        init_config(&home, &request(None, Some(other.as_str()), true)).unwrap()
    else {
        panic!("expected unchanged");
    };
    assert!(pins.intent_key_new);
    let config = load(&home).unwrap();
    assert_eq!(config.intent_key(), Some(other.as_str()));
    assert!(config.socket_group().is_some());

    // Ein ungültiger Eintrag: Er gilt zur Laufzeit nicht …
    let path = home.join("witness.json");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace(&other, "garbage");
    atomic(&path, text.as_bytes()).unwrap();
    assert!(load(&home).unwrap().intent_key().is_none());
    // … wird aber nicht still durch einen genannten ersetzt.
    let Initialized::Unchanged(pins) = init_config(
        &home,
        &request(Some(gid.as_str()), Some(key.as_str()), false),
    )
    .unwrap() else {
        panic!("expected unchanged");
    };
    assert!(pins.intent_key_refused.is_some());
    assert!(load(&home).unwrap().intent_key().is_none());
    init_config(&home, &request(None, Some(key.as_str()), true)).unwrap();
    assert_eq!(load(&home).unwrap().intent_key(), Some(key.as_str()));
}

/// Eine Freigabe gilt einmal — gemessen am Signaturzähler, der in den
/// signierten Daten steht: Auch eine anders armierte Kopie trägt ihn. Der
/// Weg über das Passwort (Steuer-Socket) hebt den Höchststand ebenso; die
/// Freigabe der Aktivierung muss die nächste Signatur sein.
#[test]
fn an_approval_activates_once_on_any_route() {
    use minds_capture::witness_proto::Frame;
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    writer.verify_activation = accept_all;
    let text = anchor(&f, "x\n");

    // Über den Steuer-Socket aktiviert, mit Zähler 5: Höchststand 5.
    let control = Frame::IntentActivate {
        anchor: text.clone(),
        signature: Some(fido::proof_at(1, 5)),
        request_id: [7; 16],
    };
    assert!(matches!(ask(&mut writer, true, control), Frame::Ack { .. }));
    assert_eq!(intent::counter_mark(&f.home, &fido::key()), Ok(5));

    let (mut client, mut theirs) = connected(false);
    let mut attempt = |writer: &mut Writer, signature: String, proof: String| {
        let nonce = nonce_of(ask_on(writer, &mut client, &mut theirs, challenge()));
        writer.last_verify = None;
        ask_on(
            writer,
            &mut client,
            &mut theirs,
            Frame::IntentActivateProven {
                anchor: text.clone(),
                signature: Some(signature),
                nonce,
                proof,
                request_id: [6; 16],
            },
        )
    };
    let used = refused("this intent approval was already used for an activation — sign again");
    // Dieselbe Freigabe per Berührung: verbraucht.
    assert_eq!(
        attempt(&mut writer, fido::proof_at(1, 5), fido::proof_at(1, 6)),
        used
    );
    // Neu armiert (andere Zeilen, angehängte Leerzeilen): derselbe Zähler.
    let rewrapped = {
        let original = fido::proof_at(1, 5);
        let body: String = original
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(7)
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect();
        format!(
            "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n\n\n",
            lines.join("\n")
        )
    };
    assert_ne!(rewrapped, fido::proof_at(1, 5));
    assert_eq!(attempt(&mut writer, rewrapped, fido::proof_at(1, 6)), used);
    // Eine neue Freigabe, aber eine Aktivierung, die nicht die nächste
    // Signatur ist: abgewiesen.
    assert_eq!(
        attempt(&mut writer, fido::proof_at(1, 7), fido::proof_at(1, 9)),
        refused("the activation touch does not directly follow the approval touch — sign again")
    );
    // Ohne Zähler (0): kein Weg per Berührung.
    assert_eq!(
        attempt(&mut writer, fido::proof_at(1, 0), fido::proof_at(1, 1)),
        refused("the security key reports no signature counter — use the password route")
    );
    // Die richtige Folge: aktiv, Höchststand 8.
    assert!(matches!(
        attempt(&mut writer, fido::proof_at(1, 7), fido::proof_at(1, 8)),
        Frame::Ack { .. }
    ));
    assert_eq!(intent::counter_mark(&f.home, &fido::key()), Ok(8));
}

/// Lässt sich der Höchststand nicht lesen oder festhalten, wird nicht
/// aktiviert — und der Witness läuft weiter.
#[test]
fn an_unrecordable_counter_refuses_the_activation() {
    use minds_capture::witness_proto::Frame;
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    writer.verify_activation = accept_all;
    let text = anchor(&f, "x\n");
    // Ein Stand, der nicht so aussieht, wie der Witness ihn schreibt.
    atomic(&f.home.join("evidence/intent-counter"), b"garbage\n").unwrap();
    assert!(intent::counter_mark(&f.home, &fido::key()).is_err());
    let (mut client, mut theirs) = connected(false);
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    assert_eq!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &nonce, fido::proof(1))
        ),
        refused(crate::intent_proof::COUNTER_UNREADABLE)
    );
    let control = Frame::IntentActivate {
        anchor: text.clone(),
        signature: Some(fido::signature()),
        request_id: [7; 16],
    };
    assert_eq!(
        ask(&mut writer, true, control),
        Frame::Nack {
            request_id: [7; 16],
            reason: crate::intent_proof::COUNTER_UNREADABLE.into(),
        }
    );
    assert!(writer.intent.is_none());
    assert!(!f.home.join("evidence/intent.json").exists());
}

/// Auf dem Weg über das Passwort hebt nur eine **gültige** Signatur des
/// festgehaltenen Schlüssels den Höchststand: Eine erfundene mit hohem
/// Zähler sperrt den Weg per Berührung nicht.
#[test]
fn only_a_valid_signature_raises_the_counter_on_the_control_route() {
    use minds_capture::witness_proto::Frame;
    fn reject(_: &str, _: &str, _: &Path, _: &str) -> Option<bool> {
        Some(false)
    }
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    writer.verify_activation = reject;
    let text = anchor(&f, "x\n");
    let forged = Frame::IntentActivate {
        anchor: text.clone(),
        signature: Some(fido::proof_at(1, u32::MAX - 1)),
        request_id: [7; 16],
    };
    // Eine Signatur, die den festgehaltenen Schlüssel nennt, aber nicht gilt:
    // abgewiesen, nichts aktiviert, der Stand bleibt.
    assert_eq!(
        ask(&mut writer, true, forged),
        Frame::Nack {
            request_id: [7; 16],
            reason: "intent signature invalid".into(),
        }
    );
    assert!(writer.intent.is_none());
    assert_eq!(intent::counter_mark(&f.home, &fido::key()), Ok(0));
    writer.verify_activation = accept_all;
    let real = Frame::IntentActivate {
        anchor: text,
        signature: Some(fido::proof_at(1, 9)),
        request_id: [7; 16],
    };
    assert!(matches!(ask(&mut writer, true, real), Frame::Ack { .. }));
    assert_eq!(intent::counter_mark(&f.home, &fido::key()), Ok(9));
}

/// Höchstens [`intent::MAX_OPEN_CHALLENGES`] Verbindungen halten eine
/// Nonce; wer schon eine hält, bekommt eine neue.
#[test]
fn only_a_few_connections_hold_a_nonce() {
    use minds_capture::witness_proto::Frame;
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    let (mut client, mut theirs) = connected(false);
    let _ = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    assert_eq!(writer.open_challenges, 1);
    writer.open_challenges = intent::MAX_OPEN_CHALLENGES;
    // Diese Verbindung hält schon eine: Sie bekommt eine neue.
    let _ = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    // Eine weitere nicht.
    assert_eq!(
        ask(&mut writer, false, challenge()),
        Frame::Nack {
            request_id: [5; 16],
            reason: "too many open approvals at this witness".into(),
        }
    );
}

/// Lässt sich der neue Höchststand nicht schreiben (das Verzeichnis ist
/// schreibgeschützt), wird nicht aktiviert, der Stand bleibt, und der
/// Witness arbeitet weiter.
#[test]
fn a_counter_that_cannot_be_written_refuses_the_activation() {
    use minds_capture::witness_proto::Frame;
    use std::os::unix::fs::PermissionsExt;
    // Als root schreibt `atomic` auch in ein schreibgeschütztes Verzeichnis.
    // SAFETY: geteuid hat keine Vorbedingungen.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    writer.verify_activation = accept_all;
    let text = anchor(&f, "x\n");
    // Ein lesbarer Stand unter der Freigabe (Zähler 1).
    intent::raise_counter_mark(&f.home, &fido::key(), 0).unwrap();
    let evidence = f.home.join("evidence");
    let (mut client, mut theirs) = connected(false);
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    fs::set_permissions(&evidence, fs::Permissions::from_mode(0o500)).unwrap();
    let answer = ask_on(
        &mut writer,
        &mut client,
        &mut theirs,
        proven(&text, &nonce, fido::proof(1)),
    );
    fs::set_permissions(&evidence, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(answer, refused(crate::intent_proof::NOT_RECORDABLE));
    assert!(writer.intent.is_none());
    assert_eq!(intent::counter_mark(&f.home, &fido::key()), Ok(0));
    // Der Witness arbeitet weiter.
    assert!(matches!(
        ask(&mut writer, false, Frame::Ping),
        Frame::Ack { .. }
    ));
}

/// Hält `key` als Freigabe-Schlüssel in `witness.json` fest — der Witness
/// liest ihn bei jeder Prüfung von dort.
fn pin(f: &Fixture, key: &str) {
    init_config(
        &f.home,
        &InitRequest {
            repo: f.root.to_str().unwrap(),
            mapping: None,
            profile: None,
            socket_group: None,
            child_repo: None,
            policy_rev: None,
            intent_key: Some(key),
            replace_intent_key: true,
        },
    )
    .unwrap();
}

/// Ist eine Signatur des festgehaltenen Schlüssels auf dem Weg über das
/// Passwort nicht prüfbar (Frist, kein `ssh-keygen`), wird nicht
/// aktiviert — sonst bliebe eine echte Freigabe unter dem Höchststand.
#[test]
fn an_uncheckable_signature_is_not_activated_on_the_control_route() {
    use minds_capture::witness_proto::Frame;
    fn unknown(_: &str, _: &str, _: &Path, _: &str) -> Option<bool> {
        None
    }
    let f = Fixture::new();
    let mut writer = f.writer();
    pin(&f, &fido::key());
    writer.verify_activation = unknown;
    let text = anchor(&f, "x\n");
    let control = Frame::IntentActivate {
        anchor: text,
        signature: Some(fido::proof_at(1, 9)),
        request_id: [7; 16],
    };
    assert_eq!(
        ask(&mut writer, true, control),
        Frame::Nack {
            request_id: [7; 16],
            reason: crate::intent_proof::NOT_CHECKABLE.into(),
        }
    );
    assert!(writer.intent.is_none());
    assert_eq!(intent::counter_mark(&f.home, &fido::key()), Ok(0));
}

/// Ein ersetzter Schlüssel gilt sofort — der alte nicht mehr, auch ohne
/// Neustart des Witness.
#[test]
fn a_replaced_intent_key_applies_without_a_restart() {
    let f = Fixture::new();
    let mut writer = f.writer();
    writer.verify_activation = accept_all;
    pin(&f, &fido::key());
    let text = anchor(&f, "x\n");
    let (mut client, mut theirs) = connected(false);
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    pin(&f, &fido::other_key());
    assert_eq!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &nonce, fido::proof(1))
        ),
        refused(crate::intent_proof::NOT_PINNED_PROOF)
    );
    assert!(writer.intent.is_none());
}

/// Auch ein Tausch an Ort und Stelle — gleiche Länge, die Änderungszeit
/// zurückgesetzt — gilt sofort: Die Statuszeit verrät ihn.
#[test]
fn an_in_place_edit_of_the_pin_is_noticed() {
    use std::io::Write;
    let f = Fixture::new();
    let mut writer = f.writer();
    writer.verify_activation = accept_all;
    pin(&f, &fido::key());
    let text = anchor(&f, "x\n");
    let (mut client, mut theirs) = connected(false);
    let nonce = nonce_of(ask_on(&mut writer, &mut client, &mut theirs, challenge()));
    let path = f.home.join("witness.json");
    let before = fs::read_to_string(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let after = before.replace(&fido::key(), &fido::other_key());
    assert_eq!(after.len(), before.len());
    {
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(after.as_bytes()).unwrap();
        file.set_modified(modified).unwrap();
    }
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert_eq!(
        ask_on(
            &mut writer,
            &mut client,
            &mut theirs,
            proven(&text, &nonce, fido::proof(1))
        ),
        refused(crate::intent_proof::NOT_PINNED_PROOF)
    );
}
