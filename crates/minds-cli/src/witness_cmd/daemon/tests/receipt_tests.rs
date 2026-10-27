//! Spec S3-MVP §6.3 (Commit 7): `minds verify --receipt` gegen Quittungen,
//! die der echte Hook (`minds-witness-hook`, pre- und post-receive) auf
//! einem lokalen Server ablegt — s3_24, s3_26, s3_27.
//!
//! Die Evidence entsteht wie in `bind_tests` (getrennte Observations von
//! Hand, Bindung durch den Witness); der Server erkennt den Witness-Schlüssel
//! über seine Signer-Datei an (`require_separated_witness = false`, der
//! Principal ist `minds-witness@…`). `Verified` heißt hier also nicht
//! „getrennter Witness nachgewiesen“ — das zeigt erst der Isolations-Job
//! (s3_23, Commit 8); hier geht es um die Prüfung der Quittung.
//!
//! Das Hook-Binary liegt neben dem Testprogramm im Zielverzeichnis; `cargo
//! test --workspace` baut es mit. Fehlt es, scheitert der Test mit klarer
//! Meldung, statt still zu überspringen.

use super::bind_tests::{PROJECT, bindings, commit, evidence, pinned, run};
use super::*;
use crate::receipt_cmd::anchor::Sources;
use crate::receipt_cmd::{Options, verify};

/// Das Hook-Binary des Server-Witness.
fn hook_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("minds-witness-hook");
    assert!(
        bin.is_file(),
        "{} fehlt — `cargo build -p minds-witness-git --bin minds-witness-hook` \
         (cargo test --workspace baut es mit)",
        bin.display()
    );
    bin
}

fn which(name: &str) -> PathBuf {
    ["/usr/bin", "/bin", "/usr/local/bin", "/opt/homebrew/bin"]
        .into_iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
        .and_then(|p| fs::canonicalize(p).ok())
        .unwrap_or_else(|| panic!("{name} nicht gefunden"))
}

/// Witness, Arbeits-Repo mit Remote `origin` und ein Server mit Hook.
struct World {
    f: Fixture,
    dir: tempfile::TempDir,
    server: PathBuf,
    key: String,
    /// Konfiguration des Hooks und die Zeile des eigenen Witness.
    config: serde_json::Value,
    line: String,
}

const SEED: [u8; 32] = [0x42; 32];

fn world(threshold: u8) -> World {
    let f = pinned(Some(PROJECT));
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("server.git");
    git(
        dir.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            server.to_str().unwrap(),
        ],
    );
    git(&server, &["config", "receive.fsckObjects", "true"]);
    // Für den Admin-Weg (`commit-tree` im Server).
    git(&server, &["config", "user.name", "Admin"]);
    git(&server, &["config", "user.email", "admin@example.invalid"]);
    let line = crate::witness_cmd::existing_signers_line(&f.home)
        .unwrap()
        .unwrap();
    let principal = line.split_whitespace().next().unwrap().to_owned();
    let signers = dir.path().join("allowed_signers");
    fs::write(&signers, format!("{line}\n")).unwrap();
    let seed = dir.path().join("seed.hex");
    fs::write(&seed, format!("{}\n", "42".repeat(32))).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&seed, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let policy = minds_witness_core::Policy {
        coverage_threshold_percent: threshold,
        ..minds_witness_core::Policy::default()
    };
    let config = serde_json::json!({
        "operation": "test",
        "project": PROJECT,
        "signers_file": signers,
        "allowed_witness_principals": [principal],
        "require_separated_witness": false,
        "ssh_keygen": which("ssh-keygen"),
        "git": which("git"),
        "policy": policy,
        "seed_file": seed,
        "key_valid_from": "2026-01-01T00:00:00Z",
        "repository": server,
        "state_dir": dir.path().join("state"),
    });
    let config_path = dir.path().join("hook.json");
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let out = Command::new(hook_bin())
        .arg("install")
        .arg(&config_path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "install: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let url = format!("file://{}", server.display());
    git(&f.root, &["remote", "add", "origin", &url]);
    let key = minds_witness_core::Signer::from_seed(
        SEED,
        minds_witness_core::Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
    )
    .public_key()
    .as_str()
    .to_owned();
    World {
        f,
        dir,
        server,
        key,
        config,
        line,
    }
}

