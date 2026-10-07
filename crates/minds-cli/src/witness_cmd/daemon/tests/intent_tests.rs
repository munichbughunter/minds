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
        .activate_intent(&text, Some(SIGNATURE.into()))
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
    writer.activate_intent(&first, None).unwrap().unwrap();
    hook_as(&mut writer, "Stop", 1);
    let second = anchor(&f, "Version 2\n");
    writer.activate_intent(&second, None).unwrap().unwrap();
    // Dieselbe Aktivierung noch einmal: nichts Neues.
    assert_eq!(
        writer.activate_intent(&second, None).unwrap().unwrap(),
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
        .activate_intent(&anchor(&f, "x\n"), None)
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
        writer.activate_intent("minds-intent-v1\n", None).unwrap(),
        Err("invalid intent anchor")
    );
    assert_eq!(
        writer
            .activate_intent(&good.replace("src/**", "src/**\u{202E}"), None)
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
            writer.activate_intent(&good, Some(signature)).unwrap(),
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
        writer.activate_intent(&leaky, None).unwrap(),
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
        writer.activate_intent(&text, None).unwrap().unwrap();
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
        .activate_intent(&anchor(&f, "y\n"), None)
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
        .activate_intent(&anchor(&f, "x\n"), None)
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
        .activate_intent(&anchor(&f, "x\n"), None)
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
        writer.activate_intent(&text, None).unwrap().unwrap();
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
            .activate_intent(&anchor(&f, "x\n"), None)
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
        .activate_intent(&anchor(&f, "x\n"), None)
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
        .activate_intent(&anchor(&f, "x\n"), None)
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
    f.writer().activate_intent(&text, None).unwrap().unwrap();
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
