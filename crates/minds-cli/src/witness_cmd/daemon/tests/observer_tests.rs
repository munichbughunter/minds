//! EA-08: der Datei-Beobachter des Witness.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Zählt jeden Hash — „nie gehasht" wird gezählt, nicht aus der Ausgabe
/// erraten.
struct Counting(Arc<AtomicUsize>);
impl observer::ContentHasher for Counting {
    fn hash(&self, bytes: &[u8]) -> minds_core::ContentHash {
        self.0.fetch_add(1, Ordering::SeqCst);
        observer::Blake3.hash(bytes)
    }
}

fn stream() -> SessionKey {
    SessionKey::new("witness", "1").unwrap()
}

/// Die `fs.observed`-Payloads des eigenen Streams.
fn observed(writer: &Writer) -> Vec<(String, Option<String>, Option<String>)> {
    writer
        .journal
        .read(&stream())
        .unwrap()
        .events
        .iter()
        .filter(|e| e.raw_kind == "fs.observed")
        .map(|e| {
            let v: serde_json::Value = serde_json::from_str(e.payload.get()).unwrap();
            (
                v["path"].as_str().unwrap().to_owned(),
                v["content"].as_str().map(str::to_owned),
                v["reason"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

fn b3(bytes: &[u8]) -> String {
    minds_core::ContentHash::from_bytes(*blake3::hash(bytes).as_bytes()).to_string()
}

fn canonical_root(f: &Fixture) -> PathBuf {
    load(&f.home).unwrap().repo_root
}

fn watching(f: &Fixture) -> Writer {
    let mut writer = f.writer();
    let observer =
        observer::Observer::watch(&canonical_root(f), Box::new(observer::Blake3)).unwrap();
    writer.observe_into(stream(), observer);
    writer
}

/// Tickt, bis `done` gilt oder die Frist abläuft; gibt die Dauer zurück.
fn tick_until(writer: &mut Writer, limit: Duration, done: impl Fn(&Writer) -> bool) -> Duration {
    let start = Instant::now();
    while !done(writer) && start.elapsed() < limit {
        writer.observe(false).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    start.elapsed()
}

#[test]
fn fs_observer_records_write() {
    let f = Fixture::new();
    let mut writer = watching(&f);
    let root = canonical_root(&f);
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/merge.rs"), b"fn merge() {}\n").unwrap();
    let took = tick_until(&mut writer, Duration::from_secs(5), |w| {
        observed(w)
            .iter()
            .any(|(path, _, _)| path == "src/merge.rs")
    });
    assert!(took < Duration::from_secs(1), "observed after {took:?}");
    let seen = observed(&writer);
    let entry = seen
        .iter()
        .find(|(path, _, _)| path == "src/merge.rs")
        .unwrap();
    assert_eq!(entry.1.as_deref(), Some(b3(b"fn merge() {}\n").as_str()));
    assert_eq!(entry.2, None);
    // Repo-relativ, nie der Host-Pfad; ohne neuen Inhalt keine zweite
    // Beobachtung (das eigene Lesen ist keine Änderung).
    assert!(seen.iter().all(|(path, _, _)| !path.starts_with('/')));
    tick_until(&mut writer, Duration::from_millis(300), |_| false);
    assert_eq!(
        observed(&writer)
            .iter()
            .filter(|(path, _, _)| path == "src/merge.rs")
            .count(),
        1
    );
    // Jede Beobachtung ist ein gekettetes Event des eigenen Streams.
    assert_eq!(
        writer.folders[&stream()].snapshot().coverage.events as usize,
        writer.journal.read(&stream()).unwrap().events.len()
    );
}

#[test]
fn fs_observer_never_hashes_secret_or_ignored() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    let outside = f._dir.path().join("outside.txt");
    fs::write(&outside, "host file").unwrap();
    fs::write(root.join(".gitignore"), "target/\n").unwrap();
    fs::create_dir_all(root.join(".git/info")).unwrap();
    fs::write(root.join(".git/info/exclude"), "local.secret.txt\n").unwrap();
    let files: &[(&str, &[u8])] = &[
        ("src/ok.rs", b"fn ok() {}\n"),
        ("target/out.bin", b"build output"),
        ("local.secret.txt", b"excluded locally"),
        (".env", b"TOKEN=x"),
        (".ssh/id_ed25519", b"PRIVATE KEY"),
    ];
    for (path, bytes) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    std::os::unix::fs::symlink(&outside, root.join("outside-link")).unwrap();
    std::os::unix::fs::symlink(root.join(".env"), root.join("env-link")).unwrap();
    std::os::unix::fs::symlink(root.join(".git/config"), root.join("git-link")).unwrap();
    // Eine getrackte Datei wird gelöscht: Das ist eine Beobachtung. (Eine
    // gelöschte, nie gesehene und ungetrackte nicht — sie kann ignoriert
    // gewesen sein.)
    fs::write(root.join("gone.rs"), "x").unwrap();
    git(&root, &["-c", "core.fsmonitor=false", "add", "gone.rs"]);
    fs::remove_file(root.join("gone.rs")).unwrap();
    // Eine Repo-Konfiguration des Agenten, die eine Host-Datei zur
    // Exclude-Liste machen will, zählt nicht.
    git(
        &root,
        &["config", "core.excludesFile", outside.to_str().unwrap()],
    );
    let count = Arc::new(AtomicUsize::new(0));
    let observer = observer::Observer::detached(&root, Box::new(Counting(count.clone()))).unwrap();
    for path in [
        "src/ok.rs",
        "target/out.bin",
        "local.secret.txt",
        ".env",
        ".ssh/id_ed25519",
        ".git/config",
        "outside-link",
        "env-link",
        "git-link",
        "gone.rs",
    ] {
        observer.note(&root.join(path));
    }
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    writer.observe(true).unwrap();
    let mut seen = observed(&writer);
    seen.sort();
    let entry = |path: &str, content: Option<String>, reason: Option<&str>| {
        (path.to_owned(), content, reason.map(str::to_owned))
    };
    assert_eq!(
        seen,
        vec![
            entry(".env", None, Some("secret_file")),
            entry(".ssh/id_ed25519", None, Some("secret_file")),
            entry("env-link", None, Some("secret_file")),
            entry("git-link", None, Some("outside_repo")),
            entry("gone.rs", None, Some("deleted")),
            entry("outside-link", None, Some("outside_repo")),
            entry("src/ok.rs", Some(b3(b"fn ok() {}\n")), None),
        ]
    );
    // Genau ein Hash: der der gewöhnlichen Datei.
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let mut journal = String::new();
    for dir in fs::read_dir(f.home.join("journal/witness")).unwrap() {
        for entry in fs::read_dir(dir.unwrap().path()).unwrap() {
            journal.push_str(&fs::read_to_string(entry.unwrap().path()).unwrap_or_default());
        }
    }
    for leaked in [
        "TOKEN=x",
        "PRIVATE KEY",
        "host file",
        "build output",
        "target/out.bin",
        "local.secret.txt",
    ] {
        assert!(!journal.contains(leaked), "{leaked}");
    }
}

#[test]
fn fs_observer_never_watches_through_a_directory_symlink() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    let outside = f._dir.path().join("host-dir");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("home")).unwrap();
    let mut writer = watching(&f);
    fs::write(outside.join("private.txt"), "host").unwrap();
    fs::write(root.join("inside.rs"), "x").unwrap();
    tick_until(&mut writer, Duration::from_secs(5), |w| {
        observed(w).iter().any(|(p, _, _)| p == "inside.rs")
    });
    tick_until(&mut writer, Duration::from_millis(300), |_| false);
    assert!(
        observed(&writer)
            .iter()
            .all(|(p, _, _)| !p.starts_with("home/")),
        "{:?}",
        observed(&writer)
    );
    // Auch eine gemeldete Änderung hinter dem Symlink wird nicht gelesen.
    let count = Arc::new(AtomicUsize::new(0));
    let observer = observer::Observer::detached(&root, Box::new(Counting(count.clone()))).unwrap();
    observer.note(&root.join("home/private.txt"));
    let mut writer = f.writer();
    writer.observe_into(SessionKey::new("witness", "2").unwrap(), observer);
    writer.observe(true).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn fs_observer_skips_nested_repositories_without_losing_the_batch() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    fs::create_dir_all(root.join("vendor/lib")).unwrap();
    git(&root.join("vendor/lib"), &["init", "-q", "--template="]);
    fs::write(root.join("vendor/lib/x.c"), "int x;").unwrap();
    fs::write(root.join("a.rs"), "a").unwrap();
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    observer.note(&root.join("vendor/lib/x.c"));
    observer.note(&root.join("a.rs"));
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    writer.observe(true).unwrap();
    let seen: Vec<String> = observed(&writer).into_iter().map(|(p, _, _)| p).collect();
    assert_eq!(seen, ["a.rs"]);
}

#[test]
fn fs_observer_evaluates_ignore_rules_without_running_git() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    // Ein Befehl in der Repo-Konfiguration des Agenten: Liefe beim
    // Beobachten ein `git`, könnte er starten.
    let marker = f._dir.path().join("ran");
    let hook = f._dir.path().join("hook.sh");
    fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(&root, &["config", "core.fsmonitor", hook.to_str().unwrap()]);
    git(
        &root,
        &["config", "core.sshCommand", hook.to_str().unwrap()],
    );
    // Getrackt bleibt beobachtet, auch wenn ein Muster passt.
    fs::write(root.join("tracked.log"), "kept").unwrap();
    git(&root, &["-c", "core.fsmonitor=false", "add", "tracked.log"]);
    fs::write(root.join(".gitignore"), "*.log\n").unwrap();
    fs::write(root.join("tracked.log"), "changed").unwrap();
    fs::write(root.join("new.log"), "untracked").unwrap();
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    for path in ["tracked.log", "new.log"] {
        observer.note(&root.join(path));
    }
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    writer.observe(true).unwrap();
    let seen: Vec<String> = observed(&writer).into_iter().map(|(p, _, _)| p).collect();
    assert_eq!(seen, ["tracked.log"]);
    assert!(
        !marker.exists(),
        "a git process ran from the agent's config"
    );
}

#[test]
fn fs_observer_classifies_a_bounded_number_of_paths_per_step() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    let mut observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    let total = observer::MAX_CLASSIFY_PER_STEP + 44;
    for i in 0..total {
        fs::write(root.join(format!("f{i:04}")), i.to_string()).unwrap();
        observer.note(&root.join(format!("f{i:04}")));
    }
    let start = Instant::now();
    assert!(observer.tick(start, false).seen.is_empty());
    let later = start + observer::DEBOUNCE;
    assert_eq!(
        observer.tick(later, false).seen.len(),
        observer::MAX_CLASSIFY_PER_STEP
    );
    assert_eq!(observer.tick(later, false).seen.len(), 44);
    assert_eq!(observer.pending_len(), 0);
}

#[test]
fn fs_observer_respects_the_size_cap_and_never_reads_hard_links() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    let cap = minds_core::observation::MAX_OBSERVED_BYTES as usize;
    fs::write(root.join("exact.bin"), vec![b'a'; cap]).unwrap();
    fs::write(root.join("over.bin"), vec![b'a'; cap + 1]).unwrap();
    let host = f._dir.path().join("pgpass");
    fs::write(&host, "db:secret").unwrap();
    fs::hard_link(&host, root.join("notes.txt")).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let observer = observer::Observer::detached(&root, Box::new(Counting(count.clone()))).unwrap();
    for path in ["exact.bin", "over.bin", "notes.txt"] {
        observer.note(&root.join(path));
    }
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    // Die 16-MiB-Datei sprengt das Byte-Budget eines Durchlaufs: Der Rest
    // kommt im nächsten.
    writer.observe(true).unwrap();
    writer.observe(true).unwrap();
    let mut seen = observed(&writer);
    seen.sort();
    assert_eq!(
        seen,
        [
            ("exact.bin".into(), Some(b3(&vec![b'a'; cap])), None),
            ("notes.txt".into(), None, Some("outside_repo".into())),
            ("over.bin".into(), None, Some("too_large".into())),
        ]
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn fs_observer_drops_only_what_unreadable_rules_govern() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    // Eine `.gitignore`, die ein Symlink nach außen ist, wird nicht gelesen —
    // was sie regeln würde, fällt weg (lieber blind als gefingerprintet).
    let outside = f._dir.path().join("rules");
    fs::write(&outside, "nothing\n").unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("sub/.gitignore")).unwrap();
    fs::write(root.join("sub/a.rs"), "a").unwrap();
    fs::write(root.join("b.rs"), "b").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let mut observer =
        observer::Observer::detached(&root, Box::new(Counting(count.clone()))).unwrap();
    observer.note(&root.join("sub/a.rs"));
    observer.note(&root.join("b.rs"));
    let batch = observer.tick(Instant::now(), true);
    assert!(batch.ignore_failed);
    let seen: Vec<&str> = batch.seen.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(seen, ["b.rs"]);
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // Eine FIFO als `info/exclude` hält nichts an; der Stapel fällt weg.
    fs::create_dir_all(root.join(".git/info")).unwrap();
    let fifo = std::ffi::CString::new(
        root.join(".git/info/exclude")
            .as_os_str()
            .as_encoded_bytes(),
    )
    .unwrap();
    // SAFETY: nul-terminierter Pfad.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    fs::write(root.join("b.rs"), "b2").unwrap();
    observer.note(&root.join("b.rs"));
    let batch = observer.tick(Instant::now(), true);
    assert!(batch.ignore_failed);
    assert!(batch.seen.is_empty());
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn fs_observer_coalesces_bursts() {
    let f = Fixture::new();
    let mut writer = watching(&f);
    let root = canonical_root(&f);
    fs::create_dir(root.join("burst")).unwrap();
    for round in 0..3 {
        for i in 0..1000 {
            fs::write(
                root.join(format!("burst/{i:04}.txt")),
                format!("{i} round {round}"),
            )
            .unwrap();
        }
    }
    let count = |w: &Writer| {
        observed(w)
            .iter()
            .filter(|(path, _, _)| path.starts_with("burst/"))
            .count()
    };
    tick_until(&mut writer, Duration::from_secs(20), |w| count(w) >= 1000);
    // Nachlaufende Meldungen derselben Endstände ergeben nichts Neues.
    tick_until(&mut writer, Duration::from_millis(500), |_| false);
    assert!(writer.observer.as_ref().unwrap().pending_len() <= observer::MAX_PENDING);
    let seen = observed(&writer);
    let burst: Vec<_> = seen
        .iter()
        .filter(|(p, _, _)| p.starts_with("burst/"))
        .collect();
    assert_eq!(burst.len(), 1000, "one observation per final state");
    for (path, content, _) in burst {
        let i: usize = path["burst/".len()..path.len() - 4].parse().unwrap();
        assert_eq!(
            content.as_deref(),
            Some(b3(format!("{i} round 2").as_bytes()).as_str())
        );
    }
}

#[test]
fn fs_observer_yields_to_hooks_during_bursts() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    for i in 0..100 {
        fs::write(root.join(format!("f{i:03}")), i.to_string()).unwrap();
        observer.note(&root.join(format!("f{i:03}")));
    }
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    // Die Ruhe zählt ab dem Einsammeln durch die Eventloop.
    writer.observe(false).unwrap();
    assert!(observed(&writer).is_empty());
    std::thread::sleep(observer::DEBOUNCE);
    // Ein Durchlauf der Eventloop: höchstens 64 fsyncende Appends, dann
    // sind wieder die Hooks dran; der Checkpoint nimmt den Rest mit.
    writer.observe(false).unwrap();
    assert_eq!(observed(&writer).len(), 64);
    writer.observe(true).unwrap();
    assert_eq!(observed(&writer).len(), 100);
}