impl World {
    /// Die **heutige** Signer-Datei des Servers: der eigene Witness, dazu
    /// optional ein zweiter Schlüssel (Principal `minds-witness@second`).
    /// Der Snapshot früherer Quittungen bleibt davon unberührt.
    fn trust_second(&self, second: Option<&Path>) {
        let mut lines = format!("{}\n", self.line);
        let mut config = self.config.clone();
        if let Some(key) = second {
            let public = fs::read_to_string(key.with_extension("pub")).unwrap();
            let mut fields = public.split_whitespace();
            let (algo, material) = (fields.next().unwrap(), fields.next().unwrap());
            lines.push_str(&format!(
                "minds-witness@second namespaces=\"minds-witness\" {algo} {material}\n"
            ));
            config["allowed_witness_principals"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!("minds-witness@second"));
        }
        fs::write(self.dir.path().join("allowed_signers"), lines).unwrap();
        fs::write(
            self.dir.path().join("hook.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();
    }

    /// Bindet `c` (falls Evidence da ist), überträgt die Evidence und pusht
    /// den Code nach `main`.
    fn push(&self, c: &str) {
        let _ = run(&self.f, c, PROJECT, false);
        self.send_evidence(c);
        git(
            &self.f.root,
            &[
                "push",
                "-q",
                "--no-verify",
                "origin",
                &format!("{c}:refs/heads/main"),
            ],
        );
    }

    fn send_evidence(&self, c: &str) {
        let view = crate::bind_cmd::open_view(&self.f.root.join(".git")).unwrap();
        let refs = crate::evidence_cmd::evidence_refs(&view, &[c.to_owned()]);
        let sent = crate::evidence_cmd::transfer(&self.f.root, "origin", &refs);
        assert!(
            !matches!(sent, crate::evidence_cmd::Transfer::Failed(_)),
            "{sent:?}"
        );
    }

    /// Admin-Weg am Server vorbei an der Prüfung des Pushes (die je Commit
    /// höchstens vier Bindungen annimmt): holt Bindungen, Seals und
    /// Observations aus dem Arbeits-Repo direkt in den Server.
    fn admin_fetch(&self, c: &str) {
        git(
            &self.server,
            &[
                "fetch",
                "-q",
                self.f.root.to_str().unwrap(),
                &format!("+refs/minds/bindings/{c}/*:refs/minds/bindings/{c}/*"),
                "+refs/minds/evidence/*:refs/minds/evidence/*",
                "+refs/minds/observations/*:refs/minds/observations/*",
            ],
        );
    }

    /// Ein frischer Clone des Servers (nur Branches, keine minds-Refs).
    fn clone(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        let url = format!("file://{}", self.server.display());
        git(
            self.dir.path(),
            &["clone", "-q", &url, path.to_str().unwrap()],
        );
        assert_eq!(git(&path, &["for-each-ref", "refs/minds/"]), "");
        git(&path, &["config", "user.name", "Prüfer"]);
        git(&path, &["config", "user.email", "pruefer@example.invalid"]);
        path
    }

    fn server_ref(&self, name: &str) -> String {
        git(&self.server, &["rev-parse", name]).trim().to_owned()
    }

    fn server_refs(&self, prefix: &str) -> Vec<String> {
        git(
            &self.server,
            &["for-each-ref", "--format=%(refname)", prefix],
        )
        .lines()
        .map(str::to_owned)
        .collect()
    }

    /// Admin-Weg: legt `name` auf einen Commit ohne Eltern mit genau
    /// `file` = `bytes` (wie die Ablage es täte).
    fn plant(&self, name: &str, file: &str, bytes: &[u8]) {
        let blob = git_stdin(&self.server, &["hash-object", "-w", "--stdin"], bytes);
        let tree = git_stdin(
            &self.server,
            &["mktree"],
            format!("100644 blob {}\t{file}\n", blob.trim()).as_bytes(),
        );
        let commit = git(&self.server, &["commit-tree", tree.trim(), "-m", "planted"]);
        git(&self.server, &["update-ref", name, commit.trim()]);
    }

    fn receipt(&self, c: &str) -> (String, Vec<u8>) {
        let names = self.server_refs(&format!("refs/minds/witness/receipts/{c}/"));
        assert_eq!(names.len(), 1, "{names:?}");
        let bytes = Command::new("git")
            .arg("-C")
            .arg(&self.server)
            .args(["cat-file", "blob", &format!("{}:receipt.json", names[0])])
            .output()
            .unwrap()
            .stdout;
        (names[0].clone(), bytes)
    }
}

fn git_stdin(dir: &Path, args: &[&str], input: &[u8]) -> String {
    use std::io::Write as _;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

/// `minds verify --receipt` im Prozess, mit `key` als `--receipt-key`.
fn check(root: &Path, rev: Option<&str>, remote: Option<&str>, key: &str) -> (u8, String) {
    check_with(root, rev, remote, key, None)
}

fn check_with(
    root: &Path,
    rev: Option<&str>,
    remote: Option<&str>,
    key: &str,
    state: Option<&str>,
) -> (u8, String) {
    let opts = Options {
        rev,
        remote,
        key: Some(key),
        key_file: None,
        require_state: state,
        require_protected_anchor: false,
    };
    let sources = Sources {
        key: Some(key),
        key_file: None,
        default_file: Path::new("/nonexistent/minds/receipt-keys"),
        config: None,
    };
    let mut out = Vec::new();
    let code = verify(root, &opts, &sources, &mut out);
    (code, String::from_utf8(out).unwrap())
}

fn has(out: &str, needle: &str) {
    assert!(out.contains(needle), "fehlt {needle:?} in:\n{out}");
}

/// Ein Commit mit `a` und `b`, beide beobachtet, gepusht.
fn verified(w: &World) -> String {
    let c = commit(
        &w.f,
        &[("src/a.rs", Some(b"x1")), ("src/b.rs", Some(b"y1"))],
        "change a and b",
    );
    evidence(
        &w.f,
        &[("src/a.rs", Some(b"x1")), ("src/b.rs", Some(b"y1"))],
        true,
    );
    w.push(&c);
    c
}

#[test]
fn s3_24_verify_receipt_independent() {
    let w = world(100);
    let c = verified(&w);
    let clone = w.clone("clone");
    let rev = Some(c.as_str());

    // Gültig: Exit 0, jede Zeile des Blocks.
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 0, "{out}");
    has(&out, &format!("für refs/heads/main {} (", &c[..12]));
    has(
        &out,
        "Vertrauensanker: ausdrücklich übergeben (--receipt-key)",
    );
    has(
        &out,
        &format!(
            "Signatur: gültig (Schlüssel {}, gültig seit 2026-01-01T00:00:00Z)",
            w.key
        ),
    );
    has(
        &out,
        "Hash stimmt mit der signierten Quittung (require_separated_witness = false",
    );
    has(
        &out,
        "Evidence: vom Remote origin beschafft (1 Bindung, 1 Seal, 1 Observations)",
    );
    has(&out, "Nachrechnung: stimmt (seal.hash, Abdeckung, Policy)");
    has(
        &out,
        "Zustand: Verified, 2/2 Dateien nachgewiesen (100,0 %), Policy erfüllt (observe, Schwelle 100 %)",
    );

    // Das erwartete Projekt (`minds.project`): gleich → genannt, anders →
    // Exit 1.
    has(
        &out,
        "Projekt: git.test/project-42 (minds.project nicht gesetzt",
    );
    git(&clone, &["config", "minds.project", "git.test/project-7"]);
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 1, "{out}");
    has(
        &out,
        "Projekt: git.test/project-42 — erwartet git.test/project-7",
    );
    git(&clone, &["config", "minds.project", PROJECT]);
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 0, "{out}");
    has(&out, "Projekt: git.test/project-42 (wie minds.project)");

