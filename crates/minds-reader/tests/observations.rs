//! EA-08: Reconciliation und Korroboration über gespeicherte, versiegelte
//! Beobachtungen des Witness — über einen echten Commit, strikt lesend.

use std::path::Path;
use std::process::Command;

use minds_core::evidence::{SCOPE_WITNESS_FS_V1, Seal, SealOutcome};
use minds_core::observation::{Observation, ObservationReason, Observations};
use minds_core::{
    Agent, ContentHash, Effect, EffectKind, Intent, Model, Role, Session, ToolCall, Turn,
};
use minds_git::Repo;
use minds_reader::observations::{
    Corroboration, corroborations, observations_in_window, session_window,
};
use minds_reader::reconcile::ReconClass;
use minds_store::{ContextStore, InRepoStore};

/// Die Zeitspannen von Beobachtungsfenstern.
fn spans(
    windows: &[minds_reader::observations::Window],
) -> Vec<(jiff::Timestamp, jiff::Timestamp)> {
    windows.iter().map(|w| (w.from, w.to)).collect()
}

/// Fenster ohne Epochen-Beschränkung.
fn plain(
    windows: &[(jiff::Timestamp, jiff::Timestamp)],
) -> Vec<minds_reader::observations::Window> {
    windows
        .iter()
        .map(|(from, to)| minds_reader::observations::Window {
            from: *from,
            to: *to,
            epochs: None,
        })
        .collect()
}

/// Beginn der Epochen in Tests, die einen Kettenanfang **nicht** verankern
/// sollen: nach dem Fensterbeginn der Test-Session (09:59:30) — der Stand vor
/// EA-08a, als kein Kettenanfang verankerte.
const UNANCHORED: &str = "2026-10-02T10:00:00Z";

fn hash(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn write(path: &str, text: &str) -> ToolCall {
    ToolCall {
        name: "Write".into(),
        arguments: serde_json::json!({ "content": text }).to_string(),
        capture: None,
        effect: Some(Effect {
            kind: EffectKind::Write,
            path: Some(path.into()),
            content: None,
            written: Some(hash(text.as_bytes())),
            written_unavailable: None,
        }),
    }
}

fn session(calls: Vec<ToolCall>) -> Session {
    let mut session = Session::new(
        Agent {
            name: "claude-code".into(),
            version: "test".into(),
        },
        Model {
            provider: "test".into(),
            id: "test".into(),
        },
        Intent {
            request: "write the files".into(),
            ..Intent::default()
        },
    );
    session.turns.push(Turn {
        role: Role::Assistant,
        text: String::new(),
        tool_calls: calls,
        parent: None,
        at: Some("2026-10-02T10:00:00Z".into()),
    });
    session
}

fn seen(
    seq: u64,
    path: &str,
    bytes: Option<&[u8]>,
    reason: Option<ObservationReason>,
) -> Observation {
    Observation {
        seq,
        at: format!("2026-10-02T10:00:0{seq}Z"),
        path: path.into(),
        content: bytes.map(hash),
        reason,
    }
}

/// Legt ein redigiertes Objekt und seinen `witness-fs/v1`-Seal ab.
fn seal_observations(store: &InRepoStore, observations: Vec<Observation>) -> ContentHash {
    let redacted = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_observations(Observations::new(UNANCHORED, observations))
        .unwrap();
    let object = store.put_observations(&redacted).unwrap();
    let seal = Seal {
        root: hash(b"root"),
        agent: "witness".into(),
        scope: SCOPE_WITNESS_FS_V1.into(),
        first_seq: 0,
        last_seq: 9,
        events: 10,
        gaps: 0,
        pre_chain: 0,
        outcome: SealOutcome::ObservationsStored {
            observations: object.to_string(),
        },
        previous: None,
        last_event_at: "2026-10-02T10:00:09Z".into(),
    };
    store.put_seal(&seal.to_text().unwrap()).unwrap()
}

#[test]
fn reconcile_with_observations_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let agent = session(vec![
        write("a.rs", "agent\n"),
        write("forged.rs", "forged\n"),
        write("secret.rs", "claimed\n"),
        write("contested.rs", "human\n"),
    ]);
    let redacted = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_session(agent.clone())
        .unwrap();
    let store = InRepoStore::open(dir.path()).unwrap();
    let id = store.put(&redacted).unwrap().id();
    let committed = [
        ("a.rs", "agent\n"),
        ("shell.rs", "shell\n"),
        ("forged.rs", "forged\n"),
        ("secret.rs", "claimed\n"),
        ("contested.rs", "human\n"),
    ];
    for (path, text) in committed {
        std::fs::write(dir.path().join(path), text).unwrap();
    }
    git(dir.path(), &["add", "."]);
    git(
        dir.path(),
        &[
            "commit",
            "-qm",
            &format!("fixture\n\nMinds-Session-Id: {id}"),
        ],
    );
    let witnessed = seal_observations(
        &store,
        vec![
            // Agent-Write, vom Witness gesehen.
            seen(1, "a.rs", Some(b"agent\n"), None),
            // Ein Shell-Schreibzugriff: nur der Witness sah ihn.
            seen(2, "shell.rs", Some(b"shell\n"), None),
            // Zuerst gesehen, danach nur noch opak: Der jüngste Stand
            // bestätigt nichts und widerspricht nichts.
            seen(3, "secret.rs", Some(b"claimed\n"), None),
            seen(4, "secret.rs", None, Some(ObservationReason::TooLarge)),
            // Der Witness sah anderen Inhalt; danach nur eine opake
            // Berührung (etwa ein harter Link in ein ignoriertes
            // Verzeichnis). Die macht den Widerspruch nicht ungeschehen.
            seen(6, "contested.rs", Some(b"agent\n"), None),
            seen(
                7,
                "contested.rs",
                None,
                Some(ObservationReason::OutsideRepo),
            ),
        ],
    );
    // Ein Objekt ohne Seal — etwa vom Agenten selbst unter
    // `refs/minds/observations/` abgelegt — zählt nie.
    let redacted_forgery = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_observations(Observations::new(
            UNANCHORED,
            vec![seen(5, "forged.rs", Some(b"forged\n"), None)],
        ))
        .unwrap();
    store.put_observations(&redacted_forgery).unwrap();

    let before = git(dir.path(), &["show-ref"]);
    let repo = Repo::open(dir.path()).unwrap();
    let commit = repo.head().unwrap().commit().unwrap();
    let (from, to) = session_window(&[&agent]).unwrap();
    // Nur der bestätigte Seal zählt; ohne Bestätigung gibt es nichts.
    let trusted = |seal: &ContentHash, _: &str| seal == &witnessed;
    let observations = observations_in_window(&store, from, to, &trusted);
    assert_eq!(observations.len(), 6);
    assert!(observations_in_window(&store, from, to, &|_, _| false).is_empty());
    // Außerhalb des Fensters: nichts.
    let late: jiff::Timestamp = "2026-10-02T11:00:00Z".parse().unwrap();
    assert!(observations_in_window(&store, late, late, &trusted).is_empty());

    let spellings = minds_reader::artifact::roots_of(&repo);
    let roots: Vec<&Path> = spellings.iter().map(|p| p.as_path()).collect();
    let assessed = minds_reader::artifact::assess(&repo, &roots, commit, &[&agent], &observations)
        .unwrap()
        .unwrap();
    let class = |path: &str| {
        assessed
            .recon
            .files
            .iter()
            .find(|f| f.path == path)
            .unwrap()
            .class
    };
    assert_eq!(class("a.rs"), ReconClass::Explained);
    assert_eq!(class("shell.rs"), ReconClass::ExplainedFsOnly);
    // Behauptet, aber kein Zeuge: höchstens „reported only".
    assert_eq!(class("forged.rs"), ReconClass::ReportedOnly);
    // Opak (zu groß, Secret): nie schlechter als ohne Witness — der Claim
    // trägt, wie ohne Beobachter.
    assert_eq!(class("secret.rs"), ReconClass::ReportedOnly);
    assert_eq!(class("contested.rs"), ReconClass::Unexplained);
    // Dieselbe Eingabe, dasselbe Ergebnis.
    assert_eq!(
        assessed,
        minds_reader::artifact::assess(&repo, &roots, commit, &[&agent], &observations)
            .unwrap()
            .unwrap()
    );

    let corroborated: Vec<(Option<String>, Corroboration)> =
        corroborations(&[&agent], &roots, &observations)
            .into_iter()
            .map(|c| (c.path, c.corroboration))
            .collect();
    assert_eq!(
        corroborated,
        [
            (Some("a.rs".into()), Corroboration::Corroborated),
            (Some("forged.rs".into()), Corroboration::Uncorroborated),
            (Some("secret.rs".into()), Corroboration::Corroborated),
            (Some("contested.rs".into()), Corroboration::Uncorroborated),
        ]
    );
    // Strikt lesend: kein Ref bewegt.
    assert_eq!(git(dir.path(), &["show-ref"]), before);
}

