//! Spec S3 §4 (Commit 2): `GitReceiptStore` gegen echte Repositories.
//! `s3_05_git_receipt_store_conflict_rules` und die Ablage der
//! Trust-Snapshots (Teil von s3_06, ohne Hook).
#![cfg(unix)]

mod support;

use std::sync::{Arc, Barrier};
use std::time::Duration;

use minds_witness_core::{
    CommitId, HashStr, KeyStr, Policy, PublicKeyEntry, PutResult, ReceiptKey, ReceiptStore,
    StoreError, Timestamp, TrustSnapshot,
};
use minds_witness_git::receipt_store::{GitReceiptStore, KEYS_REF, MIN_ORPHAN_AGE, TRUST_PREFIX};
use support::*;

fn store(server: &Repo) -> GitReceiptStore {
    GitReceiptStore::new(&git_program(), &server.git_dir, Duration::from_secs(10))
}

fn key(commit: u8, id: u8) -> ReceiptKey {
    ReceiptKey {
        commit: CommitId::parse(&format!("{commit:040x}")).unwrap(),
        id: HashStr::of_bytes(&[id]),
    }
}

fn entry(seed: u8, from: &str) -> PublicKeyEntry {
    PublicKeyEntry {
        key: KeyStr::parse(&format!("ed25519:{}", hex::encode([seed; 32]))).unwrap(),
        valid_from: Timestamp::parse(from).unwrap(),
        valid_until: None,
    }
}

fn snapshot(project: &str) -> Vec<u8> {
    TrustSnapshot::new(
        vec!["minds-witness-user@test".to_owned()],
        &Policy::default(),
        project,
        true,
        "minds-witness-user@test namespaces=\"minds-witness\" ssh-ed25519 AAAA\n",
    )
    .to_bytes()
}

#[test]
fn s3_05_git_receipt_store_conflict_rules() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let mut s = store(&server);
    let k = key(2, 1);
    let name = GitReceiptStore::receipt_ref(&k);

    // Anlegen, Lesen, Liste.
    assert_eq!(s.get(&k).unwrap(), None);
    assert_eq!(s.put(&k, b"receipt-a").unwrap(), PutResult::Stored);
    assert_eq!(s.get(&k).unwrap().as_deref(), Some(&b"receipt-a"[..]));
    assert_eq!(s.list(&k.commit).unwrap(), vec![k.clone()]);
    let oid = server.ref_value(&name).unwrap();

    // Byteidentisch: AlreadyExists, Ref unverändert.
    assert_eq!(s.put(&k, b"receipt-a").unwrap(), PutResult::AlreadyExists);
    assert_eq!(server.ref_value(&name).unwrap(), oid);

    // Anderer Inhalt: Conflict, nie überschrieben.
    assert!(matches!(
        s.put(&k, b"receipt-b"),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(server.ref_value(&name).unwrap(), oid);
    assert_eq!(s.get(&k).unwrap().as_deref(), Some(&b"receipt-a"[..]));

    // Deterministische Objekte: dieselben Bytes in einem anderen
    // Repository ergeben denselben Commit.
    let other = fx.server_named("other.git");
    let mut s2 = store(&other);
    s2.put(&k, b"receipt-a").unwrap();
    assert_eq!(other.ref_value(&name).unwrap(), oid);

    // Gesperrter Ref (fehlgeschlagene Ablage): technischer Fehler, kein Ref;
    // die Wiederholung nach Freigabe legt ab.
    let k2 = key(2, 2);
    let name2 = GitReceiptStore::receipt_ref(&k2);
    let lock = server.git_dir.join(format!("{name2}.lock"));
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, b"").unwrap();
    assert!(matches!(
        s.put(&k2, b"receipt-c"),
        Err(StoreError::Internal(_))
    ));
    assert_eq!(server.ref_value(&name2), None);
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(s.put(&k2, b"receipt-c").unwrap(), PutResult::Stored);
    let mut listed = s.list(&k.commit).unwrap();
    listed.sort();
    assert_eq!(listed.len(), 2);

    // Ein vorhandener, fehlerhaft geformter Ref wird nie ersetzt.
    let k3 = key(3, 1);
    let name3 = GitReceiptStore::receipt_ref(&k3);
    let a = server.blob(b"x");
    let b = server.blob(b"y");
    let t = server.tree(&[("100644", b"receipt.json", &a), ("100644", b"extra", &b)]);
    let c = server.commit(&t, &[]);
    server.update_ref(&name3, &c);
    assert!(matches!(s.put(&k3, b"x"), Err(StoreError::Internal(_))));
    assert!(matches!(s.get(&k3), Err(StoreError::Internal(_))));
    assert_eq!(server.ref_value(&name3).unwrap(), c);

    // Symbolischer Ref an Stelle einer Quittung: technischer Fehler.
    let k4 = key(4, 1);
    let name4 = GitReceiptStore::receipt_ref(&k4);
    server.symref(&name4, &name);
    assert!(matches!(s.get(&k4), Err(StoreError::Internal(_))));
    assert!(matches!(
        s.put(&k4, b"receipt-a"),
        Err(StoreError::Internal(_))
    ));

    // Replace-Refs werden ignoriert.
    let blob_a = server.git(&["rev-parse", &format!("{name}:receipt.json")]);
    let evil = server.blob(b"replaced");
    server.update_ref(&format!("refs/replace/{blob_a}"), &evil);
    assert_eq!(s.get(&k).unwrap().as_deref(), Some(&b"receipt-a"[..]));
}