    // Falscher Anker: Exit 1.
    let other = format!("ed25519:{}", "11".repeat(32));
    let (code, out) = check(&clone, rev, Some("origin"), &other);
    assert_eq!(code, 1, "{out}");
    has(&out, "ist nicht der Vertrauensanker");
    assert!(!out.contains("Zustand: Verified"), "{out}");

    // Manipulierte Quittung (kanonisch, aber nicht signiert): Exit 1.
    let (name, bytes) = w.receipt(&c);
    let original = w.server_ref(&name);
    let mut forged = minds_witness_core::Receipt::from_bytes(&bytes).unwrap();
    forged.coverage.paths_in_diff += 1;
    w.plant(&name, "receipt.json", &forged.to_bytes());
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 1, "{out}");
    has(&out, "Signatur: ungültig (Signatur passt nicht)");
    git(&w.server, &["update-ref", &name, &original]);

    // Snapshot: manipuliert → Exit 1, fehlend → Exit 3, nie als /1.
    let receipt = minds_witness_core::Receipt::from_bytes(&bytes).unwrap();
    let trust = receipt.trust.clone().unwrap();
    let trust_ref = format!(
        "refs/minds/witness/trust/{}",
        trust.as_str().split_once(':').unwrap().1
    );
    let trust_oid = w.server_ref(&trust_ref);
    let other_snapshot = minds_witness_core::TrustSnapshot::new(
        vec!["minds-witness-user@other".into()],
        &minds_witness_core::Policy::default(),
        PROJECT,
        true,
        "",
    );
    w.plant(&trust_ref, "trust.json", &other_snapshot.to_bytes());
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 1, "{out}");
    has(&out, "passt nicht zur signierten Quittung (Hash weicht ab)");
    git(&w.server, &["update-ref", "-d", &trust_ref]);
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 3, "{out}");
    has(&out, &format!("fehlt: Trust-Snapshot {}", trust.as_str()));
    has(&out, "ohne Trust-Snapshot kein Rückfall auf Schema /1");
    assert!(!out.contains("Vertrauenskontext: nicht enthalten"), "{out}");
    git(&w.server, &["update-ref", &trust_ref, &trust_oid]);

    // Fehlende Schlüsselliste: Exit 3.
    let keys_oid = w.server_ref("refs/minds/witness/keys");
    git(&w.server, &["update-ref", "-d", "refs/minds/witness/keys"]);
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 3, "{out}");
    has(&out, "fehlt: Schlüsselliste");
    git(
        &w.server,
        &["update-ref", "refs/minds/witness/keys", &keys_oid],
    );

    // Nachrechnung weicht ab: Observations unter ihrem Namen ersetzt →
    // verworfen, Abdeckung 0/2 statt 2/2. Exit 1.
    let obs = w.server_refs("refs/minds/observations/");
    assert_eq!(obs.len(), 1, "{obs:?}");
    let obs_oid = w.server_ref(&obs[0]);
    w.plant(&obs[0], "observations.json", b"{}");
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 1, "{out}");
    has(&out, "Nachrechnung: weicht ab (Abdeckung)");
    git(&w.server, &["update-ref", &obs[0], &obs_oid]);

    // Wieder hergestellt: wieder gültig — die Tracking-Refs folgen dem
    // Server in beide Richtungen.
    assert_eq!(check(&clone, rev, Some("origin"), &w.key).0, 0);

    // Eine `/1`-Quittung: Signatur geprüft, keine Nachrechnung, Exit 3.
    let c2 = commit(&w.f, &[("src/c.rs", Some(b"c1"))], "change c");
    git(
        &w.f.root,
        &[
            "push",
            "-q",
            "--no-verify",
            "origin",
            &format!("{c2}:refs/heads/main"),
        ],
    );
    let mut body = receipt.body();
    body.commit = minds_witness_core::CommitId::parse(&c2).unwrap();
    body.schema = minds_witness_core::RECEIPT_SCHEMA.to_owned();
    body.trust = None;
    let signer = minds_witness_core::Signer::from_seed(
        SEED,
        minds_witness_core::Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
    );
    let v1 = body.sign(&signer);
    let id = v1.id();
    let v1_ref = format!(
        "refs/minds/witness/receipts/{c2}/{}",
        id.as_str().split_once(':').unwrap().1
    );
    let existing = w.server_refs(&format!("refs/minds/witness/receipts/{c2}/"));
    for name in existing {
        git(&w.server, &["update-ref", "-d", &name]);
    }
    w.plant(&v1_ref, "receipt.json", &v1.to_bytes());
    git(&clone, &["fetch", "-q", "origin"]);
    let (code, out) = check(&clone, Some(c2.as_str()), Some("origin"), &w.key);
    assert_eq!(code, 3, "{out}");
    has(&out, "Signatur: gültig");
    has(&out, "Vertrauenskontext: nicht enthalten (Schema /1)");
    has(&out, "(laut Quittung, nicht nachgerechnet)");
}

