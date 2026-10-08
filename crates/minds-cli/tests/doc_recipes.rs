//! EA-21: Die Rezepte „Checking witnessed evidence by hand" in
//! `docs/verification-guide.md` laufen hier wortwörtlich — mit echtem `git`,
//! `b3sum` und `ssh-keygen` gegen ein Repository mit Witness-Seal,
//! Beobachtungs-Seal, signiertem Intent und CI-Gegenzeichnung. Ändert sich
//! ein Ref-Layout, ein Hash-Kontext oder ein Namespace, wird die Doku hier
//! rot, nicht erst beim Prüfer.
//!
//! Jede Prüfung des Rezepts druckt bei Erfolg eine `ok:`-Zeile; der Test
//! zählt sie — auf stdout **und** stderr, denn ein Prüfer am Terminal sieht
//! beides —, und er prüft die Ausgabe der Befehle ohne `ok:`-Zeile. Die
//! Gegenproben (je ein `#[test]` unten) zeigen, dass die Rezepte auch etwas
//! **finden**, und dass ein präpariertes Repository weder `ok:`-Zeilen
//! fälschen noch durch einen Symlink schreiben kann.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use minds_core::ContentHash;
use minds_core::evidence::{SCOPE_WITNESS_FS_V1, SCOPE_WITNESS_V1, Seal, SealOutcome};
use minds_core::first_sight::FirstSight;
use minds_core::intent_anchor::IntentSource;
use minds_core::observation::{Observation, Observations};
use minds_store::{ContextStore, InRepoStore};

const BEGIN: &str = "<!-- BEGIN recipe: witnessed-evidence";
const END: &str = "<!-- END recipe: witnessed-evidence -->";

const WITNESS: &str = "minds-witness@build-07";
const APPROVER: &str = "anna@example.org";
const CI: &str = "ci@gitlab.example.org";

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn hash(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

/// Ob `b3sum` da ist. In CI ist es Pflicht (`ci.yml` installiert es) —
/// dort darf der Test nie still entfallen; lokal ohne `b3sum` meldet er,
/// dass er nicht lief.
fn b3sum_available() -> bool {
    let found = Command::new("b3sum")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    if !found {
        assert!(
            std::env::var_os("CI").is_none(),
            "b3sum is required in CI for the documentation recipes"
        );
        eprintln!("skipped: b3sum not on PATH — the verification-guide recipes did not run");
    }
    found
}

/// Ein Ed25519-Schlüssel mit Kommentar `principal`; liefert den
/// öffentlichen Teil.
fn keygen(path: &Path, principal: &str) -> String {
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", principal, "-f"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read_to_string(path.with_extension("pub"))
        .unwrap()
        .trim()
        .to_string()
}

/// Die Shell-Blöcke zwischen den Rezept-Marken, in Reihenfolge.
fn recipe_script() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/verification-guide.md");
    let doc = std::fs::read_to_string(path).unwrap();
    assert_eq!(doc.matches(BEGIN).count(), 1, "one recipe region");
    let start = doc.find(BEGIN).unwrap();
    let end = doc.find(END).expect("recipe end marker");
    let region = &doc[start..end];
    let mut script = String::new();
    let mut inside = false;
    for line in region.lines() {
        // Außerhalb eines Blocks zählt jede Zaun-Zeile, auch eingerückte
        // oder mit `~~~`: Ein anders ausgezeichneter Block liefe hier nie —
        // und „jeder Befehl läuft im Test" wäre still falsch.
        let fence = line.trim();
        if !inside && (fence.starts_with("```") || fence.starts_with("~~~")) {
            assert_eq!(
                line.trim_end(),
                "```sh",
                "only unindented ```sh blocks belong in the recipe region"
            );
        }
        match (inside, line.trim_end()) {
            (false, "```sh") => inside = true,
            (true, "```") => {
                inside = false;
                script.push('\n');
            }
            (true, line) => {
                script.push_str(line);
                script.push('\n');
            }
            _ => {}
        }
    }
    assert!(!inside, "unterminated sh block in the recipe region");
    script
}

/// So viele Prüfungen enthalten die Rezepte — jede mit genau einer
/// `ok:`-Zeile.
fn expected_checks() -> usize {
    recipe_script().matches("echo \"ok: ").count()
}