#[test]
fn only_witnessed_sessions_open_the_window() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let mut witnessed = session(vec![]);
    witnessed.intent.request = "witnessed".into();
    let mut other = session(vec![]);
    other.intent.request = "other".into();
    let mut unwitnessed = session(vec![]);
    unwitnessed.intent.request = "unwitnessed".into();
    let ids: Vec<_> = [witnessed, other, unwitnessed]
        .into_iter()
        .map(|s| {
            store
                .put(&pipeline.redact_session(s).unwrap())
                .unwrap()
                .id()
        })
        .collect();
    let (witnessed, other, unwitnessed) = (ids[0], ids[1], ids[2]);
    let seal = |session: minds_core::SessionId, scope: &str, last: &str| Seal {
        root: hash(last.as_bytes()),
        agent: "claude-code".into(),
        scope: scope.into(),
        first_seq: 0,
        last_seq: 1,
        events: 2,
        gaps: 0,
        pre_chain: 0,
        outcome: SealOutcome::Stored {
            session: session.to_string(),
        },
        previous: None,
        last_event_at: last.into(),
    };
    let put = |seal: Seal| store.put_seal(&seal.to_text().unwrap()).unwrap();
    let own = put(seal(witnessed, "witness/v1", "2026-10-02T10:05:00Z"));
    store.record_session_seal(witnessed, &own).unwrap();
    // Ein echter, späterer Seal einer anderen Session — vom Agenten in die
    // Rückverweise der unbezeugten Session eingetragen.
    let foreign = put(seal(other, "witness/v1", "2026-10-02T22:59:00Z"));
    store.record_session_seal(unwitnessed, &foreign).unwrap();
    // Ein lokaler Bereich ist kein bezeugter.
    let local = put(seal(unwitnessed, "agent-hooks/v1", "2026-10-02T23:00:00Z"));
    store.record_session_seal(unwitnessed, &local).unwrap();

    let all = |_: &ContentHash, _: &str| true;
    let found =
        minds_reader::observations::witnessed_sessions(&store, &[witnessed, unwitnessed], &all);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, witnessed);
    assert_eq!(found[0].1.to_string(), "2026-10-02T10:05:00Z");
    // Die unbezeugte Session allein öffnet kein Fenster.
    assert!(
        minds_reader::observations::witnessed_sessions(&store, &[unwitnessed], &all).is_empty()
    );
    // Unbestätigte Seals bezeugen nichts.
    assert!(
        minds_reader::observations::witnessed_sessions(&store, &[witnessed], &|_, _| false)
            .is_empty()
    );
    // Zwei Fenster: Was zwischen ihnen beobachtet wurde, zählt nicht.
    let at = |s: &str| s.parse::<jiff::Timestamp>().unwrap();
    let windows = [
        (at("2026-10-02T09:59:30Z"), at("2026-10-02T10:00:30Z")),
        (at("2026-10-12T09:59:30Z"), at("2026-10-12T10:00:30Z")),
    ];
    let observation = |seq: u64, at: &str, path: &str| Observation {
        seq,
        at: at.into(),
        path: path.into(),
        content: Some(hash(path.as_bytes())),
        reason: None,
    };
    let object = pipeline
        .redact_observations(Observations::new(
            UNANCHORED,
            vec![
                observation(1, "2026-10-02T10:00:05Z", "a.rs"),
                observation(2, "2026-10-05T12:00:00Z", "human.rs"),
                observation(3, "2026-10-12T10:00:05Z", "b.rs"),
            ],
        ))
        .unwrap();
    let object = store.put_observations(&object).unwrap();
    put(Seal {
        root: hash(b"fs"),
        agent: "witness".into(),
        scope: SCOPE_WITNESS_FS_V1.into(),
        first_seq: 0,
        last_seq: 3,
        events: 4,
        gaps: 0,
        pre_chain: 0,
        outcome: SealOutcome::ObservationsStored {
            observations: object.to_string(),
        },
        previous: None,
        last_event_at: "2026-10-12T10:00:05Z".into(),
    });
    let seen: Vec<String> =
        minds_reader::observations::observations_in_windows(&store, &plain(&windows), &all)
            .into_iter()
            .map(|o| o.path)
            .collect();
    assert_eq!(seen, ["a.rs", "b.rs"]);
}