#[test]
fn s3_26_verify_from_fresh_clone() {
    let w = world(100);
    let c = verified(&w);
    let clone = w.clone("fresh");
    let rev = Some(c.as_str());

    // Ohne `--remote` liest verify nur lokale Refs: keine Quittung.
    let (code, out) = check(&clone, rev, None, &w.key);
    assert_eq!(code, 3, "{out}");
    has(&out, &format!("keine Quittung für Commit {}", &c[..12]));

    // Mit `--remote`: alles selbst beschafft, nur in Tracking-Refs.
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 0, "{out}");
    let local = git(
        &clone,
        &["for-each-ref", "--format=%(refname)", "refs/minds/"],
    );
    assert!(
        local
            .lines()
            .all(|n| n.starts_with("refs/minds/remotes/origin/")),
        "{local}"
    );
    for part in [
        "witness/keys",
        "witness/receipts/",
        "witness/trust/",
        "bindings/",
        "evidence/",
        "observations/",
    ] {
        assert!(
            local.contains(&format!("refs/minds/remotes/origin/{part}")),
            "{part}: {local}"
        );
    }

    // Ein lokaler Ref gleichen Namens mit anderem Inhalt wird mit
    // `--remote` nicht gelesen.
    let obs = w.server_refs("refs/minds/observations/");
    let empty_tree = git(&clone, &["hash-object", "-t", "tree", "-w", "/dev/null"]);
    let junk = git(&clone, &["commit-tree", empty_tree.trim(), "-m", "junk"]);
    git(&clone, &["update-ref", &obs[0], junk.trim()]);
    // Ebenso lokale Witness-Refs: Quittung, Schlüsselliste, Snapshot.
    let receipt_name = w.receipt(&c).0;
    for name in w
        .server_refs("refs/minds/witness/")
        .into_iter()
        .chain([receipt_name])
    {
        git(&clone, &["update-ref", &name, junk.trim()]);
    }
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 0, "{out}");
    // Ohne `--remote` gelten genau diese lokalen Refs — und taugen nicht.
    let (code, out) = check(&clone, rev, None, &w.key);
    assert_eq!(code, 4, "{out}");

    // Seal am Server entfernt: Exit 3, einzeln genannt.
    let seals = w.server_refs("refs/minds/evidence/");
    assert_eq!(seals.len(), 1, "{seals:?}");
    let seal_oid = w.server_ref(&seals[0]);
    git(&w.server, &["update-ref", "-d", &seals[0]]);
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 3, "{out}");
    has(
        &out,
        &format!("fehlt: Seal b3-{}", seals[0].rsplit('/').next().unwrap()),
    );
    git(&w.server, &["update-ref", &seals[0], &seal_oid]);

    // Mehr Bindungs-Kandidaten, als der Server je liest: nicht
    // nachrechenbar (Admin-Weg, der Push ließe sie nicht zu).
    for i in 0..65u32 {
        git(
            &w.server,
            &[
                "update-ref",
                &format!("refs/minds/bindings/{c}/{i:064x}"),
                &seal_oid,
            ],
        );
    }
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 3, "{out}");
    has(&out, "nicht nachrechenbar (mehr als 64 Bindungen)");
    for i in 0..65u32 {
        git(
            &w.server,
            &[
                "update-ref",
                "-d",
                &format!("refs/minds/bindings/{c}/{i:064x}"),
            ],
        );
    }
    assert_eq!(check(&clone, rev, Some("origin"), &w.key).0, 0);

    // Observations am Server entfernt (Admin-Weg): Exit 3, einzeln genannt.
    let obs_oid = w.server_ref(&obs[0]);
    git(&w.server, &["update-ref", "-d", &obs[0]]);
    // Lässt sich der veraltete Tracking-Ref nicht löschen (eine fremde
    // Sperrdatei), rechnet verify nicht mit ihm: Exit 4.
    let lock = clone.join(".git").join(format!(
        "{}.lock",
        obs[0].replacen("refs/minds/", "refs/minds/remotes/origin/", 1)
    ));
    fs::write(&lock, "").unwrap();
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 4, "{out}");
    has(&out, "stale tracking ref not removable");
    fs::remove_file(&lock).unwrap();
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 3, "{out}");
    let hex = obs[0].rsplit('/').next().unwrap();
    has(&out, &format!("fehlt: Observations b3-{hex}"));
    assert!(!out.contains("Zustand: Verified"), "{out}");
    git(&w.server, &["update-ref", &obs[0], &obs_oid]);

    // Ein Commit, den der Server nicht hat: feste Meldung, Exit 3.
    let missing = "ab".repeat(20);
    let (code, out) = check(&clone, Some(&missing), Some("origin"), &w.key);
    assert_eq!(code, 3, "{out}");
    has(&out, &format!("Commit {} nicht abrufbar", &missing[..12]));

    // Ein Commit, der lokal fehlt, aber am Server liegt: über die Branches.
    let later = commit(&w.f, &[("src/d.rs", Some(b"d1"))], "change d");
    w.push(&later);
    let (code, out) = check(&clone, Some(&later), Some("origin"), &w.key);
    assert!(code == 2 || code == 0, "{out}");
    has(&out, "Nachrechnung: stimmt");

    // Remote als URL: Exit 4.
    let url = format!("file://{}", w.server.display());
    let (code, out) = check(&clone, rev, Some(&url), &w.key);
    assert_eq!(code, 4, "{out}");
    has(&out, "ist kein benannter Remote");
}

