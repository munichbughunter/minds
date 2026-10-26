//! Spec S2 v0.4 §13, Ebene 1: Format `minds-commit-binding-v1` und die
//! Vertrauensprüfung ihrer Signatur (s2_01 bis s2_05).

mod common;

use common::*;
use minds_witness_git::binding::{BindingError, CommitBinding, DEFAULT_MAX_SEALS, id_of_bytes};
use minds_witness_git::budget::Budget;
use minds_witness_git::trust::{SigVerdict, TrustConfig, TrustFileError, Verifier};

fn golden_key() -> String {
    std::fs::read_to_string(data("golden_witness.pub"))
        .unwrap()
        .trim()
        .to_owned()
}

fn golden_binding() -> CommitBinding {
    CommitBinding::new(
        &"a1".repeat(20),
        &golden_key(),
        PROJECT,
        vec![seal_id(0x11), seal_id(0x22)],
        DEFAULT_MAX_SEALS,
    )
    .unwrap()
}

#[test]
fn s2_01_binding_roundtrip_and_id() {
    let b = golden_binding();
    let expected = format!(
        "{{\"commit\":\"{}\",\"key\":\"{}\",\"kind\":\"commit_binding\",\"project\":\"git.test/project-42\",\"schema\":1,\"seals\":[\"{}\",\"{}\"]}}",
        "a1".repeat(20),
        golden_key(),
        seal_id(0x11),
        seal_id(0x22)
    );
    let bytes = b.to_bytes();
    assert_eq!(String::from_utf8(bytes.clone()).unwrap(), expected);
    assert_eq!(bytes, std::fs::read(data("golden_binding")).unwrap());
    assert_eq!(
        b.id(),
        format!("b3-{}", hex(blake3::hash(&bytes).as_bytes()))
    );
    assert_eq!(b.id(), id_of_bytes(&bytes));
    let back = CommitBinding::parse(&bytes, DEFAULT_MAX_SEALS).unwrap();
    assert_eq!(back, b);
    assert_eq!(back.to_bytes(), bytes);
    assert_eq!(
        b.ref_name(),
        format!("refs/minds/bindings/{}/{}", "a1".repeat(20), &b.id()[3..])
    );
}