#[test]
fn witness_windows_end_with_the_checkpoints_observation_epoch() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let agent = session(vec![]);
    let id = store
        .put(&pipeline.redact_session(agent.clone()).unwrap())
        .unwrap()
        .id();
    let session_seal = Seal {
        root: hash(b"session"),
        agent: "claude-code".into(),
        scope: "witness/v1".into(),
        first_seq: 0,
        last_seq: 1,
        events: 2,
        gaps: 0,
        pre_chain: 0,
        outcome: SealOutcome::Stored {
            session: id.to_string(),
        },
        previous: None,
        last_event_at: "2026-10-02T10:00:30Z".into(),
    };
    let session_seal = store.put_seal(&session_seal.to_text().unwrap()).unwrap();
    // Eine Beobachtungs-Epoche; `None` als Beobachtungen: ein Block-Seal.
    let epoch = |observations: Option<Vec<(u64, &str, &str)>>,
                 last: &str,
                 previous: Option<&ContentHash>| {
        let outcome = match observations {
            Some(observations) => {
                let object = pipeline
                    .redact_observations(Observations::new(
                        UNANCHORED,
                        observations
                            .into_iter()
                            .map(|(seq, at, path)| Observation {
                                seq,
                                at: at.into(),
                                path: path.into(),
                                content: Some(hash(path.as_bytes())),
                                reason: None,
                            })
                            .collect(),
                    ))
                    .unwrap();
                SealOutcome::ObservationsStored {
                    observations: store.put_observations(&object).unwrap().to_string(),
                }
            }
            None => SealOutcome::Rejected,
        };
        let seal = Seal {
            root: hash(last.as_bytes()),
            agent: "witness".into(),
            scope: SCOPE_WITNESS_FS_V1.into(),
            first_seq: 0,
            last_seq: 9,
            events: 10,
            gaps: 0,
            pre_chain: 0,
            outcome,
            previous: previous.cloned(),
            last_event_at: last.into(),
        };
        store.put_seal(&seal.to_text().unwrap()).unwrap()
    };
    // Die Epoche vor der Session, der Checkpoint der Session (mit dem, was
    // beim Checkpoint noch eingesammelt wurde, und seiner Grenze), dann die
    // Arbeit nach dem Commit.
    let before = epoch(Some(vec![]), "2026-10-02T09:59:00Z", None);
    let checkpoint = epoch(
        Some(vec![
            (1, "2026-10-02T10:00:05Z", "a.rs"),
            (2, "2026-10-02T10:00:31Z", "settled.rs"),
        ]),
        "2026-10-02T10:00:32Z",
        Some(&before),
    );
    let after = epoch(
        Some(vec![(1, "2026-10-02T10:00:35Z", "after.rs")]),
        "2026-10-02T10:00:40Z",
        Some(&checkpoint),
    );

    let at = |s: &str| s.parse::<jiff::Timestamp>().unwrap();
    let windows_with = |trusted: &dyn Fn(&ContentHash, &str) -> bool| {
        minds_reader::observations::witness_windows(&store, &[(id, &agent)], trusted)
    };
    let all = |_: &ContentHash, _: &str| true;
    let windows = windows_with(&all);
    assert_eq!(
        spans(&windows),
        [(at("2026-10-02T09:59:30Z"), at("2026-10-02T10:00:32Z"))]
    );
    // Nur die Epochen der Kette zählen: der Checkpoint, nicht die davor
    // (endet vor dem Fenster) und nicht die danach.
    assert_eq!(
        windows[0].epochs,
        Some(std::collections::BTreeSet::from([checkpoint.clone()]))
    );
    let seen: Vec<String> =
        minds_reader::observations::observations_in_windows(&store, &windows, &all)
            .into_iter()
            .map(|o| o.path)
            .collect();
    assert_eq!(seen, ["a.rs", "settled.rs"]);

    // Ist die Epoche des Checkpoints nicht vertrauenswürdig, ist die
    // früheste vertrauenswürdige die spätere — deren Kette aber über die
    // unbestätigte führt: unvollständig, kein Fenster (wie ohne Witness).
    // Nie das Fenster bis zur späteren Epoche.
    assert!(windows_with(&|seal: &ContentHash, _: &str| seal != &checkpoint).is_empty());
    // Ebenso, wenn ihr Vorgänger nicht vertrauenswürdig ist …
    assert!(windows_with(&|seal: &ContentHash, _: &str| seal != &before).is_empty());
    // … und ohne bezeugte Beobachtungs-Epoche.
    assert!(windows_with(&|seal: &ContentHash, _: &str| seal == &session_seal).is_empty());
    let _after = after;
}

