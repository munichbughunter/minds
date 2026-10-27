//! S1 v1.6 (Spec S3 §5): `ServerKind::Git`, Quittungsschema `/2` mit
//! `trust`, strikte Schemaregel und `verify_receipt_with_trust`. Die
//! `/1`-Regeln aus S1 bleiben unverändert (t23 bis t43 laufen weiter).

mod fixtures;

use fixtures::*;
use minds_witness_core::memory::MemoryAdapter;
use minds_witness_core::*;

fn snapshot() -> TrustSnapshot {
    TrustSnapshot::new(
        vec![
            "minds-witness-user@b".to_owned(),
            "minds-witness-user@a".to_owned(),
        ],
        &policy_observe(),
        "git.test/project-42",
        true,
        "minds-witness-user@a namespaces=\"minds-witness\" ssh-ed25519 AAAA\n",
    )
}

/// Eine `/2`-Quittung über den Standard-Push, mit Snapshot-Bytes.
fn v2_receipt() -> (MemoryAdapter, Vec<u8>, Vec<u8>) {
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let snap = snapshot().to_bytes();
    let trust = HashStr::of_bytes(&snap);
    let out = post_receive_with_trust(&pre_out.findings, &identity(), &signer(), &mut a, &trust);
    assert_eq!(out.stored, 1);
    let bytes = a.receipts_for(&id(2)).remove(0);
    (a, bytes, snap)
}

fn v1_receipt() -> (MemoryAdapter, Vec<u8>) {
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let out = post_receive(&pre_out.findings, &identity(), &signer(), &mut a);
    assert_eq!(out.stored, 1);
    let bytes = a.receipts_for(&id(2)).remove(0);
    (a, bytes)
}

/// Bytes einer Quittung mit beliebig gesetzten Feldern, kanonisch und
/// gültig signiert, aber ohne die Schemaprüfung von `post_receive`.
fn signed_with(schema: &str, trust: Option<HashStr>) -> Vec<u8> {
    let (_, bytes) = v1_receipt();
    let mut body = Receipt::from_bytes(&bytes).unwrap().body();
    body.schema = schema.to_owned();
    body.trust = trust;
    body.sign(&signer()).to_bytes()
}

#[test]
fn t57_server_kind_git_and_receipt_v2() {
    assert_eq!(serde_json::to_string(&ServerKind::Git).unwrap(), "\"git\"");
    let back: ServerKind = serde_json::from_str("\"git\"").unwrap();
    assert_eq!(back, ServerKind::Git);

    let (a, bytes, snap) = v2_receipt();
    let r = Receipt::from_bytes(&bytes).unwrap();
    assert_eq!(r.schema, RECEIPT_SCHEMA_V2);
    assert_eq!(r.trust, Some(HashStr::of_bytes(&snap)));
    // Rundlauf: kanonische Bytes, Feld `trust` zwischen `signature` und `witness`.
    assert_eq!(r.to_bytes(), bytes);
    let text = String::from_utf8(bytes.clone()).unwrap();
    let (sig, trust, witness) = (
        text.find("\"signature\"").unwrap(),
        text.find("\"trust\"").unwrap(),
        text.find("\"witness\"").unwrap(),
    );
    assert!(sig < trust && trust < witness, "{text}");

    // `/1` über `verify_receipt`, `/2` über `verify_receipt_with_trust`.
    let (b, v1) = v1_receipt();
    assert!(verify_receipt(&v1, &b.keys()).is_ok());
    let (verified, parsed) = verify_receipt_with_trust(&bytes, &a.keys(), &snap).unwrap();
    assert_eq!(verified.receipt, r);
    assert_eq!(parsed, snapshot());
}

#[test]
fn t58_trust_hash_is_signed() {
    let (a, bytes, snap) = v2_receipt();
    let mut r = Receipt::from_bytes(&bytes).unwrap();
    r.trust = Some(HashStr::of_bytes(b"another snapshot"));
    assert_eq!(
        verify_receipt_with_trust(&r.to_bytes(), &a.keys(), b"another snapshot"),
        Err(VerifyError::BadSignature)
    );
    // Die Receipt-Id deckt `trust` ab.
    assert_ne!(r.id(), Receipt::from_bytes(&bytes).unwrap().id());
    assert!(verify_receipt_with_trust(&bytes, &a.keys(), &snap).is_ok());
}