#[test]
fn s2_02_binding_strict_parse() {
    let good = String::from_utf8(golden_binding().to_bytes()).unwrap();
    let key = golden_key();
    let c = "a1".repeat(20);
    let (s1, s2) = (seal_id(0x11), seal_id(0x22));
    let doc = |commit: &str, kind: &str, project: &str, schema: &str, seals: &str, extra: &str| {
        format!(
            "{{\"commit\":\"{commit}\",\"key\":\"{key}\",\"kind\":\"{kind}\",\"project\":\"{project}\",\"schema\":{schema},{extra}\"seals\":{seals}}}"
        )
    };
    let seals = format!("[\"{s1}\",\"{s2}\"]");
    let cases: Vec<(String, BindingError)> = vec![
        // Unbekanntes Feld, auch `scope`.
        (good.replacen("{", "{\"a\":1,", 1), BindingError::Json),
        (
            doc(
                &c,
                "commit_binding",
                PROJECT,
                "1",
                &seals,
                "\"scope\":\"commit\",",
            ),
            BindingError::Json,
        ),
        (
            doc(&c, "change_binding", PROJECT, "1", &seals, ""),
            BindingError::Kind,
        ),
        (
            doc(&c, "commit_binding", PROJECT, "2", &seals, ""),
            BindingError::Schema,
        ),
        (
            doc(&c, "commit_binding", PROJECT, "1.0", &seals, ""),
            BindingError::Json,
        ),
        (
            doc(&c, "commit_binding", "Git.test/project-42", "1", &seals, ""),
            BindingError::Project,
        ),
        (
            doc(
                &c,
                "commit_binding",
                "git.test/project-042",
                "1",
                &seals,
                "",
            ),
            BindingError::Project,
        ),
        (
            doc(&c, "commit_binding", "git.test/group/repo", "1", &seals, ""),
            BindingError::Project,
        ),
        (
            doc(
                &c,
                "commit_binding",
                PROJECT,
                "1",
                &format!("[\"{s2}\",\"{s1}\"]"),
                "",
            ),
            BindingError::Seals,
        ),
        (
            doc(
                &c,
                "commit_binding",
                PROJECT,
                "1",
                &format!("[\"{s1}\",\"{s1}\"]"),
                "",
            ),
            BindingError::Seals,
        ),
        (
            doc(
                &c,
                "commit_binding",
                PROJECT,
                "1",
                &format!(
                    "[\"{}\"]",
                    seal_id(0xab).to_uppercase().replacen("B3-", "b3-", 1)
                ),
                "",
            ),
            BindingError::Seals,
        ),
        (
            doc(&c, "commit_binding", PROJECT, "1", "[]", ""),
            BindingError::Seals,
        ),
        (
            doc(
                &c.to_uppercase(),
                "commit_binding",
                PROJECT,
                "1",
                &seals,
                "",
            ),
            BindingError::Commit,
        ),
        (
            doc(&c[..39], "commit_binding", PROJECT, "1", &seals, ""),
            BindingError::Commit,
        ),
        // Leerzeichen: gültiges JSON, aber nicht kanonisch.
        (
            good.replacen(",\"key\"", ", \"key\"", 1),
            BindingError::NotCanonical,
        ),
        (format!("{good}\n"), BindingError::NotCanonical),
        // Doppeltes Feld.
        (
            good.replacen("{", &format!("{{\"commit\":\"{c}\","), 1),
            BindingError::Json,
        ),
        // Schlüssel mit Kommentar.
        (
            good.replacen(&key, &format!("{key} c"), 1),
            BindingError::Key,
        ),
    ];
    for (bytes, err) in cases {
        assert_eq!(
            CommitBinding::parse(bytes.as_bytes(), DEFAULT_MAX_SEALS),
            Err(err.clone()),
            "{bytes}"
        );
    }
    // Zu viele Seals.
    let many: Vec<String> = (1..=3u8).map(seal_id).collect();
    assert_eq!(
        CommitBinding::new(&c, &key, PROJECT, many, 2),
        Err(BindingError::Seals)
    );
}

fn trust(text: &str) -> TrustConfig {
    TrustConfig::from_text(
        text,
        &ssh_keygen(),
        &[PRINCIPAL.to_owned()],
        true,
        timeout(),
    )
    .unwrap()
}

#[test]
fn s2_03_binding_signature_vector() {
    let bytes = std::fs::read(data("golden_binding")).unwrap();
    let sig = std::fs::read(data("golden_binding.sig")).unwrap();
    let b = CommitBinding::parse(&bytes, DEFAULT_MAX_SEALS).unwrap();
    let cfg = trust(&witness_line(PRINCIPAL, &golden_key()));
    let budget = Budget::unlimited();
    let v = Verifier::new(&cfg, &budget);
    assert_eq!(
        v.verify(&bytes, &sig, Some(b.key())).unwrap(),
        SigVerdict::Accepted {
            principal: PRINCIPAL.to_owned()
        }
    );
    let text = std::str::from_utf8(&bytes).unwrap();
    let armored = std::str::from_utf8(&sig).unwrap();
    for (ns, ok) in [
        ("minds-witness", true),
        ("minds", false),
        ("minds-anchor", false),
    ] {
        assert_eq!(
            minds_attest::ssh_check_novalidate_with(&ssh_keygen(), text, armored, ns).unwrap(),
            ok,
            "{ns}"
        );
    }
    // Verändert ein Byte, gilt die Signatur nicht mehr.
    let tampered = text.replacen("project-42", "project-43", 1);
    assert_eq!(
        v.verify(tampered.as_bytes(), &sig, None).unwrap(),
        SigVerdict::BadSignature
    );
}