#[test]
fn a_missing_checkpoint_epoch_never_widens_the_window() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let agent = session(vec![]);
    let id = store
        .put(&pipeline.redact_session(agent.clone()).unwrap())
        .unwrap()
        .id();
    let put = |seal: Seal| store.put_seal(&seal.to_text().unwrap()).unwrap();
    let seal =
        |scope: &str, outcome: SealOutcome, last: &str, previous: Option<ContentHash>| Seal {
            root: hash(last.as_bytes()),
            agent: "witness".into(),
            scope: scope.into(),
            first_seq: 0,
            last_seq: 1,
            events: 2,
            gaps: 0,
            pre_chain: 0,
            outcome,
            previous,
            last_event_at: last.into(),
        };
    put(seal(
        "witness/v1",
        SealOutcome::Stored {
            session: id.to_string(),
        },
        "2026-10-02T10:00:30Z",
        None,
    ));
    let empty = store
        .put_observations(
            &pipeline
                .redact_observations(Observations::new(UNANCHORED, Vec::new()))
                .unwrap(),
        )
        .unwrap();
    let stored = || SealOutcome::ObservationsStored {
        observations: empty.to_string(),
    };
    put(seal(
        "witness-fs/v1",
        stored(),
        "2026-10-02T09:59:00Z",
        None,
    ));
    // Die Epoche des Checkpoints wurde gelöscht (der Agent kann Refs unter
    // `refs/minds/` schreiben): Sie fehlt, ihr Nachfolger nennt sie aber.
    let deleted = hash(b"deleted checkpoint epoch");
    put(seal(
        "witness-fs/v1",
        stored(),
        "2026-10-02T10:10:00Z",
        Some(deleted),
    ));
    let at = |s: &str| s.parse::<jiff::Timestamp>().unwrap();
    let all = |_: &ContentHash, _: &str| true;
    // Die Kette des Nachfolgers führt über die gelöschte Epoche: kein
    // Fenster — nie eines bis 10:10 mit der Arbeit nach dem Commit.
    assert!(minds_reader::observations::witness_windows(&store, &[(id, &agent)], &all).is_empty());

    // Ein Block-Seal schließt die Epoche des Checkpoints ebenso ab — aber in
    // ihr fehlen Beobachtungen: Die jüngste vorhandene wäre nicht die
    // jüngste. Für diese Session gilt dann keine Beobachtung.
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    let id = store
        .put(&pipeline.redact_session(agent.clone()).unwrap())
        .unwrap()
        .id();
    let put = |seal: Seal| store.put_seal(&seal.to_text().unwrap()).unwrap();
    put(seal(
        "witness/v1",
        SealOutcome::Stored {
            session: id.to_string(),
        },
        "2026-10-02T10:00:30Z",
        None,
    ));
    let empty = store
        .put_observations(
            &pipeline
                .redact_observations(Observations::new(UNANCHORED, Vec::new()))
                .unwrap(),
        )
        .unwrap();
    let before = put(seal(
        "witness-fs/v1",
        SealOutcome::ObservationsStored {
            observations: empty.to_string(),
        },
        "2026-10-02T09:59:00Z",
        None,
    ));
    let rejected = put(seal(
        "witness-fs/v1",
        SealOutcome::Rejected,
        "2026-10-02T10:00:32Z",
        Some(before),
    ));
    put(seal(
        "witness-fs/v1",
        SealOutcome::ObservationsStored {
            observations: empty.to_string(),
        },
        "2026-10-02T10:10:00Z",
        Some(rejected),
    ));
    assert!(minds_reader::observations::witness_windows(&store, &[(id, &agent)], &all).is_empty());

    // Nur eine Kette, die eine Epoche **vor** dem Fensterbeginn erreicht,
    // ist vollständig. Der Anfang einer Kette (erster Checkpoint nach einem
    // Witness-Start) reicht nicht — er kann nach dem Commit liegen.
    for (previous_last, end) in [
        (None, None),
        (Some("2026-10-02T10:00:31Z"), None),
        (Some("2026-10-02T10:00:10Z"), None),
        (Some("2026-10-02T09:59:00Z"), Some("2026-10-02T10:00:32Z")),
    ] {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        let store = InRepoStore::open(dir.path()).unwrap();
        let id = store
            .put(&pipeline.redact_session(agent.clone()).unwrap())
            .unwrap()
            .id();
        let put = |seal: Seal| store.put_seal(&seal.to_text().unwrap()).unwrap();
        put(seal(
            "witness/v1",
            SealOutcome::Stored {
                session: id.to_string(),
            },
            "2026-10-02T10:00:30Z",
            None,
        ));
        let empty = store
            .put_observations(
                &pipeline
                    .redact_observations(Observations::new(UNANCHORED, Vec::new()))
                    .unwrap(),
            )
            .unwrap();
        let stored = || SealOutcome::ObservationsStored {
            observations: empty.to_string(),
        };
        let previous = previous_last.map(|last| put(seal("witness-fs/v1", stored(), last, None)));
        put(seal(
            "witness-fs/v1",
            stored(),
            "2026-10-02T10:00:32Z",
            previous,
        ));
        let windows = minds_reader::observations::witness_windows(&store, &[(id, &agent)], &all);
        match end {
            Some(end) => assert_eq!(
                spans(&windows),
                [(at("2026-10-02T09:59:30Z"), at(end))],
                "{previous_last:?}"
            ),
            None => assert!(windows.is_empty(), "{previous_last:?}"),
        }
    }
}