#[test]
fn t59_post_receive_with_trust() {
    let (_, v2, _) = v2_receipt();
    let (_, v1) = v1_receipt();
    let r2 = Receipt::from_bytes(&v2).unwrap();
    let r1 = Receipt::from_bytes(&v1).unwrap();
    assert_eq!(
        (r2.schema.as_str(), r2.trust.is_some()),
        (RECEIPT_SCHEMA_V2, true)
    );
    assert_eq!(
        (r1.schema.as_str(), r1.trust.is_some()),
        (RECEIPT_SCHEMA, false)
    );
    // Bis auf Schema, `trust` und Signatur sind beide Befunde gleich.
    let (mut b1, mut b2) = (r1.body(), r2.body());
    b1.schema.clear();
    b2.schema.clear();
    b2.trust = None;
    assert_eq!(b1, b2);
}

#[test]
fn t60_receipt_v1_fixtures_unchanged() {
    // t43 pinnt Id und Signatur der Standard-`/1`-Quittung; hier zusätzlich:
    // keine Spur von `trust` in `/1`-Bytes, und der S1-Bauweg (`body_of`)
    // ergibt byteidentisch dieselbe Quittung wie die Post-Phase.
    let (a, bytes) = v1_receipt();
    assert!(!String::from_utf8(bytes.clone()).unwrap().contains("trust"));
    let pre_out = pre(&single_commit_push(), &policy_observe());
    let by_hand = body_of(&pre_out.findings[0], &identity())
        .sign(&signer())
        .to_bytes();
    assert_eq!(by_hand, bytes);
    assert_eq!(
        Receipt::from_bytes(&bytes).unwrap().id().as_str(),
        "blake3:574b0d176e2151190b5e71f88a6e2f5be5d91dc045f88534a5eb12759c1a0ecc"
    );
    assert!(verify_receipt(&bytes, &a.keys()).is_ok());
}

#[test]
fn t61_schema_and_trust_are_strict() {
    let (a, _, snap) = v2_receipt();
    let keys = a.keys();
    let trust = HashStr::of_bytes(&snap);
    let malformed = |r: Result<Receipt, VerifyError>| matches!(r, Err(VerifyError::Malformed(_)));

    // `/1` mit `trust`, `/2` ohne `trust`: abgelehnt beim Lesen.
    let v1_with = signed_with(RECEIPT_SCHEMA, Some(trust.clone()));
    let v2_without = signed_with(RECEIPT_SCHEMA_V2, None);
    assert!(malformed(Receipt::from_bytes(&v1_with)));
    assert!(malformed(Receipt::from_bytes(&v2_without)));
    assert!(matches!(
        verify_receipt(&v1_with, &keys),
        Err(VerifyError::Malformed(_))
    ));
    assert!(matches!(
        verify_receipt_with_trust(&v2_without, &keys, &snap),
        Err(VerifyError::Malformed(_))
    ));

    // Unbekanntes Schema, mit oder ohne `trust`.
    for t in [None, Some(trust.clone())] {
        let other = signed_with("minds.witness.receipt/3", t);
        assert_eq!(
            Receipt::from_bytes(&other),
            Err(VerifyError::UnknownSchema(
                "minds.witness.receipt/3".to_owned()
            ))
        );
    }

    // `"trust":null` in `/1`: liest sich als `None`, ist aber nicht kanonisch.
    let (_, v1) = v1_receipt();
    let with_null = String::from_utf8(v1.clone()).unwrap().replacen(
        "\"witness\":",
        "\"trust\":null,\"witness\":",
        1,
    );
    assert_eq!(
        Receipt::from_bytes(with_null.as_bytes()),
        Err(VerifyError::NotCanonical)
    );

    // `trust` mit falscher Form.
    let (_, v2, _) = v2_receipt();
    let bad_form = String::from_utf8(v2.clone())
        .unwrap()
        .replacen(trust.as_str(), "blake3:XYZ", 1);
    assert!(malformed(Receipt::from_bytes(bad_form.as_bytes())));

    // Nicht kanonische Bytes beider Schemas: `NotCanonical` aus `from_bytes`.
    for b in [&v1, &v2] {
        let spaced =
            String::from_utf8(b.clone())
                .unwrap()
                .replacen("{\"commit\"", "{ \"commit\"", 1);
        assert_eq!(
            Receipt::from_bytes(spaced.as_bytes()),
            Err(VerifyError::NotCanonical)
        );
    }

    // Kein Rückfall: `/2` nie über `verify_receipt`, `/1` nie über
    // `verify_receipt_with_trust`.
    assert_eq!(
        verify_receipt(&v2, &keys),
        Err(VerifyError::TrustSnapshotRequired)
    );
    assert_eq!(
        verify_receipt_with_trust(&v1, &keys, &snap),
        Err(VerifyError::NoTrustContext)
    );

    // Fremder Snapshot, nicht kanonischer Snapshot, unsortierte Principals,
    // fremdes Snapshot-Schema.
    let mut other = snapshot();
    other.project = "git.test/project-43".to_owned();
    assert_eq!(
        verify_receipt_with_trust(&v2, &keys, &other.to_bytes()),
        Err(VerifyError::TrustMismatch)
    );
    let spaced = String::from_utf8(snap.clone())
        .unwrap()
        .replacen("{", "{ ", 1);
    assert!(matches!(
        verify_receipt_with_trust(&v2, &keys, spaced.as_bytes()),
        Err(VerifyError::TrustSnapshotMalformed(_))
    ));
    let mut unsorted = snapshot();
    unsorted.allowed_witness_principals.reverse();
    assert!(matches!(
        TrustSnapshot::from_bytes(&unsorted.to_bytes()),
        Err(VerifyError::TrustSnapshotMalformed(_))
    ));
    let mut schema = snapshot();
    schema.schema = "minds.witness.trust/2".to_owned();
    assert!(matches!(
        TrustSnapshot::from_bytes(&schema.to_bytes()),
        Err(VerifyError::TrustSnapshotMalformed(_))
    ));
    assert!(matches!(
        TrustSnapshot::from_bytes(b"{\"extra\":1}"),
        Err(VerifyError::TrustSnapshotMalformed(_))
    ));
}

