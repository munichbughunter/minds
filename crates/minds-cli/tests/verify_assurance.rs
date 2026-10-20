//! EA-12: `minds verify` spricht unter den drei Achsen die Assurance und
//! die Grenzen der Stufe aus (`Assurance`, `Not proven`), nennt unbestätigte
//! Schreib-Claims, prüft auf Wunsch das Ledger des Witness und kennt das
//! Gate `--require-assurance` — auch in `minds fsck`.
//!
//! Die Golden-Tests frieren den ganzen Verdikt-Block ein; Hashes werden zu
//! `b3-<hash>` normalisiert (Seal- und Session-Ids hängen an Fixture-Bytes,
//! nicht an dem, was die Tests festhalten). A2 ist aus Material heute nicht
//! erreichbar (siehe `verify_cmd/assurance.rs`); sein Golden steht als
//! Unit-Test dort (`verify_assurance_golden_a2_clean`).
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use minds_core::evidence::{SCOPE_WITNESS_FS_V1, SCOPE_WITNESS_V1, Seal, SealOutcome};
use minds_core::observation::{
    GapReason, Isolation, Observation, ObservationGap, Observations, WitnessFacts,
};
use minds_core::{ContentHash, SessionId};
use minds_store::{ContextStore, InRepoStore};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");
const PRINCIPAL: &str = "minds-witness@build-07";
const AGENT_HOOKS: &str = "agent-hooks/v1";

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

