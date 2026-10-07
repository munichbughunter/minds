//! EA-16: `minds intent bind --issue` bindet ein GitLab-Issue in seiner
//! heutigen Fassung; `minds verify --online` prüft, ob es diese Fassung
//! (noch) gibt. Gegen einen lokalen Stub, der wie GitLab antwortet — mit
//! einem echten `curl` dazwischen.

#![cfg(unix)]

#[path = "support/hook_witness.rs"]
mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use minds_core::intent_anchor::{IntentAnchor, IntentSource, content_hash, issue_snapshot};
use support::*;

const TOKEN: &str = "glpat-StubT0kenNeverPrinted";
const ISSUE: &str = include_str!("../../minds-gitlab/fixtures/issue_42.json");
const ISSUE_CHANGED: &str = include_str!("../../minds-gitlab/fixtures/issue_42_changed.json");
const HISTORY: &str = include_str!("../../minds-gitlab/fixtures/graphql_history.json");
const NO_HISTORY_FIELD: &str =
    include_str!("../../minds-gitlab/fixtures/graphql_unknown_field.json");
const UNAUTHORIZED: &str = include_str!("../../minds-gitlab/fixtures/error_401.json");
const NOT_FOUND: &str = include_str!("../../minds-gitlab/fixtures/error_404.json");

const TITLE: &str = "Retry mit exponentiellem Backoff";
const DESCRIPTION: &str = "Max. 5 Versuche, 200 ms Basis.";
const UPDATED_AT: &str = "2026-10-01T08:15:00.123Z";

/// Eine Route: Methode, Pfad, Status, Body.
type Route = (String, String, u16, String);

/// Ein empfangener Request: Methode, Pfad, Header.
type Seen = (String, String, Vec<String>);