#[test]
fn s3_05_parallel_puts_create_exactly_once() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    for (round, same) in [(0u8, false), (1u8, true)] {
        let k = key(10 + round, 1);
        let barrier = Arc::new(Barrier::new(6));
        let handles: Vec<_> = (0..6u8)
            .map(|i| {
                let mut s = store(&server);
                let k = k.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let bytes = if same { vec![7u8] } else { vec![i] };
                    barrier.wait();
                    s.put(&k, &bytes)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let stored = results
            .iter()
            .filter(|r| matches!(r, Ok(PutResult::Stored)))
            .count();
        assert_eq!(stored, 1, "{results:?}");
        for r in &results {
            if same {
                assert!(r.is_ok(), "{r:?}");
            } else {
                assert!(
                    matches!(r, Ok(PutResult::Stored) | Err(StoreError::Conflict(_))),
                    "{r:?}"
                );
            }
        }
    }
}

#[test]
fn s3_05_public_keys_compare_and_swap() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let mut s = store(&server);
    assert!(s.public_keys().unwrap().is_empty());

    let e1 = entry(1, "2026-01-01T00:00:00Z");
    s.register_public_key(&e1).unwrap();
    let oid = server.ref_value(KEYS_REF).unwrap();
    // Gleicher Schlüssel, gleiches `valid_from`: nichts geändert.
    s.register_public_key(&e1).unwrap();
    assert_eq!(server.ref_value(KEYS_REF).unwrap(), oid);
    // Gleicher Schlüssel, anderes `valid_from`: Konflikt, nichts geändert.
    assert!(matches!(
        s.register_public_key(&entry(1, "2026-06-01T00:00:00Z")),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(server.ref_value(KEYS_REF).unwrap(), oid);

    // Fortschreibung als Kind-Commit.
    let e2 = entry(2, "2026-02-01T00:00:00Z");
    s.register_public_key(&e2).unwrap();
    let parent = server.git(&["rev-parse", &format!("{KEYS_REF}^")]);
    assert_eq!(parent, oid);
    assert_eq!(s.public_keys().unwrap(), vec![e1.clone(), e2.clone()]);

    // Zwei neue Schlüssel gleichzeitig: beide landen in der Liste.
    for round in 0..5u8 {
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2u8)
            .map(|i| {
                let mut st = store(&server);
                let barrier = barrier.clone();
                let e = entry(10 + round * 2 + i, "2026-03-01T00:00:00Z");
                std::thread::spawn(move || {
                    barrier.wait();
                    st.register_public_key(&e).map(|()| e)
                })
            })
            .collect();
        for h in handles {
            let e = h.join().unwrap().unwrap();
            assert!(s.public_keys().unwrap().contains(&e));
        }
    }
    assert_eq!(s.public_keys().unwrap().len(), 12);

    // Eine manipulierte, nicht kanonische Liste ist ein technischer Fehler.
    let blob = server.blob(b"[ ]");
    let t = server.tree(&[("100644", b"keys.json", &blob)]);
    let c = server.commit(&t, &[]);
    server.update_ref(KEYS_REF, &c);
    assert!(matches!(s.public_keys(), Err(StoreError::Internal(_))));
}

#[test]
fn s3_06_trust_snapshot_store() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let s = store(&server);

    // Nur gültige, kanonische Snapshots.
    assert!(matches!(s.put_trust(b"{}"), Err(StoreError::Internal(_))));
    let mut spaced = snapshot("git.test/project-42");
    spaced.insert(1, b' ');
    assert!(matches!(s.put_trust(&spaced), Err(StoreError::Internal(_))));

    let bytes = snapshot("git.test/project-42");
    let (hash, r) = s.put_trust(&bytes).unwrap();
    assert_eq!(r, PutResult::Stored);
    assert_eq!(hash, HashStr::of_bytes(&bytes));
    let name = GitReceiptStore::trust_ref(&hash);
    assert!(name.starts_with(TRUST_PREFIX));
    assert_eq!(
        s.get_trust(&hash).unwrap().as_deref(),
        Some(bytes.as_slice())
    );
    let (_, again) = s.put_trust(&bytes).unwrap();
    assert_eq!(again, PutResult::AlreadyExists);

    // Ein anderer Inhalt unter dem Namen dieses Hashes (nur am Hook vorbei
    // möglich): nie überschrieben, Lesen meldet den Widerspruch.
    let other = snapshot("git.test/project-43");
    let oid = server.ref_value(&name).unwrap();
    let blob = server.blob(&other);
    let t = server.tree(&[("100644", b"trust.json", &blob)]);
    let c = server.commit(&t, &[]);
    server.git(&["update-ref", &name, &c, &oid]);
    assert!(matches!(s.put_trust(&bytes), Err(StoreError::Conflict(_))));
    assert_eq!(server.ref_value(&name).unwrap(), c);
    assert!(matches!(s.get_trust(&hash), Err(StoreError::Internal(_))));
    assert_eq!(s.get_trust(&HashStr::of_bytes(b"none")).unwrap(), None);
}

