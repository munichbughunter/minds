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
use minds_witness_git::receipt_store::{GitReceiptStore, KEYS_REF, STALE_LOCK_AGE, TRUST_PREFIX};
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

#[test]
fn s3_05_stale_lock_is_recoverable() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let mut s = store(&server);
    let k = key(30, 1);
    let name = GitReceiptStore::receipt_ref(&k);
    let lock = server.git_dir.join(format!("{name}.lock"));
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let f = std::fs::File::create(&lock).unwrap();
    // Abgebrochener Prozess: Die Sperre bleibt und blockiert die Ablage.
    assert!(matches!(s.put(&k, b"r"), Err(StoreError::Internal(_))));
    // Frische Sperren bleiben (ein lebender Prozess könnte sie halten).
    assert!(s.clear_stale_locks(STALE_LOCK_AGE).unwrap().is_empty());
    assert!(lock.exists());
    // Verwaiste Sperren werden entfernt; danach gelingt die Wiederholung.
    f.set_modified(std::time::SystemTime::now() - 2 * STALE_LOCK_AGE)
        .unwrap();
    drop(f);
    assert_eq!(
        s.clear_stale_locks(STALE_LOCK_AGE).unwrap(),
        vec![name.clone()]
    );
    assert_eq!(s.put(&k, b"r").unwrap(), PutResult::Stored);
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

/// Nachprüfung Commit 2: Die Aufräumung ist gegen sich selbst serialisiert
/// und entfernt nie eine junge Sperre.
#[test]
fn s3_05_stale_lock_cleanup_is_serialized() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let s = store(&server);
    let name = GitReceiptStore::receipt_ref(&key(40, 1));
    let lock = server.git_dir.join(format!("{name}.lock"));
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let f = std::fs::File::create(&lock).unwrap();
    f.set_modified(std::time::SystemTime::now() - 2 * STALE_LOCK_AGE)
        .unwrap();
    // Ein laufender anderer Aufräumer (frische Wächterdatei): nichts tun.
    let guard = server.git_dir.join("minds-witness-clear-locks.lock");
    std::fs::write(&guard, b"").unwrap();
    assert!(s.clear_stale_locks(STALE_LOCK_AGE).unwrap().is_empty());
    assert!(lock.exists());
    // Eine verwaiste Wächterdatei wird übernommen.
    std::fs::File::options()
        .write(true)
        .open(&guard)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - 2 * STALE_LOCK_AGE)
        .unwrap();
    assert_eq!(s.clear_stale_locks(STALE_LOCK_AGE).unwrap(), vec![name]);
    assert!(!guard.exists());
    // Ein kürzeres Alter als STALE_LOCK_AGE wird nie angewendet.
    let fresh = server.git_dir.join("refs/minds/witness/keys.lock");
    std::fs::write(&fresh, b"").unwrap();
    assert!(
        s.clear_stale_locks(Duration::from_secs(0))
            .unwrap()
            .is_empty()
    );
    assert!(fresh.exists());
}