#[test]
fn only_a_complete_chain_of_epochs_counts() {
    let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let agent = session(vec![]);
    let all = |_: &ContentHash, _: &str| true;
    let at = |s: &str| s.parse::<jiff::Timestamp>().unwrap();
    // Ein frisches Repo mit der bezeugten Session (bezeugt bis 10:00:30).
    let setup = || {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        let store = InRepoStore::open(dir.path()).unwrap();
        let id = store
            .put(&pipeline.redact_session(agent.clone()).unwrap())
            .unwrap()
            .id();
        let seal = Seal {
            root: hash(b"session"),
            agent: "claude-code".into(),
            scope: "witness/v1".into(),
            first_seq: 0,
            last_seq: 1,
            events: 2,
            gaps: 0,
            pre_chain: 0,
            outcome: SealOutcome::Stored {
                session: id.to_string(),
            },
            previous: None,
            last_event_at: "2026-10-02T10:00:30Z".into(),
        };
        store.put_seal(&seal.to_text().unwrap()).unwrap();
        (dir, store, id)
    };
    // Eine Epoche: eine Beobachtung `(at, path, inhalt)` oder ein Block-Seal.
    let epoch = |store: &InRepoStore,
                 observation: Option<(&str, &str, &[u8])>,
                 last: &str,
                 previous: Option<ContentHash>| {
        let outcome = match observation {
            Some((at, path, bytes)) => {
                let object = pipeline
                    .redact_observations(Observations::new(
                        UNANCHORED,
                        vec![Observation {
                            seq: 1,
                            at: at.into(),
                            path: path.into(),
                            content: Some(hash(bytes)),
                            reason: None,
                        }],
                    ))
                    .unwrap();
                SealOutcome::ObservationsStored {
                    observations: store.put_observations(&object).unwrap().to_string(),
                }
            }
            None => SealOutcome::Rejected,
        };
        let seal = Seal {
            root: hash(last.as_bytes()),
            agent: "witness".into(),
            scope: SCOPE_WITNESS_FS_V1.into(),
            first_seq: 0,
            last_seq: 1,
            events: 2,
            gaps: 0,
            pre_chain: 0,
            outcome,
            previous,
            last_event_at: last.into(),
        };
        store.put_seal(&seal.to_text().unwrap()).unwrap()
    };

    // Eine gelöschte Epoche mitten in der Kette: E1 sah X, die gelöschte
    // sah Y (der jüngere Stand). Ohne sie wäre X „die jüngste" — kein
    // Fenster.
    let (_dir, store, id) = setup();
    epoch(
        &store,
        Some(("2026-10-02T10:00:05Z", "a.rs", b"X")),
        "2026-10-02T10:00:06Z",
        None,
    );
    epoch(
        &store,
        Some(("2026-10-02T10:00:31Z", "b.rs", b"B")),
        "2026-10-02T10:00:32Z",
        Some(hash(b"deleted middle epoch")),
    );
    assert!(minds_reader::observations::witness_windows(&store, &[(id, &agent)], &all).is_empty());

    // Die Epoche des Checkpoints ist ein Block-Seal (ihre Beobachtungen
    // fehlen) — auch mit einem Anker vor dem Fenster kein Fenster, und die
    // ältere Lesung X wird nicht zur jüngsten.
    let (_dir, store, id) = setup();
    let anchor = epoch(&store, None, "2026-10-02T09:59:00Z", None);
    let older = epoch(
        &store,
        Some(("2026-10-02T10:00:05Z", "a.rs", b"X")),
        "2026-10-02T10:00:06Z",
        Some(anchor),
    );
    epoch(&store, None, "2026-10-02T10:00:32Z", Some(older));
    assert!(minds_reader::observations::witness_windows(&store, &[(id, &agent)], &all).is_empty());

    // Neustart, dessen erste Epoche die des Checkpoints wäre, während die
    // des alten Laufs gelöscht ist: Der neue Kettenanfang belegt nicht, dass
    // er vor dem Commit begann — kein Fenster (sonst zählte die Arbeit nach
    // dem Commit).
    let (_dir, store, id) = setup();
    epoch(
        &store,
        Some(("2026-10-02T10:05:00Z", "a.rs", b"after commit")),
        "2026-10-02T10:05:01Z",
        None,
    );
    assert!(minds_reader::observations::witness_windows(&store, &[(id, &agent)], &all).is_empty());

    // Eine vollständige Kette (sie erreicht eine Epoche vor dem Fenster):
    // Es zählen nur ihre Epochen — die Lesung X einer fremden Kette nicht,
    // und ein Block-Seal vor dem Fenster stört nicht.
    let (_dir, store, id) = setup();
    epoch(
        &store,
        Some(("2026-10-02T10:00:05Z", "a.rs", b"X")),
        "2026-10-02T10:00:06Z",
        None,
    );
    let anchor = epoch(&store, None, "2026-10-02T09:59:00Z", None);
    let middle = epoch(
        &store,
        Some(("2026-10-02T10:00:10Z", "b.rs", b"B")),
        "2026-10-02T10:00:11Z",
        Some(anchor),
    );
    let checkpoint = epoch(
        &store,
        Some(("2026-10-02T10:00:20Z", "a.rs", b"Y")),
        "2026-10-02T10:00:32Z",
        Some(middle.clone()),
    );
    let windows = minds_reader::observations::witness_windows(&store, &[(id, &agent)], &all);
    assert_eq!(
        spans(&windows),
        [(at("2026-10-02T09:59:30Z"), at("2026-10-02T10:00:32Z"))]
    );
    assert_eq!(
        windows[0].epochs,
        Some(std::collections::BTreeSet::from([middle, checkpoint]))
    );
    let seen: Vec<Option<ContentHash>> =
        minds_reader::observations::observations_in_windows(&store, &windows, &all)
            .into_iter()
            .map(|o| o.observed.hash)
            .collect();
    assert_eq!(seen, [Some(hash(b"B")), Some(hash(b"Y"))]);
}