/// Review-Befunde zu Commit 2: strenge Lesart und Wiederholbarkeit.
#[test]
fn s3_05_strict_refs_are_never_repaired() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let mut s = store(&server);
    let blob = server.blob(b"r");
    let tree = server.tree(&[("100644", b"receipt.json", &blob)]);
    let commit = server.commit(&tree, &[]);

    // (a) Symref ins Leere: nicht „nicht vorhanden“, nie ersetzt.
    let k = key(20, 1);
    let name = GitReceiptStore::receipt_ref(&k);
    server.symref(&name, "refs/minds/nowhere");
    assert!(matches!(s.get(&k), Err(StoreError::Internal(_))));
    assert!(matches!(s.put(&k, b"r"), Err(StoreError::Internal(_))));
    assert_eq!(server.git(&["symbolic-ref", &name]), "refs/minds/nowhere");

    // (b) Kaputter loser Ref (Unsinn als Inhalt): Fehler bei get, put und list.
    let k = key(21, 1);
    let name = GitReceiptStore::receipt_ref(&k);
    let path = server.git_dir.join(&name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"garbage\n").unwrap();
    assert!(matches!(s.get(&k), Err(StoreError::Internal(_))));
    assert!(matches!(s.put(&k, b"r"), Err(StoreError::Internal(_))));
    assert!(matches!(s.list(&k.commit), Err(StoreError::Internal(_))));
    assert_eq!(std::fs::read(&path).unwrap(), b"garbage\n");

    // (c) Kaputter `keys`-Ref: nie eine leere Liste (die Widerrufe verbergen würde).
    let keys_path = server.git_dir.join(KEYS_REF);
    std::fs::create_dir_all(keys_path.parent().unwrap()).unwrap();
    std::fs::write(&keys_path, b"garbage\n").unwrap();
    assert!(matches!(s.public_keys(), Err(StoreError::Internal(_))));
    std::fs::remove_file(&keys_path).unwrap();

    // (d) Annotiertes Tag, Blob und Baum statt Commit; falsche Elternzahl.
    server.git(&["tag", "-a", "-m", "t", "rt", &commit]);
    let tag_oid = server.git(&["rev-parse", "refs/tags/rt"]);
    let parent_commit = server.commit(&tree, &[&commit]);
    for (n, target) in [
        (22u8, &tag_oid),
        (23, &blob),
        (24, &tree),
        (25, &parent_commit),
    ] {
        let k = key(n, 1);
        let name = GitReceiptStore::receipt_ref(&k);
        server.update_ref(&name, target);
        assert!(matches!(s.get(&k), Err(StoreError::Internal(_))), "{n}");
        assert!(
            matches!(s.put(&k, b"r"), Err(StoreError::Internal(_))),
            "{n}"
        );
        assert_eq!(
            server.ref_value(&name).as_deref(),
            Some(target.as_str()),
            "{n}"
        );
    }

    // (e) Gepackte Refs: dieselben Regeln.
    let k = key(26, 1);
    assert_eq!(s.put(&k, b"packed").unwrap(), PutResult::Stored);
    server.git(&["pack-refs", "--all"]);
    assert!(
        !server
            .git_dir
            .join(GitReceiptStore::receipt_ref(&k))
            .exists()
    );
    assert_eq!(s.put(&k, b"packed").unwrap(), PutResult::AlreadyExists);
    assert!(matches!(s.put(&k, b"other"), Err(StoreError::Conflict(_))));
    assert_eq!(s.get(&k).unwrap().as_deref(), Some(&b"packed"[..]));
}