#[test]
fn fs_observer_bounds_the_checkpoint_flush_too() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    let total = observer::MAX_CLASSIFY_PER_STEP + 10;
    for i in 0..total {
        fs::write(root.join(format!("f{i:04}")), i.to_string()).unwrap();
        observer.note(&root.join(format!("f{i:04}")));
    }
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    // Eine Checkpoint-Anfrage wartet die Ruhe nicht ab, sprengt aber kein
    // Budget: Der Rest kommt in die nächste Epoche.
    writer.observe(true).unwrap();
    assert_eq!(observed(&writer).len(), observer::MAX_CLASSIFY_PER_STEP);
    writer.observe(true).unwrap();
    assert_eq!(observed(&writer).len(), total);
}

#[test]
fn fs_observer_never_hashes_secret_content_or_unscannable_bytes() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    fs::create_dir(root.join("config")).unwrap();
    // Ein Name ohne Secret-Muster, aber ein Secret im Inhalt: Ein Hash über
    // diese paar Bytes wäre ein Wörterbuch-Orakel.
    fs::write(root.join("config/db.toml"), "password = \"hunter2\"\n").unwrap();
    fs::write(
        root.join("config/ci.yml"),
        "token: glpat-AbCdEfGhIjKlMnOpQrSt\n",
    )
    .unwrap();
    fs::write(root.join("logo.png"), [0x89, b'P', b'N', b'G', 0xff, 0x00]).unwrap();
    fs::write(root.join("plain.rs"), "fn main() {}\n").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let observer = observer::Observer::detached(&root, Box::new(Counting(count.clone()))).unwrap();
    for path in ["config/db.toml", "config/ci.yml", "logo.png", "plain.rs"] {
        observer.note(&root.join(path));
    }
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    writer.observe(true).unwrap();
    let mut seen = observed(&writer);
    seen.sort();
    assert_eq!(
        seen,
        [
            (
                "config/ci.yml".into(),
                None,
                Some("redacted_content".into())
            ),
            (
                "config/db.toml".into(),
                None,
                Some("redacted_content".into())
            ),
            ("logo.png".into(), None, Some("unscannable".into())),
            ("plain.rs".into(), Some(b3(b"fn main() {}\n")), None),
        ]
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn fs_observer_searches_new_directories() {
    // inotify meldet Dateien nicht, die in einem neuen Verzeichnis vor
    // dessen Watch entstanden: Ein neues Verzeichnis wird durchsucht.
    let f = Fixture::new();
    let root = canonical_root(&f);
    fs::create_dir_all(root.join("src/newmod/deep")).unwrap();
    fs::write(root.join("src/newmod/foo.rs"), "foo").unwrap();
    fs::write(root.join("src/newmod/deep/bar.rs"), "bar").unwrap();
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    observer.note_created(&root.join("src/newmod"));
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    for _ in 0..3 {
        writer.observe(true).unwrap();
    }
    let mut seen: Vec<String> = observed(&writer).into_iter().map(|(p, _, _)| p).collect();
    seen.sort();
    assert_eq!(seen, ["src/newmod/deep/bar.rs", "src/newmod/foo.rs"]);
}

#[test]
fn fs_observer_records_files_written_into_a_new_directory() {
    let f = Fixture::new();
    let mut writer = watching(&f);
    let root = canonical_root(&f);
    fs::create_dir_all(root.join("src/newmod")).unwrap();
    fs::write(root.join("src/newmod/foo.rs"), "fn foo() {}\n").unwrap();
    tick_until(&mut writer, Duration::from_secs(5), |w| {
        observed(w).iter().any(|(p, _, _)| p == "src/newmod/foo.rs")
    });
    assert!(
        observed(&writer)
            .iter()
            .any(|(p, c, _)| p == "src/newmod/foo.rs"
                && c.as_deref() == Some(b3(b"fn foo() {}\n").as_str())),
        "{:?}",
        observed(&writer)
    );
}

#[test]
fn fs_observer_fingerprints_the_link_text_git_commits() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    fs::create_dir_all(root.join("shared")).unwrap();
    fs::write(root.join("shared/a.rs"), "shared content").unwrap();
    std::os::unix::fs::symlink("shared/a.rs", root.join("link.rs")).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let observer = observer::Observer::detached(&root, Box::new(Counting(count.clone()))).unwrap();
    observer.note(&root.join("link.rs"));
    // Unbekannt, ungetrackt, gelöscht: kann ignoriert gewesen sein — nie.
    observer.note(&root.join("never-seen.rs"));
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    writer.observe(true).unwrap();
    assert_eq!(
        observed(&writer),
        [("link.rs".into(), Some(b3(b"shared/a.rs")), None)]
    );
}