/// EA-08a: ein frisches Repo mit der bezeugten Session (Zug um 10:00:00,
/// Fensterbeginn 09:59:30, bezeugt bis 10:00:30).
fn witnessed_repo() -> (
    tempfile::TempDir,
    InRepoStore,
    minds_core::SessionId,
    Session,
) {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    let agent = session(vec![]);
    let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let id = store
        .put(&pipeline.redact_session(agent.clone()).unwrap())
        .unwrap()
        .id();
    let seal = Seal {
        root: hash(b"session"),
        agent: "claude-code".into(),
        scope: "witness/v1".into(),
        first_seq: 0,
        last_seq: 1,
        events: 2,
        gaps: 0,
        pre_chain: 0,
        outcome: SealOutcome::Stored {
            session: id.to_string(),
        },
        previous: None,
        last_event_at: "2026-10-02T10:00:30Z".into(),
    };
    store.put_seal(&seal.to_text().unwrap()).unwrap();
    (dir, store, id, agent)
}

/// Legt ein Observation-Objekt (Schema 2) mit Beginn `started_at` und einer
/// Beobachtung von `a.rs` um `at` ab.
fn started_object(store: &InRepoStore, started_at: &str, at: &str) -> ContentHash {
    let object = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_observations(Observations::new(
            started_at,
            vec![Observation {
                seq: 1,
                at: at.into(),
                path: "a.rs".into(),
                content: Some(hash(b"A")),
                reason: None,
            }],
        ))
        .unwrap();
    store.put_observations(&object).unwrap()
}

/// Legt einen `witness-fs/v1`-Seal ab, der `object` nennt (`None`: ein
/// Block-Seal).
fn fs_epoch(
    store: &InRepoStore,
    object: Option<&ContentHash>,
    last: &str,
    previous: Option<ContentHash>,
) -> ContentHash {
    let seal = Seal {
        root: hash(format!("{last}{object:?}").as_bytes()),
        agent: "witness".into(),
        scope: SCOPE_WITNESS_FS_V1.into(),
        first_seq: 0,
        last_seq: 1,
        events: 2,
        gaps: 0,
        pre_chain: 0,
        outcome: match object {
            Some(object) => SealOutcome::ObservationsStored {
                observations: object.to_string(),
            },
            None => SealOutcome::Rejected,
        },
        previous,
        last_event_at: last.into(),
    };
    store.put_seal(&seal.to_text().unwrap()).unwrap()
}

fn windows_of(
    store: &InRepoStore,
    id: minds_core::SessionId,
    agent: &Session,
) -> Vec<minds_reader::observations::Window> {
    minds_reader::observations::witness_windows(store, &[(id, agent)], &|_, _| true)
}

#[test]
fn chain_start_before_window_anchors() {
    let at = |s: &str| s.parse::<jiff::Timestamp>().unwrap();
    // Witness-Start vor dem Fensterbeginn — auch genau auf ihm.
    for started_at in ["2026-10-02T09:59:00Z", "2026-10-02T09:59:30Z"] {
        let (_dir, store, id, agent) = witnessed_repo();
        let object = started_object(&store, started_at, "2026-10-02T10:00:05Z");
        let start = fs_epoch(&store, Some(&object), "2026-10-02T10:00:32Z", None);
        let windows = windows_of(&store, id, &agent);
        assert_eq!(
            spans(&windows),
            [(at("2026-10-02T09:59:30Z"), at("2026-10-02T10:00:32Z"))],
            "{started_at}"
        );
        assert_eq!(
            windows[0].epochs,
            Some(std::collections::BTreeSet::from([start]))
        );
        let seen: Vec<String> =
            minds_reader::observations::observations_in_windows(&store, &windows, &|_, _| true)
                .into_iter()
                .map(|o| o.path)
                .collect();
        assert_eq!(seen, ["a.rs"]);
    }

    // Der Anfang darf auch weiter zurück liegen: Die Kette läuft über eine
    // Epoche, die im Fenster endet, bis zum Anfang des Laufs.
    let (_dir, store, id, agent) = witnessed_repo();
    let first = started_object(&store, "2026-10-02T09:00:00Z", "2026-10-02T09:59:50Z");
    let first = fs_epoch(&store, Some(&first), "2026-10-02T10:00:01Z", None);
    let object = started_object(&store, "2026-10-02T10:00:01.001Z", "2026-10-02T10:00:05Z");
    let checkpoint = fs_epoch(
        &store,
        Some(&object),
        "2026-10-02T10:00:32Z",
        Some(first.clone()),
    );
    let windows = windows_of(&store, id, &agent);
    assert_eq!(
        windows[0].epochs,
        Some(std::collections::BTreeSet::from([first, checkpoint]))
    );
}

#[test]
fn chain_start_after_window_start_stays_incomplete() {
    // Neustart während der Session (nach dem Fensterbeginn): Was davor
    // geschah, sah dieser Lauf nicht — kein Fenster.
    for started_at in ["2026-10-02T09:59:30.001Z", "2026-10-02T10:00:01Z"] {
        let (_dir, store, id, agent) = witnessed_repo();
        let object = started_object(&store, started_at, "2026-10-02T10:00:05Z");
        fs_epoch(&store, Some(&object), "2026-10-02T10:00:32Z", None);
        assert!(windows_of(&store, id, &agent).is_empty(), "{started_at}");
    }
    // Ein Block-Seal als Anfang trägt keinen Beginn.
    let (_dir, store, id, agent) = witnessed_repo();
    fs_epoch(&store, None, "2026-10-02T10:00:32Z", None);
    assert!(windows_of(&store, id, &agent).is_empty());
    // Ein Anfang, dessen Objekt fehlt (gelöscht), ebenso nicht.
    let (_dir, store, id, agent) = witnessed_repo();
    fs_epoch(
        &store,
        Some(&hash(b"deleted object")),
        "2026-10-02T10:00:32Z",
        None,
    );
    assert!(windows_of(&store, id, &agent).is_empty());
}

