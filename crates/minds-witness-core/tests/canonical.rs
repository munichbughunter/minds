//! Tests der kanonischen Form (Abschnitt 9 und 12): Golden-Body, Escaping wie
//! RFC 8785 und validierende Deserialisierung der Id-Newtypes.

mod fixtures;

use fixtures::*;
use minds_witness_core::*;

fn standard_body() -> ReceiptBody {
    let a = single_commit_push();
    let out = pre(&a, &policy_observe());
    body_of(&out.findings[0], &identity())
}

fn canonical_string(body: &ReceiptBody) -> String {
    String::from_utf8(canonical_bytes(body)).unwrap()
}

#[test]
fn t26_golden_body() {
    let id1 = id(1);
    let id2 = id(2);
    let seal = summary(2, true, &[]).hash;
    let ident = identity();
    let (id1, id2, seal, binary, key) = (
        id1.as_str(),
        id2.as_str(),
        seal.as_str(),
        ident.binary.as_str(),
        ident.key.as_str(),
    );
    let golden = format!(
        r#"{{"commit":"{id2}","coverage":{{"paths_covered":2,"paths_in_diff":2,"status":"computed","uncovered":[],"uncovered_truncated":false}},"policy":{{"mode":"observe","result":"pass","version":1}},"push":{{"new":"{id2}","new_commits":1,"old":"{id1}","ref":"refs/heads/main","truncated":false}},"pusher":{{"id":"user-42","username":"pdoering"}},"repo":"acme/payments","schema":"minds.witness.receipt/1","seal":{{"hash":"{seal}","ref":"refs/minds/seals/s1","status":"valid"}},"server":{{"host":"gitlab.example","kind":"memory","time":"2026-10-25T10:00:00Z"}},"witness":{{"binary":"{binary}","key":"{key}"}}}}"#
    );
    assert_eq!(canonical_string(&standard_body()), golden);
}

#[test]
fn t27_escaping() {
    let mut body = standard_body();
    body.repo = "a\"b\\c\u{1f}ä".to_owned();
    let s = canonical_string(&body);
    assert!(
        s.contains(r#""repo":"a\"b\\c\u001fä""#),
        "repo falsch escaped: {s}"
    );
}

#[test]
fn t36_raw_non_ascii() {
    let mut body = standard_body();
    body.repo = "x\u{7f}y\u{2028}z🦀".to_owned();
    let bytes = canonical_bytes(&body);
    let s = String::from_utf8(bytes).unwrap();
    assert!(s.contains("\"repo\":\"x\u{7f}y\u{2028}z🦀\""), "{s:?}");
    assert!(!s.contains("\\u007f"));
    assert!(!s.contains("\\u2028"));

    body.repo = "n\u{0}n".to_owned();
    let s = canonical_string(&body);
    assert!(s.contains(r#""repo":"n\u0000n""#), "{s:?}");
}

#[test]
fn t34_validating_deserialize() {
    let q = |s: &str| format!("\"{s}\"");
    let zeros40 = "0".repeat(40);
    let hex64 = "0123456789abcdef".repeat(4);
    let hex41 = "a".repeat(41);

    assert!(serde_json::from_str::<CommitId>(&q("ABC")).is_err());
    assert!(serde_json::from_str::<CommitId>(&q(&zeros40)).is_ok());
    assert!(serde_json::from_str::<CommitId>(&q(&hex64)).is_ok());
    assert!(serde_json::from_str::<CommitId>(&q(&hex41)).is_err());

    assert!(serde_json::from_str::<HashStr>(&q(&format!("sha1:{hex64}"))).is_err());
    assert!(serde_json::from_str::<HashStr>(&q(&format!("blake3:{}", &hex64[..63]))).is_err());
    assert!(serde_json::from_str::<HashStr>(&q(&format!("blake3:{hex64}"))).is_ok());

    let key = format!("ed25519:{hex64}");
    assert!(serde_json::from_str::<KeyStr>(&q(&key)).is_ok());
    assert!(serde_json::from_str::<SigStr>(&q(&key)).is_err());
    assert!(serde_json::from_str::<SigStr>(&q(&format!("ed25519:{hex64}{hex64}"))).is_ok());
}