#[test]
fn fs_observer_records_a_gap_on_overflow() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    // Mehr Meldungen, als der Kanal fasst: eine sichtbare, gekettete Lücke.
    for i in 0..=observer::MAX_PENDING {
        observer.note(&root.join(format!("f{i}")));
    }
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    writer.observe(false).unwrap();
    let gaps = writer
        .journal
        .read(&stream())
        .unwrap()
        .events
        .iter()
        .filter(|e| e.raw_kind == "fs.gap")
        .count();
    assert_eq!(gaps, 1);
}

#[test]
fn witness_checkpoint_seals_both_streams() {
    use minds_core::evidence::{Seal, SealOutcome};
    let f = Fixture::new();
    let signers_line = f.keygen();
    let signers = f._dir.path().join("allowed_signers");
    fs::write(&signers, &signers_line).unwrap();
    let principal = signers_line.split_whitespace().next().unwrap();
    let root = canonical_root(&f);
    let mut writer = f.writer();
    writer
        .lifecycle(&stream(), "witness.start", serde_json::json!({}))
        .unwrap();
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    fs::write(root.join("merge.rs"), "fn merge() {}\n").unwrap();
    // Noch nicht beruhigt: Der Checkpoint nimmt es trotzdem mit.
    observer.note(&root.join("merge.rs"));
    writer.observe_into(stream(), observer);
    append(&mut writer);
    writer.checkpoint_now(None).unwrap();

    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    let read_seals = || -> Vec<(minds_core::ContentHash, Seal)> {
        store
            .list_seals()
            .unwrap()
            .into_iter()
            .map(|id| {
                let seal = Seal::parse(&store.seal_text(&id).unwrap().unwrap()).unwrap();
                (id, seal)
            })
            .collect()
    };
    let seals = read_seals();
    let mut scopes: Vec<&str> = seals.iter().map(|(_, s)| s.scope.as_str()).collect();
    scopes.sort();
    assert_eq!(scopes, ["witness-fs/v1", "witness/v1"]);
    let ledger = fs::read_to_string(f.home.join("ledger")).unwrap();
    for (id, seal) in &seals {
        let text = store.seal_text(id).unwrap().unwrap();
        let signature = store.seal_signature(id).unwrap().unwrap();
        assert!(
            minds_attest::ssh_verify_ns(
                &text,
                &signature,
                &signers,
                principal,
                minds_attest::NS_WITNESS
            )
            .unwrap(),
            "{}",
            seal.scope
        );
        assert_eq!(
            ledger
                .lines()
                .filter(|l| l.starts_with(id.as_str()))
                .count(),
            1
        );
    }
    let (fs_id, fs_seal) = seals
        .iter()
        .find(|(_, s)| s.scope == "witness-fs/v1")
        .unwrap();
    assert_eq!(fs_seal.agent, "witness");
    // Lebenszyklus, Beobachtung und Checkpoint-Grenze sind gekettet,
    // lückenlos — und die Epoche endet nicht vor der Session.
    assert_eq!((fs_seal.events, fs_seal.gaps), (3, 0));
    let session_seal = seals
        .iter()
        .find(|(_, s)| s.scope == "witness/v1")
        .map(|(_, s)| s)
        .unwrap();
    let at = |s: &str| s.parse::<jiff::Timestamp>().unwrap();
    assert!(at(&fs_seal.last_event_at) >= at(&session_seal.last_event_at));
    let SealOutcome::ObservationsStored { observations } = &fs_seal.outcome else {
        panic!("{:?}", fs_seal.outcome)
    };
    let object = store
        .get_observations(&observations.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(object.observations.len(), 1);
    assert_eq!(object.observations[0].path, "merge.rs");
    assert_eq!(
        object.observations[0].content.as_ref().unwrap().to_string(),
        b3(b"fn merge() {}\n")
    );
    // Die Epoche ist verworfen; die nächste setzt die Kette fort.
    assert!(writer.journal.read(&stream()).unwrap().events.is_empty());
    assert!(!writer.fs_dirty);
    fs::write(root.join("merge.rs"), "fn merge2() {}\n").unwrap();
    writer
        .observer
        .as_ref()
        .unwrap()
        .note(&root.join("merge.rs"));
    writer.observe(true).unwrap();
    assert!(writer.fs_dirty);
    writer.checkpoint_now(None).unwrap();
    let next = read_seals()
        .into_iter()
        .map(|(_, seal)| seal)
        .find(|s| s.scope == "witness-fs/v1" && s.previous.is_some())
        .unwrap();
    assert_eq!(next.previous.as_ref(), Some(fs_id));
    git(&f.root, &["fsck", "--no-dangling"]);
}

#[test]
fn fs_observer_records_deletions_under_a_vanished_directory() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    fs::create_dir_all(root.join("old")).unwrap();
    fs::write(root.join(".gitignore"), "old/build.log\n").unwrap();
    for (path, text) in [
        ("old/tracked.rs", "t"),
        ("old/seen.rs", "s"),
        ("old/build.log", "l"),
    ] {
        fs::write(root.join(path), text).unwrap();
    }
    git(
        &root,
        &["-c", "core.fsmonitor=false", "add", "old/tracked.rs"],
    );
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    // `seen.rs` ist untrackt, aber beobachtet; `build.log` ignoriert.
    observer.note(&root.join("old/seen.rs"));
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    writer.observe(true).unwrap();
    // Das Verzeichnis wird aus dem Repo verschoben: gemeldet wird nur es.
    fs::rename(root.join("old"), f._dir.path().join("moved-out")).unwrap();
    writer.observer.as_ref().unwrap().note(&root.join("old"));
    writer.observe(true).unwrap();
    let mut deleted: Vec<String> = observed(&writer)
        .into_iter()
        .filter(|(_, _, reason)| reason.as_deref() == Some("deleted"))
        .map(|(path, _, _)| path)
        .collect();
    deleted.sort();
    assert_eq!(deleted, ["old/seen.rs", "old/tracked.rs"]);
}