/// `git` mit Eingabe auf stdin, liefert stdout (getrimmt).
fn git_out(root: &Path, args: &[&str], input: Option<&str>) -> String {
    use std::io::Write;
    let mut child = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(input) = input {
        stdin.write_all(input.as_bytes()).unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn hash(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

/// Ersetzt jede volle Id (`b3-` + 64 Hex) durch `b3-<hash>`.
fn normalize(output: &str) -> String {
    let mut out = String::new();
    let mut rest = output;
    while let Some(at) = rest.find("b3-") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 3..];
        let hex = tail
            .bytes()
            .take_while(u8::is_ascii_hexdigit)
            .take(64)
            .count();
        if hex == 64 {
            out.push_str("b3-<hash>");
            rest = &tail[64..];
        } else {
            out.push_str("b3-");
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

struct Run {
    code: i32,
    stdout: String,
    all: String,
}

struct Fixture {
    dir: tempfile::TempDir,
    store: InRepoStore,
    key: PathBuf,
    signers: PathBuf,
    session: SessionId,
    seals: Vec<ContentHash>,
}

impl Fixture {
    /// Ein Commit, den ein Shell-Befehl schrieb (`generated.rs`), verknüpft
    /// per Trailer mit einer Session, die je Eintrag in `scopes` einen Seal
    /// trägt (verkettet, in dieser Reihenfolge). Witness-Scopes sind unter
    /// `minds-witness` signiert. `claim`: Die Session meldet, `generated.rs`
    /// mit diesen Bytes geschrieben zu haben (Write-Tool).
    fn new(scopes: &[&str], claim: Option<&[u8]>) -> Self {
        Self::build(scopes, claim, &[])
    }

    /// Wie [`Fixture::new`]; die Seals an den Positionen `unreferenced`
    /// fehlen im Rückverweis (`evidence.json`) — der Agent kann ihn
    /// schreiben.
    fn build(scopes: &[&str], claim: Option<&[u8]>, unreferenced: &[usize]) -> Self {
        Self::build_with(scopes, claim, unreferenced, None)
    }

    /// Ein einzelner Witness-Bereich, dessen Session die Herkunft `lineage`
    /// trägt (Ruhe, Ende) — wie der Checkpoint sie schreibt.
    fn with_lineage(lineage: minds_core::Lineage) -> Self {
        Self::build_with(&[SCOPE_WITNESS_V1], Some(b"shell\n"), &[], Some(lineage))
    }

    fn build_with(
        scopes: &[&str],
        claim: Option<&[u8]>,
        unreferenced: &[usize],
        lineage: Option<minds_core::Lineage>,
    ) -> Self {
        assert!(minds_attest::ssh_keygen_available());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "--template="]);
        git(root, &["config", "user.email", "human@example.invalid"]);
        git(root, &["config", "user.name", "Assurance Test"]);
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
        let tool_calls = claim
            .map(|bytes| {
                vec![minds_core::ToolCall {
                    outcome: None,
                    name: "Write".into(),
                    arguments: String::new(),
                    capture: Some(minds_core::Capture {
                        note: None,
                        status: minds_core::CaptureStatus::Interpreted,
                        adapter: "claude-code".into(),
                        adapter_version: 1,
                    }),
                    effect: Some(minds_core::Effect {
                        kind: minds_core::EffectKind::Write,
                        path: Some("generated.rs".into()),
                        content: None,
                        written: Some(hash(bytes)),
                        written_unavailable: None,
                        written_at: None,
                    }),
                }]
            })
            .unwrap_or_default();
        session.turns.push(minds_core::Turn {
            role: minds_core::Role::Assistant,
            text: String::new(),
            tool_calls,
            parent: None,
            at: Some("2026-10-02T10:00:00Z".into()),
        });
        session.lineage = lineage;
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
        let mut fixture = Self {
            dir,
            store,
            key,
            signers,
            session: id,
            seals: Vec::new(),
        };
        let mut previous = None;
        for (index, scope) in scopes.iter().enumerate() {
            let mut seal =
                Seal::parse(include_str!("fixtures/checkpoint-core/epoch-0.seal")).unwrap();
            seal.scope = (*scope).into();
            seal.first_seq = 2 * index as u64;
            seal.last_seq = 2 * index as u64 + 1;
            seal.previous = previous.clone();
            seal.last_event_at = if index + 1 == scopes.len() {
                "2026-10-02T10:00:30Z".into()
            } else {
                format!("2026-10-02T10:00:{:02}Z", 10 + index)
            };
            seal.outcome = SealOutcome::Stored {
                session: id.to_string(),
            };
            let text = seal.to_text().unwrap();
            let seal_id = fixture.store.put_seal(&text).unwrap();
            if !unreferenced.contains(&index) {
                fixture.store.record_session_seal(id, &seal_id).unwrap();
            }
            if *scope == SCOPE_WITNESS_V1 {
                let signature =
                    minds_attest::ssh_sign_ns(&text, &fixture.key, minds_attest::NS_WITNESS)
                        .unwrap();
                fixture
                    .store
                    .put_seal_signature(&seal_id, &signature)
                    .unwrap();
            }
            previous = Some(seal_id.clone());
            fixture.seals.push(seal_id);
        }
        fixture
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Der Witness lief schon vor der Session (Anker-Epoche) und sah in der
    /// Epoche des Checkpoints `generated.rs` mit `bytes` — signiert.
    fn observe(&self, bytes: &[u8]) -> ContentHash {
        self.observe_as(bytes, None)
    }

    /// Wie [`Self::observe`], beide Epochen mit den Fakten `witness`.
    fn observe_as(&self, bytes: &[u8], witness: Option<WitnessFacts>) -> ContentHash {
        self.observe_split(bytes, witness.clone(), witness)
    }

    /// Wie [`Self::observe`]: die Anker-Epoche mit `first`, die Epoche mit
    /// der Beobachtung mit `second`.
    fn observe_split(
        &self,
        bytes: &[u8],
        first: Option<WitnessFacts>,
        second: Option<WitnessFacts>,
    ) -> ContentHash {
        self.observe_full(bytes, first, second, Vec::new())
    }

    /// Wie [`Self::observe`]: Der Beobachter war in der Epoche der
    /// Beobachtung blind — ein voller Kanal um 10:00:03, drei Meldungen
    /// verloren.
    fn observe_gapped(&self, bytes: &[u8]) -> ContentHash {
        self.observe_full(
            bytes,
            None,
            None,
            vec![ObservationGap {
                seq: 2,
                at: "2026-10-02T10:00:03Z".into(),
                reason: GapReason::Overflow,
                dropped: Some(3),
            }],
        )
    }

    fn observe_full(
        &self,
        bytes: &[u8],
        first: Option<WitnessFacts>,
        second: Option<WitnessFacts>,
        gaps: Vec<ObservationGap>,
    ) -> ContentHash {
        let with = |object: Observations, witness: &Option<WitnessFacts>| match witness {
            Some(facts) => object.with_witness(facts.clone()),
            None => object,
        };
        let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
        let empty = pipeline
            .redact_observations(with(
                Observations::new("2026-10-02T08:59:00Z", Vec::new()),
                &first,
            ))
            .unwrap();
        let anchor = self.put_epoch(
            self.store.put_observations(&empty).unwrap(),
            None,
            "2026-10-02T09:00:00Z",
        );
        let object = pipeline
            .redact_observations(with(
                Observations::new(
                    "2026-10-02T09:00:00.001Z",
                    vec![Observation {
                        seq: 1,
                        at: "2026-10-02T10:00:05Z".into(),
                        path: "generated.rs".into(),
                        content: Some(hash(bytes)),
                        reason: None,
                    }],
                )
                .with_gaps(gaps),
                &second,
            ))
            .unwrap();
        self.put_epoch(
            self.store.put_observations(&object).unwrap(),
            Some(anchor),
            "2026-10-02T10:00:31Z",
        )
    }

    fn put_epoch(
        &self,
        object: ContentHash,
        previous: Option<ContentHash>,
        last: &str,
    ) -> ContentHash {
        let seal = Seal {
            root: hash(last.as_bytes()),
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
        let signature =
            minds_attest::ssh_sign_ns(&seal, &self.key, minds_attest::NS_WITNESS).unwrap();
        self.store.put_seal_signature(&id, &signature).unwrap();
        id
    }

    /// Ein Witness-Home mit einem Ledger aus `seals`.
    fn witness_home(&self, seals: &[ContentHash]) -> PathBuf {
        let home = self.root().join("witness-home");
        std::fs::create_dir_all(&home).unwrap();
        let ledger: String = seals
            .iter()
            .map(|id| format!("{id} witness/v1 2026-10-02T10:00:30Z\n"))
            .collect();
        std::fs::write(home.join("ledger"), ledger).unwrap();
        home
    }

    fn minds(&self, args: &[&str]) -> Run {
        let root = self.root();
        let out = {
            let mut minds = Command::new(MINDS);
            minds
                .env("MINDS_WITNESS", "off")
                .env_remove("MINDS_WITNESS_HOME")
                .env_remove("MINDS_WITNESS_SOCKET");
            minds
        }
        .current_dir(root)
        .env("HOME", root.join("user-home"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .output()
        .unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            all: text(&out),
        }
    }

    fn signers(&self) -> &str {
        self.signers.to_str().unwrap()
    }

    /// Trägt den Witness-Schlüssel unter `principal` in die Signer-Datei
    /// ein — etwa als getrennten Witness (`minds-witness-user@…`).
    fn register_as(&self, principal: &str) {
        let public = std::fs::read_to_string(self.key.with_extension("pub")).unwrap();
        std::fs::write(
            &self.signers,
            format!("{principal} namespaces=\"minds-witness\" {}", public.trim()),
        )
        .unwrap();
    }

    /// Legt beliebige Bytes unter den Ref des Seals.
    fn replace_bytes(&self, seal: &ContentHash, bytes: &[u8]) {
        let root = self.root();
        let refs = git_out(
            root,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/minds/evidence/",
            ],
            None,
        );
        let hex = &seal.to_string()[3..];
        let name = refs
            .lines()
            .find(|name| name.contains(hex))
            .expect("seal ref")
            .to_owned();
        let file = root.join("replacement.bin");
        std::fs::write(&file, bytes).unwrap();
        let blob = git_out(root, &["hash-object", "-w", file.to_str().unwrap()], None);
        let tree = git_out(
            root,
            &["mktree"],
            Some(&format!("100644 blob {blob}\tseal\n")),
        );
        let commit = git_out(root, &["commit-tree", &tree, "-m", "forged"], None);
        git_out(root, &["update-ref", &name, &commit], None);
    }

    /// Ändert die gespeicherten Bytes des Seals unter seiner alten Id.
    fn tamper(&self, seal: &ContentHash) {
        let root = self.root();
        let refs = git_out(
            root,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/minds/evidence/",
            ],
            None,
        );
        let hex = &seal.to_string()[3..];
        let name = refs
            .lines()
            .find(|name| name.contains(hex))
            .expect("seal ref")
            .to_owned();
        let forged = self
            .store
            .seal_text(seal)
            .unwrap()
            .unwrap()
            .replacen("events=2", "events=9", 1);
        let blob = git_out(root, &["hash-object", "-w", "--stdin"], Some(&forged));
        let tree = git_out(
            root,
            &["mktree"],
            Some(&format!("100644 blob {blob}\tseal\n")),
        );
        let commit = git_out(root, &["commit-tree", &tree, "-m", "forged"], None);
        git_out(root, &["update-ref", &name, &commit], None);
    }
}

/// Die Zeilen des Blocks ab `Integrity` (Seal- und Session-Zeilen tragen
/// nur Ids, die schon anderswo eingefroren sind).
fn axes(stdout: &str) -> String {
    let start = stdout.find("Integrity").expect(stdout);
    normalize(&stdout[start..])
}

#[test]
fn verify_assurance_golden_a1() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let run = fixture.minds(&["verify", "HEAD"]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert_eq!(
        axes(&run.stdout),
        "\
Integrity      intact
Coverage       complete within the boundary (boundary: witness/v1 — activity outside it is not captured · 0 gaps · artifact 0/1 lines explained)
  why            1 unexplained line(s): 1 with no tool claim or shell mention found
  unexplained    generated.rs:1  no tool claim or shell mention found
Interpretation complete
Assurance      A1 observed    (range 1: witness signature not checked — no trusted allowed_signers)
Intent         intent not bound
Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)
Overall        VERIFIED
"
    );
}

#[test]
fn verify_assurance_golden_mixed() {
    // Ein Rückfall-Bereich der Hooks, dann der Witness: gemischt, Minimum
    // A1 — mit dem Grund der ersten (schwächsten) Range.
    let fixture = Fixture::new(&[AGENT_HOOKS, SCOPE_WITNESS_V1], None);
    fixture.observe(b"shell\n");
    let run = fixture.minds(&["verify", "HEAD", "--signers", fixture.signers()]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert_eq!(
        axes(&run.stdout),
        "\
Integrity      intact
Coverage       complete within the boundary (boundary: agent-hooks/v1, witness/v1 — activity outside it is not captured · 0 gaps · artifact 1/1 lines explained)
Interpretation complete
Assurance      A1 observed    (range 1: observed by the agent's hooks only (scope agent-hooks/v1))
Intent         intent not bound
Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)
Overall        VERIFIED
"
    );
}

#[test]
fn verify_assurance_golden_forged_claim() {
    // Der Agent meldet einen Write, den der Witness so nie sah: Er steht
    // als `uncorroborated` da; die Zeile erklärt die Beobachtung.
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], Some(b"forged\n"));
    fixture.observe(b"shell\n");
    let run = fixture.minds(&["verify", "HEAD", "--signers", fixture.signers()]);
    assert_eq!(run.code, 0, "{}", run.all);
    let claimed = &hash(b"forged\n").to_string()[..11];
    assert_eq!(
        axes(&run.stdout),
        format!(
            "\
Integrity      intact
Coverage       complete within the boundary (boundary: witness/v1 — activity outside it is not captured · 0 gaps · artifact 1/1 lines explained)
  uncorroborated  turn 1 call 1  Write generated.rs  {claimed}…  no file-system observation
Interpretation complete
Assurance      A1 observed    (range 1: witness profile unknown)
Intent         intent not bound
Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)
Overall        VERIFIED
"
        )
    );
}