fn age(path: &std::path::Path, by: Duration) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - by)
        .unwrap();
}

fn old_lock(server: &Repo, name: &str) -> std::path::PathBuf {
    let lock = server.git_dir.join(format!("{name}.lock"));
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, b"").unwrap();
    age(&lock, 2 * MIN_ORPHAN_AGE);
    lock
}

fn coord_token(server: &Repo, sub: &str, file: &str, pid: u32) -> std::path::PathBuf {
    let dir = server.git_dir.join("minds-witness").join(sub);
    std::fs::create_dir_all(&dir).unwrap();
    let start = minds_witness_git::process::start_time(pid).unwrap_or_else(|| "-".to_owned());
    let path = dir.join(file);
    std::fs::write(&path, format!("{pid} {start}\n")).unwrap();
    path
}

#[test]
fn s3_05_stale_lock_is_recoverable() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let mut s = store(&server);
    let k = key(30, 1);
    let name = GitReceiptStore::receipt_ref(&k);
    let lock = server.git_dir.join(format!("{name}.lock"));
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, b"").unwrap();
    // Abgebrochener Prozess: Die Sperre bleibt und blockiert die Ablage.
    assert!(matches!(s.put(&k, b"r"), Err(StoreError::Internal(_))));
    // Zu junge Sperren bleiben, auch ohne lebenden Ablageprozess.
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert_eq!(
        (r.busy, r.removed.len(), r.kept_young.clone()),
        (false, 0, vec![name.clone()])
    );
    assert!(lock.exists());
    // Nachweislich verwaist (kein lebender Ablageprozess, bekannter Name,
    // alt genug): entfernt; danach gelingt die Wiederholung.
    age(&lock, 2 * MIN_ORPHAN_AGE);
    let r = s.clear_stale_locks(Duration::from_secs(0)).unwrap();
    assert_eq!(r.removed, vec![name.clone()]);
    assert_eq!(s.put(&k, b"r").unwrap(), PutResult::Stored);
}