/// Erzeugt eine Bindung von Hand: signiert mit `key` (privat) unter
/// `minds-witness`, abgelegt im Arbeits-Repo. Liefert den Ref-Namen.
fn craft_binding(f: &Fixture, c: &str, seals: &[String], key: &Path) -> String {
    let public = fs::read_to_string(key.with_extension("pub")).unwrap();
    let mut fields = public.split_whitespace();
    let line = format!("{} {}", fields.next().unwrap(), fields.next().unwrap());
    let mut seals = seals.to_vec();
    seals.sort();
    let b = minds_witness_git::binding::CommitBinding::new(c, &line, PROJECT, seals, 32).unwrap();
    let text = String::from_utf8(b.to_bytes()).unwrap();
    let sig = minds_attest::ssh_sign_ns(&text, key, minds_attest::NS_WITNESS).unwrap();
    let blob = git_stdin(&f.root, &["hash-object", "-w", "--stdin"], text.as_bytes());
    let sig_blob = git_stdin(&f.root, &["hash-object", "-w", "--stdin"], sig.as_bytes());
    let tree = git_stdin(
        &f.root,
        &["mktree"],
        format!(
            "100644 blob {}\tbinding\n100644 blob {}\tbinding.sig\n",
            blob.trim(),
            sig_blob.trim()
        )
        .as_bytes(),
    );
    let commit = git(&f.root, &["commit-tree", tree.trim(), "-m", "binding"]);
    git(&f.root, &["update-ref", &b.ref_name(), commit.trim()]);
    b.ref_name()
}