#[test]
fn fs_observer_turns_too_deep_paths_into_a_gap() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    let deep: PathBuf = (0..=observer::MAX_DEPTH).map(|_| "d").collect();
    let count = Arc::new(AtomicUsize::new(0));
    let mut observer =
        observer::Observer::detached(&root, Box::new(Counting(count.clone()))).unwrap();
    observer.note(&root.join(&deep).join("x.rs"));
    let batch = observer.tick(Instant::now(), true);
    assert!(batch.overflow);
    assert!(batch.seen.is_empty());
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn fs_gaps_are_coalesced_per_reason() {
    let f = Fixture::new();
    let mut writer = f.writer();
    let gaps = |writer: &Writer| {
        writer
            .journal
            .read(&stream())
            .unwrap()
            .events
            .iter()
            .filter(|e| e.raw_kind == "fs.gap")
            .count()
    };
    // Die Zeit wird gesetzt, nicht abgewartet: Unter Last kann schon ein
    // einzelner fsyncender Append länger als eine Sekunde dauern.
    let just_now = || Instant::now();
    writer.gap(&stream(), "overflow", "test").unwrap();
    writer.last_gap.insert("overflow", just_now());
    writer.gap(&stream(), "overflow", "test").unwrap();
    assert_eq!(gaps(&writer), 1);
    // Ein anderer Grund ist eine eigene Lücke.
    writer.gap(&stream(), "panic", "test").unwrap();
    assert_eq!(gaps(&writer), 2);
    // Nach mehr als einer Sekunde wieder.
    writer
        .last_gap
        .insert("overflow", just_now() - Duration::from_millis(1100));
    writer.gap(&stream(), "overflow", "test").unwrap();
    assert_eq!(gaps(&writer), 3);
    assert!(writer.fs_dirty);
}

