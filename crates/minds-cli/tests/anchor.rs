//! EA-19: `minds anchor` als Binary, gegen ein echtes Repository, mit echtem
//! `ssh-keygen` und echtem `curl` gegen einen GitLab-Stub, der Notes annimmt
//! und wieder ausliefert — und `minds verify`, das die Gegenzeichnung zeigt
//! und mit `--online` einen gelöschten Ref an der Note erkennt.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use minds_core::evidence::{Seal, SealOutcome};
use minds_core::first_sight::FirstSight;
use minds_core::{ContentHash, SessionId};
use minds_store::{ContextStore, FirstSightRef, InRepoStore};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");
const TOKEN: &str = "glpat-AnchorStubT0kenNeverPrinted";
const PRINCIPAL: &str = "minds-anchor@ci";
/// Die Benutzer-Id des Tokens im Stub.
const BOT: u64 = 5;

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
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Ersetzt jede volle Id (`b3-` + 64 Hex) durch `b3-<hash>` und jeden
/// Zeitstempel der CI-Uhr (`20..-..-..T..:..:..Z`) durch `<at>`.
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
    let mut clean = String::new();
    let mut i = 0;
    while i < out.len() {
        let is_stamp = out.get(i..i + 20).is_some_and(|c| {
            let b = c.as_bytes();
            c.starts_with("20")
                && b[4] == b'-'
                && b[7] == b'-'
                && b[10] == b'T'
                && b[13] == b':'
                && b[16] == b':'
                && b[19] == b'Z'
        });
        if is_stamp {
            clean.push_str("<at>");
            i += 20;
        } else {
            let ch = out[i..].chars().next().unwrap();
            clean.push(ch);
            i += ch.len_utf8();
        }
    }
    clean
}

// ---------------------------------------------------------------------------
// GitLab-Stub mit Zustand: angenommene Notes werden wieder ausgeliefert.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Gitlab {
    url: String,
    /// Notes am Merge Request 7: Autor und Text.
    notes: Arc<Mutex<Vec<(u64, String)>>>,
    seen: Arc<Mutex<Vec<(String, String)>>>,
    /// Ein Status, mit dem jede Anfrage scheitert (Header-Echo im Body).
    fail: Arc<Mutex<Option<u16>>>,
    /// Alle Notes gelten als nachträglich bearbeitet.
    edited: Arc<Mutex<bool>>,
    /// `created_at` der Notes des Bots — Default: lange vor jedem Lauf.
    /// Notes anderer Autoren sind immer alt (2020).
    created_at: Arc<Mutex<Option<String>>>,
    /// Der Token hat den Scope `api`.
    api_scope: Arc<Mutex<bool>>,
}