fn foreign_key(dir: &Path) -> PathBuf {
    let key = dir.join("foreign_ed25519");
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "foreign", "-f"])
        .arg(&key)
        .status()
        .unwrap();
    assert!(status.success());
    key
}

#[test]
fn s3_27_late_binding_does_not_break_verify() {
    let w = world(100);
    let c = commit(
        &w.f,
        &[("src/a.rs", Some(b"x1")), ("src/b.rs", Some(b"y1"))],
        "change a and b",
    );
    evidence(&w.f, &[("src/a.rs", Some(b"x1"))], true);
    w.push(&c);
    let clone = w.clone("clone");
    let rev = Some(c.as_str());
    let (before, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(before, 2, "{out}");
    has(&out, "Zustand: Partial, 1/2 Dateien nachgewiesen (50,0 %)");

    // Später: eine zweite, gültige Bindung (a und b) für denselben Commit.
    evidence(&w.f, &[("src/b.rs", Some(b"y1"))], true);
    assert!(run(&w.f, &c, PROJECT, false).unwrap().starts_with("bound "));
    assert_eq!(bindings(&w.f, &c).len(), 2);
    w.send_evidence(&c);
    assert_eq!(w.server_refs(&format!("refs/minds/bindings/{c}/")).len(), 2);
    let (after, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(after, before, "Exit wie vorher:\n{out}");
    has(&out, "Nachrechnung: stimmt");
    has(&out, "(1 Bindung, 1 Seal, 1 Observations)");
    has(&out, "Zustand: Partial, 1/2");

    // Die Bindung der Quittung fehlt am Server: keine passende Teilmenge →
    // Exit 1, nie Verified.
    let receipt = minds_witness_core::Receipt::from_bytes(&w.receipt(&c).1).unwrap();
    let first = bindings(&w.f, &c)
        .into_iter()
        .find(|b| {
            minds_witness_git::inspector::binding_hash("accepted_bindings", &c, vec![b.id()])
                == *receipt.seal.hash.as_ref().unwrap()
        })
        .unwrap();
    let first_oid = w.server_ref(&first.ref_name());
    git(&w.server, &["update-ref", "-d", &first.ref_name()]);
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 1, "{out}");
    has(&out, "keine passende Teilmenge");
    assert!(!out.contains("Zustand: Verified"), "{out}");
    has(&out, "Zustand: nicht festgestellt");
    git(&w.server, &["update-ref", &first.ref_name(), &first_oid]);

    // Heute erkennt der Server einen zweiten Schlüssel an; dessen Bindung
    // nimmt er an. Für die Quittung zählt sie nicht: Sie ist unter dem
    // signierten historischen Snapshot nicht anerkannt.
    let second = foreign_key(w.dir.path());
    w.trust_second(Some(&second));
    let seal_b = evidence(&w.f, &[("src/b.rs", Some(b"y1"))], true);
    craft_binding(&w.f, &c, std::slice::from_ref(&seal_b), &second);
    w.send_evidence(&c);
    assert_eq!(w.server_refs(&format!("refs/minds/bindings/{c}/")).len(), 3);
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, before, "{out}");
    has(&out, "Nachrechnung: stimmt");
    // Dazu eigene Bindungen bis genau acht akzeptierte (zwei gibt es). Der
    // Server nimmt per Push höchstens vier je Commit an; der Rest kommt über
    // den Admin-Weg (die Regel in `verify` gilt unabhängig davon).
    let own = w.f.home.join("key/witness_ed25519");
    for _ in 0..6 {
        let s = evidence(&w.f, &[("src/a.rs", Some(b"x1"))], true);
        craft_binding(&w.f, &c, &[s], &own);
    }
    w.admin_fetch(&c);
    assert_eq!(w.server_refs(&format!("refs/minds/bindings/{c}/")).len(), 9);
    // Unter der heutigen Signer-Datei wären es neun (nicht nachrechenbar),
    // unter dem Snapshot sind es acht.
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, before, "{out}");
    has(&out, "Nachrechnung: stimmt");

    // Eine neunte anerkannte: nicht nachrechenbar, Exit 3, nie Verified.
    let s = evidence(&w.f, &[("src/a.rs", Some(b"x1"))], true);
    craft_binding(&w.f, &c, &[s], &own);
    w.admin_fetch(&c);
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 3, "{out}");
    has(
        &out,
        "nicht nachrechenbar (mehr als 8 akzeptierte Bindungen)",
    );
    has(&out, "Zustand: nicht festgestellt");
}