#[test]
fn verify_assurance_golden_observer_gap() {
    // Der Beobachter war blind, als der Agent schrieb: Der Claim ist nicht
    // „nie beobachtet", sondern fiel in eine Lücke — die Zeile sagt es,
    // und die Assurance nennt den Grund der Lücke.
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], Some(b"forged\n"));
    fixture.observe_gapped(b"shell\n");
    let run = fixture.minds(&["verify", "HEAD", "--signers", fixture.signers()]);
    assert_eq!(run.code, 0, "{}", run.all);
    let claimed = &hash(b"forged\n").to_string()[..11];
    assert_eq!(
        axes(&run.stdout),
        format!(
            "\
Integrity      intact
Coverage       complete within the boundary (boundary: witness/v1 — activity outside it is not captured · 0 gaps · artifact 1/1 lines explained)
  uncorroborated  turn 1 call 1  Write generated.rs  {claimed}…  observation gap (overflow: 3 events dropped)
Interpretation complete
Assurance      A1 observed    (range 1: observation gap (overflow))
Intent         intent not bound
Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)
Overall        VERIFIED
"
        )
    );
}

/// Die Fakten, die ein Witness beim Versiegeln festhielt.
fn witness_facts(profile: &str, witness_uid: u32) -> WitnessFacts {
    WitnessFacts {
        profile: profile.into(),
        isolation: Isolation {
            witness_uid,
            agent_uids: vec![501],
            home_private: true,
            binary_protected: true,
        },
        spec: None,
    }
}

/// Die Assurance-Zeile (mit ihrem ersten Grund).
fn assurance_line(fixture: &Fixture) -> String {
    let run = fixture.minds(&["verify", "HEAD", "--signers", fixture.signers()]);
    assert_eq!(run.code, 0, "{}", run.all);
    run.stdout
        .lines()
        .find(|line| line.starts_with("Assurance"))
        .unwrap_or_else(|| panic!("no assurance line: {}", run.all))
        .to_owned()
}

/// Der Witness hält Profil und Trennung im signierten Objekt fest: Ein
/// `local`-Witness (dieselbe Kennung wie der Agent) steht als nicht
/// qualifiziert da — nicht mehr als „unbekannt".
#[test]
fn verify_reads_profile_and_isolation_from_the_witness_facts() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], Some(b"shell\n"));
    fixture.observe_as(b"shell\n", Some(witness_facts("local", 501)));
    let line = assurance_line(&fixture);
    assert!(
        line.contains("A1 observed") && line.contains("profile local not qualified for A2"),
        "{line}"
    );
}

/// Ein getrennter `user`-Witness, den die Signer-Datei als getrennt
/// einträgt (`minds-witness-user@…`): Profil und Trennung tragen. Die Gründe
/// stehen in fester Regel-Reihenfolge — Profil, Trennung, Sessionende,
/// Intent —, die Zeile nennt den ersten: Steht dort einer nach der Trennung,
/// sind Profil und Trennung erfüllt. Zu A2 fehlt hier nur noch das Ende und
/// der gebundene Intent.
#[test]
fn verify_accepts_a_separated_user_witness() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], Some(b"shell\n"));
    fixture.register_as("minds-witness-user@build-07");
    fixture.observe_as(b"shell\n", Some(witness_facts("user", 498)));
    let line = assurance_line(&fixture);
    assert!(line.contains("A1 observed"), "{line}");
    for unwanted in ["profile", "isolation"] {
        assert!(!line.contains(unwanted), "{line}");
    }
    assert!(
        line.contains("session active at checkpoint") || line.contains("intent"),
        "{line}"
    );
}

/// Die Herkunft, wie der Checkpoint sie schreibt: `quiescence` und `closed`.
fn lineage(quiescence: Option<minds_core::Quiescence>, closed: bool) -> minds_core::Lineage {
    minds_core::Lineage {
        closed,
        quiescence,
        ..minds_core::Lineage::new("s1")
    }
}

/// Ein getrennter `user`-Witness mit Profil und Trennung, über `fixture`.
fn separated_user_witness(fixture: &Fixture) {
    fixture.register_as("minds-witness-user@build-07");
    fixture.observe_as(b"shell\n", Some(witness_facts("user", 498)));
}