#[test]
fn checkpoint_requests_settle_at_most_once_per_loop_step() {
    let f = Fixture::new();
    let root = canonical_root(&f);
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    let mut writer = f.writer();
    writer.observe_into(stream(), observer);
    let write = |writer: &Writer, name: &str| {
        fs::write(root.join(name), name).unwrap();
        writer.observer.as_ref().unwrap().note(&root.join(name));
    };
    write(&writer, "a.rs");
    writer.settle_once().unwrap();
    assert_eq!(observed(&writer).len(), 1);
    // Eine zweite Anfrage im selben Durchlauf: keine zweite Arbeit.
    write(&writer, "b.rs");
    writer.settle_once().unwrap();
    assert_eq!(observed(&writer).len(), 1);
    // Der nächste Durchlauf darf wieder.
    writer.next_step();
    writer.settle_once().unwrap();
    assert_eq!(observed(&writer).len(), 2);
}

#[test]
fn a_failed_observation_store_still_closes_the_epoch() {
    use minds_core::evidence::{Seal, SealOutcome};
    let f = Fixture::new();
    f.keygen();
    let root = canonical_root(&f);
    // Ein Ref genau dort, wo der Namensraum der Observation-Objekte beginnt:
    // Das Ablegen scheitert (Verzeichnis/Datei-Konflikt der Refs).
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    git(
        &f.root,
        &["update-ref", "refs/minds/observations", head.trim()],
    );
    let mut writer = f.writer();
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    writer.observe_into(stream(), observer);
    fs::write(root.join("a.rs"), "a").unwrap();
    writer.observer.as_ref().unwrap().note(&root.join("a.rs"));
    append(&mut writer);
    writer.checkpoint_now(None).unwrap();
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    let fs_seals = || -> Vec<(minds_core::ContentHash, Seal)> {
        store
            .list_seals()
            .unwrap()
            .into_iter()
            .map(|id| {
                let seal = Seal::parse(&store.seal_text(&id).unwrap().unwrap()).unwrap();
                (id, seal)
            })
            .filter(|(_, seal)| seal.scope == "witness-fs/v1")
            .collect()
    };
    // Die Epoche ist geschlossen — als Block-Seal, nicht offen gelassen.
    let first = fs_seals();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].1.outcome, SealOutcome::Rejected);
    assert!(writer.journal.read(&stream()).unwrap().events.is_empty());
    // Der Stream kettet danach weiter (kein „journal disappeared").
    git(&f.root, &["update-ref", "-d", "refs/minds/observations"]);
    fs::write(root.join("b.rs"), "b").unwrap();
    writer.observer.as_ref().unwrap().note(&root.join("b.rs"));
    writer.observe(true).unwrap();
    append(&mut writer);
    writer.next_step();
    writer.checkpoint_now(None).unwrap();
    let all = fs_seals();
    let next = all
        .iter()
        .find(|(_, seal)| matches!(seal.outcome, SealOutcome::ObservationsStored { .. }))
        .unwrap();
    assert_eq!(next.1.previous.as_ref(), Some(&first[0].0));
}