/// Die Zahl, die der Fließtext der Doku nennt („steps 0–5 print N") — eine
/// neue Prüfung ohne angepassten Text fällt hier auf.
fn documented_checks() -> usize {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/verification-guide.md");
    let doc = std::fs::read_to_string(path).unwrap();
    let marker = "steps 0–5 print ";
    let start = doc
        .find(marker)
        .expect("the guide states the number of checks")
        + marker.len();
    doc[start..]
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|digits| digits.parse().ok())
        .expect("a number after the marker")
}

/// Ein Lauf der Rezepte.
struct Run {
    out: Output,
    ok: Vec<String>,
}

impl Run {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.out.stdout).into_owned()
    }

    fn has(&self, prefix: &str) -> bool {
        self.ok.iter().any(|line| line.starts_with(prefix))
    }

    /// Keine Zeile auf stdout oder stderr enthält `ok:`, außer den echten
    /// `ok:`-Zeilen des Rezepts — sonst könnte ein Repository eine davon
    /// vortäuschen (Steuerzeichen, Zeilenumbruch am Terminal,
    /// Fehlermeldung).
    fn assert_no_forged_ok(&self) {
        // Jede echte Zeile wortgleich; `$PRINCIPAL` nur durch einen der
        // Principals, die die Fixture (oder eine Gegenprobe) vergibt.
        let principals = [WITNESS, APPROVER, CI, "dev@example.org"];
        let real: Vec<String> = recipe_script()
            .lines()
            .filter_map(|line| line.split_once("echo \"ok: "))
            .flat_map(|(_, rest)| {
                let text = format!("ok: {}", rest.split('"').next().unwrap());
                if text.contains("$PRINCIPAL") {
                    principals
                        .iter()
                        .map(|principal| text.replace("$PRINCIPAL", principal))
                        .collect()
                } else {
                    vec![text]
                }
            })
            .collect();
        for stream in [&self.out.stdout, &self.out.stderr] {
            for line in String::from_utf8_lossy(stream).lines() {
                if line.contains("ok:") {
                    assert!(
                        real.iter().any(|check| line == check),
                        "a line the recipe did not print as a check: {line:?}"
                    );
                }
            }
        }
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    /// Der `witness/v1`-Seal (64 Hex).
    seal: String,
    /// Der `witness-fs/v1`-Seal (64 Hex).
    fs_seal: String,
    /// Das Beobachtungsobjekt des `witness-fs/v1`-Seals (64 Hex).
    observations: String,
    /// Der Intent-Anker (64 Hex).
    intent: String,
    witness_public: String,
    /// Der Schlüssel des Approvers (für Gegenproben unter fremdem Namespace).
    intent_key: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        assert!(minds_attest::ssh_keygen_available());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "--template="]);
        git(&root, &["config", "user.email", "human@example.invalid"]);
        git(&root, &["config", "user.name", "Recipe Test"]);
        git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        );
        // Schlüssel außerhalb des Repositorys — je Rolle einer.
        let keys = dir.path().join("keys");
        std::fs::create_dir(&keys).unwrap();
        let witness_key = keys.join("witness");
        let intent_key = keys.join("approver");
        let anchor_key = keys.join("ci");
        let witness_public = keygen(&witness_key, WITNESS);
        let intent_public = keygen(&intent_key, APPROVER);
        let anchor_public = keygen(&anchor_key, CI);

        let store = InRepoStore::open(&root).unwrap();
        let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();

        // 1/2: eine bezeugte Session, ihr Seal unter `minds-witness`.
        let session = minds_core::Session::new(
            minds_core::Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            minds_core::Model {
                provider: "test".into(),
                id: "test".into(),
            },
            minds_core::Intent::default(),
        );
        let session = store
            .put(&pipeline.redact_session(session).unwrap())
            .unwrap()
            .id();
        let mut seal = Seal::parse(include_str!("fixtures/checkpoint-core/epoch-0.seal")).unwrap();
        seal.scope = SCOPE_WITNESS_V1.into();
        seal.agent = "witness".into();
        seal.outcome = SealOutcome::Stored {
            session: session.to_string(),
        };
        let seal = put_signed_seal(&store, &seal, &witness_key);

        // 3: die Beobachtungen des Datei-Beobachters, ihr Seal unter
        // `minds-witness`.
        let observations = pipeline
            .redact_observations(Observations::new(
                "2026-10-02T09:00:00Z",
                vec![Observation {
                    seq: 1,
                    at: "2026-10-02T10:00:05Z".into(),
                    path: "src/retry.rs".into(),
                    content: Some(hash(b"fn retry() {}\n")),
                    reason: None,
                }],
            ))
            .unwrap();
        let observations = store.put_observations(&observations).unwrap();
        let fs_seal = Seal {
            root: hash(b"root"),
            agent: "witness".into(),
            scope: SCOPE_WITNESS_FS_V1.into(),
            first_seq: 0,
            last_seq: 1,
            events: 2,
            gaps: 0,
            pre_chain: 0,
            outcome: SealOutcome::ObservationsStored {
                observations: observations.to_string(),
            },
            previous: None,
            last_event_at: "2026-10-02T10:00:31Z".into(),
        };
        let fs_seal = put_signed_seal(&store, &fs_seal, &witness_key);

        // 4: ein Intent, vom Approver unter `minds-intent` signiert.
        let requirement = b"# Retry\n\nRetry failed uploads three times.\n";
        let intent = pipeline
            .redact_intent(
                IntentSource::File {
                    path: "docs/retry.md".into(),
                    blob: "0123456789abcdef0123456789abcdef01234567".into(),
                },
                vec!["src/**".into()],
                requirement.to_vec(),
            )
            .unwrap();
        let intent_id = store.put_intent(&intent).unwrap();
        let signature =
            minds_attest::ssh_sign_ns(intent.text(), &intent_key, minds_attest::NS_INTENT).unwrap();
        store.put_intent_signature(&intent_id, &signature).unwrap();

        // 5: die Gegenzeichnung der CI unter `minds-anchor`.
        let first_sight = FirstSight {
            seal: seal.clone(),
            project: "group/project".into(),
            pipeline: 4711,
            at: "2026-10-02T11:00:00Z".into(),
        };
        let signature = minds_attest::ssh_sign_ns(
            &first_sight.to_text().unwrap(),
            &anchor_key,
            minds_attest::NS_ANCHOR,
        )
        .unwrap();
        assert!(store.put_first_sight(&first_sight, &signature).unwrap());

        // Je Rolle eine Trust-Datei, jede Zeile auf ihren Namespace
        // beschränkt — wie in der Doku.
        let fixture = Self {
            seal: seal.hex().to_string(),
            fs_seal: fs_seal.hex().to_string(),
            observations: observations.hex().to_string(),
            intent: intent_id.hex().to_string(),
            witness_public,
            intent_key,
            dir,
        };
        fixture.write_witness_signers(&format!(
            "{WITNESS} namespaces=\"minds-witness\" {}\n",
            fixture.witness_public
        ));
        fixture.write(
            "intent_signers",
            &format!("{APPROVER} namespaces=\"minds-intent\" {intent_public}\n"),
        );
        fixture.write(
            "anchor_signers",
            &format!("{CI} namespaces=\"minds-anchor\" {anchor_public}\n"),
        );
        fixture
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.path(name), content).unwrap();
    }

    fn write_witness_signers(&self, content: &str) {
        self.write("witness_signers", content);
    }

    /// Legt unter `refs/minds/evidence/<id>` einen Seal-Text ab, an jeder
    /// Prüfung des Stores vorbei — so, wie ein Agent mit Schreibzugriff auf
    /// das Repository es per Git-Plumbing könnte. Liefert die Id
    /// (`derive_key` über den Text, also für Schritt 1 und 3 „passend").
    fn put_raw_seal(&self, seal: &str) -> String {
        let root = self.root();
        let id: String = blake3::derive_key("minds/evidence/v1/seal", seal.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let path = self.path("raw.seal");
        std::fs::write(&path, seal).unwrap();
        let blob = git(&root, &["hash-object", "-w", path.to_str().unwrap()]);
        let listing_path = self.path("raw-tree.txt");
        std::fs::write(&listing_path, format!("100644 blob {blob}\tseal\n")).unwrap();
        let tree = Command::new("git")
            .current_dir(&root)
            .arg("mktree")
            .stdin(std::fs::File::open(&listing_path).unwrap())
            .output()
            .unwrap();
        assert!(tree.status.success(), "{}", text(&tree));
        let tree = String::from_utf8(tree.stdout).unwrap().trim().to_string();
        let commit = git(&root, &["commit-tree", &tree, "-m", "raw"]);
        git(
            &root,
            &["update-ref", &format!("refs/minds/evidence/{id}"), &commit],
        );
        id
    }

    /// Ersetzt im Baum unter `reference` den Eintrag `name` durch `bytes` —
    /// Ref-Name und übrige Einträge (Signaturen) bleiben.
    fn replace_blob(&self, reference: &str, name: &str, bytes: &[u8]) {
        let root = self.root();
        let path = self.path("replacement");
        std::fs::write(&path, bytes).unwrap();
        let blob = git(&root, &["hash-object", "-w", path.to_str().unwrap()]);
        let listing = git(&root, &["ls-tree", reference]);
        let mut replaced = false;
        let entries: String = listing
            .lines()
            .map(|line| {
                let (meta, entry) = line.split_once('\t').unwrap();
                if entry == name {
                    replaced = true;
                    format!("100644 blob {blob}\t{entry}\n")
                } else {
                    format!("{meta}\t{entry}\n")
                }
            })
            .collect();
        assert!(replaced, "{name} not in {reference}");
        let listing_path = self.path("tree.txt");
        std::fs::write(&listing_path, entries).unwrap();
        let tree = Command::new("git")
            .current_dir(&root)
            .arg("mktree")
            .stdin(std::fs::File::open(&listing_path).unwrap())
            .output()
            .unwrap();
        assert!(tree.status.success(), "{}", text(&tree));
        let tree = String::from_utf8(tree.stdout).unwrap().trim().to_string();
        let commit = git(&root, &["commit-tree", &tree, "-m", "edited"]);
        git(&root, &["update-ref", reference, &commit]);
    }

    /// Führt die Rezepte aus der Doku aus — gegen einen frischen
    /// `git clone --mirror`, wie die Doku es verlangt. Bewusst **im** Klon
    /// gestartet, wie ein Prüfer, der vorher hineingewechselt ist: Das Rezept
    /// muss selbst in ein leeres Verzeichnis wechseln.
    fn run_recipes(&self, seal: &str) -> Run {
        self.run_recipes_with(seal, &self.fs_seal)
    }

    fn run_recipes_with(&self, seal: &str, fs_seal: &str) -> Run {
        let mirror = self.path("audit.git");
        if mirror.exists() {
            std::fs::remove_dir_all(&mirror).unwrap();
        }
        git(
            self.dir.path(),
            &[
                "clone",
                "-q",
                "--mirror",
                "--no-local",
                self.root().to_str().unwrap(),
                mirror.to_str().unwrap(),
            ],
        );
        let out = Command::new("sh")
            .arg("-u")
            .arg("-c")
            .arg(recipe_script())
            .current_dir(self.root())
            .env("REPO", &mirror)
            // `mktemp -d` des Rezepts landet im Test-Verzeichnis, nicht im
            // System-`$TMPDIR` — es räumt mit der Fixture ab.
            .env("TMPDIR", self.dir.path())
            .env("WITNESS_SIGNERS", self.path("witness_signers"))
            .env("INTENT_SIGNERS", self.path("intent_signers"))
            .env("ANCHOR_SIGNERS", self.path("anchor_signers"))
            .env("SEAL", seal)
            .env("FS_SEAL", fs_seal)
            .env("INTENT", &self.intent)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        // Beide Ströme: Am Terminal sieht der Prüfer auch stderr.
        let both = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let ok = both
            .lines()
            .filter(|line| line.starts_with("ok: "))
            .map(str::to_string)
            .collect();
        Run { out, ok }
    }
}