/// EA-11, A2 ohne `/exit`: Versiegelt der Checkpoint die Session mitten im
/// Tool-Aufruf, bleibt sie bei A1, und der Grund nennt das offene Event;
/// ruht sie (letztes Event `Stop`), fällt die Ruhe als Grund weg — zu A2
/// fehlt dann nur noch der Intent —, und `Not proven` nennt das fehlende
/// Sessionende als Information, nicht als Abzug.
#[test]
fn verify_names_an_open_tool_call_and_takes_a_quiescent_range_as_witnessed() {
    use minds_core::Quiescence;
    let fixture = Fixture::with_lineage(lineage(Some(Quiescence::ToolCallOpen { seq: 3 }), false));
    separated_user_witness(&fixture);
    let line = assurance_line(&fixture);
    assert!(
        line.contains("A1 observed")
            && line.contains("session active at checkpoint (PreToolUse #3 open)"),
        "{line}"
    );

    let fixture = Fixture::with_lineage(lineage(Some(Quiescence::Active { seq: 5 }), false));
    separated_user_witness(&fixture);
    let line = assurance_line(&fixture);
    assert!(
        line.contains("session active at checkpoint (last event #5 is not a stop)"),
        "{line}"
    );

    // Ruhend, ohne `SessionEnd`: Die Ruhe ist kein Grund mehr; die Zeile
    // nennt den nächsten (Intent). Das Ende steht unter `Not proven`.
    let fixture = Fixture::with_lineage(lineage(Some(Quiescence::Quiescent), false));
    separated_user_witness(&fixture);
    let line = assurance_line(&fixture);
    assert!(line.contains("(range 1: intent not bound)"), "{line}");
    let run = fixture.minds(&["verify", "HEAD", "--signers", fixture.signers(), "--limits"]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(
        run.stdout
            .contains("  - Not that the session has ended: the last sealed range ends at a stop"),
        "{}",
        run.stdout
    );

    // Mit `SessionEnd`: dieselbe Stufe, der Satz fehlt.
    let fixture = Fixture::with_lineage(lineage(Some(Quiescence::Quiescent), true));
    separated_user_witness(&fixture);
    let run = fixture.minds(&["verify", "HEAD", "--signers", fixture.signers(), "--limits"]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(
        !run.stdout.contains("Not that the session has ended"),
        "{}",
        run.stdout
    );
    // Ein älteres Objekt ohne `quiescence`, das mit `SessionEnd` schließt,
    // ruht.
    let fixture = Fixture::with_lineage(lineage(None, true));
    separated_user_witness(&fixture);
    let line = assurance_line(&fixture);
    assert!(line.contains("(range 1: intent not bound)"), "{line}");
}

/// Dieselben Fakten unter dem gewöhnlichen Witness-Principal: Was der
/// Witness über sich schreibt, bezeugt allein sein Schlüssel — und den
/// Schlüssel eines `local`-Witness kann der Agent lesen. Ohne Bürgschaft der
/// Signer-Datei keine Trennung.
#[test]
fn verify_refuses_separation_without_the_separated_principal() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], Some(b"shell\n"));
    fixture.observe_as(b"shell\n", Some(witness_facts("user", 498)));
    let line = assurance_line(&fixture);
    assert!(line.contains("witness isolation not proven"), "{line}");
}

/// Getrennte Beobachtungs-Epochen, aber die Hook-Bereiche der Session unter
/// einem gewöhnlichen Witness-Principal — etwa dem Schlüssel eines
/// `local`-Witness, den der Agent lesen kann und der noch in der Signer-Datei
/// steht. Damit ließe sich eine Session fälschen: keine Trennung.
#[test]
fn verify_refuses_hook_ranges_under_an_ordinary_principal() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], Some(b"shell\n"));
    fixture.observe_as(b"shell\n", Some(witness_facts("user", 498)));
    let local = fixture.root().join("local_ed25519");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&local)
            .status()
            .unwrap()
            .success()
    );
    for seal in &fixture.seals {
        let text = fixture.store.seal_text(seal).unwrap().unwrap();
        let signature = minds_attest::ssh_sign_ns(&text, &local, minds_attest::NS_WITNESS).unwrap();
        fixture.store.put_seal_signature(seal, &signature).unwrap();
    }
    let separated = std::fs::read_to_string(fixture.key.with_extension("pub")).unwrap();
    let ordinary = std::fs::read_to_string(local.with_extension("pub")).unwrap();
    std::fs::write(
        &fixture.signers,
        format!(
            "minds-witness-user@build-07 namespaces=\"minds-witness\" {}\n\
             minds-witness@build-07 namespaces=\"minds-witness\" {}\n",
            separated.trim(),
            ordinary.trim()
        ),
    )
    .unwrap();
    let line = assurance_line(&fixture);
    assert!(line.contains("witness isolation not proven"), "{line}");
}

/// Ein `user`-Witness, der die Kennung mit dem Agenten teilt: Auch unter
/// dem getrennten Principal trägt der Profilname allein nicht.
#[test]
fn verify_refuses_a_user_witness_without_separation() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], Some(b"shell\n"));
    fixture.register_as("minds-witness-user@build-07");
    fixture.observe_as(b"shell\n", Some(witness_facts("user", 501)));
    let line = assurance_line(&fixture);
    assert!(line.contains("witness isolation not proven"), "{line}");
}

/// Die Anker-Epoche vor dem Fenster zählt nicht: Ohne Fakten dort bleibt
/// der getrennte Witness getrennt.
#[test]
fn verify_ignores_facts_of_the_epoch_before_the_window() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], Some(b"shell\n"));
    fixture.register_as("minds-witness-user@build-07");
    fixture.observe_split(b"shell\n", None, Some(witness_facts("user", 498)));
    let line = assurance_line(&fixture);
    for unwanted in ["profile", "isolation"] {
        assert!(!line.contains(unwanted), "{line}");
    }
}

/// Zwei Epochen im Fenster, eine davon ohne Fakten (ein älteres Objekt):
/// Für einen Teil des Fensters ist das Profil unbekannt (fail-closed).
#[test]
fn verify_treats_an_epoch_without_facts_as_unknown() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], Some(b"shell\n"));
    fixture.register_as("minds-witness-user@build-07");
    let facts = witness_facts("user", 498);
    let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let put = |object: Observations, previous, last: &str| {
        let redacted = pipeline.redact_observations(object).unwrap();
        fixture.put_epoch(
            fixture.store.put_observations(&redacted).unwrap(),
            previous,
            last,
        )
    };
    let anchor = put(
        Observations::new("2026-10-02T08:59:00Z", Vec::new()).with_witness(facts.clone()),
        None,
        "2026-10-02T09:00:00Z",
    );
    // Mitten im Fenster: keine Fakten.
    let middle = put(
        Observations::new("2026-10-02T09:00:00.001Z", Vec::new()),
        Some(anchor),
        "2026-10-02T10:00:10Z",
    );
    put(
        Observations::new(
            "2026-10-02T10:00:10.001Z",
            vec![Observation {
                seq: 1,
                at: "2026-10-02T10:00:20Z".into(),
                path: "generated.rs".into(),
                content: Some(hash(b"shell\n")),
                reason: None,
            }],
        )
        .with_witness(facts),
        Some(middle),
        "2026-10-02T10:00:31Z",
    );
    let line = assurance_line(&fixture);
    assert!(line.contains("witness profile unknown"), "{line}");
}