#[test]
fn sessions_wait_for_their_observation_epoch() {
    // Lässt sich die Beobachtungs-Epoche nicht versiegeln (hier: ein Ref
    // genau dort, wo die Seals beginnen), versiegelt der Witness auch keine
    // Session — sonst verschmölze die Epoche mit der nächsten, und das
    // Fenster der Session reichte über den Commit hinaus.
    let f = Fixture::new();
    f.keygen();
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    git(&f.root, &["update-ref", "refs/minds/evidence", head.trim()]);
    let root = canonical_root(&f);
    let mut writer = f.writer();
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    writer.observe_into(stream(), observer);
    append(&mut writer);
    let err = writer.checkpoint_now(None).unwrap_err();
    assert!(
        err.to_string().contains("observation epoch not sealed"),
        "{err}"
    );
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    assert!(store.list_seals().unwrap().is_empty());
    assert_eq!(writer.journal.read(&key()).unwrap().events.len(), 1);
    // Ist das Hindernis weg, geht beides — die Epoche zuerst.
    git(&f.root, &["update-ref", "-d", "refs/minds/evidence"]);
    writer.next_step();
    writer.checkpoint_now(None).unwrap();
    assert_eq!(store.list_seals().unwrap().len(), 2);
    assert!(writer.journal.read(&key()).unwrap().events.is_empty());
}