fn put_signed_seal(store: &InRepoStore, seal: &Seal, key: &Path) -> ContentHash {
    let text = seal.to_text().unwrap();
    let id = store.put_seal(&text).unwrap();
    let signature = minds_attest::ssh_sign_ns(&text, key, minds_attest::NS_WITNESS).unwrap();
    store.put_seal_signature(&id, &signature).unwrap();
    id
}

#[test]
fn recipes_pass_on_witnessed_evidence() {
    if !b3sum_available() {
        return;
    }
    let fixture = Fixture::new();
    let run = fixture.run_recipes(&fixture.seal);
    assert!(run.out.status.success(), "{}", text(&run.out));
    assert_eq!(expected_checks(), documented_checks());
    assert_eq!(run.ok.len(), expected_checks(), "{:#?}", run.ok);
    for line in [
        "ok: working in a fresh scratch directory",
        "ok: WITNESS_SIGNERS holds minds-witness keys only",
        "ok: INTENT_SIGNERS holds minds-intent keys only",
        "ok: ANCHOR_SIGNERS holds minds-anchor keys only",
        "ok: seal id matches its text",
        "ok: a witness/v1 seal",
        "ok: witness signature by minds-witness@build-07",
        "ok: file-system seal id matches its text",
        "ok: file-system seal",
        "ok: file-system seal signed by minds-witness@build-07",
        "ok: observations are the ones the witness sealed",
        "ok: intent id matches its anchor",
        "ok: anchor names this snapshot",
        "ok: intent approved by anna@example.org",
        "ok: countersignature names this seal",
        "ok: countersigned by ci@gitlab.example.org",
    ] {
        assert!(
            run.ok.iter().any(|seen| seen == line),
            "{line} missing: {:#?}",
            run.ok
        );
    }
    // Die Befehle ohne `ok:`-Zeile: Auflistung, Scope/Session, CI-Zeit.
    let stdout = run.stdout();
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.contains(&fixture.seal.as_str()), "{stdout}");
    assert!(lines.contains(&fixture.fs_seal.as_str()), "{stdout}");
    assert!(lines.contains(&"scope=witness/v1"), "{stdout}");
    assert!(
        lines.iter().any(|line| line.starts_with("session=b3-")),
        "{stdout}"
    );
    assert!(lines.contains(&"at=2026-10-02T11:00:00Z"), "{stdout}");
    assert!(lines.contains(&"project=group/project"), "{stdout}");
    run.assert_no_forged_ok();
}