/// Ein Prüffall: erwartete Zeile, Issue-Antwort, GraphQL-Antwort, Token.
type Case<'a> = (
    &'a str,
    (u16, &'a str),
    Option<(u16, &'a str)>,
    Option<&'a str>,
);

/// Ein Stub, der je Route antwortet: `(Methode, Pfad) → (Status, Body)`.
/// Die Antworten lassen sich zwischen zwei Aufrufen austauschen; jeder
/// Request wird festgehalten.
#[derive(Clone)]
struct Stub {
    url: String,
    routes: Arc<Mutex<Vec<Route>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stub = Self {
            url,
            routes: Arc::default(),
            seen: Arc::default(),
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
                let mut headers = Vec::new();
                let mut length = 0usize;
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
                    headers.push(header);
                }
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                served
                    .seen
                    .lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), headers));
                let (status, body) = served
                    .routes
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(m, p, _, _)| *m == method && *p == path)
                    .map(|(_, _, s, b)| (*s, b.clone()))
                    .unwrap_or((404, r#"{"message":"404 Not found"}"#.to_owned()));
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        stub
    }

    /// Setzt die Antworten für das Issue und GraphQL neu.
    fn serve(&self, issue: (u16, &str), graphql: Option<(u16, &str)>) {
        let mut routes = self.routes.lock().unwrap();
        routes.clear();
        routes.push((
            "GET".into(),
            "/api/v4/projects/team%2Fminds/issues/42".into(),
            issue.0,
            issue.1.into(),
        ));
        if let Some((status, body)) = graphql {
            routes.push(("POST".into(), "/api/graphql".into(), status, body.into()));
        }
        self.seen.lock().unwrap().clear();
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn git_command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Human")
        .env("GIT_AUTHOR_EMAIL", "human@example.invalid")
        .env("GIT_COMMITTER_NAME", "Human")
        .env("GIT_COMMITTER_EMAIL", "human@example.invalid")
        .env_remove(SOCKET_ENV);
    cmd
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = git_command(root).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// `minds` im Repo, ohne GitLab-Umgebung des Testlaufs; `token` setzt
/// `MINDS_GITLAB_TOKEN`.
fn minds_in(f: &Fixture, token: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = minds();
    cmd.current_dir(&f.root)
        .env("HOME", f.dir.path())
        .env("XDG_STATE_HOME", f.dir.path().join("state"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("MINDS_WITNESS_HOME")
        .env_remove("MINDS_GITLAB_TOKEN")
        .env_remove("MINDS_GITLAB_URL")
        .env_remove("CI_SERVER_URL")
        .args(args);
    if let Some(token) = token {
        cmd.env("MINDS_GITLAB_TOKEN", token);
    }
    cmd.output().unwrap()
}

fn repo() -> Fixture {
    let f = Fixture::repo_only();
    git(&f.root, &["config", "user.name", "Human"]);
    git(&f.root, &["config", "user.email", "human@example.invalid"]);
    f
}

/// Der erwartete Anker über das unveränderte Issue (sauber: der Snapshot
/// ist die kanonische Rohform).
fn expected(scope: &[&str]) -> (IntentAnchor, String) {
    let anchor = IntentAnchor {
        source: IntentSource::Issue {
            project: "team/minds".into(),
            iid: 42,
            updated_at: UPDATED_AT.into(),
        },
        content: content_hash(&issue_snapshot(TITLE, DESCRIPTION)),
        scope: scope.iter().map(|s| (*s).to_owned()).collect(),
    };
    let id = IntentAnchor::id_of_text(&anchor.to_text().unwrap()).to_string();
    (anchor, id)
}

fn bind(f: &Fixture, stub: &Stub, token: Option<&str>) -> Output {
    minds_in(
        f,
        token,
        &[
            "intent",
            "bind",
            "--issue",
            "team/minds#42",
            "--scope",
            "src/retry/**",
            "--gitlab-url",
            &stub.url,
        ],
    )
}

#[test]
fn intent_bind_issue_golden() {
    let f = repo();
    let stub = Stub::start();
    stub.serve((200, ISSUE), None);
    let out = bind(&f, &stub, Some(TOKEN));
    assert!(out.status.success(), "{}", text(&out));
    let (anchor, id) = expected(&["src/retry/**"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "intent  issue:team/minds#42@{UPDATED_AT}  content {}…\n\
             scope   src/retry/**\n\
             anchor  {id}\n\
             issue   https://gitlab.example/team/minds/-/issues/42\n",
            &anchor.content.as_str()[..7],
        )
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");

    // Genau ein Aufruf, mit dem Token im Header — nirgends sonst.
    let requests = stub.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].0, "GET");
    assert!(
        requests[0]
            .2
            .iter()
            .any(|h| h == &format!("PRIVATE-TOKEN: {TOKEN}"))
    );

    // Abgelegt unter refs/minds/intents/, fsck sauber; der Snapshot ist das
    // kanonische JSON.
    assert_eq!(
        git(
            &f.root,
            &["for-each-ref", "--format=%(refname)", "refs/minds/"]
        ),
        format!("refs/minds/intents/{}", id.trim_start_matches("b3-"))
    );
    let fsck = git_command(&f.root)
        .args(["fsck", "--strict"])
        .output()
        .unwrap();
    assert!(fsck.status.success(), "{}", text(&fsck));
    let show = minds_in(&f, None, &["intent", "show"]);
    let shown = text(&show);
    assert!(shown.contains("proof   ok\n"), "{shown}");
    assert!(
        shown.contains(&format!(
            "  {}\n",
            String::from_utf8(issue_snapshot(TITLE, DESCRIPTION)).unwrap()
        )),
        "{shown}"
    );
}

/// Ein Secret im Issue: Es wird redigiert, bevor es gehasht und abgelegt
/// wird — und `bind` sagt es.
#[test]
fn intent_bind_issue_redacts_secrets() {
    let f = repo();
    let stub = Stub::start();
    let secret = concat!("ghp_", "R4nd0mT0k3nV4lu3F0rT3st1ngPurp0s3s00");
    let body = serde_json::json!({
        "iid": 42,
        "title": "Token rotieren",
        "description": format!("Der alte Token war {secret}.\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAx7Vn9pQmKtLb\n-----END RSA PRIVATE KEY-----\n"),
        "updated_at": UPDATED_AT,
        "web_url": "https://gitlab.example/team/minds/-/issues/42",
        "confidential": true,
    })
    .to_string();
    stub.serve((200, &body), None);
    // Vertraulich: ohne Zustimmung kein Anker, nichts abgelegt.
    let refused = bind(&f, &stub, Some(TOKEN));
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(
        text(&refused).contains("--allow-confidential"),
        "{}",
        text(&refused)
    );
    assert_eq!(
        git(
            &f.root,
            &["for-each-ref", "--format=%(refname)", "refs/minds/"]
        ),
        ""
    );
    let out = minds_in(
        &f,
        Some(TOKEN),
        &[
            "intent",
            "bind",
            "--issue",
            "team/minds#42",
            "--gitlab-url",
            &stub.url,
            "--allow-confidential",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("the anchor binds the redacted version"),
        "{}",
        text(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is confidential"),
        "{}",
        text(&out)
    );
    let dump = git(&f.root, &["log", "--all", "-p", "--format=%B", "--", "."]);
    let objects = git(&f.root, &["rev-list", "--objects", "--all"]);
    for needle in [secret, "MIIEowIBAAKCAQEAx7Vn9pQmKtLb"] {
        assert!(!dump.contains(needle), "{dump}");
        for line in objects.lines() {
            let sha = line.split_whitespace().next().unwrap();
            let content = git_command(&f.root)
                .args(["cat-file", "-p", sha])
                .output()
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&content.stdout).contains(needle),
                "secret in object {sha}"
            );
        }
    }
}

/// Fixtures 401 und 404, fehlender Token, unzulässige Instanz — jedes Mal
/// kein Anker, und der Token steht nie in der Ausgabe.
#[test]
fn intent_bind_issue_refusals_never_print_the_token() {
    let f = repo();
    let stub = Stub::start();

    stub.serve((401, UNAUTHORIZED), None);
    let out = bind(&f, &stub, Some(TOKEN));
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("unauthorized (HTTP 401)"),
        "{}",
        text(&out)
    );
    assert!(!text(&out).contains(TOKEN), "{}", text(&out));

    // Ein Server, der den Token in seiner Antwort spiegelt.
    let echo = format!(r#"{{"message":"401 Unauthorized","token":"{TOKEN}"}}"#);
    stub.serve((401, &echo), None);
    let out = bind(&f, &stub, Some(TOKEN));
    assert!(!text(&out).contains(TOKEN), "{}", text(&out));

    stub.serve((404, NOT_FOUND), None);
    let out = bind(&f, &stub, Some(TOKEN));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out).contains("issue not found (HTTP 404)"),
        "{}",
        text(&out)
    );

    // Ohne Token: benannt, keine Anfrage.
    stub.serve((200, ISSUE), None);
    let out = bind(&f, &stub, None);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).contains("MINDS_GITLAB_TOKEN"), "{}", text(&out));
    assert!(stub.requests().is_empty());

    // Klartext-HTTP zu einem fremden Host: abgelehnt, bevor etwas hinausgeht.
    let out = minds_in(
        &f,
        Some(TOKEN),
        &[
            "intent",
            "bind",
            "--issue",
            "team/minds#42",
            "--gitlab-url",
            "http://gitlab.example.com",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).contains("https://"), "{}", text(&out));

    // `.git/config` nennt keine Instanz: Der Agent könnte sie umbiegen.
    git(&f.root, &["config", "minds.gitlabUrl", &stub.url]);
    let out = minds_in(
        &f,
        Some(TOKEN),
        &["intent", "bind", "--issue", "team/minds#42"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).contains("no GitLab instance"), "{}", text(&out));
    assert!(stub.requests().is_empty());

    // Eine Referenz, die keine ist.
    for bad in ["team/minds", "team/minds#0", "team%2Fminds#1"] {
        let out = minds_in(&f, Some(TOKEN), &["intent", "bind", "--issue", bad]);
        assert_eq!(out.status.code(), Some(1), "{bad}");
        assert!(text(&out).contains("--issue expects"), "{}", text(&out));
    }
    let out = minds_in(
        &f,
        Some(TOKEN),
        &[
            "intent",
            "bind",
            "--issue",
            "team/minds#42",
            "--file",
            "x.md",
        ],
    );
    assert!(text(&out).contains("exclude each other"), "{}", text(&out));

    assert_eq!(
        git(
            &f.root,
            &["for-each-ref", "--format=%(refname)", "refs/minds/"]
        ),
        ""
    );
}

