//! Spec S3 §3.1 (Commit 3): `s3_01_pending_roundtrip_and_mac`.
#![cfg(unix)]

use minds_witness_core::{CommitId, HashStr, RefUpdate};
use minds_witness_git::hook::pending::{
    CONFIRMED_SCHEMA, Confirmation, Handoff, PENDING_SCHEMA, PendingRecord,
};

/// Startangabe im eigenen Geltungsbereich.
fn start(t: &str) -> String {
    format!("{}|{t}", minds_witness_git::process::scope())
}

fn record(pid: u32, nonce: &str) -> PendingRecord {
    PendingRecord {
        created: "2026-10-27T10:00:00Z".to_owned(),
        findings: Vec::new(),
        nonce: nonce.to_owned(),
        rp_pid: pid,
        rp_start: Some(start("123")),
        schema: PENDING_SCHEMA.to_owned(),
        trust: HashStr::of_bytes(b"t"),
        trust_snapshot: "{}".to_owned(),
        updates: vec![RefUpdate {
            old: CommitId::parse(&"0".repeat(40)).unwrap(),
            new: CommitId::parse(&"1".repeat(40)).unwrap(),
            ref_name: "refs/heads/main".to_owned(),
        }],
        witness_binary: HashStr::of_bytes(b"bin"),
    }
}

#[test]
fn s3_01_pending_roundtrip_and_mac() {
    let dir = tempfile::tempdir().unwrap();
    let h = Handoff::new(dir.path(), &[1u8; 32]);
    let r = record(4242, "aa");
    let name = h.write_pending(&r).unwrap();
    assert_eq!(name, "4242-aa.json");
    assert_eq!(
        h.candidates(4242, Some(&start("123"))).unwrap(),
        vec![(name.clone(), r.clone())]
    );
    // Andere Startzeit derselben PID: kein Kandidat.
    assert!(h.candidates(4242, Some(&start("999"))).unwrap().is_empty());
    // Nie überschrieben: gleicher Name scheitert, Inhalt bleibt.
    let mut other = r.clone();
    other.created = "2026-10-27T11:00:00Z".to_owned();
    assert!(h.write_pending(&other).is_err());
    assert_eq!(h.candidates(4242, Some(&start("123"))).unwrap()[0].1, r);

    // Falscher Schlüssel bzw. manipulierte Datei: verworfen.
    let foreign = Handoff::new(dir.path(), &[2u8; 32]);
    assert!(
        foreign
            .candidates(4242, Some(&start("123")))
            .unwrap()
            .is_empty()
    );
    let path = dir.path().join("pending").join(&name);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("refs/heads/main", "refs/heads/evil")).unwrap();
    assert!(h.candidates(4242, Some(&start("123"))).unwrap().is_empty());
    std::fs::write(&path, text).unwrap();

    // Fehlende Startzeit passt nie zu einer bekannten (und umgekehrt).
    assert!(h.candidates(4242, None).unwrap().is_empty());
    let mut unknown = record(4343, "dd");
    unknown.rp_start = None;
    h.write_pending(&unknown).unwrap();
    assert!(h.candidates(4343, Some(&start("123"))).unwrap().is_empty());
    assert_eq!(h.candidates(4343, None).unwrap().len(), 1);
    // Dateiname und Inhalt widersprechen sich (umbenannter Datensatz):
    // nie ein Kandidat, weder für die PID im Namen noch für die im Inhalt.
    let moved = h.write_pending(&record(99, "ee")).unwrap();
    let pending_dir = dir.path().join("pending");
    std::fs::rename(pending_dir.join(&moved), pending_dir.join("4444-ee.json")).unwrap();
    assert!(h.candidates(4444, Some(&start("123"))).unwrap().is_empty());
    assert!(h.candidates(99, Some(&start("123"))).unwrap().is_empty());

    // Bestätigung: einmal angelegt, Datensatz entfernt; zweiter Lauf: false.
    let c = Confirmation {
        confirmed_at: "2026-10-27T10:00:01Z".to_owned(),
        confirmed_updates: r.updates.clone(),
        pending: r.clone(),
        schema: CONFIRMED_SCHEMA.to_owned(),
    };
    assert!(h.confirm(&name, &c).unwrap());
    assert!(!path.exists());
    assert!(!h.confirm(&name, &c).unwrap());
    let (open, bad) = h.confirmations().unwrap();
    assert_eq!((open.len(), bad.len()), (1, 0));
    assert_eq!(open[0].1, c);
    // Manipulierte Bestätigung: als ungültig gemeldet, nie verwendet.
    let cpath = dir.path().join("confirmed").join(&name);
    let text = std::fs::read_to_string(&cpath).unwrap();
    std::fs::write(&cpath, text.replace("10:00:01", "10:00:02")).unwrap();
    let (open, bad) = h.confirmations().unwrap();
    assert_eq!((open.len(), bad), (0, vec![name.clone()]));

    // Altlasten derselben PID wandern nach stale/ und werden gelöscht.
    h.write_pending(&record(77, "bb")).unwrap();
    h.write_pending(&record(78, "cc")).unwrap();
    // Datensatz derselben PID aus einem anderen Geltungsbereich (anderer
    // Container mit gemeinsamem Verzeichnis): bleibt unangetastet.
    let mut other_scope = record(77, "ff");
    other_scope.rp_start = Some("fremd|1".to_owned());
    h.write_pending(&other_scope).unwrap();
    assert_eq!(h.move_stale(77).unwrap(), vec!["77-bb.json".to_owned()]);
    assert!(pending_dir.join("77-ff.json").exists());
    std::fs::remove_file(pending_dir.join("77-ff.json")).unwrap();
    std::fs::remove_file(pending_dir.join("4444-ee.json")).unwrap();
    std::fs::remove_file(pending_dir.join("4343-dd.json")).unwrap();
    assert!(h.candidates(77, Some(&start("123"))).unwrap().is_empty());
    assert_eq!(h.candidates(78, Some(&start("123"))).unwrap().len(), 1);
    let expired = h.expire(std::time::Duration::from_secs(3600)).unwrap();
    assert_eq!(expired, vec!["stale/77-bb.json".to_owned()]);
    let expired = h.expire(std::time::Duration::from_secs(0)).unwrap();
    assert_eq!(expired, vec!["pending/78-cc.json".to_owned()]);
}