/// Der Witness sah ein Soll mit einem Umschlag, der heute nicht mehr im
/// Store liegt: `verify` sagt es unter Conformance — als Auskunft, ohne
/// Gate (die Umschläge hielt der Witness ungeprüft fest).
#[test]
fn verify_reports_spec_envelopes_the_witness_saw_but_the_store_lost() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], Some(b"shell\n"));
    let mut facts = witness_facts("user", 498);
    facts.spec = Some(minds_core::observation::SpecSeen {
        id: ContentHash::from_bytes([0x5a; 32]),
        envelopes: vec![ContentHash::from_bytes([0x01; 32])],
    });
    fixture.observe_as(b"shell\n", Some(facts));
    // Unter einem gewöhnlichen Witness-Principal (dessen Schlüssel der Agent
    // lesen könnte) zählt die Auskunft nicht.
    let ordinary = fixture.minds(&["verify", "HEAD", "--signers", fixture.signers()]);
    assert!(
        !ordinary.stdout.contains("witnessed spec"),
        "{}",
        ordinary.all
    );
    fixture.register_as("minds-witness-user@build-07");
    let run = fixture.minds(&["verify", "HEAD", "--signers", fixture.signers()]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(
        run.stdout.contains(
            "witnessed spec b3-5a5a5a5a5a5a during the sessions (1 envelope(s) seen by the witness)"
        ),
        "{}",
        run.all
    );
    assert!(
        run.stdout.contains("none of them is in the store any more"),
        "{}",
        run.all
    );
    let gated = fixture.minds(&[
        "verify",
        "HEAD",
        "--signers",
        fixture.signers(),
        "--conformance",
    ]);
    // Das Gate scheitert hier nur, weil der Commit kein Soll trägt.
    assert!(
        gated.stdout.contains("conformance not assessed (no spec"),
        "{}",
        gated.all
    );
    // Die Auskunft steht auch dort; sie ist kein Gate.
    assert!(
        gated.stdout.contains("witnessed spec b3-5a5a5a5a5a5a"),
        "{}",
        gated.all
    );
    assert_eq!(
        gated.stdout.matches("Gate           conformance").count(),
        1,
        "{}",
        gated.all
    );
}

#[test]
fn verify_assurance_golden_human_edit() {
    // Witness-signiert, aber kein Beobachter-Fenster: Die menschliche Zeile
    // bleibt unerklärt, die Stufe nennt das fehlende Fenster.
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let run = fixture.minds(&["verify", "HEAD", "--signers", fixture.signers()]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert_eq!(
        axes(&run.stdout),
        "\
Integrity      intact
Coverage       complete within the boundary (boundary: witness/v1 — activity outside it is not captured · 0 gaps · artifact 0/1 lines explained)
  why            1 unexplained line(s): 1 with no tool claim or shell mention found
  unexplained    generated.rs:1  no tool claim or shell mention found
Interpretation complete
Assurance      A1 observed    (range 1: no file-system observation window covering the session)
Intent         intent not bound
Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)
Overall        VERIFIED
"
    );
}

#[test]
fn verify_assurance_golden_missing_ledger_seal() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let gone = hash(b"a seal the witness wrote, deleted from the repository");
    let home = fixture.witness_home(&[fixture.seals[0].clone(), gone.clone()]);
    let run = fixture.minds(&[
        "verify",
        "HEAD",
        "--signers",
        fixture.signers(),
        "--witness-home",
        home.to_str().unwrap(),
    ]);
    assert_eq!(run.code, 1, "{}", run.all);
    assert!(
        run.stdout.contains(&format!(
            "Integrity      VIOLATED  witnessed seal {gone} missing from the repository\n"
        )),
        "{}",
        run.stdout
    );
    assert_eq!(
        axes(&run.stdout),
        "\
Integrity      VIOLATED  witnessed seal b3-<hash> missing from the repository
Coverage       not assessable (boundary: witness/v1 — activity outside it is not captured · 0 gaps · artifact 0/1 lines explained)
  why            1 unexplained line(s): 1 with no tool claim or shell mention found
  unexplained    generated.rs:1  no tool claim or shell mention found
Interpretation complete
Assurance      A0 claimed     (witnessed seal(s) missing from the repository: b3-<hash>)
Intent         intent not bound
Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)
Overall        TAMPERED
"
    );
}

#[test]
fn verify_ledger_missing_seal_is_tampered() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    // Ein vollständiges Ledger ändert nichts.
    let home = fixture.witness_home(&fixture.seals);
    let home = home.to_str().unwrap();
    let run = fixture.minds(&["verify", "HEAD", "--witness-home", home]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(
        run.stdout.contains("Integrity      intact\n"),
        "{}",
        run.stdout
    );
    // Fehlt ein bezeugter Seal, ist es TAMPERED — auch ohne Signer, und
    // kein Gate macht daraus etwas anderes als 1.
    let gone = hash(b"deleted");
    fixture.witness_home(&[gone]);
    for extra in [&[][..], &["--require-assurance", "A0"][..]] {
        let mut args = vec!["verify", "HEAD", "--witness-home", home];
        args.extend_from_slice(extra);
        let run = fixture.minds(&args);
        assert_eq!(run.code, 1, "{}", run.all);
        assert!(
            run.stdout.contains("Overall        TAMPERED"),
            "{}",
            run.stdout
        );
    }
    // Ein nicht lesbares Ledger ist operativ (4), nie „abgeglichen".
    let missing = fixture.root().join("no-such-home");
    let run = fixture.minds(&[
        "verify",
        "HEAD",
        "--witness-home",
        missing.to_str().unwrap(),
    ]);
    assert_eq!(run.code, 4, "{}", run.all);
    assert!(run.all.contains("witness ledger"), "{}", run.all);
    // Eine kaputte Zeile mittendrin ebenso.
    std::fs::write(Path::new(home).join("ledger"), "garbage\n").unwrap();
    let run = fixture.minds(&["verify", "HEAD", "--witness-home", home]);
    assert_eq!(run.code, 4, "{}", run.all);
}