/// Freigabe 27.10.2026: nie die Sperre eines lebenden Prozesses entfernen;
/// unbekannte Sperren bleiben und werden gemeldet; Aufräumen und Ablage
/// entsperren sich nicht gegenseitig.
#[test]
fn s3_05_lock_recovery_never_unlocks_a_live_process() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let s = store(&server);
    let name = GitReceiptStore::receipt_ref(&key(40, 1));
    let lock = old_lock(&server, &name);

    // (a) Ein lebender Git-Kindprozess der Ablage (simuliert mit `sleep`):
    // nichts wird entfernt, solange er lebt.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let token = coord_token(&server, "children", "live", child.id());
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert!(r.busy, "{r:?}");
    assert!(r.removed.is_empty());
    assert!(lock.exists());
    assert!(token.exists());
    child.kill().unwrap();
    child.wait().unwrap();
    // Danach ist er tot: Token weg, Sperre entfernt.
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert_eq!((r.busy, r.removed.clone()), (false, vec![name.clone()]));
    assert!(!token.exists());

    // (b) Ein lebender Schreiber (dieser Prozess): nichts entfernt.
    let lock = old_lock(&server, &name);
    let token = coord_token(&server, "writers", "me", std::process::id());
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert!(r.busy && lock.exists());
    std::fs::remove_file(&token).unwrap();

    // (c) Ein laufender anderer Aufräumer (Marke eines lebenden Prozesses):
    // nichts tun; die Marke eines toten Prozesses wird übernommen.
    let marker = coord_token(&server, "", "clearing", std::process::id());
    assert!(s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap().busy);
    assert!(lock.exists());
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let gone_pid = gone.id();
    gone.wait().unwrap();
    let scope = minds_witness_git::process::scope();
    std::fs::write(&marker, format!("{gone_pid} {scope}|0\n")).unwrap();
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert_eq!((r.busy, r.removed.clone()), (false, vec![name.clone()]));
    assert!(!marker.exists());

    // (d) Unbekannte Sperren (fremder Name, Verzeichnis) bleiben erhalten
    // und werden gemeldet; sie sind nie „verwaist“.
    let foreign = old_lock(&server, "refs/minds/witness/foreign");
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert_eq!(
        r.kept_unknown,
        vec!["refs/minds/witness/foreign".to_owned()]
    );
    assert!(foreign.exists());
    // Eine Altersangabe unter MIN_ORPHAN_AGE wird nie angewendet.
    let young = server.git_dir.join("refs/minds/witness/keys.lock");
    std::fs::write(&young, b"").unwrap();
    let r = s.clear_stale_locks(Duration::from_secs(0)).unwrap();
    assert_eq!(r.kept_young, vec![KEYS_REF.to_owned()]);
    assert!(young.exists());
    std::fs::remove_file(&young).unwrap();
    std::fs::remove_file(&foreign).unwrap();

    // (e) Eine Marke aus einem anderen Geltungsbereich (anderer Container)
    // ist nicht prüfbar: nie übernommen.
    let lock = old_lock(&server, &name);
    std::fs::write(&marker, format!("{gone_pid} fremd|0\n")).unwrap();
    age(&marker, 2 * MIN_ORPHAN_AGE);
    assert!(s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap().busy);
    assert!(lock.exists() && marker.exists());
    std::fs::remove_file(&marker).unwrap();
}

/// Leere oder unlesbare Marken und Tokens (abgebrochenes Schreiben,
/// Fremdeingriff): jung als lebend behandelt und gemeldet, nach
/// `UNREADABLE_TOKEN_AGE` als tot. Nie blockieren sie dauerhaft.
#[test]
fn s3_05_empty_marker_and_token_do_not_block_forever() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let mut s = store(&server);
    let k = key(41, 1);
    let name = GitReceiptStore::receipt_ref(&k);
    let lock = old_lock(&server, &name);
    let coord = server.git_dir.join("minds-witness");

    // Leere, junge Marke: Aufräumen hält sich zurück, Ablage bleibt möglich
    // (die Ablage selbst wartet nicht auf die Marke bis in alle Ewigkeit).
    std::fs::create_dir_all(&coord).unwrap();
    let marker = coord.join("clearing");
    std::fs::write(&marker, b"").unwrap();
    assert!(s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap().busy);
    assert!(lock.exists());
    // Gealtert: übernommen, Sperre entfernt, Marke weg.
    age(&marker, Duration::from_secs(31));
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert_eq!((r.busy, r.removed.clone()), (false, vec![name.clone()]));
    assert!(!marker.exists());

    // Leerer, junger Schreiber-Token: belegt nicht „verwaist“; die Sperre
    // bleibt und wird als nicht belegbar gemeldet.
    let lock = old_lock(&server, &name);
    let writers = coord.join("writers");
    std::fs::create_dir_all(&writers).unwrap();
    let token = writers.join("1-empty");
    std::fs::write(&token, b"").unwrap();
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert!(r.busy, "{r:?}");
    assert_eq!(r.kept_busy, vec![name.clone()]);
    assert!(lock.exists());
    // Gealtert: als tot behandelt, Token und Sperre entfernt; danach
    // gelingt die Ablage.
    age(&token, Duration::from_secs(31));
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert_eq!((r.busy, r.removed.clone()), (false, vec![name.clone()]));
    assert!(!token.exists());
    assert_eq!(s.put(&k, b"r").unwrap(), PutResult::Stored);
}

