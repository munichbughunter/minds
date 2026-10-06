//! EA-14: `intent_of` über gespeichertes Material — Bindung, Verkettung,
//! Anfang der Session, Wechsel, Snapshot-Abgleich, Epochen-Kette.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use minds_core::evidence::{SCOPE_WITNESS_V1, Seal, SealOutcome};
use minds_core::intent_anchor::{IntentEvent, IntentSource};
use minds_core::{Agent, ContentHash, Intent, Model, Session, SessionId};
use minds_reader::assurance::{IntentSignature, IntentState, SignerKind};
use minds_reader::intent::{EpochChain, epoch_chain, intent_of};
use minds_store::{ContextStore, InRepoStore};

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

struct Fixture {
    _dir: tempfile::TempDir,
    store: InRepoStore,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        let store = InRepoStore::open(dir.path()).unwrap();
        Self { _dir: dir, store }
    }

    /// Legt einen Anker über `snapshot` ab und gibt seine Id zurück.
    fn anchor(&self, snapshot: &str) -> ContentHash {
        let intent = minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact_intent(
                IntentSource::Prompt,
                vec!["src/**".into()],
                snapshot.as_bytes().to_vec(),
            )
            .unwrap();
        self.store.put_intent(&intent).unwrap()
    }
}

fn session() -> Session {
    Session::new(
        Agent {
            name: "claude-code".into(),
            version: "test".into(),
        },
        Model {
            provider: "test".into(),
            id: "test".into(),
        },
        Intent {
            request: "mach x".into(),
            ..Intent::default()
        },
    )
}

fn event(seq: u64, anchor_id: &ContentHash, opens_session: bool) -> IntentEvent {
    IntentEvent {
        seq,
        anchor_id: anchor_id.clone(),
        opens_session,
    }
}

fn unchecked(_: &str, _: &str) -> IntentSignature {
    IntentSignature::NotChecked
}

fn witnessed(sessions: &[&Session]) -> EpochChain {
    EpochChain {
        sessions: sessions.iter().map(|s| (*s).clone()).collect(),
        witnessed: true,
    }
}

fn of(f: &Fixture, chain: &EpochChain) -> IntentState {
    intent_of(chain, &f.store, &unchecked)
}

#[derive(Clone, Copy)]
struct Expect {
    chained: bool,
    signature: IntentSignature,
    matches: bool,
    from_start: bool,
    changed: bool,
}

const GENESIS: Expect = Expect {
    chained: true,
    signature: IntentSignature::Unsigned,
    matches: true,
    from_start: true,
    changed: false,
};

fn bound(anchor_id: &ContentHash, e: Expect) -> IntentState {
    IntentState::Bound {
        anchor_id: anchor_id.clone(),
        chained: e.chained,
        signature: e.signature,
        snapshot_matches: e.matches,
        from_session_start: e.from_start,
        changed_mid_session: e.changed,
    }
}

#[test]
fn legacy_session_intent_unbound() {
    let f = Fixture::new();
    let legacy = session();
    assert_eq!(of(&f, &witnessed(&[&legacy])), IntentState::Unbound);
    assert_eq!(of(&f, &witnessed(&[])), IntentState::Unbound);
    // Eingefrorene Bytes einer Session von vor EA-14: Sie liest sich, und
    // ihre kanonische Form (und damit ihre Id) bleibt byte-gleich — keines
    // der neuen Felder erscheint. Der eingefrorene Hash dazu steht in
    // `minds-core` (`id.rs`, `GOLDEN_CANONICAL`) und gilt unverändert.
    const LEGACY: &str = r#"{"agent":{"name":"claude-code","version":"test"},"intent":{"constraints":[],"discarded":[],"request":"mach x"},"model":{"id":"test","provider":"test"},"produced":{"files":[]},"redaction":{"applied":false,"counts":{"pii":0,"secrets":0}},"schema_version":2,"turns":[],"usage":{"input_tokens":0,"output_tokens":0}}"#;
    let back: Session = serde_json::from_str(LEGACY).unwrap();
    assert_eq!(back, legacy);
    assert_eq!(minds_core::to_canonical_string(&back).unwrap(), LEGACY);
    assert_eq!(
        SessionId::of(&back).unwrap(),
        SessionId::of(&legacy).unwrap()
    );
}