#[test]
fn verify_require_assurance_gate() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    // Bestanden: kein Gate-Satz, Exit 0.
    let run = fixture.minds(&["verify", "HEAD", "--require-assurance", "A1"]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(!run.stdout.contains("Gate"), "{}", run.stdout);
    // Verfehlt: genau eine Gate-Zeile nach dem Block, Exit 2.
    let run = fixture.minds(&["verify", "HEAD", "--require-assurance", "A2"]);
    assert_eq!(run.code, 2, "{}", run.all);
    assert!(
        run.stdout
            .ends_with("Overall        VERIFIED\nGate           assurance A1 observed < required A2 witnessed\n"),
        "{}",
        run.stdout
    );
    // Ein ungültiger Wert ist ein Bedienfehler (4), kein bestandenes Gate.
    for bad in ["a2", "A4", "witnessed"] {
        let run = fixture.minds(&["verify", "HEAD", "--require-assurance", bad]);
        assert_eq!(run.code, 4, "{bad}: {}", run.all);
    }
    // Die Flags gehören nur zum Verdikt-Modus.
    let seal = fixture.seals[0].to_string();
    for flag in [
        &["--require-assurance", "A1"][..],
        &["--witness-home", "x"][..],
        &["--limits"][..],
    ] {
        let mut args = vec!["verify", "--evidence", &seal];
        args.extend_from_slice(flag);
        assert_eq!(fixture.minds(&args).code, 4, "{flag:?}");
    }
}

#[test]
fn verify_gate_never_masks_tampered_or_unverifiable() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    // Ein veränderter Seal: TAMPERED bleibt 1, auch wenn das Gate verfehlt —
    // und die Stufe fällt auf A0.
    fixture.tamper(&fixture.seals[0]);
    let run = fixture.minds(&["verify", "HEAD", "--require-assurance", "A3"]);
    assert_eq!(run.code, 1, "{}", run.all);
    assert!(
        run.stdout.contains("Overall        TAMPERED"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains(
            "Assurance      A0 claimed     (integrity broken — seal material was altered)\n"
        ),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("Gate           assurance A0 claimed"));
    // NOT VERIFIABLE (kein Seal) bleibt 3.
    let legacy = Fixture::new(&[], None);
    let run = legacy.minds(&["verify", "HEAD", "--require-assurance", "A3"]);
    assert_eq!(run.code, 3, "{}", run.all);
    assert!(
        run.stdout
            .contains("Assurance      A0 claimed     (captured before the evidence chain)\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("Gate           assurance A0 claimed < required A3 reproduced"),
        "{}",
        run.stdout
    );
}

#[test]
fn verify_limits_flag() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let run = fixture.minds(&["verify", "HEAD", "--limits"]);
    assert_eq!(run.code, 0, "{}", run.all);
    let a1: Vec<_> = minds_core::evidence::limits_at(minds_core::evidence::Level::A1).collect();
    assert!(
        run.stdout.contains(&format!(
            "Not proven     {} limit(s) at A1 observed\n",
            a1.len()
        )),
        "{}",
        run.stdout
    );
    for limit in &a1 {
        assert!(
            run.stdout.contains(&format!("  - {}\n", limit.text)),
            "{}",
            limit.id
        );
    }
    assert!(!run.stdout.contains("(minds verify --limits)"));
}

#[test]
fn fsck_require_assurance_gate() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let signers = fixture.signers();
    // Ein agent-authored Commit unter A2: das Gate verfehlt, Exit 2.
    let run = fixture.minds(&["fsck", "--require-assurance", "A2", "--signers", signers]);
    assert_eq!(run.code, 2, "{}", run.all);
    assert!(
        run.stdout.contains(&format!(
            "  below A2: {} — A1 observed (range 1: no file-system observation window covering the session)\n",
            fixture.session
        )),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("Assurance: 1 session(s) checked, 1 below A2 witnessed\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("Gate           assurance A1 observed < required A2 witnessed\n"),
        "{}",
        run.stdout
    );
    // Bestanden: 0.
    let run = fixture.minds(&["fsck", "--require-assurance", "A1", "--signers", signers]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(!run.stdout.contains("Gate"), "{}", run.stdout);
    // Ein ungültiger Wert ist kein bestandenes Gate.
    let run = fixture.minds(&["fsck", "--require-assurance", "A9"]);
    assert_ne!(run.code, 0, "{}", run.all);
}

#[test]
fn fsck_assurance_gate_never_masks_a_finding() {
    // Ein Trailer ins Leere ist ein Befund (1) — das Gate macht keine 2
    // daraus.
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let orphan = hash(b"no such session");
    std::fs::write(fixture.root().join("other.rs"), "x\n").unwrap();
    git(fixture.root(), &["add", "other.rs"]);
    git(
        fixture.root(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            &format!("other\n\nMinds-Session-Id: {orphan}"),
        ],
    );
    let run = fixture.minds(&["fsck", "--require-assurance", "A2"]);
    assert_eq!(run.code, 1, "{}", run.all);
}

#[test]
fn verify_ledger_replaced_seal_is_tampered() {
    // Ersetzen statt Löschen: Der Ref bleibt, die Bytes sind andere — ein
    // Seal, den der Verdikt-Block gar nicht prüft (eine Beobachtungs-Epoche).
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let epoch = fixture.observe(b"shell\n");
    let home = fixture.witness_home(&[fixture.seals[0].clone(), epoch.clone()]);
    let home = home.to_str().unwrap();
    let run = fixture.minds(&["verify", "HEAD", "--witness-home", home]);
    assert_eq!(run.code, 0, "{}", run.all);
    fixture.tamper(&epoch);
    let run = fixture.minds(&["verify", "HEAD", "--witness-home", home]);
    assert_eq!(run.code, 1, "{}", run.all);
    assert!(
        run.stdout.contains(&format!(
            "Integrity      VIOLATED  witnessed seal {epoch} altered in the repository\n"
        )),
        "{}",
        run.stdout
    );
}

#[test]
fn verify_checks_seals_missing_from_the_back_reference() {
    // Der Agent lässt den ersten (schwachen) Bereich aus `evidence.json`
    // weg: Er wird trotzdem geprüft und genannt — die Stufe nennt ihn.
    let fixture = Fixture::build(&[AGENT_HOOKS, SCOPE_WITNESS_V1], None, &[0]);
    let run = fixture.minds(&["verify", "HEAD"]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(
        run.stdout.contains(&format!(
            "Note           seal {} names this session but is missing from the back-reference (evidence.json) — checked anyway\n",
            fixture.seals[0]
        )),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains(
            "Assurance      A1 observed    (range 1: observed by the agent's hooks only (scope agent-hooks/v1))\n"
        ),
        "{}",
        run.stdout
    );
    // Die Seal-Zeilen stehen in der Ordnung, in der `range N` zählt.
    let first = run.stdout.find(&fixture.seals[0].to_string()).unwrap();
    let second = run
        .stdout
        .find(&format!("Seal           {}", fixture.seals[1]))
        .unwrap();
    assert!(
        run.stdout[first..].starts_with(&fixture.seals[0].to_string()) && first < second,
        "{}",
        run.stdout
    );
}

#[test]
fn verify_without_session_keeps_ledger_and_gate_findings() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    std::fs::write(fixture.root().join("human.rs"), "x\n").unwrap();
    git(fixture.root(), &["add", "human.rs"]);
    git(
        fixture.root(),
        &["-c", "commit.gpgsign=false", "commit", "-qm", "human"],
    );
    // Kein Session-Block, aber ein fehlender bezeugter Seal: TAMPERED.
    let gone = hash(b"gone");
    let home = fixture.witness_home(std::slice::from_ref(&gone));
    let run = fixture.minds(&["verify", "HEAD", "--witness-home", home.to_str().unwrap()]);
    assert_eq!(run.code, 1, "{}", run.all);
    assert!(
        run.stdout.contains(&format!(
            "Integrity      VIOLATED  witnessed seal {gone} missing from the repository\n"
        )),
        "{}",
        run.stdout
    );
    // Ohne Session ist das Gate nicht bestanden — und 3 bleibt 3.
    let run = fixture.minds(&["verify", "HEAD", "--require-assurance", "A0"]);
    assert_eq!(run.code, 3, "{}", run.all);
    assert!(
        run.stdout
            .contains("Gate           assurance not assessed (no session) — required A0 claimed\n"),
        "{}",
        run.stdout
    );
}

#[test]
fn verify_reports_a_torn_ledger_tail() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let home = fixture.witness_home(&fixture.seals);
    let ledger = home.join("ledger");
    let mut text = std::fs::read_to_string(&ledger).unwrap();
    text.push_str("b3-0123");
    std::fs::write(&ledger, text).unwrap();
    let run = fixture.minds(&["verify", "HEAD", "--witness-home", home.to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(
        run.stdout.starts_with(
            "Note           witness ledger ends in a torn line — seals after it are not ledgered\n"
        ),
        "{}",
        run.stdout
    );
}

#[test]
fn fsck_and_verify_agree_on_a_signer_file_from_the_git_config() {
    // `verify` prüft Seal-Signaturen auch gegen `gpg.ssh.allowedSignersFile`;
    // kennt die Datei den Witness-Schlüssel nicht, ist der Seal TAMPERED. Das
    // Gate in `fsck` darf dann nicht milder urteilen.
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let other = fixture.root().join("other");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&other)
            .status()
            .unwrap()
            .success()
    );
    let public = std::fs::read_to_string(other.with_extension("pub")).unwrap();
    let developer = fixture.root().join("developer_signers");
    std::fs::write(&developer, format!("dev@example.invalid {}", public.trim())).unwrap();
    git(
        fixture.root(),
        &[
            "config",
            "gpg.ssh.allowedSignersFile",
            developer.to_str().unwrap(),
        ],
    );
    let run = fixture.minds(&["verify", "HEAD"]);
    assert_eq!(run.code, 1, "{}", run.all);
    assert!(
        run.stdout.contains("Assurance      A0 claimed "),
        "{}",
        run.stdout
    );
    let run = fixture.minds(&["fsck", "--require-assurance", "A1"]);
    assert_eq!(run.code, 2, "{}", run.all);
    assert!(
        run.stdout
            .contains("Gate           assurance A0 claimed < required A1 observed\n"),
        "{}",
        run.stdout
    );
    // `--signers` ohne Gate ist ein Bedienfehler, kein stiller No-op.
    let run = fixture.minds(&["fsck", "--signers", fixture.signers()]);
    assert_eq!(run.code, 1, "{}", run.all);
}