#[test]
fn an_unrestricted_key_in_the_witness_file_is_flagged() {
    if !b3sum_available() {
        return;
    }
    // Ein Entwickler-Schlüssel ohne `namespaces=` — der Agent kann ihn über
    // `ssh-agent` nutzen und seinen eigenen Seal unter `minds-witness`
    // signieren. `ssh-keygen -Y verify` nimmt das hin; die Trust-Datei-Prüfung
    // aus Schritt 0 darf es nicht.
    let fixture = Fixture::new();
    let developer = fixture.path("developer");
    let developer_public = keygen(&developer, "dev@example.org");
    let store = InRepoStore::open(fixture.root()).unwrap();
    let mut own = Seal::parse(include_str!("fixtures/checkpoint-core/epoch-0.seal")).unwrap();
    own.scope = SCOPE_WITNESS_V1.into();
    own.first_seq = 7;
    let own = put_signed_seal(&store, &own, &developer);
    fixture.write_witness_signers(&format!(
        "{WITNESS} namespaces=\"minds-witness\" {}\ndev@example.org {developer_public}\n",
        fixture.witness_public
    ));
    let run = fixture.run_recipes(own.hex());
    assert!(!run.has("ok: WITNESS_SIGNERS"), "{:#?}", run.ok);
    // Was `ssh-keygen` allein sagt — genau deshalb gibt es Schritt 0.
    assert!(run.has("ok: witness signature by dev@example.org"));
    assert!(run.ok.len() < expected_checks());
}