#[test]
fn s2_04_untrusted_principal() {
    let dir = tempfile::tempdir().unwrap();
    let (good_key, good) = keypair(dir.path(), "good");
    let (other_key, other) = keypair(dir.path(), "other");
    let base = witness_line(PRINCIPAL, &good);
    let payload = golden_binding().to_bytes();

    // Der fremde Schlüssel steht in der Datei, aber nicht unter einem
    // erlaubten Principal: ohne Beschränkung, mit zweitem Namespace, unter
    // einem anderen Principal, oder gar nicht.
    for extra in [
        format!("dev@test {other}\n"),
        format!("dev@test namespaces=\"minds-witness,minds\" {other}\n"),
        witness_line("minds-witness-user@other", &other),
        String::new(),
    ] {
        let cfg = trust(&format!("{base}{extra}"));
        let budget = Budget::unlimited();
        let v = Verifier::new(&cfg, &budget);
        let sig = sign(&payload, &other_key, "minds-witness");
        assert_eq!(
            v.verify(&payload, &sig, None).unwrap(),
            SigVerdict::Untrusted,
            "{extra}"
        );
        assert_eq!(budget.processes(), 0, "Vorfilter startet keinen Prozess");
    }

    // Erlaubter Principal ohne Präfix `minds-witness-user@`: bei
    // getrennter Witness-Pflicht nie anerkannt, ohne Prozess.
    let plain = "witness@test".to_owned();
    let cfg = TrustConfig::from_text(
        &witness_line(&plain, &good),
        &ssh_keygen(),
        std::slice::from_ref(&plain),
        true,
        timeout(),
    )
    .unwrap();
    let budget = Budget::unlimited();
    let sig = sign(&payload, &good_key, "minds-witness");
    assert_eq!(
        Verifier::new(&cfg, &budget)
            .verify(&payload, &sig, None)
            .unwrap(),
        SigVerdict::Untrusted
    );
    assert_eq!(budget.processes(), 0);

    // Ein erlaubter Principal mit unbeschränkter Zeile ist kein Urteil,
    // sondern ein Konfigurationsfehler (Trust-Datei-Regel 1, s2_23).
    assert_eq!(
        TrustConfig::from_text(
            &format!("{PRINCIPAL} {good}\n"),
            &ssh_keygen(),
            &[PRINCIPAL.to_owned()],
            true,
            timeout()
        )
        .unwrap_err(),
        TrustFileError::NotRestricted(PRINCIPAL.to_owned())
    );

    // Gegenprobe: der anerkannte Schlüssel, genau ein Prozess.
    let cfg = trust(&base);
    let budget = Budget::unlimited();
    let v = Verifier::new(&cfg, &budget);
    assert!(matches!(
        v.verify(&payload, &sig, None).unwrap(),
        SigVerdict::Accepted { .. }
    ));
    assert_eq!(budget.processes(), 1);
}

#[test]
fn s2_05_key_field_must_match_signature() {
    let dir = tempfile::tempdir().unwrap();
    let (good_key, good) = keypair(dir.path(), "good");
    let (_, other) = keypair(dir.path(), "other");
    let cfg = trust(&witness_line(PRINCIPAL, &good));
    let budget = Budget::unlimited();
    let v = Verifier::new(&cfg, &budget);
    let b = CommitBinding::new(
        &"a1".repeat(20),
        &other,
        PROJECT,
        vec![seal_id(0x11)],
        DEFAULT_MAX_SEALS,
    )
    .unwrap();
    let bytes = b.to_bytes();
    let sig = sign(&bytes, &good_key, "minds-witness");
    assert_eq!(
        v.verify(&bytes, &sig, Some(b.key())).unwrap(),
        SigVerdict::BadSignature
    );
    assert_eq!(budget.processes(), 0);
    // Mit dem passenden Schlüssel im Feld gilt dieselbe Signatur.
    let b = CommitBinding::new(&"a1".repeat(20), &good, PROJECT, vec![seal_id(0x11)], 32).unwrap();
    let bytes = b.to_bytes();
    let sig = sign(&bytes, &good_key, "minds-witness");
    assert!(matches!(
        v.verify(&bytes, &sig, Some(b.key())).unwrap(),
        SigVerdict::Accepted { .. }
    ));
}