/// Klasse „rejected“: Die einzige Bindung trug beim Push einen Schlüssel,
/// den der Server inzwischen nicht mehr anerkannte. Später kommt eine
/// gültige hinzu: Hinweiszeile, Exit nach Zustand. Ebenso ein Push ganz
/// ohne Evidence, der später Bindungen bekommt.
#[test]
fn s3_27_rejected_receipt_with_later_candidates() {
    let w = world(100);
    let c = commit(&w.f, &[("src/a.rs", Some(b"x1"))], "change a");
    let seal = evidence(&w.f, &[("src/a.rs", Some(b"x1"))], true);
    let second = foreign_key(w.dir.path());
    w.trust_second(Some(&second));
    craft_binding(&w.f, &c, std::slice::from_ref(&seal), &second);
    w.send_evidence(&c);
    w.trust_second(None);
    git(
        &w.f.root,
        &[
            "push",
            "-q",
            "--no-verify",
            "origin",
            &format!("{c}:refs/heads/main"),
        ],
    );
    let clone = w.clone("clone");
    let rev = Some(c.as_str());
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 2, "{out}");
    has(&out, "Nachrechnung: stimmt");
    has(&out, "Zustand: Invalid");
    assert!(!out.contains("nicht nachrechenbar"), "{out}");

    assert!(run(&w.f, &c, PROJECT, false).unwrap().starts_with("bound "));
    w.send_evidence(&c);
    let (code, out) = check(&clone, rev, Some("origin"), &w.key);
    assert_eq!(code, 2, "{out}");
    has(
        &out,
        "seal.hash nicht nachrechenbar (Kandidatenmenge seither erweitert)",
    );
    has(&out, "Zustand: Invalid");
    // Mit `--require-state Invalid` genügt der Zustand.
    let (code, out) = check_with(&clone, rev, Some("origin"), &w.key, Some("Invalid"));
    assert_eq!(code, 0, "{out}");

    // Ohne Evidence gepusht, später gebunden: dasselbe.
    let c2 = commit(&w.f, &[("src/e.rs", Some(b"e1"))], "change e");
    git(
        &w.f.root,
        &[
            "push",
            "-q",
            "--no-verify",
            "origin",
            &format!("{c2}:refs/heads/main"),
        ],
    );
    evidence(&w.f, &[("src/e.rs", Some(b"e1"))], true);
    assert!(
        run(&w.f, &c2, PROJECT, false)
            .unwrap()
            .starts_with("bound ")
    );
    w.send_evidence(&c2);
    let (code, out) = check(&clone, Some(c2.as_str()), Some("origin"), &w.key);
    assert_eq!(code, 2, "{out}");
    has(
        &out,
        "seal.hash nicht nachrechenbar (Kandidatenmenge seither erweitert)",
    );
    has(&out, "Zustand: Unverified, 0/1");
}