#[test]
fn a_witnessed_genesis_intent_is_bound_from_the_start() {
    let f = Fixture::new();
    let id = f.anchor("Retry exponentiell.\n");
    let mut s = session();
    s.intent_events = vec![event(0, &id, true)];
    assert_eq!(of(&f, &witnessed(&[&s])), bound(&id, GENESIS));
}

#[test]
fn a_later_epoch_inherits_the_genesis_intent() {
    let f = Fixture::new();
    let id = f.anchor("Retry exponentiell.\n");
    let mut first = session();
    first.intent_events = vec![event(0, &id, true)];
    let later = session();
    assert_eq!(of(&f, &witnessed(&[&first, &later])), bound(&id, GENESIS));
    // Allein betrachtet wüsste die spätere Epoche nichts davon.
    assert_eq!(of(&f, &witnessed(&[&later])), IntentState::Unbound);
}

#[test]
fn intent_change_mid_session_is_flagged() {
    let f = Fixture::new();
    let v1 = f.anchor("Version 1\n");
    let v2 = f.anchor("Version 2\n");

    // Erst ungebunden, dann gebunden: Der Anfang ist nicht belegt — aber
    // gewechselt hat nichts.
    let mut late = session();
    late.intent_events = vec![event(4, &v1, false)];
    assert_eq!(
        of(&f, &witnessed(&[&late])),
        bound(
            &v1,
            Expect {
                from_start: false,
                ..GENESIS
            }
        )
    );

    // Genesis v1, später v2 — in einer späteren Epoche: v2 gilt, gemeldet.
    let mut first = session();
    first.intent_events = vec![event(0, &v1, true)];
    let mut second = session();
    second.intent_events = vec![event(0, &v2, false)];
    assert_eq!(
        of(&f, &witnessed(&[&first, &second])),
        bound(
            &v2,
            Expect {
                changed: true,
                ..GENESIS
            }
        )
    );

    // Derselbe Anker noch einmal (etwa nach einem Absturz): kein Wechsel.
    let mut resent = session();
    resent.intent_events = vec![event(0, &v1, true), event(7, &v1, false)];
    assert_eq!(of(&f, &witnessed(&[&resent])), bound(&v1, GENESIS));
}

#[test]
fn the_local_file_binding_is_unchained() {
    let f = Fixture::new();
    let id = f.anchor("lokal\n");
    let mut s = session();
    s.intent_anchor = Some(id.clone());
    let unchained = Expect {
        chained: false,
        from_start: false,
        ..GENESIS
    };
    assert_eq!(of(&f, &witnessed(&[&s])), bound(&id, unchained));
    // Verkettete Events gehen vor.
    let chained = f.anchor("bezeugt\n");
    s.intent_events = vec![event(0, &chained, true)];
    assert_eq!(of(&f, &witnessed(&[&s])), bound(&chained, GENESIS));
}

#[test]
fn unwitnessed_intent_events_are_claims_not_chains() {
    let f = Fixture::new();
    let id = f.anchor("behauptet\n");
    let mut s = session();
    s.intent_events = vec![event(0, &id, true)];
    let chain = EpochChain {
        sessions: vec![s],
        witnessed: false,
    };
    assert_eq!(
        of(&f, &chain),
        bound(
            &id,
            Expect {
                chained: false,
                from_start: false,
                ..GENESIS
            }
        )
    );
}

#[test]
fn signature_is_checked_by_the_caller_over_the_anchor_text() {
    let f = Fixture::new();
    let id = f.anchor("signiert\n");
    f.store
        .put_intent_signature(
            &id,
            "-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n",
        )
        .unwrap();
    let mut s = session();
    s.intent_events = vec![event(0, &id, true)];
    let text = f.store.get_intent(&id).unwrap().unwrap().text;
    let check = |anchor: &str, signature: &str| {
        assert_eq!(anchor, text);
        assert!(signature.starts_with("-----BEGIN SSH SIGNATURE-----"));
        IntentSignature::Valid(SignerKind::SecurityKey)
    };
    assert_eq!(
        intent_of(&witnessed(&[&s]), &f.store, &check),
        bound(
            &id,
            Expect {
                signature: IntentSignature::Valid(SignerKind::SecurityKey),
                ..GENESIS
            }
        )
    );
}

