//! EA-08: `minds verify` zieht Datei-Beobachtungen nur aus Witness-signierten
//! `witness-fs/v1`-Seals heran — eine vom Agenten abgelegte Beobachtung
//! erklärt keine Zeile.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use minds_core::ContentHash;
use minds_core::evidence::{SCOPE_WITNESS_FS_V1, SCOPE_WITNESS_V1, Seal, SealOutcome};
use minds_core::observation::{Observation, Observations};
use minds_store::{ContextStore, InRepoStore};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");
const PRINCIPAL: &str = "minds-witness@host";

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
}

fn hash(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

struct Fixture {
    dir: tempfile::TempDir,
    store: InRepoStore,
    key: PathBuf,
    signers: PathBuf,
}

impl Fixture {
    /// Ein Commit, den ein Shell-Befehl schrieb: Die verknüpfte Session
    /// claimt nichts — ohne Zeugen ist die Zeile unerklärt.
    fn new() -> Self {
        Self::with_session_scope(Some(SCOPE_WITNESS_V1))
    }

    /// `witnessed`: Der Witness hat die Session versiegelt (`witness/v1`,
    /// unter `minds-witness` signiert) — sonst ein lokaler Bereich.
    fn with_session_scope(witnessed: Option<&str>) -> Self {
        assert!(minds_attest::ssh_keygen_available());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "--template="]);
        git(root, &["config", "user.email", "human@example.invalid"]);
        git(root, &["config", "user.name", "Witness Test"]);
        let store = InRepoStore::open(root).unwrap();
        let mut session = minds_core::Session::new(
            minds_core::Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            minds_core::Model {
                provider: "test".into(),
                id: "test".into(),
            },
            minds_core::Intent {
                request: "generate".into(),
                ..minds_core::Intent::default()
            },
        );
        session.turns.push(minds_core::Turn {
            role: minds_core::Role::Assistant,
            text: String::new(),
            tool_calls: Vec::new(),
            parent: None,
            at: Some("2026-10-02T10:00:00Z".into()),
        });
        let session = minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact_session(session)
            .unwrap();
        let id = store.put(&session).unwrap().id();
        std::fs::write(root.join("generated.rs"), "shell\n").unwrap();
        git(root, &["add", "generated.rs"]);
        git(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                &format!("generated\n\nMinds-Session-Id: {id}"),
            ],
        );
        let key = root.join("id");
        assert!(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        let signers = root.join("allowed_signers");
        let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        std::fs::write(
            &signers,
            format!("{PRINCIPAL} namespaces=\"minds-witness\" {}", public.trim()),
        )
        .unwrap();
        // Der Seal der Session — damit `verify` bis zur Coverage-Zeile kommt
        // und, wenn bezeugt, das Beobachtungsfenster öffnet.
        let mut seal = Seal::parse(include_str!("fixtures/checkpoint-core/epoch-0.seal")).unwrap();
        seal.scope = witnessed.unwrap_or("agent-hooks/v1").into();
        seal.last_event_at = "2026-10-02T10:00:30Z".into();
        seal.outcome = SealOutcome::Stored {
            session: id.to_string(),
        };
        let text = seal.to_text().unwrap();
        let seal_id = store.put_seal(&text).unwrap();
        store.record_session_seal(id, &seal_id).unwrap();
        if witnessed.is_some() {
            let signature =
                minds_attest::ssh_sign_ns(&text, &key, minds_attest::NS_WITNESS).unwrap();
            store.put_seal_signature(&seal_id, &signature).unwrap();
        }
        Self {
            dir,
            store,
            key,
            signers,
        }
    }

    /// Die Beobachtung des Shell-Schreibzugriffs samt Seal, signiert unter
    /// `namespace` (oder gar nicht) — in der Epoche des Checkpoints, der die
    /// Session versiegelte.
    fn observe(&self, namespace: Option<&str>) {
        self.observe_epoch(
            namespace,
            "2026-10-02T10:00:05Z",
            b"shell\n",
            "2026-10-02T10:00:31Z",
        );
    }

    /// Die Epoche vor der Session (der Witness lief schon vorher): der
    /// Anker, den eine vollständige Kette erreicht. Immer dieselben Bytes,
    /// also derselbe Seal.
    fn anchor(&self) -> minds_core::ContentHash {
        let empty = minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact_observations(Observations::new("2026-10-02T08:59:00Z", Vec::new()))
            .unwrap();
        let object = self.store.put_observations(&empty).unwrap();
        let seal = Seal {
            root: hash(b"anchor"),
            agent: "witness".into(),
            scope: SCOPE_WITNESS_FS_V1.into(),
            first_seq: 0,
            last_seq: 0,
            events: 1,
            gaps: 0,
            pre_chain: 0,
            outcome: SealOutcome::ObservationsStored {
                observations: object.to_string(),
            },
            previous: None,
            last_event_at: "2026-10-02T09:00:00Z".into(),
        }
        .to_text()
        .unwrap();
        let id = self.store.put_seal(&seal).unwrap();
        let signature =
            minds_attest::ssh_sign_ns(&seal, &self.key, minds_attest::NS_WITNESS).unwrap();
        self.store.put_seal_signature(&id, &signature).unwrap();
        id
    }

    /// Eine Beobachtungs-Epoche mit einer Beobachtung von `generated.rs`,
    /// verkettet an den Anker.
    fn observe_epoch(&self, namespace: Option<&str>, at: &str, bytes: &[u8], last: &str) {
        let anchor = self.anchor();
        self.put_epoch(
            namespace,
            "2026-10-02T09:00:00.001Z",
            at,
            bytes,
            last,
            Some(anchor),
        );
    }

    /// Die erste Epoche nach einem Witness-Start (kein Vorgänger), die um
    /// `started_at` begann und den Shell-Schreibzugriff sah — signiert vom
    /// Witness.
    fn observe_from_start(&self, started_at: &str) {
        self.put_epoch(
            Some(minds_attest::NS_WITNESS),
            started_at,
            "2026-10-02T10:00:05Z",
            b"shell\n",
            "2026-10-02T10:00:31Z",
            None,
        );
    }

    fn put_epoch(
        &self,
        namespace: Option<&str>,
        started_at: &str,
        at: &str,
        bytes: &[u8],
        last: &str,
        previous: Option<ContentHash>,
    ) {
        let object = minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact_observations(Observations::new(
                started_at,
                vec![Observation {
                    seq: 1,
                    at: at.into(),
                    path: "generated.rs".into(),
                    content: Some(hash(bytes)),
                    reason: None,
                }],
            ))
            .unwrap();
        let object = self.store.put_observations(&object).unwrap();
        let seal = Seal {
            root: hash(b"root"),
            agent: "witness".into(),
            scope: SCOPE_WITNESS_FS_V1.into(),
            first_seq: 0,
            last_seq: 1,
            events: 2,
            gaps: 0,
            pre_chain: 0,
            outcome: SealOutcome::ObservationsStored {
                observations: object.to_string(),
            },
            previous,
            last_event_at: last.into(),
        }
        .to_text()
        .unwrap();
        let id = self.store.put_seal(&seal).unwrap();
        if let Some(namespace) = namespace {
            let signature = minds_attest::ssh_sign_ns(&seal, &self.key, namespace).unwrap();
            self.store.put_seal_signature(&id, &signature).unwrap();
        }
    }

    fn verify(&self, signers: bool) -> String {
        let root = self.dir.path();
        let mut cmd = Command::new(MINDS);
        cmd.current_dir(root)
            .env("HOME", root.join("user-home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["verify", "HEAD"]);
        if signers {
            cmd.arg("--signers").arg(&self.signers);
        }
        text(&cmd.output().unwrap())
    }
}

#[test]
fn verify_uses_only_witness_signed_observations() {
    let f = Fixture::new();
    assert!(f.verify(true).contains("artifact 0/1 lines explained"));
    // Vom Agenten abgelegt: unsigniert, oder mit einem Schlüssel unter dem
    // falschen Namespace — erklärt nichts.
    f.observe(None);
    assert!(f.verify(true).contains("artifact 0/1 lines explained"));
    f.observe(Some(minds_attest::NS_DEFAULT));
    let out = f.verify(true);
    assert!(out.contains("artifact 0/1 lines explained"), "{out}");
}

#[test]
fn verify_uses_observations_only_for_witnessed_sessions() {
    // Eine nur lokal erfasste Session: Ihre Zeitpunkte kann der Agent
    // setzen — sie öffnet kein Beobachtungsfenster, auch nicht für eine
    // echte, signierte Beobachtung.
    let f = Fixture::with_session_scope(None);
    f.observe(Some(minds_attest::NS_WITNESS));
    let out = f.verify(true);
    assert!(out.contains("artifact 0/1 lines explained"), "{out}");
}

#[test]
fn verify_never_takes_the_witness_trust_file_from_the_repository() {
    let f = Fixture::new();
    f.observe(Some(minds_attest::NS_WITNESS));
    // Die Repo-Konfiguration gehört dem Agenten: eine dort benannte
    // Signer-Datei begründet kein Vertrauen in eine Beobachtung.
    git(
        f.dir.path(),
        &[
            "config",
            "gpg.ssh.allowedSignersFile",
            f.signers.to_str().unwrap(),
        ],
    );
    let out = f.verify(false);
    assert!(out.contains("artifact 0/1 lines explained"), "{out}");
}

#[test]
fn verify_requires_a_witness_principal_restricted_to_its_namespace() {
    let f = Fixture::new();
    f.observe(Some(minds_attest::NS_WITNESS));
    // Derselbe Schlüssel, aber unbeschränkt (wie ein Entwickler-Schlüssel):
    // verifiziert unter jedem Namespace — und zählt deshalb nicht.
    let public = std::fs::read_to_string(f.key.with_extension("pub")).unwrap();
    std::fs::write(&f.signers, format!("{PRINCIPAL} {}", public.trim())).unwrap();
    let out = f.verify(true);
    assert!(out.contains("artifact 0/1 lines explained"), "{out}");
}

#[test]
fn verify_ignores_work_continued_after_the_commit() {
    // Committet, dann weitergearbeitet: Die nächste Beobachtungs-Epoche sah
    // `generated.rs` mit anderem Inhalt, Sekunden nach dem bezeugten Bereich.
    // Sie widerspricht dem Commit nicht — das Fenster endet mit der Epoche
    // des Checkpoints.
    let f = Fixture::new();
    f.observe(Some(minds_attest::NS_WITNESS));
    f.observe_epoch(
        Some(minds_attest::NS_WITNESS),
        "2026-10-02T10:00:35Z",
        b"later\n",
        "2026-10-02T10:00:40Z",
    );
    let out = f.verify(true);
    assert!(out.contains("artifact 1/1 lines explained"), "{out}");
}

#[test]
fn verify_explains_a_shell_write_with_a_witness_signed_observation() {
    let f = Fixture::new();
    f.observe(Some(minds_attest::NS_WITNESS));
    let out = f.verify(true);
    assert!(out.contains("artifact 1/1 lines explained"), "{out}");
    // Ohne allowed_signers ist die Signatur nicht prüfbar: fail-closed.
    let out = f.verify(false);
    assert!(out.contains("artifact 0/1 lines explained"), "{out}");
}

#[test]
fn verify_uses_the_first_epoch_after_a_witness_start_that_began_before_the_session() {
    // EA-08a: Der Witness startete um 09:59:00, die Session beginnt um 10:00
    // (Fenster ab 09:59:30), der erste Checkpoint schließt die erste Epoche
    // des Laufs — ohne Vorgänger. Ihr signierter Beginn verankert die Kette:
    // Die Beobachtung zählt (früher: `reported only`).
    let f = Fixture::new();
    f.observe_from_start("2026-10-02T09:59:00Z");
    let out = f.verify(true);
    assert!(out.contains("artifact 1/1 lines explained"), "{out}");
    // Ohne Vertrauen in die Signatur bleibt es dabei: nichts.
    let out = f.verify(false);
    assert!(out.contains("artifact 0/1 lines explained"), "{out}");
}

#[test]
fn verify_ignores_a_witness_restarted_during_the_session() {
    // Neustart nach dem Fensterbeginn: Was davor geschah, sah dieser Lauf
    // nicht — kein Fenster, wie ohne Witness.
    let f = Fixture::new();
    f.observe_from_start("2026-10-02T10:00:01Z");
    let out = f.verify(true);
    assert!(out.contains("artifact 0/1 lines explained"), "{out}");
}