/// Stromausfall während Ablage und Aufräumen: Token, Marke und Sperre aus
/// einem früheren Systemstart blockieren nach dem Neustart nicht für immer.
/// Eine andere Boot-Id mit jüngerer Datei (anderer Rechner möglich) bleibt
/// dagegen unangetastet.
#[cfg(target_os = "linux")]
#[test]
fn s3_05_tokens_from_a_previous_boot_are_dead() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let mut s = store(&server);
    let k = key(43, 1);
    let name = GitReceiptStore::receipt_ref(&k);
    let scope = minds_witness_git::process::scope();
    let ns = scope.split_once('/').unwrap().1.to_owned();
    let old_boot = format!("00000000-0000-0000-0000-000000000000/{ns}");
    let coord = server.git_dir.join("minds-witness");
    std::fs::create_dir_all(coord.join("writers")).unwrap();
    let before_boot = Duration::from_secs(10 * 365 * 24 * 3600);

    // Fremde Boot-Id, aber jung: nicht prüfbar, nichts entfernt.
    let lock = old_lock(&server, &name);
    let token = coord.join("writers").join("1-old");
    std::fs::write(&token, format!("{} {old_boot}|5\n", std::process::id())).unwrap();
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert!(r.busy && lock.exists() && token.exists(), "{r:?}");
    // Vor dem aktuellen Systemstart geschrieben: tot; Sperre entfernt.
    age(&token, before_boot);
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert_eq!((r.busy, r.removed.clone()), (false, vec![name.clone()]));
    assert!(!token.exists());

    // Marke eines Aufräumers aus dem früheren Start: übernommen.
    let lock = old_lock(&server, &name);
    let marker = coord.join("clearing");
    std::fs::write(&marker, format!("{} {old_boot}|5\n", std::process::id())).unwrap();
    age(&marker, before_boot);
    let r = s.clear_stale_locks(MIN_ORPHAN_AGE).unwrap();
    assert_eq!((r.busy, r.removed.clone()), (false, vec![name.clone()]));
    assert!(!marker.exists() && !lock.exists());
    assert_eq!(s.put(&k, b"r").unwrap(), PutResult::Stored);
}

/// Freigabe 27.10.2026, gezielter Test: eine aktiv gehaltene Sperre und
/// gleichzeitiges Aufräumen ohne Altersgrenze. Belegt allein durch das
/// Token-Protokoll, nicht durch das Alter.
#[test]
fn s3_05_held_lock_survives_concurrent_recovery_without_age_floor() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cleaner = {
        let s = store(&server);
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut runs = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let r = s.clear_stale_locks_without_age_floor().unwrap();
                assert!(r.removed.is_empty(), "{r:?}");
                runs += 1;
            }
            runs
        })
    };
    // Aktiv gehaltene Sperre: ein lebender Schreiber mit Token.
    let name = GitReceiptStore::receipt_ref(&key(42, 1));
    let held = old_lock(&server, &name);
    let token = coord_token(&server, "writers", "held", std::process::id());
    let writers: Vec<_> = (0..4u8)
        .map(|w| {
            let mut s = store(&server);
            std::thread::spawn(move || {
                for i in 0..25u8 {
                    let k = key(210 + w, i);
                    assert_eq!(s.put(&k, &[w, i]).unwrap(), PutResult::Stored);
                }
            })
        })
        .collect();
    for h in writers {
        h.join().unwrap();
    }
    assert!(held.exists(), "gehaltene Sperre entfernt");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(cleaner.join().unwrap() > 0);
    let s = store(&server);
    for w in 0..4u8 {
        for i in 0..25u8 {
            assert_eq!(s.get(&key(210 + w, i)).unwrap(), Some(vec![w, i]));
        }
    }
    // Ohne Schreiber: nun nachweislich verwaist.
    std::fs::remove_file(&token).unwrap();
    let r = s.clear_stale_locks_without_age_floor().unwrap();
    assert_eq!(r.removed, vec![name]);
}

/// Ablage und Aufräumen gleichzeitig: Keine Ablage scheitert, keine wird
/// überschrieben, und das Aufräumen entfernt keine lebende Sperre.
#[test]
fn s3_05_store_and_lock_recovery_run_concurrently() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cleaner = {
        let s = store(&server);
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut runs = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let r = s.clear_stale_locks(Duration::from_secs(0)).unwrap();
                assert!(r.removed.is_empty(), "{r:?}");
                runs += 1;
            }
            runs
        })
    };
    let writers: Vec<_> = (0..4u8)
        .map(|w| {
            let mut s = store(&server);
            std::thread::spawn(move || {
                for i in 0..25u8 {
                    let k = key(200 + w, i);
                    assert_eq!(s.put(&k, &[w, i]).unwrap(), PutResult::Stored);
                    assert_eq!(s.put(&k, &[w, i]).unwrap(), PutResult::AlreadyExists);
                }
            })
        })
        .collect();
    for h in writers {
        h.join().unwrap();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(cleaner.join().unwrap() > 0);
    let s = store(&server);
    for w in 0..4u8 {
        for i in 0..25u8 {
            assert_eq!(s.get(&key(200 + w, i)).unwrap(), Some(vec![w, i]));
        }
    }
}