#[test]
fn a_missing_anchor_does_not_match() {
    let f = Fixture::new();
    let absent = ContentHash::from_bytes([7; 32]);
    let mut s = session();
    s.intent_events = vec![event(0, &absent, true)];
    assert_eq!(
        of(&f, &witnessed(&[&s])),
        bound(
            &absent,
            Expect {
                matches: false,
                signature: IntentSignature::NotChecked,
                ..GENESIS
            }
        )
    );
}

// ---------------------------------------------------------------------------
// Die Epochen-Kette
// ---------------------------------------------------------------------------

fn stored(f: &Fixture, mut s: Session, marker: &str) -> (SessionId, Session) {
    s.intent.request = marker.into();
    let redacted = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_session(s)
        .unwrap();
    let id = f.store.put(&redacted).unwrap().id();
    (id, f.store.get(id).unwrap().unwrap())
}

fn seal_for(
    f: &Fixture,
    session: SessionId,
    previous: Option<ContentHash>,
    n: u64,
) -> (ContentHash, Seal) {
    let seal = Seal {
        root: ContentHash::from_bytes([n as u8; 32]),
        agent: "claude-code".into(),
        scope: SCOPE_WITNESS_V1.into(),
        first_seq: 0,
        last_seq: n,
        events: n + 1,
        gaps: 0,
        pre_chain: 0,
        outcome: SealOutcome::Stored {
            session: session.to_string(),
        },
        previous,
        last_event_at: format!("2026-10-02T10:00:0{n}Z"),
    };
    (f.store.put_seal(&seal.to_text().unwrap()).unwrap(), seal)
}

/// Drei Epochen: Genesis-Intent in der ersten, ein Wechsel in der zweiten.
struct Epochs {
    anchor: ContentHash,
    changed: ContentHash,
    ids: [SessionId; 3],
    sessions: [Session; 3],
    seals: [(ContentHash, Seal); 3],
}

fn three_epochs(f: &Fixture) -> Epochs {
    let anchor = f.anchor("Retry exponentiell.\n");
    let changed = f.anchor("Retry linear.\n");
    let mut first = session();
    first.intent_events = vec![event(0, &anchor, true)];
    let (id1, s1) = stored(f, first, "epoch 1");
    let w1 = seal_for(f, id1, None, 1);
    let mut second = session();
    second.intent_events = vec![event(0, &changed, false)];
    let (id2, s2) = stored(f, second, "epoch 2");
    let w2 = seal_for(f, id2, Some(w1.0.clone()), 2);
    let (id3, s3) = stored(f, session(), "epoch 3");
    let w3 = seal_for(f, id3, Some(w2.0.clone()), 3);
    Epochs {
        anchor,
        changed,
        ids: [id1, id2, id3],
        sessions: [s1, s2, s3],
        seals: [w1, w2, w3],
    }
}

#[test]
fn epoch_chain_walks_trusted_seals_back_to_the_genesis_intent() {
    let f = Fixture::new();
    let e = three_epochs(&f);
    let all = |_: &ContentHash, _: &Seal| true;
    let chain = epoch_chain(
        &f.store,
        e.ids[2],
        e.sessions[2].clone(),
        std::slice::from_ref(&e.seals[2]),
        &all,
    );
    assert!(chain.witnessed);
    assert_eq!(chain.sessions, e.sessions.to_vec());
    assert_eq!(
        of(&f, &chain),
        bound(
            &e.changed,
            Expect {
                changed: true,
                ..GENESIS
            }
        )
    );
    // Die erste Epoche selbst: ihr eigener Anfang.
    let chain = epoch_chain(
        &f.store,
        e.ids[0],
        e.sessions[0].clone(),
        std::slice::from_ref(&e.seals[0]),
        &all,
    );
    assert_eq!(chain.sessions, vec![e.sessions[0].clone()]);
    assert_eq!(of(&f, &chain), bound(&e.anchor, GENESIS));
}