#[test]
fn verify_ledger_seal_replaced_by_non_text_is_tampered_not_operational() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let epoch = fixture.observe(b"shell\n");
    let home = fixture.witness_home(&[fixture.seals[0].clone(), epoch.clone()]);
    fixture.replace_bytes(&epoch, b"\xff\xfe not text");
    let run = fixture.minds(&["verify", "HEAD", "--witness-home", home.to_str().unwrap()]);
    assert_eq!(run.code, 1, "{}", run.all);
    assert!(
        run.stdout.contains(&format!(
            "Integrity      VIOLATED  witnessed seal {epoch} altered in the repository\n"
        )),
        "{}",
        run.stdout
    );
}

#[test]
fn session_wide_reasons_name_no_range() {
    // Ein veränderter Seal neben einem intakten: „integrity broken" trifft
    // die Session, nicht die erste lesbare Range.
    let fixture = Fixture::new(&[AGENT_HOOKS, SCOPE_WITNESS_V1], None);
    fixture.tamper(&fixture.seals[0]);
    let run = fixture.minds(&["verify", "HEAD"]);
    assert_eq!(run.code, 1, "{}", run.all);
    assert!(
        run.stdout.contains(
            "Assurance      A0 claimed     (integrity broken — seal material was altered)\n"
        ),
        "{}",
        run.stdout
    );
}