#[test]
fn restart_and_deleted_epoch_never_widens_window() {
    // Regression aus dem EA-08-Review: Die Session lief im alten Lauf.
    let at = |s: &str| s.parse::<jiff::Timestamp>().unwrap();
    let old_run = |store: &InRepoStore| {
        let object = started_object(store, "2026-10-02T09:00:00Z", "2026-10-02T09:58:00Z");
        fs_epoch(store, Some(&object), "2026-10-02T09:59:00Z", None)
    };
    // Der neue Lauf begann nach dem Commit; seine erste Epoche sah die
    // Arbeit danach.
    let new_run = |store: &InRepoStore| {
        let object = started_object(store, "2026-10-02T10:02:00Z", "2026-10-02T10:05:00Z");
        fs_epoch(store, Some(&object), "2026-10-02T10:05:01Z", None)
    };
    // Steht die Checkpoint-Epoche des alten Laufs noch da, endet das Fenster
    // mit ihr.
    let (_dir, store, id, agent) = witnessed_repo();
    let anchor = old_run(&store);
    let object = started_object(&store, "2026-10-02T09:59:00.001Z", "2026-10-02T10:00:05Z");
    fs_epoch(&store, Some(&object), "2026-10-02T10:00:32Z", Some(anchor));
    new_run(&store);
    assert_eq!(
        spans(&windows_of(&store, id, &agent)),
        [(at("2026-10-02T09:59:30Z"), at("2026-10-02T10:00:32Z"))]
    );
    // Ist sie gelöscht, wäre die erste Epoche des neuen Laufs die früheste
    // danach. Ihr Beginn liegt nach dem Fensterbeginn — sie verankert nicht,
    // kein Fenster bis 10:05.
    let (_dir, store, id, agent) = witnessed_repo();
    old_run(&store);
    new_run(&store);
    assert!(windows_of(&store, id, &agent).is_empty());
}

#[test]
fn schema1_chain_start_never_anchors() {
    // Ein Kettenanfang aus der Zeit vor EA-08a: Sein Objekt nennt keinen
    // Beginn — kein Beleg, kein Fenster, auch wenn die Beobachtung passt.
    let (dir, store, id, agent) = witnessed_repo();
    let bytes = format!(
        concat!(
            r#"{{"first_at":"2026-10-02T10:00:05Z","last_at":"2026-10-02T10:00:05Z","#,
            r#""observations":[{{"at":"2026-10-02T10:00:05Z","content":"{}","#,
            r#""path":"a.rs","reason":null,"seq":1}}],"schema":1}}"#
        ),
        hash(b"A")
    );
    let object = put_raw_observations(dir.path(), bytes.as_bytes());
    let read = store.get_observations(&object).unwrap().unwrap();
    assert_eq!((read.schema, read.started_at()), (1, None));
    fs_epoch(&store, Some(&object), "2026-10-02T10:00:32Z", None);
    assert!(windows_of(&store, id, &agent).is_empty());
    // Ein Schema-1-Objekt **mit** `started_at` (so schrieb es nie ein
    // Witness) verankert ebenso nicht.
    let (dir, store, id, agent) = witnessed_repo();
    let forged = bytes.replace(
        r#""schema":1"#,
        r#""schema":1,"started_at":"2026-10-02T09:00:00Z""#,
    );
    let object = put_raw_observations(dir.path(), forged.as_bytes());
    fs_epoch(&store, Some(&object), "2026-10-02T10:00:32Z", None);
    assert!(windows_of(&store, id, &agent).is_empty());
    // Ein künftiges Schema mit frühem Beginn: Was `started_at` dort heißt,
    // weiß dieses Binary nicht — es verankert nicht (fail-closed).
    let (dir, store, id, agent) = witnessed_repo();
    let future = bytes.replace(
        r#""schema":1"#,
        r#""schema":3,"started_at":"2026-10-02T09:00:00Z""#,
    );
    let object = put_raw_observations(dir.path(), future.as_bytes());
    fs_epoch(&store, Some(&object), "2026-10-02T10:00:32Z", None);
    assert!(windows_of(&store, id, &agent).is_empty());
    // Dieselben Bytes mit Schema 2 verankern — es liegt am Schema.
    let (dir, store, id, agent) = witnessed_repo();
    let current = bytes.replace(
        r#""schema":1"#,
        r#""schema":2,"started_at":"2026-10-02T09:00:00Z""#,
    );
    let object = put_raw_observations(dir.path(), current.as_bytes());
    fs_epoch(&store, Some(&object), "2026-10-02T10:00:32Z", None);
    assert_eq!(windows_of(&store, id, &agent).len(), 1);
}

/// Legt Bytes als Observation-Objekt ab, an der Pipeline vorbei (wie ein
/// altes Binary) — über Git-Plumbing.
fn put_raw_observations(repo: &Path, bytes: &[u8]) -> ContentHash {
    use std::io::Write as _;
    let id = Observations::id_of_bytes(bytes);
    let file = repo.join("raw-observations");
    std::fs::write(&file, bytes).unwrap();
    let blob = git(repo, &["hash-object", "-w", file.to_str().unwrap()]);
    let mut mktree = Command::new("git")
        .current_dir(repo)
        .arg("mktree")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    mktree
        .stdin
        .take()
        .unwrap()
        .write_all(format!("100644 blob {}\tobservations.json\n", blob.trim()).as_bytes())
        .unwrap();
    let tree = String::from_utf8(mktree.wait_with_output().unwrap().stdout).unwrap();
    let commit = git(repo, &["commit-tree", tree.trim(), "-m", "raw"]);
    git(
        repo,
        &[
            "update-ref",
            &format!("refs/minds/observations/{}", id.hex()),
            commit.trim(),
        ],
    );
    id
}