#[test]
fn s3_05_repository_hooks_do_not_run_in_the_store() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let marker = fx.dir.path().join("hook-ran");
    let hook = server.git_dir.join("hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(
        &hook,
        format!("#!/bin/sh\ntouch '{}'\nsleep 30\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut s = GitReceiptStore::new(&git_program(), &server.git_dir, Duration::from_secs(5));
    assert_eq!(s.put(&key(31, 1), b"r").unwrap(), PutResult::Stored);
    s.register_public_key(&entry(1, "2026-01-01T00:00:00Z"))
        .unwrap();
    assert!(!marker.exists());
}

#[test]
fn s3_05_sha256_repository() {
    let fx = GitTestFixture::new();
    let path = fx.dir.path().join("sha256.git");
    std::fs::create_dir_all(&path).unwrap();
    let repo = Repo {
        path: path.clone(),
        git_dir: path,
        home: fx.dir.path().join("home"),
    };
    std::fs::create_dir_all(&repo.home).unwrap();
    repo.git(&["init", "-q", "--bare", "--object-format=sha256"]);
    let mut s = store(&repo);
    let k = ReceiptKey {
        commit: CommitId::parse(&"ab".repeat(32)).unwrap(),
        id: HashStr::of_bytes(b"x"),
    };
    assert_eq!(s.put(&k, b"r").unwrap(), PutResult::Stored);
    assert_eq!(s.put(&k, b"r").unwrap(), PutResult::AlreadyExists);
    assert!(matches!(s.put(&k, b"z"), Err(StoreError::Conflict(_))));
    s.register_public_key(&entry(1, "2026-01-01T00:00:00Z"))
        .unwrap();
    s.register_public_key(&entry(2, "2026-01-01T00:00:00Z"))
        .unwrap();
    assert_eq!(s.public_keys().unwrap().len(), 2);
    let (_, r) = s.put_trust(&snapshot("git.test/project-42")).unwrap();
    assert_eq!(r, PutResult::Stored);
}

#[test]
fn s3_05_registration_keeps_withdrawn_entries() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let mut s = store(&server);
    // Eine von der Schlüsselverwaltung zurückgezogene Liste (am Store vorbei).
    let mut withdrawn = entry(1, "2026-01-01T00:00:00Z");
    withdrawn.valid_until = Some(Timestamp::parse("2026-06-01T00:00:00Z").unwrap());
    let bytes = minds_witness_core::canonical_bytes(&vec![withdrawn.clone()]);
    let blob = server.blob(&bytes);
    let t = server.tree(&[("100644", b"keys.json", &blob)]);
    let c = server.commit(&t, &[]);
    server.update_ref(KEYS_REF, &c);
    // Erneute Registrierung desselben Schlüssels: kein Wiederaufleben.
    s.register_public_key(&entry(1, "2026-01-01T00:00:00Z"))
        .unwrap();
    assert_eq!(s.public_keys().unwrap(), vec![withdrawn.clone()]);
    // Neuer Schlüssel: der zurückgezogene bleibt unverändert erhalten.
    s.register_public_key(&entry(2, "2026-07-01T00:00:00Z"))
        .unwrap();
    assert_eq!(
        s.public_keys().unwrap(),
        vec![withdrawn, entry(2, "2026-07-01T00:00:00Z")]
    );
}

/// Nachprüfung Commit 2: Gleichzeitige identische Ablagen dürfen nie
/// fälschlich „kaputter Ref“ melden (Lesung während des Anlegens).
#[test]
fn s3_05_identical_parallel_puts_never_report_broken() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    for round in 0..30u8 {
        let k = key(100 + round, 1);
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let mut s = store(&server);
                let k = k.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (s.put(&k, b"same"), s.list(&k.commit).map(|l| l.len()))
                })
            })
            .collect();
        for h in handles {
            let (put, list) = h.join().unwrap();
            assert!(put.is_ok(), "round {round}: {put:?}");
            assert!(list.is_ok(), "round {round}: {list:?}");
        }
    }
}