/// Spec S3 §3.1: Ein dauerhaft scheiternder Eintrag blockiert die Charge
/// nicht; `defer` stellt ihn hinten an, der Inhalt bleibt.
#[test]
fn s3_01_deferred_confirmation_moves_to_the_back() {
    let dir = tempfile::tempdir().unwrap();
    let h = Handoff::new(dir.path(), &[1u8; 32]);
    let mut names = Vec::new();
    for (pid, nonce) in [(1u32, "aa"), (2, "bb"), (3, "cc")] {
        let r = record(pid, nonce);
        let name = h.write_pending(&r).unwrap();
        let c = Confirmation {
            confirmed_at: "2026-10-27T10:00:01Z".to_owned(),
            confirmed_updates: r.updates.clone(),
            pending: r,
            schema: CONFIRMED_SCHEMA.to_owned(),
        };
        assert!(h.confirm(&name, &c).unwrap());
        // Gleiche Ausgangslage für alle: gleich alt.
        let path = dir.path().join("confirmed").join(&name);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10))
            .unwrap();
        names.push(name);
    }
    let order = |h: &Handoff| -> Vec<String> {
        h.confirmations()
            .unwrap()
            .0
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    };
    assert_eq!(order(&h), names);
    let before = h.confirmations().unwrap().0[0].1.clone();
    h.defer(&names[0]).unwrap();
    assert_eq!(
        order(&h),
        vec![names[1].clone(), names[2].clone(), names[0].clone()]
    );
    // Inhalt und MAC unverändert.
    let after = h.confirmations().unwrap().0;
    assert_eq!(after[2].1, before);
}