/// EA-17: Nennt ein bezeugter Seal ein Objekt, das fehlt, ist das Material
/// unvollständig — Scope-Befunde dürfen dann nicht „keine" heißen.
#[test]
fn a_missing_observations_object_makes_the_window_incomplete() {
    use minds_reader::observations::{Window, observations_in_windows_checked};

    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    let window = |epochs: Option<std::collections::BTreeSet<ContentHash>>| Window {
        from: "2026-10-02T10:00:00Z".parse().unwrap(),
        to: "2026-10-02T10:00:09Z".parse().unwrap(),
        epochs,
    };
    let readable = seal_observations(&store, vec![seen(1, "a.rs", Some(b"a\n"), None)]);
    let trusted = |_: &ContentHash, _: &str| true;
    let read = observations_in_windows_checked(&store, &[window(None)], &trusted);
    assert!(read.complete);
    assert_eq!(read.observations.len(), 1);

    // Ein zweiter Seal nennt ein Objekt, das nicht (mehr) im Store liegt.
    let orphan = Seal {
        root: hash(b"other root"),
        agent: "witness".into(),
        scope: SCOPE_WITNESS_FS_V1.into(),
        first_seq: 0,
        last_seq: 3,
        events: 4,
        gaps: 0,
        pre_chain: 0,
        outcome: SealOutcome::ObservationsStored {
            observations: hash(b"removed object").to_string(),
        },
        previous: None,
        last_event_at: "2026-10-02T10:00:05Z".into(),
    };
    let orphan = store.put_seal(&orphan.to_text().unwrap()).unwrap();
    let read = observations_in_windows_checked(&store, &[window(None)], &trusted);
    assert!(!read.complete);
    // Was lesbar ist, bleibt lesbar.
    assert_eq!(read.observations.len(), 1);

    // Nur Epochen, die ein Fenster nennt, zählen; ein unbestätigter Seal nie.
    let only_readable = window(Some([readable.clone()].into()));
    assert!(observations_in_windows_checked(&store, &[only_readable], &trusted).complete);
    let both = window(Some([readable, orphan.clone()].into()));
    assert!(
        !observations_in_windows_checked(&store, std::slice::from_ref(&both), &trusted).complete
    );
    let untrusted = |id: &ContentHash, _: &str| *id != orphan;
    assert!(observations_in_windows_checked(&store, &[both], &untrusted).complete);

    // Eine Epoche mit Lücken: Der Witness hat Ereignisse verloren — auch
    // mit lesbarem Objekt unvollständig.
    let object = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_observations(Observations::new(
            UNANCHORED,
            vec![seen(2, "b.rs", Some(b"b\n"), None)],
        ))
        .unwrap();
    let gapped = Seal {
        root: hash(b"gapped root"),
        agent: "witness".into(),
        scope: SCOPE_WITNESS_FS_V1.into(),
        first_seq: 0,
        last_seq: 3,
        events: 3,
        gaps: 1,
        pre_chain: 0,
        outcome: SealOutcome::ObservationsStored {
            observations: store.put_observations(&object).unwrap().to_string(),
        },
        previous: None,
        last_event_at: "2026-10-02T10:00:06Z".into(),
    };
    let gapped = store.put_seal(&gapped.to_text().unwrap()).unwrap();
    let read = observations_in_windows_checked(&store, &[window(Some([gapped].into()))], &trusted);
    assert!(!read.complete);
    assert_eq!(read.observations.len(), 1);
}

/// EA-17: Eine bezeugte Session ohne geschlossenes Beobachtungsfenster (hier:
/// kein `witness-fs/v1`-Seal mehr — etwa das Objekt der ersten Epoche
/// gelöscht) ist unvollständig, nicht „nichts beobachtet". Eine unbezeugte
/// Session ohne Fenster ist vollständig: Es gab nichts zu beobachten.
#[test]
fn a_witnessed_session_without_a_window_is_incomplete() {
    use minds_reader::observations::session_observations;

    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let store = InRepoStore::open(dir.path()).unwrap();
    let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let mut witnessed = session(vec![]);
    witnessed.intent.request = "witnessed".into();
    let mut local = session(vec![]);
    local.intent.request = "local".into();
    let put_session = |s: &Session| {
        store
            .put(&pipeline.redact_session(s.clone()).unwrap())
            .unwrap()
            .id()
    };
    let (witnessed_id, local_id) = (put_session(&witnessed), put_session(&local));
    let seal = Seal {
        root: hash(b"witnessed"),
        agent: "claude-code".into(),
        scope: "witness/v1".into(),
        first_seq: 0,
        last_seq: 1,
        events: 2,
        gaps: 0,
        pre_chain: 0,
        outcome: SealOutcome::Stored {
            session: witnessed_id.to_string(),
        },
        previous: None,
        last_event_at: "2026-10-02T10:05:00Z".into(),
    };
    store.put_seal(&seal.to_text().unwrap()).unwrap();
    let all = |_: &ContentHash, _: &str| true;

    let (windows, read) = session_observations(&store, witnessed_id, &witnessed, &all);
    assert!(windows.is_empty());
    assert!(!read.complete);
    assert!(read.observations.is_empty());

    let (windows, read) = session_observations(&store, local_id, &local, &all);
    assert!(windows.is_empty());
    assert!(read.complete);
    // Unbestätigt bezeugt nichts — auch keine Unvollständigkeit.
    let (_, read) = session_observations(&store, witnessed_id, &witnessed, &|_, _| false);
    assert!(read.complete);
}