#[test]
fn a_comment_imitating_the_restriction_is_flagged() {
    if !b3sum_available() {
        return;
    }
    // Den Kommentar seines `.pub` bestimmt, wer den Schlüssel erzeugt —
    // eine Teilstring-Prüfung auf `namespaces="minds-witness" ` fiele darauf
    // herein, die feldweise Prüfung nicht.
    let fixture = Fixture::new();
    let developer = fixture.path("developer");
    let developer_public = keygen(&developer, "namespaces=\"minds-witness\" x");
    fixture.write_witness_signers(&format!(
        "{WITNESS} namespaces=\"minds-witness\" {}\ndev@example.org {developer_public}\n",
        fixture.witness_public
    ));
    let run = fixture.run_recipes(&fixture.seal);
    assert!(!run.has("ok: WITNESS_SIGNERS"), "{:#?}", run.ok);
}

#[test]
fn a_file_system_seal_passed_as_seal_is_flagged() {
    if !b3sum_available() {
        return;
    }
    // Auch der fs-Seal trägt eine gültige Witness-Signatur — dass er kein
    // `witness/v1`-Seal ist, muss Schritt 1 sagen.
    let fixture = Fixture::new();
    let run = fixture.run_recipes(&fixture.fs_seal);
    assert!(!run.has("ok: a witness/v1 seal"), "{:#?}", run.ok);
    assert!(run.has("ok: witness signature by"));
}

