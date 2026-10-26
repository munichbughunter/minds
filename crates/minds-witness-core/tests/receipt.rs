//! Tests der Quittung (Abschnitt 12): Determinismus, Manipulationserkennung,
//! Schema- und Feldprüfung sowie zwei unabhängig berechnete Vektoren
//! (RFC 8032 für die Schlüsselableitung, t43 für Digest und Signatur).

mod fixtures;

use fixtures::*;
use minds_witness_core::memory::MemoryAdapter;
use minds_witness_core::*;

fn standard_receipt_bytes() -> (MemoryAdapter, Vec<u8>) {
    let mut a = single_commit_push();
    let pre_out = pre(&a, &policy_observe());
    let out = post_receive(&pre_out.findings, &identity(), &signer(), &mut a);
    assert_eq!(out.stored, 1);
    let bytes = a.receipts_for(&id(2)).remove(0);
    (a, bytes)
}

#[test]
fn t23_deterministic_bytes() {
    let (a, _) = standard_receipt_bytes();
    let (b, _) = standard_receipt_bytes();
    assert_eq!(a.receipts_for(&id(2)), b.receipts_for(&id(2)));
    assert_eq!(a.receipts_for(&id(2)).len(), 1);
}

#[test]
fn t24_tamper_detected() {
    let (a, bytes) = standard_receipt_bytes();
    let keys = a.keys();
    assert!(verify_receipt(&bytes, &keys).is_ok());

    // (a) Inhalt geändert und neu kanonisiert: Signatur passt nicht mehr.
    let mut r = Receipt::from_bytes(&bytes).unwrap();
    r.repo = "acme/other".to_owned();
    assert_eq!(
        verify_receipt(&r.to_bytes(), &keys),
        Err(VerifyError::BadSignature)
    );

    // (b) Eingefügtes Leerzeichen: inhaltlich gleich, aber nicht kanonisch.
    let spaced =
        String::from_utf8(bytes.clone())
            .unwrap()
            .replacen("{\"commit\"", "{ \"commit\"", 1);
    assert_eq!(
        verify_receipt(spaced.as_bytes(), &keys),
        Err(VerifyError::NotCanonical)
    );

    // (c) Leere Schlüsselliste.
    assert_eq!(
        verify_receipt(&bytes, &[]),
        Err(VerifyError::UnknownKey(identity().key.as_str().to_owned()))
    );

    // (d) Schlüssel erst ab 2027 gültig.
    let late = [PublicKeyEntry {
        key: identity().key,
        valid_from: ts("2027-01-01T00:00:00Z"),
        valid_until: None,
    }];
    assert_eq!(
        verify_receipt(&bytes, &late),
        Err(VerifyError::KeyNotValidAt {
            time: "2026-10-25T10:00:00Z".to_owned()
        })
    );

    // (e) commit-Wert in Großbuchstaben.
    // SPEC-GAP: Die Standard-Quittung hat `commit = id(2)` =
    // "000…0002"; diese Id enthält keine Hex-Buchstaben, ihre
    // Großbuchstabenform ist also byteidentisch und der Fall wäre wörtlich
    // genommen leer. Der Test prüft die Absicht (Großbuchstaben-Hex wird bei
    // der Deserialisierung abgelehnt) daher an einer sonst gleichen, gültig
    // signierten Quittung, deren commit Hex-Buchstaben enthält. Erwartung
    // unverändert: `Malformed`.
    let lower = id(2).as_str().to_owned();
    assert_eq!(
        lower.to_uppercase(),
        lower,
        "id(2) hat keine Hex-Buchstaben"
    );
    let mut body = Receipt::from_bytes(&bytes).unwrap().body();
    body.commit = CommitId::parse(&"ab".repeat(20)).unwrap();
    let lettered = body.sign(&signer()).to_bytes();
    assert!(verify_receipt(&lettered, &keys).is_ok());
    let upper =
        String::from_utf8(lettered)
            .unwrap()
            .replacen(&"ab".repeat(20), &"AB".repeat(20), 1);
    assert!(matches!(
        verify_receipt(upper.as_bytes(), &keys),
        Err(VerifyError::Malformed(_))
    ));
}

#[test]
fn t25_unknown_schema_and_fields() {
    let (a, bytes) = standard_receipt_bytes();
    let keys = a.keys();

    let mut r = Receipt::from_bytes(&bytes).unwrap();
    r.schema = "x/9".to_owned();
    assert_eq!(
        verify_receipt(&r.to_bytes(), &keys),
        Err(VerifyError::UnknownSchema("x/9".to_owned()))
    );

    let extra = String::from_utf8(bytes)
        .unwrap()
        .replacen('{', "{\"extra\":1,", 1);
    assert!(matches!(
        verify_receipt(extra.as_bytes(), &keys),
        Err(VerifyError::Malformed(_))
    ));
}

#[test]
fn t35_ed25519_test_vector() {
    let mut seed = [0u8; 32];
    hex::decode_to_slice(
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        &mut seed,
    )
    .unwrap();
    let s = Signer::from_seed(seed, ts("2026-01-01T00:00:00Z"));
    assert_eq!(
        s.public_key().as_str(),
        "ed25519:d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    );
}

#[test]
fn t43_signature_golden_vector() {
    assert_eq!(
        identity().key.as_str(),
        "ed25519:ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
    );
    let (a, bytes) = standard_receipt_bytes();
    let receipt = Receipt::from_bytes(&bytes).unwrap();
    assert_eq!(canonical_bytes(&receipt.body()).len(), 884);
    assert_eq!(
        receipt.id().as_str(),
        "blake3:574b0d176e2151190b5e71f88a6e2f5be5d91dc045f88534a5eb12759c1a0ecc"
    );
    assert_eq!(
        receipt.signature.as_str(),
        "ed25519:818e44788d285635e44d2333fa611374d911b07dffb89c3b01495c115280a3a88e71e76c941c44c8d2b30c34455c1e74ade309284c3d8b2e1989ed16e4249801"
    );
    assert!(verify_receipt(&bytes, &a.keys()).is_ok());
}