/// Review-Befund 1 (Commit 1): Der Snapshot ist öffentlich lesbar und darf
/// das Salz der Pusher-Pseudonymisierung nicht enthalten.
#[test]
fn t62_snapshot_never_publishes_the_pusher_salt() {
    let salt = "geheimes-salz-0815";
    let policy = Policy {
        pusher_mode: PusherMode::Hashed,
        pusher_salt: salt.to_owned(),
        ..Policy::default()
    };
    let snap = TrustSnapshot::new(vec!["p".to_owned()], &policy, "x/project-1", true, "");
    let text = String::from_utf8(snap.to_bytes()).unwrap();
    assert!(!text.contains(salt), "{text}");
    assert_eq!(
        snap.policy.pusher_salt_hash,
        Some(HashStr::of_bytes(salt.as_bytes()))
    );
    // Die Nachrechnungs-Policy trägt kein Salz und rechnet keinen Pusher.
    let eval = snap.policy.evaluation_policy();
    assert_eq!(
        (eval.pusher_mode, eval.pusher_salt.as_str()),
        (PusherMode::Plain, "")
    );
    assert_eq!(TrustSnapshot::from_bytes(&snap.to_bytes()).unwrap(), snap);
    // Plain: kein Feld `pusher_salt_hash`.
    let plain = String::from_utf8(snapshot().to_bytes()).unwrap();
    assert!(!plain.contains("pusher_salt_hash"));
}

/// Review-Befund 2: eingefrorene, alphabetische Byte-Form mit Golden-Werten
/// für Snapshot und `/2`-Quittung.
#[test]
fn t63_trust_snapshot_and_v2_golden_vectors() {
    let snap = snapshot();
    assert_eq!(
        String::from_utf8(snap.to_bytes()).unwrap(),
        concat!(
            r#"{"allowed_witness_principals":["minds-witness-user@a","minds-witness-user@b"],"#,
            r#""policy":{"coverage_threshold_percent":100,"enforce_refs":[],"evaluate_refs":["refs/heads/*"],"#,
            r#""max_commits_per_push":200,"mode":"observe","pusher_mode":"plain","uncovered_cap":100,"version":1},"#,
            r#""project":"git.test/project-42","require_separated_witness":true,"#,
            r#""schema":"minds.witness.trust/1","#,
            r#""signers":"minds-witness-user@a namespaces=\"minds-witness\" ssh-ed25519 AAAA\n"}"#
        )
    );
    assert_eq!(snap.hash().as_str(), GOLDEN_TRUST);
    let (_, bytes, _) = v2_receipt();
    let r = Receipt::from_bytes(&bytes).unwrap();
    assert_eq!(r.id().as_str(), GOLDEN_V2_ID);
    assert_eq!(r.signature.as_str(), GOLDEN_V2_SIG);
}