#[test]
fn a_witness_key_under_another_namespace_fails_the_recipe() {
    if !b3sum_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write_witness_signers(&format!(
        "{WITNESS} namespaces=\"minds\" {}\n",
        fixture.witness_public
    ));
    let run = fixture.run_recipes(&fixture.seal);
    assert!(!run.has("ok: WITNESS_SIGNERS"));
    assert!(!run.has("ok: witness signature"));
    assert!(!run.has("ok: file-system seal signed"));
    // Die übrigen Rollen sind davon unberührt.
    assert!(run.has("ok: intent approved"));
    assert!(run.has("ok: countersigned by"));
}

#[test]
fn an_edited_seal_fails_the_recipe() {
    if !b3sum_available() {
        return;
    }
    let fixture = Fixture::new();
    let reference = format!("refs/minds/evidence/{}", fixture.seal);
    let original = git(
        &fixture.root(),
        &["cat-file", "blob", &format!("{reference}:seal")],
    );
    let edited = format!("{}\n", original.replace("gaps=0", "gaps=1"));
    assert_ne!(edited.trim(), original);
    fixture.replace_blob(&reference, "seal", edited.as_bytes());
    let run = fixture.run_recipes(&fixture.seal);
    assert!(!run.has("ok: seal id matches its text"));
    assert!(!run.has("ok: witness signature"));
}

#[test]
fn exchanged_observations_fail_the_recipe() {
    if !b3sum_available() {
        return;
    }
    let fixture = Fixture::new();
    let reference = format!("refs/minds/observations/{}", fixture.observations);
    fixture.replace_blob(&reference, "observations.json", b"{\"schema\":2}");
    let run = fixture.run_recipes(&fixture.seal);
    assert!(!run.has("ok: observations are the ones the witness sealed"));
    assert!(run.has("ok: file-system seal signed"));
}

#[test]
fn an_edited_intent_snapshot_fails_the_recipe() {
    if !b3sum_available() {
        return;
    }
    let fixture = Fixture::new();
    let reference = format!("refs/minds/intents/{}", fixture.intent);
    fixture.replace_blob(&reference, "snapshot", b"# Retry\n\nRetry forever.\n");
    let run = fixture.run_recipes(&fixture.seal);
    assert!(!run.has("ok: anchor names this snapshot"));
    // Anker und Signatur sind unverändert — nur der Snapshot passt nicht.
    assert!(run.has("ok: intent approved"));
}

#[cfg(unix)]
#[test]
fn a_symlink_in_the_audited_clone_is_never_written_through() {
    if !b3sum_available() {
        return;
    }
    let fixture = Fixture::new();
    let target = fixture.path("outside");
    std::os::unix::fs::symlink(&target, fixture.root().join("seal.txt")).unwrap();
    let run = fixture.run_recipes(&fixture.seal);
    assert!(run.out.status.success(), "{}", text(&run.out));
    assert!(!target.exists(), "the recipe wrote through a symlink");
    assert_eq!(run.ok.len(), expected_checks());
}