#[test]
fn the_checkpoint_mark_never_precedes_a_session_event() {
    // Springt die Wanduhr zurück, liegt die Grenze trotzdem hinter dem
    // letzten Event der Sessions, die der Checkpoint versiegelt.
    let f = Fixture::new();
    f.keygen();
    let root = canonical_root(&f);
    let mut writer = f.writer();
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    writer.observe_into(stream(), observer);
    let future = (
        "2099-01-01T00:00:00Z".to_owned(),
        4_070_908_800_000_000_000u64,
    );
    writer.hook("claude-code", None, payload(), future).unwrap();
    writer.checkpoint_now(None).unwrap();
    let store = minds_store::InRepoStore::open(&f.root).unwrap();
    let seals: Vec<minds_core::evidence::Seal> = store
        .list_seals()
        .unwrap()
        .iter()
        .map(|id| {
            minds_core::evidence::Seal::parse(&store.seal_text(id).unwrap().unwrap()).unwrap()
        })
        .collect();
    let last = |scope: &str| {
        seals
            .iter()
            .find(|s| s.scope == scope)
            .unwrap()
            .last_event_at
            .parse::<jiff::Timestamp>()
            .unwrap()
    };
    assert!(last("witness-fs/v1") >= last("witness/v1"));
}

/// Die gespeicherten Observation-Objekte der `witness-fs/v1`-Seals.
fn stored_objects(root: &Path) -> Vec<minds_core::observation::Observations> {
    use minds_core::evidence::{Seal, SealOutcome};
    let store = minds_store::InRepoStore::open(root).unwrap();
    store
        .list_seals()
        .unwrap()
        .into_iter()
        .filter_map(|id| {
            let seal = Seal::parse(&store.seal_text(&id).unwrap().unwrap()).unwrap();
            match seal.outcome {
                SealOutcome::ObservationsStored { observations } => Some(
                    store
                        .get_observations(&observations.parse().unwrap())
                        .unwrap()
                        .unwrap(),
                ),
                _ => None,
            }
        })
        .collect()
}

#[test]
fn observations_carry_epoch_start() {
    // EA-08a: Das Objekt der ersten Epoche beginnt mit `witness.start`, das
    // der nächsten mit ihrem ersten Event nach dem Seal.
    let f = Fixture::new();
    f.keygen();
    let root = canonical_root(&f);
    let mut writer = f.writer();
    writer
        .lifecycle(&stream(), "witness.start", serde_json::json!({}))
        .unwrap();
    let started = writer.journal.read(&stream()).unwrap().events[0].at.clone();
    let observer = observer::Observer::detached(&root, Box::new(observer::Blake3)).unwrap();
    writer.observe_into(stream(), observer);
    fs::write(root.join("a.rs"), "a").unwrap();
    writer.observer.as_ref().unwrap().note(&root.join("a.rs"));
    append(&mut writer);
    writer.checkpoint_now(None).unwrap();
    let objects = stored_objects(&f.root);
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].schema, 2);
    assert_eq!(objects[0].started_at.as_deref(), Some(started.as_str()));
    assert!(
        objects[0].started_at() <= objects[0].first_at.as_deref().map(|at| at.parse().unwrap())
    );

    // Die zweite Epoche beginnt mit dem ersten Event nach dem Seal — hier
    // der Beobachtung selbst.
    fs::write(root.join("b.rs"), "b").unwrap();
    writer.observer.as_ref().unwrap().note(&root.join("b.rs"));
    writer.next_step();
    writer.observe(true).unwrap();
    let first = writer.journal.read(&stream()).unwrap().events[0].clone();
    assert_eq!(first.raw_kind, "fs.observed");
    writer.next_step();
    writer.checkpoint_now(None).unwrap();
    let objects = stored_objects(&f.root);
    let second = objects
        .iter()
        .find(|o| o.observations.iter().any(|o| o.path == "b.rs"))
        .unwrap();
    assert_eq!(second.started_at.as_deref(), Some(first.at.as_str()));
    assert!(second.started_at() > objects.iter().find(|o| o != &second).unwrap().started_at());
}