const GOLDEN_TRUST: &str =
    "blake3:3e9dabda6d4b6a721bf335e677dbabee7c4923954258ddcb109046b61137c304";
const GOLDEN_V2_ID: &str =
    "blake3:2c309efaccb390cb7a1eb6c9af742ea25c735480526ae861d7fea7dea5b9377c";
const GOLDEN_V2_SIG: &str = "ed25519:0508e2f295dcf081e0e6a9524f81aa371069880a8eb4ded4d5ecfe89d2271acf8734a4e520535ebf34a5996c1c96b00bced3d335280908626a96b5f07736e104";

/// Review-Befunde 3, 4 und Testlücken aus t61.
#[test]
fn t64_strictness_edges() {
    let (a, v2, snap) = v2_receipt();
    let keys = a.keys();
    let (_, v1) = v1_receipt();
    let trust = HashStr::of_bytes(&snap);

    // Die Schemaregel geht der Kanonik vor: `/1` mit `trust` und
    // Leerzeichen → Malformed, nicht NotCanonical.
    let bad = String::from_utf8(signed_with(RECEIPT_SCHEMA, Some(trust.clone())))
        .unwrap()
        .replacen("{\"commit\"", "{ \"commit\"", 1);
    assert!(matches!(
        Receipt::from_bytes(bad.as_bytes()),
        Err(VerifyError::Malformed(_))
    ));

    // `/2` mit `"trust":null` → Malformed (Regel), nicht gültig.
    let null = String::from_utf8(v2.clone()).unwrap().replacen(
        &format!("\"trust\":\"{}\"", trust.as_str()),
        "\"trust\":null",
        1,
    );
    assert!(matches!(
        Receipt::from_bytes(null.as_bytes()),
        Err(VerifyError::Malformed(_))
    ));

    // Doppelter Schlüssel → Malformed.
    let dup = String::from_utf8(v1.clone()).unwrap().replacen(
        "{\"commit\":",
        "{\"repo\":\"x\",\"commit\":",
        1,
    );
    assert!(matches!(
        Receipt::from_bytes(dup.as_bytes()),
        Err(VerifyError::Malformed(_))
    ));

    // Bau: `check` lehnt beide verbotenen Kombinationen ab.
    let mut body = Receipt::from_bytes(&v1).unwrap().body();
    assert!(body.check().is_ok());
    body.trust = Some(trust.clone());
    assert!(matches!(body.check(), Err(VerifyError::Malformed(_))));
    body.schema = RECEIPT_SCHEMA_V2.to_owned();
    assert!(body.check().is_ok());
    body.trust = None;
    assert!(matches!(body.check(), Err(VerifyError::Malformed(_))));

    // Snapshot mit ungültiger Policy → abgelehnt, auch kanonisch.
    for broken in [
        |p: &mut TrustPolicy| p.coverage_threshold_percent = 255,
        |p: &mut TrustPolicy| p.max_commits_per_push = 0,
        |p: &mut TrustPolicy| p.enforce_refs = vec!["refs/tags/*".to_owned()],
        |p: &mut TrustPolicy| p.pusher_mode = PusherMode::Hashed,
    ] {
        let mut s = snapshot();
        broken(&mut s.policy);
        assert!(matches!(
            TrustSnapshot::from_bytes(&s.to_bytes()),
            Err(VerifyError::TrustSnapshotMalformed(_))
        ));
    }

    // Fehlertexte des Snapshots sind feste Sätze, ohne Inhalte.
    let evil = br#"{"MARKER\u001b[31m":1}"#;
    let Err(VerifyError::TrustSnapshotMalformed(msg)) = TrustSnapshot::from_bytes(evil) else {
        panic!("nicht abgelehnt");
    };
    assert!(!msg.contains("MARKER"), "{msg}");
    assert!(verify_receipt_with_trust(&v2, &keys, &snap).is_ok());
}