#[test]
fn an_empty_or_cert_authority_trust_file_is_flagged() {
    if !b3sum_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write_witness_signers("# no keys yet\n\n");
    let anchor = std::fs::read_to_string(fixture.path("anchor_signers")).unwrap();
    // So, wie OpenSSH die Option liest: nach dem Principal, vor dem
    // Namespace.
    let ca = anchor.replacen("namespaces=", "cert-authority,namespaces=", 1);
    assert_ne!(ca, anchor);
    fixture.write("anchor_signers", &ca);
    let run = fixture.run_recipes(&fixture.seal);
    assert!(!run.has("ok: WITNESS_SIGNERS"), "{:#?}", run.ok);
    assert!(!run.has("ok: ANCHOR_SIGNERS"), "{:#?}", run.ok);
    assert!(run.has("ok: INTENT_SIGNERS"));
}

#[test]
fn an_intent_signed_under_another_namespace_fails_the_recipe() {
    if !b3sum_available() {
        return;
    }
    let fixture = Fixture::new();
    let reference = format!("refs/minds/intents/{}", fixture.intent);
    let anchor = git(
        &fixture.root(),
        &["cat-file", "blob", &format!("{reference}:anchor")],
    );
    // `git` schneidet das Zeilenende ab; signiert sind die gespeicherten Bytes.
    let signature =
        minds_attest::ssh_sign_ns(&format!("{anchor}\n"), &fixture.intent_key, "minds").unwrap();
    fixture.replace_blob(&reference, "anchor.sig", signature.as_bytes());
    let run = fixture.run_recipes(&fixture.seal);
    assert!(!run.has("ok: intent approved"), "{:#?}", run.ok);
    assert!(run.has("ok: intent id matches its anchor"));
    // Kontrolle: dieselben Bytes unter `minds-intent` bestehen — der Befund
    // oben liegt am Namespace, nicht an den Bytes.
    let signature = minds_attest::ssh_sign_ns(
        &format!("{anchor}\n"),
        &fixture.intent_key,
        minds_attest::NS_INTENT,
    )
    .unwrap();
    fixture.replace_blob(&reference, "anchor.sig", signature.as_bytes());
    assert!(
        fixture
            .run_recipes(&fixture.seal)
            .has("ok: intent approved")
    );
}

#[test]
fn a_crafted_session_line_cannot_forge_ok_lines() {
    if !b3sum_available() {
        return;
    }
    // Ein selbst gebauter `witness-fs/v1`-Seal ohne Signatur: Id und Scope
    // stimmen, Signatur und Beobachtungen nicht. Seine `session=`-Zeilen
    // würden — unbesehen als Argument an `git cat-file` gereicht — in Gits
    // Fehlermeldung zu zwei gefälschten `ok:`-Zeilen, genau anstelle der
    // beiden fehlenden.
    let fixture = Fixture::new();
    // Ein beliebiger Ref vor dem `:` — erst dann sucht Git den Pfad und
    // zitiert ihn samt Zeilenumbrüchen in der Fehlermeldung.
    let head = git(&fixture.root(), &["rev-parse", "HEAD"]);
    git(
        &fixture.root(),
        &["update-ref", "refs/minds/observations/x", &head],
    );
    let forged = fixture.put_raw_seal(
        "minds-seal-v1\n\
         scope=witness-fs/v1\n\
         session=b3-x:\n\
         session=b3-ok: file-system seal signed by minds-witness@build-07\n\
         session=b3-ok: observations are the ones the witness sealed\n\
         session=b3-z\n",
    );
    let run = fixture.run_recipes_with(&fixture.seal, &forged);
    assert!(run.has("ok: file-system seal id matches its text"));
    assert!(!run.has("ok: file-system seal signed"), "{:#?}", run.ok);
    assert!(!run.has("ok: observations are"), "{:#?}", run.ok);
    assert_eq!(run.ok.len(), expected_checks() - 2, "{:#?}", run.ok);
    run.assert_no_forged_ok();
}