/// Der post-commit-Hook ruft den Checkpoint — wie `minds enable` ihn schreibt.
fn install_post_commit(f: &Fixture) {
    let hooks = f.dir.path().join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    let script = hooks.join("post-commit");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nexec \"{}\" checkpoint --commit \"$(git rev-parse HEAD)\"\n",
            env!("CARGO_BIN_EXE_minds")
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.root,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
}

/// Eine lokal (A1) erfasste Session, gebunden an den Issue-Anker.
fn session_bound_to_issue(f: &Fixture, stub: &Stub) -> String {
    stub.serve((200, ISSUE), None);
    let out = bind(f, stub, Some(TOKEN));
    assert!(out.status.success(), "{}", text(&out));
    let (_, id) = expected(&["src/retry/**"]);
    session_with_active(f, &id);
    id
}

/// Aktiviert `id` über die A1-Datei und erfasst eine Session mit Commit.
fn session_with_active(f: &Fixture, id: &str) {
    fs::create_dir_all(f.root.join(".git/minds/intent")).unwrap();
    fs::write(f.root.join(".git/minds/intent/active"), format!("{id}\n")).unwrap();
    install_post_commit(f);
    for event in ["UserPromptSubmit", "Stop"] {
        let stdin = f.payload("run", event, r#","prompt":"retry""#);
        assert_silent_success(&f.hook(None, stdin.as_bytes()).0);
    }
    fs::write(f.root.join("retry.rs"), "// retry\n").unwrap();
    git(&f.root, &["add", "retry.rs"]);
    git(&f.root, &["commit", "-q", "-m", "feat: retry"]);
}

fn verify(f: &Fixture, stub: &Stub, online: bool, token: Option<&str>) -> Output {
    let mut args = vec!["verify", "HEAD"];
    if online {
        args.extend(["--online", "--gitlab-url", &stub.url]);
    }
    minds_in(f, token, &args)
}

fn version_line(out: &Output) -> String {
    let report = text(out);
    report
        .lines()
        .find(|line| line.starts_with("Issue version  "))
        .unwrap_or_else(|| panic!("no Issue version line:\n{report}"))
        .to_owned()
}

/// Die Fixtures gegen `verify`: offline, unverändert, geändert (mit und
/// ohne Historie), 401 und 404 — und der Exit-Code bleibt jedes Mal der
/// des Offline-Laufs: Die Prüfung wertet nichts auf und nichts ab.
#[test]
fn verify_reports_the_issue_version() {
    let f = repo();
    let stub = Stub::start();
    let id = session_bound_to_issue(&f, &stub);

    stub.serve((200, ISSUE), None);
    let offline = verify(&f, &stub, false, Some(TOKEN));
    assert_eq!(
        version_line(&offline),
        "Issue version  not checked (offline)"
    );
    assert!(stub.requests().is_empty(), "offline fragt nicht");
    let report = text(&offline);
    assert!(
        report.contains(&format!(
            "Intent         intent unsigned ({}…, unchained)",
            &id[..11]
        )),
        "{report}"
    );
    let code = offline.status.code();

    let cases: [Case<'_>; 7] = [
        (
            "Issue version  current (anchor unsigned)",
            (200, ISSUE),
            None,
            Some(TOKEN),
        ),
        (
            "Issue version  changed since binding — bound version confirmed in description history (anchor unsigned)",
            (200, ISSUE_CHANGED),
            Some((200, HISTORY)),
            Some(TOKEN),
        ),
        (
            "Issue version  changed since binding — no description history available",
            (200, ISSUE_CHANGED),
            Some((200, NO_HISTORY_FIELD)),
            Some(TOKEN),
        ),
        (
            "Issue version  version check unavailable (unauthorized (HTTP 401))",
            (401, UNAUTHORIZED),
            None,
            Some(TOKEN),
        ),
        (
            "Issue version  version check unavailable (issue not found (HTTP 404))",
            (404, NOT_FOUND),
            None,
            Some(TOKEN),
        ),
        (
            "Issue version  changed since binding — description history query failed",
            (200, ISSUE_CHANGED),
            Some((401, UNAUTHORIZED)),
            Some(TOKEN),
        ),
        (
            "Issue version  version check unavailable (MINDS_GITLAB_TOKEN is not set)",
            (200, ISSUE),
            None,
            None,
        ),
    ];
    for (line, issue, graphql, token) in cases {
        stub.serve(issue, graphql);
        let out = verify(&f, &stub, true, token);
        assert_eq!(version_line(&out), line, "{}", text(&out));
        assert_eq!(out.status.code(), code, "{}", text(&out));
        assert!(!text(&out).contains(TOKEN), "{}", text(&out));
        // Alles andere im Block bleibt, wie es offline war.
        let strip = |o: &Output| {
            text(o)
                .lines()
                .filter(|l| !l.starts_with("Issue version"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip(&out), strip(&offline));
    }

    // `--gitlab-url` ohne `--online` ist ein Bedienfehler (4).
    let out = minds_in(&f, None, &["verify", "HEAD", "--gitlab-url", &stub.url]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out));
}

/// Security-Review EA-16: Eine `.curlrc`, die der Agent unter demselben
/// Nutzer schreiben kann, hat keine Wirkung — kein Trace mit dem Token,
/// kein Proxy, der ein „passendes" Issue liefert.
#[test]
fn a_planted_curlrc_is_ignored() {
    let f = repo();
    let stub = Stub::start();
    stub.serve((200, ISSUE), None);
    let trace = f.dir.path().join("trace.txt");
    let curlrc = format!(
        "trace-ascii = \"{}\"\nverbose\nproxy = \"http://127.0.0.1:9\"\n",
        trace.display()
    );
    fs::write(f.dir.path().join(".curlrc"), &curlrc).unwrap();
    let out = minds()
        .current_dir(&f.root)
        .env("HOME", f.dir.path())
        .env("XDG_CONFIG_HOME", f.dir.path())
        .env("CURL_HOME", f.dir.path())
        .env("XDG_STATE_HOME", f.dir.path().join("state"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("MINDS_WITNESS_HOME")
        .env_remove("MINDS_GITLAB_URL")
        .env_remove("CI_SERVER_URL")
        .env("MINDS_GITLAB_TOKEN", TOKEN)
        .args([
            "intent",
            "bind",
            "--issue",
            "team/minds#42",
            "--gitlab-url",
            &stub.url,
        ])
        .output()
        .unwrap();
    // Mit Proxy auf Port 9 (discard) käme keine Antwort — der Aufruf ging
    // also direkt an den Stub.
    assert!(out.status.success(), "{}", text(&out));
    assert!(!trace.exists(), "curl hat die .curlrc gelesen");
    assert!(!text(&out).contains(TOKEN), "{}", text(&out));
}

/// Security-Review EA-16: Trägt der gebundene Snapshot Platzhalter, sagt
/// `current` nichts über die redigierten Stellen — und sagt das.
#[test]
fn current_names_redacted_spans_it_did_not_compare() {
    let f = repo();
    let stub = Stub::start();
    let secret = concat!("ghp_", "R4nd0mT0k3nV4lu3F0rT3st1ngPurp0s3s00");
    let body = serde_json::json!({
        "iid": 42,
        "title": "Token rotieren",
        "description": format!("Der alte Token war {secret}."),
        "updated_at": UPDATED_AT,
        "confidential": false,
    })
    .to_string();
    stub.serve((200, &body), None);
    let out = bind(&f, &stub, Some(TOKEN));
    assert!(out.status.success(), "{}", text(&out));
    let id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("anchor  ").map(str::to_owned))
        .unwrap();
    session_with_active(&f, &id);
    let out = verify(&f, &stub, true, Some(TOKEN));
    assert_eq!(
        version_line(&out),
        "Issue version  current (redacted spans not compared, anchor unsigned)",
        "{}",
        text(&out)
    );
}