#[test]
fn witness_clock_survives_restart_monotonic() {
    // Der alte Lauf stempelte eine Stunde „in der Zukunft" — dann springt
    // die Wanduhr über den Neustart zurück. Der neue Lauf stempelt trotzdem
    // nie davor: weder im Journal noch als Text, noch beim Schlüssel seines
    // Streams.
    let f = Fixture::new();
    let ahead = clock::now().1 + 3_600_000_000_000;
    let ahead_text = clock::rfc3339_from_nanos(ahead);
    {
        let mut writer = f.writer();
        writer
            .hook("claude-code", None, payload(), (ahead_text.clone(), ahead))
            .unwrap();
        // Ohne Journal bleibt nur die Uhr-Datei als Gedächtnis.
        writer.journal.discard(&key()).unwrap();
    }
    let meta = fs::symlink_metadata(f.home.join(witness_clock::CLOCK_FILE)).unwrap();
    assert_eq!(meta.mode() & 0o777, 0o600);
    let mut writer = f.writer();
    assert!(writer.clock.peek().unwrap() > ahead);
    let restarted = SessionKey::new("witness", "2").unwrap();
    for _ in 0..3 {
        writer
            .lifecycle(&restarted, "witness.start", serde_json::json!({}))
            .unwrap();
    }
    let events = writer.journal.read(&restarted).unwrap().events;
    let mut previous = (ahead, ahead_text.parse::<jiff::Timestamp>().unwrap());
    for event in events {
        let text = event.at.parse::<jiff::Timestamp>().unwrap();
        assert!(event.at_nanos > previous.0, "{}", event.at);
        assert!(text > previous.1, "{}", event.at);
        assert_eq!(event.at, clock::rfc3339_from_nanos(event.at_nanos));
        previous = (event.at_nanos, text);
    }

    // Auch ein Witness von vor EA-08a (ohne Uhr-Datei) stempelt nicht vor
    // seinem Journal.
    let f = Fixture::new();
    {
        let mut writer = f.writer();
        writer
            .hook("claude-code", None, payload(), (ahead_text.clone(), ahead))
            .unwrap();
    }
    fs::remove_file(f.home.join(witness_clock::CLOCK_FILE)).unwrap();
    let mut writer = f.writer();
    writer
        .lifecycle(&stream(), "witness.start", serde_json::json!({}))
        .unwrap();
    assert!(writer.journal.read(&stream()).unwrap().events[0].at_nanos > ahead);
}

#[test]
fn the_watcher_is_armed_before_witness_start() {
    // `started_at` der ersten Epoche ist der Stempel von `witness.start`; der
    // Leser verankert an ihm. Also muss der Watcher schon laufen, wenn er
    // vergeben wird — sonst läge ein Schreibzugriff im Spalt ungesehen
    // innerhalb einer „vollständigen" Kette.
    let f = Fixture::new();
    let mut writer = f.writer();
    let journal = Journal::at(f.home.join("journal"));
    let armed = writer
        .start_stream(&stream(), serde_json::json!({}), || {
            journal.read(&stream()).unwrap().events.len()
        })
        .unwrap();
    assert_eq!(armed, 0, "witness.start was appended before arming");
    let events = writer.journal.read(&stream()).unwrap().events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].raw_kind, "witness.start");
    assert_eq!(writer.stream, Some(stream()));
}

#[test]
fn a_missing_clock_file_still_never_stamps_before_the_ledger() {
    // Versiegelte Journale sind verworfen — das Ledger bleibt. Fehlt die
    // Uhr-Datei (Upgrade, Verlust), hebt es die Marke an.
    let f = Fixture::new();
    let sealed = clock::now().1 + 3_600_000_000_000;
    let mut ledger = private_append(&f.home.join("ledger")).unwrap();
    writeln!(ledger, "b3-x witness-fs/v1 kaputt").unwrap();
    writeln!(
        ledger,
        "b3-y witness-fs/v1 {}",
        clock::rfc3339_from_nanos(sealed)
    )
    .unwrap();
    let mut writer = f.writer();
    assert!(!f.home.join(witness_clock::CLOCK_FILE).exists());
    writer
        .lifecycle(&stream(), "witness.start", serde_json::json!({}))
        .unwrap();
    assert!(writer.journal.read(&stream()).unwrap().events[0].at_nanos > sealed);
}

#[test]
fn witness_refuses_corrupt_clock_state() {
    let f = Fixture::new();
    // Keine Datei: erster Lauf, kein Fehler.
    assert!(Writer::open(&f.home, load(&f.home).unwrap(), false).is_ok());
    let path = f.home.join(witness_clock::CLOCK_FILE);
    for bad in [&b""[..], b"garbage\n", b"12", b"-5\n", b"0012\n"] {
        fs::write(&path, bad).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let err = Writer::open(&f.home, load(&f.home).unwrap(), false)
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "corrupt witness clock state", "{bad:?}");
        // Nie still zurückgesetzt: Die Datei bleibt, wie sie war.
        assert_eq!(fs::read(&path).unwrap(), bad);
    }
    // Ein Verzeichnis an ihrer Stelle ebenso.
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let err = Writer::open(&f.home, load(&f.home).unwrap(), false)
        .err()
        .unwrap();
    assert_eq!(err.to_string(), "corrupt witness clock state");
}

#[test]
fn witness_refuses_home_inside_repo() {
    let f = Fixture::new();
    assert!(refuse_home_in_repo(&f.home, &load(&f.home).unwrap()).is_ok());
    let mut config = load(&f.home).unwrap();
    config.repo_root = f._dir.path().canonicalize().unwrap();
    let err = refuse_home_in_repo(&f.home, &config).unwrap_err();
    assert!(err.to_string().contains("inside the repository"), "{err}");
    // Das Home ist die Repo-Wurzel selbst.
    config.repo_root = f.home.canonicalize().unwrap();
    assert!(refuse_home_in_repo(&f.home, &config).is_err());
    // Über einen Symlink benannt: zählt das Ziel.
    let alias = f._dir.path().join("alias");
    std::os::unix::fs::symlink(&f.home, &alias).unwrap();
    let mut config = load(&f.home).unwrap();
    config.repo_root = f._dir.path().canonicalize().unwrap();
    assert!(refuse_home_in_repo(&alias, &config).is_err());
}