#[test]
fn a_legacy_session_keeps_its_line_under_a_ledger_finding() {
    let fixture = Fixture::new(&[], None);
    let home = fixture.witness_home(&[hash(b"gone")]);
    let run = fixture.minds(&["verify", "HEAD", "--witness-home", home.to_str().unwrap()]);
    assert_eq!(run.code, 1, "{}", run.all);
    assert!(
        run.stdout
            .contains("Seals          none — captured before the evidence chain\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("Overall        TAMPERED"),
        "{}",
        run.stdout
    );
}

/// Die Conformance neben einem VERIFIED-Urteil: gezeigt, ohne den Exit-Code
/// zu ändern; erst `--conformance` macht DRIFT zum Gate (Exit 2). Das Soll
/// signiert ein eigener Schlüssel, beschränkt auf `minds-spec`.
#[test]
fn verify_shows_conformance_and_gates_only_with_the_flag() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let root = fixture.root();
    let spec_key = root.join("spec_id");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "spec@human", "-f"])
            .arg(&spec_key)
            .status()
            .unwrap()
            .success()
    );
    let public = std::fs::read_to_string(spec_key.with_extension("pub")).unwrap();
    let mut signers = std::fs::read_to_string(&fixture.signers).unwrap();
    signers.push_str(&format!(
        "\nspec@human namespaces=\"minds-spec\" {}\n",
        public.trim()
    ));
    std::fs::write(&fixture.signers, signers).unwrap();

    std::fs::create_dir_all(root.join(".minds")).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join(".minds/spec.json"),
        r#"{"version":"minds-spec-v1","rules":[
            {"id":"config","kind":"json-schema","schema":"config.schema.json","files":"config/*.json"}
        ]}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("config.schema.json"),
        r#"{"type":"object","required":["name"]}"#,
    )
    .unwrap();
    std::fs::write(root.join("config/a.json"), r#"{"name":"a"}"#).unwrap();
    git(root, &["add", "-A"]);
    git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "spec"],
    );
    let signed = fixture.minds(&["spec", "sign", "--key", spec_key.to_str().unwrap()]);
    assert_eq!(signed.code, 0, "{}", signed.all);
    assert!(
        signed.stdout.contains("signed  spec@human"),
        "{}",
        signed.all
    );

    // Die Session hängt am Commit davor; die Conformance gilt `--commit`.
    let verify = |extra: &[&str]| {
        let mut args = vec![
            "verify",
            "HEAD~1",
            "--commit",
            "HEAD",
            "--signers",
            fixture.signers(),
        ];
        args.extend_from_slice(extra);
        fixture.minds(&args)
    };
    let run = verify(&[]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(
        run.stdout
            .contains("Conformance    PASS  2 pass · 0 drift · 0 unknown — spec b3-"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("signed by spec@human"),
        "{}",
        run.stdout
    );
    let run = verify(&["--conformance"]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(!run.stdout.contains("Gate"), "{}", run.stdout);

    // Eine Datei bricht den Vertrag: gezeigt, Exit unverändert — außer mit
    // `--conformance`.
    std::fs::write(root.join("config/b.json"), r#"{"other":1}"#).unwrap();
    git(root, &["add", "-A"]);
    git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "broken"],
    );
    let verify = |extra: &[&str]| {
        let mut args = vec![
            "verify",
            "HEAD~2",
            "--commit",
            "HEAD",
            "--signers",
            fixture.signers(),
        ];
        args.extend_from_slice(extra);
        fixture.minds(&args)
    };
    let run = verify(&[]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(
        run.stdout.contains("DRIFT    config [json-schema]"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("observed  config/b.json: /name violates `required`"),
        "{}",
        run.stdout
    );
    let run = verify(&["--conformance"]);
    assert_eq!(run.code, 2, "{}", run.all);
    assert!(
        run.stdout
            .ends_with("Gate           conformance: 1 drift, 0 unknown — required all PASS\n"),
        "{}",
        run.stdout
    );

    // Ohne vertrauenswürdige Signer ist das Soll UNKNOWN, nie PASS.
    let run = fixture.minds(&["verify", "HEAD~2", "--commit", "HEAD~1", "--conformance"]);
    assert!(
        run.stdout.contains("UNKNOWN  spec [signature]"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("Gate           conformance:"),
        "{}",
        run.stdout
    );

    // Nur im Verdikt-Modus.
    let seal = fixture.seals[0].to_string();
    let run = fixture.minds(&["verify", "--evidence", &seal, "--conformance"]);
    assert_eq!(run.code, 4, "{}", run.all);
}

/// Ein unbeschränkter Schlüssel gibt kein Soll frei (wie bei Intents): Ein
/// Entwickler-Schlüssel im ssh-agent könnte auch der Agent benutzen.
#[test]
fn an_unrestricted_key_does_not_approve_a_spec() {
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let root = fixture.root();
    let spec_key = root.join("spec_id");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "dev", "-f"])
            .arg(&spec_key)
            .status()
            .unwrap()
            .success()
    );
    let public = std::fs::read_to_string(spec_key.with_extension("pub")).unwrap();
    let mut signers = std::fs::read_to_string(&fixture.signers).unwrap();
    signers.push_str(&format!("\ndev {}\n", public.trim()));
    std::fs::write(&fixture.signers, signers).unwrap();
    std::fs::create_dir_all(root.join(".minds")).unwrap();
    std::fs::write(
        root.join(".minds/spec.json"),
        r#"{"version":"minds-spec-v1","rules":[{"id":"s","kind":"graph","expect":"session"}]}"#,
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "spec"],
    );
    let signed = fixture.minds(&["spec", "sign", "--key", spec_key.to_str().unwrap()]);
    assert_eq!(signed.code, 0, "{}", signed.all);
    let run = fixture.minds(&["spec", "show", "--signers", fixture.signers()]);
    assert!(
        run.stdout
            .contains("state   no valid signature of a trusted minds-spec signer"),
        "{}",
        run.all
    );
    let run = fixture.minds(&[
        "verify",
        "HEAD~1",
        "--commit",
        "HEAD",
        "--signers",
        fixture.signers(),
    ]);
    assert_eq!(run.code, 0, "{}", run.all);
    assert!(
        run.stdout.contains("DRIFT    spec [signature]"),
        "{}",
        run.stdout
    );
}

/// Ein `ssh-keygen`, das der Agent in den Checkout auf `PATH` legt, prüft
/// keine Soll-Signatur: Es wird übergangen, die gepflanzte Signatur fällt
/// beim echten durch.
#[cfg(unix)]
#[test]
fn a_planted_ssh_keygen_does_not_approve_a_spec() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(&[SCOPE_WITNESS_V1], None);
    let root = fixture.root();
    std::fs::create_dir_all(root.join(".minds")).unwrap();
    let text =
        r#"{"version":"minds-spec-v1","rules":[{"id":"s","kind":"graph","expect":"session"}]}"#;
    std::fs::write(root.join(".minds/spec.json"), text).unwrap();
    git(root, &["add", "-A"]);
    git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "spec"],
    );
    let head = git_out(root, &["rev-parse", "HEAD"], None);
    // Der Agent: ein Umschlag mit wertloser Signatur im Store ...
    let spec = minds_core::spec::Spec::parse(text.as_bytes()).unwrap();
    let signed = minds_core::spec::SignedSpec::new(spec, Default::default(), head.trim()).unwrap();
    let redacted = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_spec(signed)
        .unwrap();
    fixture
        .store
        .put_spec(
            &redacted,
            "-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n",
            &|_| true,
        )
        .unwrap();
    // ... und ein `ssh-keygen`, das alles gutheißt, vorn auf PATH.
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("ssh-keygen");
    std::fs::write(&fake, "#!/bin/sh\necho spec@human\nexit 0\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let key = root.join("spec_id");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "spec@human", "-f"])
            .arg(&key)
            .status()
            .unwrap()
            .success()
    );
    let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    let mut signers = std::fs::read_to_string(&fixture.signers).unwrap();
    signers.push_str(&format!(
        "\nspec@human namespaces=\"minds-spec\" {}\n",
        public.trim()
    ));
    std::fs::write(&fixture.signers, signers).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = {
        let mut minds = Command::new(MINDS);
        minds
            .env("MINDS_WITNESS", "off")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        minds
    }
    .current_dir(root)
    .env("HOME", root.join("user-home"))
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .env("PATH", path)
    .args([
        "verify",
        "HEAD~1",
        "--commit",
        "HEAD",
        "--signers",
        fixture.signers(),
    ])
    .output()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("PASS     spec [signature]"), "{stdout}");
    assert!(stdout.contains("DRIFT    spec [signature]"), "{stdout}");
}