#[test]
fn epoch_chain_never_invents_a_start() {
    let f = Fixture::new();
    let e = three_epochs(&f);
    let witness: BTreeSet<ContentHash> = e.seals.iter().map(|(id, _)| id.clone()).collect();
    let trusted = |id: &ContentHash, _: &Seal| witness.contains(id);
    let all = |_: &ContentHash, _: &Seal| true;

    // Ein beschreibbarer Rückverweis auf einen unsignierten Seal, der für
    // Epoche 2 „kein Vorgänger" behauptet: ignoriert — der Weg geht über den
    // geprüften Seal weiter und findet Genesis und Wechsel.
    let forged = seal_for(&f, e.ids[1], None, 7);
    let chain = epoch_chain(
        &f.store,
        e.ids[1],
        e.sessions[1].clone(),
        &[forged.clone(), e.seals[1].clone()],
        &trusted,
    );
    assert_eq!(chain.sessions.len(), 2);
    assert!(matches!(
        of(&f, &chain),
        IntentState::Bound {
            from_session_start: true,
            changed_mid_session: true,
            ..
        }
    ));

    // Nur der unsignierte Seal: nichts bezeugt.
    let chain = epoch_chain(
        &f.store,
        e.ids[1],
        e.sessions[1].clone(),
        std::slice::from_ref(&forged),
        &trusted,
    );
    assert!(!chain.witnessed);
    assert!(matches!(
        of(&f, &chain),
        IntentState::Bound {
            chained: false,
            from_session_start: false,
            ..
        }
    ));

    // Ein Seal einer anderen Session zählt nicht.
    let chain = epoch_chain(
        &f.store,
        e.ids[1],
        e.sessions[1].clone(),
        std::slice::from_ref(&e.seals[2]),
        &trusted,
    );
    assert!(!chain.witnessed);

    // Selbst ein vertrauenswürdiger Seal mit „kein Vorgänger" für die
    // zweite Epoche erfindet keinen Anfang: Ihr Intent-Event trägt kein
    // `opens_session`.
    let chain = epoch_chain(
        &f.store,
        e.ids[1],
        e.sessions[1].clone(),
        std::slice::from_ref(&forged),
        &all,
    );
    assert_eq!(chain.sessions.len(), 1);
    assert!(matches!(
        of(&f, &chain),
        IntentState::Bound {
            from_session_start: false,
            ..
        }
    ));

    // Ein unsignierter Seal, der Epoche 2 (den Wechsel) überspringt, wird
    // nicht gegangen — der Wechsel bleibt sichtbar.
    let skipping = seal_for(&f, e.ids[2], Some(e.seals[0].0.clone()), 8);
    let chain = epoch_chain(
        &f.store,
        e.ids[2],
        e.sessions[2].clone(),
        &[skipping.clone(), e.seals[2].clone()],
        &trusted,
    );
    assert_eq!(chain.sessions.len(), 3);
    assert!(matches!(
        of(&f, &chain),
        IntentState::Bound {
            changed_mid_session: true,
            ..
        }
    ));

    // Geprüfte Seals, die sich über den Vorgänger widersprechen: Der Weg
    // endet hier.
    let chain = epoch_chain(
        &f.store,
        e.ids[2],
        e.sessions[2].clone(),
        &[skipping, e.seals[2].clone()],
        &all,
    );
    assert_eq!(chain.sessions.len(), 1);

    // Ein fehlender Vorgänger und keine Seals: kein Anfang in Sicht.
    let dangling = (
        e.seals[2].0.clone(),
        Seal {
            previous: Some(ContentHash::from_bytes([0xee; 32])),
            ..e.seals[2].1.clone()
        },
    );
    let chain = epoch_chain(&f.store, e.ids[2], e.sessions[2].clone(), &[dangling], &all);
    assert_eq!(chain.sessions.len(), 1);
    let chain = epoch_chain(&f.store, e.ids[2], e.sessions[2].clone(), &[], &all);
    assert!(!chain.witnessed);

    // Ein `agent-hooks/v1`-Seal bezeugt nie etwas — auch nicht, wenn die
    // Prüfung des Aufrufers (zu großzügig) jeden Seal annimmt.
    let hooks = Seal {
        scope: minds_core::evidence::SCOPE_AGENT_HOOKS_V1.into(),
        previous: None,
        ..e.seals[0].1.clone()
    };
    // Id und Session vertauscht: Die Seals der einen bezeugen nicht die
    // Events der anderen.
    let chain = epoch_chain(
        &f.store,
        e.ids[1],
        e.sessions[0].clone(),
        std::slice::from_ref(&e.seals[1]),
        &all,
    );
    assert!(!chain.witnessed);

    let hooks_id = f.store.put_seal(&hooks.to_text().unwrap()).unwrap();
    let chain = epoch_chain(
        &f.store,
        e.ids[0],
        e.sessions[0].clone(),
        &[(hooks_id, hooks)],
        &all,
    );
    assert!(!chain.witnessed);
}