#[test]
fn a_form_feed_line_is_not_skipped_as_a_comment() {
    if !b3sum_available() {
        return;
    }
    // OpenSSH überspringt nur Leerzeichen, Tab und CR vor `#` — eine Zeile
    // mit Seitenvorschub ist für `ssh-keygen` ein Schlüssel (Principal `*`,
    // unbeschränkt). Schritt 0 muss sie mitzählen.
    let fixture = Fixture::new();
    let developer = fixture.path("developer");
    let developer_public = keygen(&developer, "dev");
    fixture.write_witness_signers(&format!(
        "{WITNESS} namespaces=\"minds-witness\" {}\n\x0c#,* {developer_public}\n",
        fixture.witness_public
    ));
    let run = fixture.run_recipes(&fixture.seal);
    assert!(!run.has("ok: WITNESS_SIGNERS"), "{:#?}", run.ok);
}

#[test]
fn a_line_wrapped_ok_cannot_be_painted_onto_the_terminal() {
    if !b3sum_available() {
        return;
    }
    // Ein selbst gebauter `witness/v1`-Seal ohne Signatur. Seine
    // `session=`-Zeile trägt nach vielen Leerzeichen den Text der fehlenden
    // Signatur-Zeile — auf einem 80 Spalten breiten Terminal stünde er genau
    // dort, wo die echte `ok:`-Zeile fehlt.
    let fixture = Fixture::new();
    let forged = fixture.put_raw_seal(&format!(
        "minds-seal-v1\nscope=witness/v1\nsession={}ok: witness signature by {WITNESS}\n",
        " ".repeat(72)
    ));
    let run = fixture.run_recipes(&forged);
    assert!(run.has("ok: a witness/v1 seal"));
    assert!(!run.has("ok: witness signature"), "{:#?}", run.ok);
    run.assert_no_forged_ok();
}

#[test]
fn a_missing_observation_object_is_not_the_empty_hash() {
    if !b3sum_available() {
        return;
    }
    // BLAKE3 über null Bytes — so hieße die leere Datei, die `> file`
    // anlegt, wenn `cat-file` scheitert. Ohne Objekt darf es kein `ok:`
    // geben.
    let fixture = Fixture::new();
    let empty: String = blake3::hash(b"")
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let forged = fixture.put_raw_seal(&format!(
        "minds-seal-v1\nscope=witness-fs/v1\nsession=b3-{empty}\n"
    ));
    let run = fixture.run_recipes_with(&fixture.seal, &forged);
    assert!(run.has("ok: file-system seal id matches its text"));
    assert!(!run.has("ok: observations are"), "{:#?}", run.ok);
}

#[test]
fn a_countersignature_moved_to_another_seal_fails_the_recipe() {
    if !b3sum_available() {
        return;
    }
    // Die gültige Gegenzeichnung von Seal A unter den Namen von Seal B
    // gehängt: Signatur gültig, aber sie nennt A.
    let fixture = Fixture::new();
    let store = InRepoStore::open(fixture.root()).unwrap();
    let mut other = Seal::parse(include_str!("fixtures/checkpoint-core/epoch-0.seal")).unwrap();
    other.scope = SCOPE_WITNESS_V1.into();
    other.first_seq = 9;
    let other = put_signed_seal(&store, &other, &fixture.path("keys/witness"));
    git(
        &fixture.root(),
        &[
            "update-ref",
            &format!("refs/minds/anchors/first-sight/{}", other.hex()),
            &format!("refs/minds/anchors/first-sight/{}", fixture.seal),
        ],
    );
    let run = fixture.run_recipes(other.hex());
    assert!(run.has("ok: witness signature"));
    assert!(run.has("ok: countersigned by"));
    assert!(
        !run.has("ok: countersignature names this seal"),
        "{:#?}",
        run.ok
    );
}

#[test]
fn a_crafted_seal_id_cannot_forge_ok_lines() {
    if !b3sum_available() {
        return;
    }
    // Eine Id aus einem unsignierten Dokument, kopiert statt getippt: Ohne
    // die Prüfung in Schritt 0 zitierte Gits Fehlermeldung den Pfad nach
    // dem `:` samt Zeilenumbruch — eine gefälschte `ok:`-Zeile.
    let fixture = Fixture::new();
    let crafted = format!("{}:x\nok: witness signature by {WITNESS}\ny", fixture.seal);
    let run = fixture.run_recipes(&crafted);
    assert!(!run.has("ok: seal id matches its text"), "{:#?}", run.ok);
    assert!(!run.has("ok: witness signature"), "{:#?}", run.ok);
    run.assert_no_forged_ok();
}