impl Gitlab {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stub = Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            ..Self::default()
        };
        let served = stub.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_owned();
                let path = parts.next().unwrap_or_default().to_owned();
                let mut length = 0usize;
                let mut headers = String::new();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() {
                        break;
                    }
                    let header = header.trim_end().to_owned();
                    if header.is_empty() {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                    headers.push_str(&header);
                    headers.push('\n');
                }
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                let body = String::from_utf8_lossy(&body).into_owned();
                served
                    .seen
                    .lock()
                    .unwrap()
                    .push((method.clone(), path.clone()));
                let (status, reply) = served.answer(&method, &path, &body, &headers);
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        stub
    }

    fn answer(&self, method: &str, path: &str, body: &str, headers: &str) -> (u16, String) {
        if let Some(status) = *self.fail.lock().unwrap() {
            // Ein Server, der die Request-Header spiegelt.
            return (
                status,
                serde_json::json!({ "message": "denied", "headers": headers }).to_string(),
            );
        }
        if !headers.contains(&format!("PRIVATE-TOKEN: {TOKEN}")) {
            return (401, r#"{"message":"401 Unauthorized"}"#.into());
        }
        let notes = "/api/v4/projects/group%2Frepo/merge_requests/7/notes";
        match method {
            "GET" if path == "/api/v4/user" => (200, format!(r#"{{"id":{BOT}}}"#)),
            "GET" if path == "/api/v4/personal_access_tokens/self" => {
                if *self.api_scope.lock().unwrap() {
                    (200, r#"{"id":1,"scopes":["api"]}"#.into())
                } else {
                    (200, r#"{"id":1,"scopes":["read_api"]}"#.into())
                }
            }
            "GET" if path == "/api/v4/projects/group%2Frepo" => {
                (200, r#"{"default_branch":"main"}"#.into())
            }
            "GET" if path.starts_with(&format!("{notes}?")) => {
                let list: Vec<serde_json::Value> = if path.contains("page=1&") {
                    self.notes
                        .lock()
                        .unwrap()
                        .iter()
                        .enumerate()
                        .map(|(id, (author, b))| {
                            let edited = *self.edited.lock().unwrap();
                            serde_json::json!({
                                "id": id,
                                "body": b,
                                "system": false,
                                "author": {"id": author},
                                "created_at": if *author == BOT {
                                    self.created_at.lock().unwrap().clone().unwrap_or_else(|| "2026-01-01T00:00:00Z".into())
                                } else {
                                    "2020-01-01T00:00:00Z".into()
                                },
                                "last_edited_at": if edited { serde_json::json!("2026-01-02T00:00:00Z") } else { serde_json::Value::Null },
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                (200, serde_json::Value::Array(list).to_string())
            }
            "POST" if path == notes => {
                let value: serde_json::Value = serde_json::from_str(body).unwrap();
                self.notes
                    .lock()
                    .unwrap()
                    .push((BOT, value["body"].as_str().unwrap().to_owned()));
                (201, r#"{"id":1}"#.into())
            }
            "GET" if path.contains("/repository/commits/") && path.contains("/merge_requests") => {
                let first = path.contains("page=1");
                (
                    200,
                    if first {
                        r#"[{"iid":7,"state":"merged","target_branch":"main"}]"#
                    } else {
                        "[]"
                    }
                    .into(),
                )
            }
            _ => (404, r#"{"message":"404 Not found"}"#.into()),
        }
    }

    fn posts(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == "POST")
            .count()
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    dir: tempfile::TempDir,
    /// Außerhalb des Repositorys: Schlüssel und Signer-Datei.
    keys: tempfile::TempDir,
    store: InRepoStore,
    /// Der Seal der Session am HEAD-Commit.
    seal: ContentHash,
    /// Der Seal einer anderen Session, deren Commit derselbe Push brachte.
    other: ContentHash,
    /// Der Commit vor dem Push (`CI_COMMIT_BEFORE_SHA`).
    before: String,
}

/// Eine redigierte Session mit `request`, abgelegt.
fn put_session(store: &InRepoStore, request: &str) -> SessionId {
    let session = minds_core::Session::new(
        minds_core::Agent {
            name: "claude-code".into(),
            version: "1".into(),
        },
        minds_core::Model {
            provider: "test".into(),
            id: "test".into(),
        },
        minds_core::Intent {
            request: request.into(),
            ..minds_core::Intent::default()
        },
    );
    let session = minds_redact::RedactionConfig::default()
        .pipeline()
        .unwrap()
        .redact_session(session)
        .unwrap();
    store.put(&session).unwrap().id()
}

/// Ein Seal, der `session` nennt, abgelegt und rückverwiesen.
fn put_seal(store: &InRepoStore, session: SessionId, first_seq: u64) -> ContentHash {
    let mut seal = Seal::parse(include_str!("fixtures/checkpoint-core/epoch-0.seal")).unwrap();
    seal.first_seq = first_seq;
    seal.last_seq = first_seq + 1;
    seal.outcome = SealOutcome::Stored {
        session: session.to_string(),
    };
    let id = store.put_seal(&seal.to_text().unwrap()).unwrap();
    store.record_session_seal(session, &id).unwrap();
    id
}

impl Fixture {
    /// Ein Push aus zwei Commits über einer Wurzel: der erste nennt eine
    /// andere Session, HEAD die geprüfte — je mit einem Seal.
    fn new() -> Self {
        assert!(minds_attest::ssh_keygen_available());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "--template="]);
        git(root, &["config", "user.email", "ci@example.invalid"]);
        git(root, &["config", "user.name", "Anchor Test"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        git(root, &["commit", "-q", "--allow-empty", "-m", "root"]);
        let before = git(root, &["rev-parse", "HEAD"]);
        let store = InRepoStore::open(root).unwrap();

        let other_session = put_session(&store, "earlier work");
        let other = put_seal(&store, other_session, 10);
        git(
            root,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &format!("earlier\n\nMinds-Session-Id: {other_session}"),
            ],
        );
        let id = put_session(&store, "anchor me");
        let seal = put_seal(&store, id, 0);
        std::fs::write(root.join("file.txt"), "x\n").unwrap();
        git(root, &["add", "file.txt"]);
        git(
            root,
            &["commit", "-qm", &format!("work\n\nMinds-Session-Id: {id}")],
        );

        let keys = tempfile::tempdir().unwrap();
        let key = keys.path().join("anchor_ed25519");
        assert!(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", PRINCIPAL, "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        std::fs::write(
            keys.path().join("allowed_signers"),
            format!("{PRINCIPAL} namespaces=\"minds-anchor\" {}", public.trim()),
        )
        .unwrap();
        // Unbeschränkt: nur für den Beweis, dass die Signatur selbst an
        // `minds-anchor` hängt (nicht erst die Beschränkung).
        std::fs::write(
            keys.path().join("unrestricted_signers"),
            format!("{PRINCIPAL} {}", public.trim()),
        )
        .unwrap();
        Self {
            dir,
            keys,
            store,
            seal,
            other,
            before,
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn key(&self) -> PathBuf {
        self.keys.path().join("anchor_ed25519")
    }

    fn signers(&self) -> String {
        self.keys
            .path()
            .join("allowed_signers")
            .to_str()
            .unwrap()
            .to_owned()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(MINDS);
        command
            .current_dir(self.root())
            .args(args)
            .env("HOME", self.root().join("user-home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null");
        // Läuft diese Suite selbst in einer CI, soll deren Lage nicht färben.
        for name in [
            "GITLAB_CI",
            "CI_PROJECT_PATH",
            "CI_PIPELINE_ID",
            "CI_PIPELINE_SOURCE",
            "CI_COMMIT_SHA",
            "CI_COMMIT_BEFORE_SHA",
            "CI_COMMIT_BRANCH",
            "CI_COMMIT_REF_PROTECTED",
            "CI_MERGE_REQUEST_IID",
            "CI_EXTERNAL_PULL_REQUEST_IID",
            "CI_SERVER_URL",
            "CI_DEFAULT_BRANCH",
            "CI_JOB_STARTED_AT",
            "MINDS_ANCHOR_GITLAB_TOKEN",
            "MINDS_ANCHOR_KEY_FILE",
            "MINDS_GITLAB_URL",
            "MINDS_GITLAB_TOKEN",
            "MINDS_GITLAB_PROJECT",
            // Auch die Lage einer GitHub-Actions-CI: Ein `pull_request`
            // färbte sonst jeden Anker-Lauf als Review-Pipeline.
            "GITHUB_ACTIONS",
            "GITHUB_EVENT_NAME",
            "GITHUB_REF_PROTECTED",
            "GITHUB_SHA",
            "GITHUB_SERVER_URL",
            "GITHUB_REPOSITORY",
        ] {
            command.env_remove(name);
        }
        command
    }

    /// `minds anchor` (bzw. `--mirror` gegen `gitlab`) als Push-Pipeline
    /// `pipeline` des geschützten Branches `main`.
    fn anchor_command(&self, pipeline: &str, gitlab: Option<&Gitlab>) -> Command {
        let args: &[&str] = if gitlab.is_some() {
            &["anchor", "--mirror"]
        } else {
            &["anchor"]
        };
        let mut command = self.command(args);
        command
            .env("GITLAB_CI", "true")
            .env("CI_PROJECT_PATH", "group/repo")
            .env("CI_PIPELINE_ID", pipeline)
            .env("CI_PIPELINE_SOURCE", "push")
            .env("CI_COMMIT_REF_PROTECTED", "true")
            .env("CI_COMMIT_BRANCH", "main")
            .env("CI_DEFAULT_BRANCH", "main")
            .env("CI_COMMIT_BEFORE_SHA", &self.before)
            .env("CI_COMMIT_SHA", git(self.root(), &["rev-parse", "HEAD"]))
            .env("MINDS_ANCHOR_KEY_FILE", self.key());
        if let Some(gitlab) = gitlab {
            command
                .env("CI_SERVER_URL", &gitlab.url)
                .env("MINDS_ANCHOR_GITLAB_TOKEN", TOKEN);
        }
        command
    }

    fn anchor(&self, pipeline: &str) -> (i32, String) {
        let out = self.anchor_command(pipeline, None).output().unwrap();
        (out.status.code().unwrap_or(-1), text(&out))
    }

    fn mirror(&self, pipeline: &str, gitlab: &Gitlab) -> (i32, String) {
        let out = self
            .anchor_command(pipeline, Some(gitlab))
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1), text(&out))
    }

    fn verify(&self, extra: &[&str], gitlab: Option<&Gitlab>) -> (i32, String) {
        let mut args = vec!["verify", "HEAD"];
        args.extend_from_slice(extra);
        let mut command = self.command(&args);
        if let Some(gitlab) = gitlab {
            command
                .env("MINDS_GITLAB_URL", &gitlab.url)
                .env("MINDS_GITLAB_PROJECT", "group/repo")
                .env("MINDS_GITLAB_TOKEN", TOKEN);
        }
        let out = command.output().unwrap();
        (out.status.code().unwrap_or(-1), text(&out))
    }

    fn reference(&self, seal: &ContentHash) -> String {
        format!("refs/minds/anchors/first-sight/{}", seal.hex())
    }

    fn anchor_refs(&self) -> String {
        git(
            self.root(),
            &["for-each-ref", "--format=%(refname)", "refs/minds/anchors"],
        )
    }

    fn stored(&self) -> (String, String) {
        match self.store.first_sight(&self.seal).unwrap() {
            FirstSightRef::Present {
                text: Some(text),
                signature: Some(signature),
            } => (text, signature),
            other => panic!("no anchor: {other:?}"),
        }
    }

    /// `ssh-keygen -Y verify` gegen `signers` unter `namespace`.
    fn keygen_verify(&self, signers: &Path, namespace: &str) -> bool {
        let (text, signature) = self.stored();
        let sig = self.keys.path().join("anchor.sig");
        std::fs::write(&sig, signature).unwrap();
        let mut child = Command::new("ssh-keygen")
            .args(["-Y", "verify", "-f"])
            .arg(signers)
            .args(["-I", PRINCIPAL, "-n", namespace, "-s"])
            .arg(&sig)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        child.wait().unwrap().success()
    }

    /// Ein neuer Schlüssel neben dem CI-Schlüssel.
    fn other_key(&self, name: &str) -> PathBuf {
        let key = self.keys.path().join(name);
        assert!(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        key
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Fehlende CI-Variablen: Exit 4, eine klare Meldung, kein Ref.
#[test]
fn anchor_requires_ci_variables() {
    let f = Fixture::new();
    let out = f.command(&["anchor"]).output().unwrap();
    assert_eq!(out.status.code(), Some(4), "{}", text(&out));
    assert_eq!(
        text(&out),
        "minds anchor: missing CI variables: GITLAB_CI=true, CI_PROJECT_PATH, CI_PIPELINE_ID, \
         CI_COMMIT_SHA, MINDS_ANCHOR_KEY_FILE — minds anchor runs only in GitLab CI with the \
         protected anchor key, never with a developer key\n"
    );
    assert_eq!(f.anchor_refs(), "");
    // Spiegeln ohne Token: ebenso ein klarer Abbruch.
    let out = f
        .anchor_command("100", None)
        .arg("--mirror")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "{}", text(&out));
    assert_eq!(
        text(&out),
        "minds anchor: MINDS_ANCHOR_GITLAB_TOKEN is not set — needed for the merge request note\n"
    );
}

/// Security-Review EA-19: In einer Merge-Request-Pipeline stellt der Branch
/// des Agenten die CI-Konfiguration — dort wird nie signiert.
#[test]
fn anchor_refuses_merge_request_pipelines() {
    let f = Fixture::new();
    for (name, value) in [
        ("CI_MERGE_REQUEST_IID", "7"),
        ("CI_PIPELINE_SOURCE", "merge_request_event"),
        ("CI_COMMIT_REF_PROTECTED", "false"),
        ("CI_COMMIT_SHA", "0123456789abcdef0123456789abcdef01234567"),
    ] {
        let out = f
            .anchor_command("100", None)
            .env(name, value)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(4), "{name}: {}", text(&out));
        assert!(text(&out).contains("refusing to sign"), "{}", text(&out));
        assert_eq!(f.anchor_refs(), "", "{name}");
    }
}

/// AC (EA-19): Golden-Text, Signatur nur unter `minds-anchor`, und
/// idempotent — ein zweiter Lauf in derselben und in einer späteren
/// Pipeline schreibt nichts Neues. Gegengezeichnet werden die Seals der
/// Commits dieses Pushs.
#[test]
fn anchor_countersigns_on_first_sight_and_is_idempotent() {
    let f = Fixture::new();
    let (code, out) = f.anchor("100");
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        normalize(&out),
        "anchor   + b3-<hash>\n\
         anchor   + b3-<hash>\n\
         anchor   2 new, 0 already anchored — pipeline #100, <at>\n"
    );
    let (text, _) = f.stored();
    assert_eq!(
        normalize(&text),
        "minds-anchor-v1\nseal=b3-<hash>\nproject=group/repo\npipeline=100\nat=<at>\n"
    );
    let anchor = FirstSight::parse(&text).unwrap();
    assert_eq!(anchor.seal, f.seal);
    assert!(out.contains(&anchor.at), "{out}");
    assert!(out.contains(&f.other.to_string()), "{out}");

    // Die Signatur hängt an `minds-anchor` — auch gegen eine unbeschränkte
    // Signer-Zeile verifiziert sie unter keinem anderen Namespace.
    let unrestricted = f.keys.path().join("unrestricted_signers");
    assert!(f.keygen_verify(&unrestricted, "minds-anchor"));
    for other in ["minds", "minds-witness", "minds-intent"] {
        assert!(!f.keygen_verify(&unrestricted, other), "{other}");
    }

    let before = f.anchor_refs();
    let tip = git(f.root(), &["rev-parse", &f.reference(&f.seal)]);
    for pipeline in ["100", "101"] {
        let (code, out) = f.anchor(pipeline);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            normalize(&out),
            format!("anchor   0 new, 2 already anchored — pipeline #{pipeline}, <at>\n")
        );
    }
    assert_eq!(f.anchor_refs(), before);
    assert_eq!(git(f.root(), &["rev-parse", &f.reference(&f.seal)]), tip);
    // Git-unsichtbar: nur unter `refs/minds/`, `fsck` sauber.
    git(f.root(), &["fsck", "--strict", "--no-progress"]);

    // Ohne `CI_COMMIT_BEFORE_SHA` zählt nur HEAD (erster Elternteil).
    let only_head = Fixture::new();
    let out = only_head
        .anchor_command("100", None)
        .env_remove("CI_COMMIT_BEFORE_SHA")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", self::text(&out));
    assert_eq!(
        only_head.anchor_refs(),
        only_head.reference(&only_head.seal)
    );
}

/// Security-Review EA-19: Ein vorab belegter Ref — fremd signiert oder mit
/// dem öffentlichen CI-Schlüssel, aber ohne gültige Signatur — wird nie
/// überschrieben, aber laut gemeldet und nie in die Note übernommen.
#[test]
fn a_squatted_first_sight_ref_is_reported_not_overwritten() {
    let f = Fixture::new();
    let forger = f.other_key("forger");
    let squat = FirstSight {
        seal: f.seal.clone(),
        project: "group/repo".into(),
        pipeline: 100,
        at: "2020-01-01T00:00:00Z".into(),
    };
    let signature =
        minds_attest::ssh_sign_ns(&squat.to_text().unwrap(), &forger, minds_attest::NS_ANCHOR)
            .unwrap();
    assert!(f.store.put_first_sight(&squat, &signature).unwrap());
    // Für den anderen Seal: eine echte CI-Signatur — über einen anderen
    // Text. Sie nennt den CI-Schlüssel, gilt hier aber nicht.
    let borrowed =
        minds_attest::ssh_sign_ns("something else\n", &f.key(), minds_attest::NS_ANCHOR).unwrap();
    let claim = FirstSight {
        seal: f.other.clone(),
        ..squat.clone()
    };
    assert!(f.store.put_first_sight(&claim, &borrowed).unwrap());

    let (code, out) = f.anchor("100");
    assert_eq!(code, 4, "{out}");
    assert_eq!(
        normalize(&out),
        "anchor   kept b3-<hash>: its first-sight ref holds a countersignature not made with \
         this anchor key (claims pipeline #100) — never overwritten\n\
         anchor   kept b3-<hash>: its first-sight ref holds a countersignature not made with \
         this anchor key (claims pipeline #100) — never overwritten\n\
         anchor   0 new, 0 already anchored, 2 occupied by foreign or unreadable refs — \
         pipeline #100, <at>\n\
         minds anchor: 2 seal(s) of this push cannot be anchored: their first-sight ref is \
         occupied by something this key did not sign\n"
    );
    assert_eq!(f.stored().0, squat.to_text().unwrap());

    // Die Note gibt keinen der beiden wieder.
    let gitlab = Gitlab::start();
    let (code, out) = f.mirror("100", &gitlab);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        out,
        "anchor   nothing anchored for this push — no merge request note\n"
    );
    assert_eq!(gitlab.posts(), 0);

    // `verify` zählt den fremden nicht.
    let (_, out) = f.verify(&["--signers", &f.signers()], None);
    assert!(
        out.contains(
            "anchor not signed under minds-anchor (pipeline #100, 2020-01-01T00:00:00Z claimed) — not counted"
        ),
        "{out}"
    );
}

/// `verify` zeigt die Gegenzeichnung je Seal — gezählt nur mit gültiger
/// Signatur eines auf `minds-anchor` beschränkten Principals.
#[test]
fn verify_shows_the_anchor_per_seal() {
    let f = Fixture::new();
    assert_eq!(f.anchor("100").0, 0);
    let (text, _) = f.stored();
    let at = FirstSight::parse(&text).unwrap().at;

    let (_, out) = f.verify(&["--signers", &f.signers()], None);
    assert!(
        out.contains(&format!("\n               anchored: pipeline #100, {at}\n")),
        "{out}"
    );
    let (_, out) = f.verify(&[], None);
    assert!(
        out.contains(&format!(
            "\n               anchor claims pipeline #100, {at} (signature not checked — verify with --signers)\n"
        )),
        "{out}"
    );
    // Ein Signer, der nicht auf `minds-anchor` beschränkt ist, zählt nicht.
    let unrestricted = f.keys.path().join("unrestricted_signers");
    let (_, out) = f.verify(&["--signers", unrestricted.to_str().unwrap()], None);
    assert!(
        out.contains("anchor not signed under minds-anchor (pipeline #100, "),
        "{out}"
    );
}

/// AC (EA-19): Der gelöschte Ref fällt mit `--online` an der Note auf —
/// als Integritätsbefund; ebenso ein gelöschter Seal. Die Note entsteht
/// einmal je Pipeline, nach dem Signieren.
#[test]
fn a_deleted_anchor_ref_with_an_online_note_is_tampered() {
    let f = Fixture::new();
    let gitlab = Gitlab::start();
    assert_eq!(f.anchor("100").0, 0);
    let (code, out) = f.mirror("100", &gitlab);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, "anchor   MR !7: note posted (2 seal(s))\n");
    assert_eq!(gitlab.posts(), 1);
    let note = gitlab.notes.lock().unwrap()[0].1.clone();
    assert!(note.starts_with("<!-- minds:anchor:100:1 -->"), "{note}");
    assert!(note.contains(&f.seal.to_string()), "{note}");

    // Wiederholt (Job-Retry): keine zweite Note.
    let (code, out) = f.mirror("100", &gitlab);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, "anchor   MR !7: note already there\n");
    assert_eq!(gitlab.posts(), 1);

    let signers = f.signers();
    let (code, out) = f.verify(&["--online", "--signers", &signers], Some(&gitlab));
    assert_ne!(code, 1, "{out}");
    assert!(
        out.contains("Anchor notes   checked (1 MR note entry for this session)\n"),
        "{out}"
    );

    // Jemand löscht den Ref (Push-Recht auf `refs/minds/*` genügt).
    git(f.root(), &["update-ref", "-d", &f.reference(&f.seal)]);
    let (code, out) = f.verify(&["--online", "--signers", &signers], Some(&gitlab));
    assert_eq!(code, 1, "{out}");
    assert!(
        normalize(&out).contains(
            "Integrity      VIOLATED  seal b3-<hash>: anchor ref missing, MR note present (pipeline #100)\n"
        ),
        "{out}"
    );
    assert!(out.contains("\nOverall        TAMPERED\n"), "{out}");
    // Offline sieht man es nicht — und behauptet nichts.
    let (code, out) = f.verify(&["--signers", &signers], None);
    assert_ne!(code, 1, "{out}");
    assert!(!out.contains("Anchor notes"), "{out}");
    assert!(!out.contains(TOKEN), "{out}");

    // Ein gelöschter Seal (andere Session, gleiche Note): fällt genauso
    // auf — mehr zu löschen hilft nicht.
    git(
        f.root(),
        &[
            "update-ref",
            "-d",
            &format!("refs/minds/evidence/{}", f.other.hex()),
        ],
    );
    let (code, out) = f.verify(&["--online", "--signers", &signers], Some(&gitlab));
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains(&format!(
            "seal {}: seal missing, MR anchor note present (pipeline #100)",
            f.other
        )),
        "{out}"
    );

    // Security-Review EA-19: Ein Seal-Ref, der auf Fremdes zeigt, ist so weg
    // wie ein gelöschter — auch wenn der Name noch da ist.
    let fresh = Fixture::new();
    // Ein eigenes GitLab: Die Notes von `f` trügen fremd signierte Einträge.
    let fresh_gitlab = Gitlab::start();
    assert_eq!(fresh.anchor("100").0, 0);
    assert_eq!(fresh.mirror("100", &fresh_gitlab).0, 0);
    let foreign = git(
        fresh.root(),
        &[
            "rev-parse",
            &format!("refs/minds/evidence/{}", fresh.seal.hex()),
        ],
    );
    git(
        fresh.root(),
        &[
            "update-ref",
            &format!("refs/minds/evidence/{}", fresh.other.hex()),
            &foreign,
        ],
    );
    let (code, out) = fresh.verify(
        &["--online", "--signers", &fresh.signers()],
        Some(&fresh_gitlab),
    );
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains(&format!(
            "seal {}: seal missing, MR anchor note present (pipeline #100)",
            fresh.other
        )),
        "{out}"
    );

    // Alle Anker gelöscht (oder nie geholt): nie grün, nie geraten — Exit 4.
    for reference in f.anchor_refs().lines() {
        git(f.root(), &["update-ref", "-d", reference]);
    }
    let (code, out) = f.verify(&["--online", "--signers", &signers], Some(&gitlab));
    assert_eq!(code, 4, "{out}");
    assert!(
        out.contains("may be older than the MR note — fetch refs/minds/* and rerun; if they were fetched, they were deleted"),
        "{out}"
    );
}

/// Ohne geholte Anker und ohne Note heißt ein fehlender Ref nichts: nicht
/// geprüft. Scheitert die Prüfung selbst, ist das Exit 4 — nie grün.
#[test]
fn an_online_check_that_cannot_judge_never_passes_silently() {
    let f = Fixture::new();
    let gitlab = Gitlab::start();
    let signers = f.signers();
    let (code, out) = f.verify(&["--online", "--signers", &signers], Some(&gitlab));
    assert_ne!(code, 1, "{out}");
    assert!(
        out.contains(
            "Anchor notes   not checked (no refs/minds/anchors/first-sight in this clone — fetch refs/minds/* first)"
        ),
        "{out}"
    );

    assert_eq!(f.anchor("100").0, 0);
    *gitlab.fail.lock().unwrap() = Some(500);
    let (code, out) = f.verify(&["--online", "--signers", &signers], Some(&gitlab));
    assert_eq!(code, 4, "{out}");
    assert!(out.contains("Anchor notes   check failed ("), "{out}");
    assert!(out.contains("anchor notes could not be checked"), "{out}");
    assert!(!out.contains(TOKEN), "{out}");
}

/// Eine Note, die jemand anders schreibt (ohne den CI-Schlüssel), macht nie
/// einen Befund — auch nicht, wenn sie den öffentlichen CI-Schlüssel nennt.
#[test]
fn a_forged_note_never_makes_a_finding() {
    let f = Fixture::new();
    let gitlab = Gitlab::start();
    assert_eq!(f.anchor("100").0, 0);
    git(f.root(), &["update-ref", "-d", &f.reference(&f.seal)]);
    let forger = f.other_key("forger");
    let anchor = FirstSight {
        seal: f.seal.clone(),
        project: "group/repo".into(),
        pipeline: 100,
        at: "2026-10-08T12:00:00Z".into(),
    };
    let text = anchor.to_text().unwrap();
    let mut forged = Vec::new();
    for signature in [
        minds_attest::ssh_sign_ns(&text, &forger, minds_attest::NS_ANCHOR).unwrap(),
        minds_attest::ssh_sign_ns("other\n", &f.key(), minds_attest::NS_ANCHOR).unwrap(),
    ] {
        forged.push(minds_gitlab::anchor::Entry {
            text: text.clone(),
            signature,
        });
    }
    for entry in forged {
        let body = minds_gitlab::anchor::note_bodies(100, &[entry]).remove(0).1;
        gitlab.notes.lock().unwrap().push((999, body));
    }

    let (code, out) = f.verify(&["--online", "--signers", &f.signers()], Some(&gitlab));
    // Keine gültige Note: nie ein Befund — aber auch nie „geprüft": Die
    // Fälschungen beweisen nichts, der Abgleich sagt nichts (Exit 4).
    assert_eq!(code, 4, "{out}");
    assert!(!out.contains("anchor ref missing"), "{out}");
    assert!(
        out.contains(
            "Anchor notes   check failed (2 MR note entries could not be validated — pass \
             --signers with a principal restricted to minds-anchor"
        ),
        "{out}"
    );
}

/// AC (EA-19): Weder Token noch Schlüsselpfad erscheinen — nicht im Erfolg,
/// nicht bei einem GitLab, das die Header spiegelt, nicht bei einem
/// kaputten Schlüssel, dessen Pfad `ssh-keygen` zitiert.
#[test]
fn anchor_never_prints_the_token_or_the_key_path() {
    let f = Fixture::new();
    let gitlab = Gitlab::start();
    let key = f.key();
    let key_path = key.to_str().unwrap();
    let key_dir = f.keys.path().to_str().unwrap();
    let assert_clean = |out: &str| {
        assert!(!out.contains(TOKEN), "{out}");
        assert!(!out.contains(key_path), "{out}");
        assert!(!out.contains(key_dir), "{out}");
    };

    let (code, out) = f.anchor("100");
    assert_eq!(code, 0, "{out}");
    assert_clean(&out);
    *gitlab.fail.lock().unwrap() = Some(403);
    let (code, out) = f.mirror("100", &gitlab);
    assert_eq!(code, 4, "{out}");
    assert!(out.contains("merge request note not posted"), "{out}");
    assert!(out.contains("403"), "{out}");
    assert_clean(&out);

    // Die Refs liegen (die Gegenzeichnung ist unabhängig von GitLab); ein
    // neuer Versuch derselben Pipeline holt die Note nach.
    *gitlab.fail.lock().unwrap() = None;
    let (code, out) = f.mirror("100", &gitlab);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, "anchor   MR !7: note posted (2 seal(s))\n");
    assert_clean(&out);

    // Ein Schlüssel, den `ssh-keygen` nicht laden kann: Seine Meldung nennt
    // den Pfad — ersetzt. Nichts wird geschrieben.
    let fresh = Fixture::new();
    std::fs::write(
        fresh.key(),
        "-----BEGIN OPENSSH PRIVATE KEY-----\nbroken\n-----END OPENSSH PRIVATE KEY-----\n",
    )
    .unwrap();
    let (code, out) = fresh.anchor("100");
    assert_eq!(code, 4, "{out}");
    assert!(out.contains("the anchor key cannot be read"), "{out}");
    assert!(!out.contains(fresh.key().to_str().unwrap()), "{out}");
    assert!(!out.contains(fresh.keys.path().to_str().unwrap()), "{out}");
    assert_eq!(fresh.anchor_refs(), "");

    // Eine öffentliche Schlüsseldatei signierte über den ssh-agent: nie.
    let public = Fixture::new();
    std::fs::copy(public.key().with_extension("pub"), public.key()).unwrap();
    let (code, out) = public.anchor("100");
    assert_eq!(code, 4, "{out}");
    assert!(out.contains("does not name a private key file"), "{out}");
    assert_eq!(public.anchor_refs(), "");
}

/// Ein Schlüssel im Repository (den ein Agent beschreiben kann) wird nie
/// benutzt.
#[test]
fn a_key_inside_the_repository_is_refused() {
    let f = Fixture::new();
    let inside = f.root().join(".git/anchor_ed25519");
    std::fs::copy(f.key(), &inside).unwrap();
    let out = f
        .anchor_command("100", None)
        .env("MINDS_ANCHOR_KEY_FILE", &inside)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "{}", text(&out));
    assert_eq!(
        text(&out),
        "minds anchor: MINDS_ANCHOR_KEY_FILE points into the repository — refusing to sign\n"
    );
    assert_eq!(f.anchor_refs(), "");
}

/// AC (EA-19): Auch die Note entsteht nur einmal — ein Spiegeln in einer
/// späteren Pipeline über denselben Push schickt nichts Neues.
#[test]
fn a_later_pipeline_mirrors_nothing_new() {
    let f = Fixture::new();
    let gitlab = Gitlab::start();
    assert_eq!(f.anchor("100").0, 0);
    assert_eq!(f.mirror("100", &gitlab).0, 0);
    assert_eq!(gitlab.posts(), 1);
    let (code, out) = f.anchor("101");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("0 new, 2 already anchored"), "{out}");
    let (code, out) = f.mirror("101", &gitlab);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, "anchor   MR !7: note already there\n");
    assert_eq!(gitlab.posts(), 1);
}

/// Code-Review EA-19: Ein Klon, dessen Anker älter sind als die Note (nicht
/// neu geholt), ist nie TAMPERED — und nie grün: Exit 4 mit dem Hinweis,
/// neu zu holen.
#[test]
fn a_stale_clone_is_not_tampered() {
    let f = Fixture::new();
    let gitlab = Gitlab::start();
    assert_eq!(f.anchor("100").0, 0);
    assert_eq!(f.mirror("100", &gitlab).0, 0);

    // Ein zweiter Push: neue Session, neuer Seal, Pipeline 101.
    let previous = git(f.root(), &["rev-parse", "HEAD"]);
    let id = put_session(&f.store, "later work");
    let later = put_seal(&f.store, id, 20);
    git(
        f.root(),
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("later\n\nMinds-Session-Id: {id}"),
        ],
    );
    for args in [None, Some(&gitlab)] {
        let out = f
            .anchor_command("101", args)
            .env("CI_COMMIT_BEFORE_SHA", &previous)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    }
    assert_eq!(gitlab.posts(), 2);

    // Der Klon hat die Refs von Pipeline 101 nicht geholt — und kein Anker
    // im Klon wurde nach der Note gezeichnet.
    *gitlab.created_at.lock().unwrap() = Some("2099-01-01T00:00:00Z".into());
    git(f.root(), &["update-ref", "-d", &f.reference(&later)]);
    let (code, out) = f.verify(&["--online", "--signers", &f.signers()], Some(&gitlab));
    assert_eq!(code, 4, "{out}");
    assert!(
        out.contains(
            "older than the MR note — fetch refs/minds/* and rerun; if they were fetched, they \
             were deleted: seal "
        ),
        "{out}"
    );
    assert!(
        out.contains("anchor ref missing, MR note present (pipeline #101)"),
        "{out}"
    );
    assert!(!out.contains("TAMPERED"), "{out}");
}

/// Security-Review EA-19: Eine Anker-Note schreibt `--mirror` einmal — eine
/// bearbeitete hat jemand anders angefasst: nie still „geprüft".
#[test]
fn an_edited_anchor_note_fails_the_check() {
    let f = Fixture::new();
    let gitlab = Gitlab::start();
    assert_eq!(f.anchor("100").0, 0);
    assert_eq!(f.mirror("100", &gitlab).0, 0);
    *gitlab.edited.lock().unwrap() = true;
    let (code, out) = f.verify(&["--online", "--signers", &f.signers()], Some(&gitlab));
    assert_eq!(code, 4, "{out}");
    assert!(
        out.contains("merge request note(s) with anchor entries were edited after posting"),
        "{out}"
    );
}

/// In GitLab-CI belegt die Zeit, dass ein Ref gelöscht wurde: Eine Note,
/// die vor dem Start des Jobs angelegt wurde, nennt einen Ref, der beim
/// Fetch schon lag — kein „veralteter Klon", sondern TAMPERED. (Fehlen
/// **alle** Anker, ist eher der Fetch unvollständig: Exit 4.)
#[test]
fn in_ci_a_note_older_than_the_job_proves_the_deletion() {
    let f = Fixture::new();
    let gitlab = Gitlab::start();
    assert_eq!(f.anchor("100").0, 0);
    assert_eq!(f.mirror("100", &gitlab).0, 0);
    git(f.root(), &["update-ref", "-d", &f.reference(&f.seal)]);
    let ci = |started: &str| {
        let mut command = f.command(&["verify", "HEAD", "--online", "--signers", &f.signers()]);
        command
            .env("GITLAB_CI", "true")
            .env("CI_SERVER_URL", &gitlab.url)
            .env("CI_PROJECT_PATH", "group/repo")
            .env("CI_DEFAULT_BRANCH", "main")
            .env("CI_JOB_STARTED_AT", started)
            .env("MINDS_GITLAB_TOKEN", TOKEN);
        let out = command.output().unwrap();
        (out.status.code().unwrap_or(-1), text(&out))
    };
    // Der Job begann nach der Note: gelöscht.
    let (code, out) = ci("2030-01-01T00:00:00Z");
    assert_eq!(code, 1, "{out}");
    assert!(
        normalize(&out).contains(
            "Integrity      VIOLATED  seal b3-<hash>: anchor ref missing, MR note present (pipeline #100)"
        ),
        "{out}"
    );
    // Der Job begann vor der Note — aber ein gültiger Anker im Klon wurde
    // deutlich nach ihr gezeichnet: Geholt wurde nach der Note, also ist der
    // Ref gelöscht. (Ohne einen solchen Anker: `a_stale_clone_is_not_tampered`,
    // `an_old_forged_comment_never_proves_a_deletion`.)
    let (code, out) = ci("2025-01-01T00:00:00Z");
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("anchor ref missing, MR note present"), "{out}");
}

/// `verify --online` in GitLab-CI (Pipeline-Variablen gesetzt).
fn verify_in_ci(f: &Fixture, gitlab: &Gitlab, started: &str) -> (i32, String) {
    let mut command = f.command(&["verify", "HEAD", "--online", "--signers", &f.signers()]);
    command
        .env("GITLAB_CI", "true")
        .env("CI_SERVER_URL", &gitlab.url)
        .env("CI_PROJECT_PATH", "group/repo")
        .env("CI_DEFAULT_BRANCH", "main")
        .env("CI_JOB_STARTED_AT", started)
        .env("MINDS_GITLAB_TOKEN", TOKEN);
    let out = command.output().unwrap();
    (out.status.code().unwrap_or(-1), text(&out))
}

/// Review EA-19: Ein alter Kommentar mit Müll-Signatur beweist nie, dass
/// ein Ref beim Fetch hätte da sein müssen — kein falsches TAMPERED.
#[test]
fn an_old_forged_comment_never_proves_a_deletion() {
    let f = Fixture::new();
    let gitlab = Gitlab::start();
    assert_eq!(f.anchor("100").0, 0);
    // Die echte Note entsteht erst nach dem Start des prüfenden Jobs.
    *gitlab.created_at.lock().unwrap() = Some("2099-01-01T00:00:00Z".into());
    assert_eq!(f.mirror("100", &gitlab).0, 0);
    // Vorab gepostet: ein Eintrag für den Seal mit Müll-Signatur.
    let (text, _) = f.stored();
    let junk = minds_gitlab::anchor::note_bodies(
        100,
        &[minds_gitlab::anchor::Entry {
            text,
            signature: "-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n".into(),
        }],
    )
    .remove(0)
    .1;
    gitlab.notes.lock().unwrap().push((999, junk));
    // Der Klon des Jobs hat den Ref nicht (geholt vor dem Push).
    git(f.root(), &["update-ref", "-d", &f.reference(&f.seal)]);
    let (code, out) = verify_in_ci(&f, &gitlab, "2030-01-01T00:00:00Z");
    assert_eq!(code, 4, "{out}");
    assert!(out.contains("may be older than the MR note"), "{out}");
    assert!(!out.contains("Integrity      VIOLATED"), "{out}");
}

/// Security-Review EA-19: In CI darf der Token von `verify` nur lesen — mit
/// `api` könnte der Agent (der ihn im Merge Request sieht) die Notes des
/// Bots löschen.
#[test]
fn in_ci_a_writing_verify_token_is_refused() {
    let f = Fixture::new();
    let gitlab = Gitlab::start();
    assert_eq!(f.anchor("100").0, 0);
    assert_eq!(f.mirror("100", &gitlab).0, 0);
    *gitlab.api_scope.lock().unwrap() = true;
    let (code, out) = verify_in_ci(&f, &gitlab, "2030-01-01T00:00:00Z");
    assert_eq!(code, 4, "{out}");
    assert!(
        out.contains(
            "MINDS_GITLAB_TOKEN has the api scope — in CI, verify --online needs a read_api token"
        ),
        "{out}"
    );
    assert!(!out.contains(TOKEN), "{out}");
}
