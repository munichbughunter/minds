//! `minds audit --export` — die Provenienz-Kette als Bündel (Schicht 3, R6).
//!
//! Geprüft wird, was ein Auditor damit anfangen können muss: Die Kette hängt
//! zusammen (Change → Commit → Session → Verdict), die kanonischen Payloads sind
//! byte-genau da, und die **Grenzen** stehen im Artefakt selbst — nicht nur in
//! der Doku, die beim Weiterreichen zurückbleibt.

use std::path::Path;
use std::process::{Command, Output};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.env("PATH", path_with_minds());
    without_user_config(&mut cmd).output().expect("git läuft")
}

/// Schneidet die Git-Config des Entwicklers ab: Ein global gesetztes
/// `core.hooksPath` (husky, lefthook) verschiebt seit #9 auch hier das
/// Hook-Verzeichnis, `commit.gpgsign` verlangt eine Signatur. Beides machte den
/// Lauf von der Maschine abhängig. `/dev/null` schaltet die Config-Ebene ab.
fn without_user_config(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
}

/// Der `PATH` für Git-Aufrufe: vorneweg das Verzeichnis des Test-Binaries.
///
/// Die von `minds enable` installierten Hooks rufen `minds` **ohne Pfad** auf.
/// Ohne diesen Eintrag greift der Aufruf ins Leere, `|| true` schluckt ihn, und
/// der Commit bekäme weder Change-Id noch Checkpoint — der Test wäre rot aus
/// einem Grund, der nichts mit der Zusage zu tun hat. Das Verzeichnis steht
/// **vorn**, damit auch eine global installierte `minds` den Lauf nicht
/// verfälscht.
fn path_with_minds() -> std::ffi::OsString {
    let bin_dir = Path::new(MINDS)
        .parent()
        .expect("Binary hat ein Verzeichnis");
    let mut dirs = vec![bin_dir.to_path_buf()];
    dirs.extend(
        std::env::var_os("PATH")
            .iter()
            .flat_map(std::env::split_paths),
    );
    std::env::join_paths(dirs).expect("PATH lässt sich zusammensetzen")
}

fn minds(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir).args(args);
    without_user_config(&mut cmd).output().expect("minds läuft")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Ein Repo mit einer erfassten Session an HEAD.
fn repo_with_session() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    if !git(dir.path(), &["init", "-q", "-b", "main"])
        .status
        .success()
    {
        return None;
    }
    git(dir.path(), &["config", "user.email", "anna@example.org"]);
    git(dir.path(), &["config", "user.name", "Anna"]);
    minds(dir.path(), &["enable"]);

    std::fs::write(dir.path().join("a.txt"), "eins\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "Basis"]);

    // Ein Hook-Event, damit eine echte Session entsteht.
    let payload = format!(
        r#"{{"session_id":"sess-audit","cwd":"{}","hook_event_name":"UserPromptSubmit","prompt":"Retry-Test reparieren"}}"#,
        dir.path().display()
    );
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir.path())
        .args(["hook", "--agent", "claude-code"]);
    let mut child = without_user_config(&mut cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
    }
    child.wait().unwrap();

    std::fs::write(dir.path().join("a.txt"), "zwei\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "fix: retry"]);
    Some(dir)
}

fn change_id_of_head(dir: &Path) -> String {
    let body = stdout(&git(dir, &["show", "-s", "--format=%B", "HEAD"]));
    body.lines()
        .find_map(|line| line.strip_prefix("Minds-Change-Id: "))
        .map(|id| id.trim().to_owned())
        .unwrap_or_else(|| panic!("kein Change-Id-Trailer:\n{body}"))
}

#[test]
fn the_bundle_carries_the_whole_chain_and_its_limits() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();

    let change = change_id_of_head(dir);
    minds(
        dir,
        &["review", &change, "--approve", "--summary", "geprüft"],
    );
    minds(dir, &["comment", &change, "--on", "a.txt:1", "hier ok"]);

    let out = minds(dir, &["audit", "--export"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let bundle: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("gültiges JSON");

    assert_eq!(bundle["schema_version"], 3);

    // Die Grenzen stehen im Artefakt, nicht nur in der Doku — jede mit
    // stabiler Id.
    let limits = bundle["does_not_prove"].as_array().expect("does_not_prove");
    assert!(!limits.is_empty());
    assert!(
        limits.iter().any(|line| line["id"] == "record_complete"
            && line["text"]
                .as_str()
                .is_some_and(|text| text.contains("fail-open"))),
        "die fail-open-Lücke muss benannt sein: {limits:?}"
    );

    // Die Kette: Change → Commit → Session → Verdict.
    let changes = bundle["changes"].as_array().expect("changes");
    let entry = changes
        .iter()
        .find(|entry| entry["change_id"] == serde_json::json!(change))
        .unwrap_or_else(|| panic!("Change {change} fehlt im Bündel: {changes:?}"));

    assert!(!entry["commits"].as_array().unwrap().is_empty());

    let session = &entry["sessions"][0];
    assert!(
        session["id"].as_str().unwrap().starts_with("b3-"),
        "{session:?}"
    );
    assert_eq!(session["payload"], "present");
    assert_eq!(session["intent"], "Retry-Test reparieren");
    // Byte-genau der Text, über den `minds sign` signiert.
    let attestation = session["attestation_payload"].as_str().unwrap();
    assert!(
        attestation.starts_with("minds-attestation-v1\n"),
        "{attestation}"
    );
    assert!(attestation.contains(session["id"].as_str().unwrap()));

    let verdict = &entry["verdicts"][0];
    assert_eq!(verdict["decision"], "approve");
    assert_eq!(verdict["reviewer"], "anna@example.org");
    let payload = verdict["review_payload"].as_str().unwrap();
    assert!(payload.starts_with("minds-review-v1\n"), "{payload}");
    assert!(payload.contains(verdict["hash"].as_str().unwrap()));

    assert_eq!(entry["comments"][0]["anchor"], "a.txt:1");
}

#[test]
fn a_forgotten_session_stays_visible_in_the_chain() {
    // Der Punkt an einer redigierbaren Nutzlast: Die Löschung ist nachweisbar,
    // nicht spurlos. Ein Bündel, in dem die Session einfach fehlte, sähe aus wie
    // eines, in dem sie nie erfasst wurde.
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();

    let bundle: serde_json::Value =
        serde_json::from_str(&stdout(&minds(dir, &["audit", "--export"]))).unwrap();
    let id = bundle["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|entry| entry["sessions"][0]["id"].as_str())
        .expect("eine Session")
        .to_owned();

    let forgotten = minds(dir, &["forget", &id, "--reason", "DSGVO-Auskunft"]);
    assert!(forgotten.status.success(), "{}", stdout(&forgotten));

    let after: serde_json::Value =
        serde_json::from_str(&stdout(&minds(dir, &["audit", "--export"]))).unwrap();
    let session = after["changes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|entry| entry["sessions"].as_array().unwrap())
        .find(|session| session["id"] == serde_json::json!(id))
        .expect("die Referenz muss in der Kette bleiben");

    assert_eq!(session["payload"], "forgotten");
    assert!(
        session["intent"].as_str().unwrap_or_default().is_empty(),
        "der Inhalt darf nicht mehr im Bündel stehen: {session:?}"
    );
    // Die Seals überleben das forget — und mit ihnen die Stufe (EA-13).
    assert_eq!(session["assurance"]["level"], "A1", "{session:?}");
}

/// Schickt ein Hook-Event über stdin — wie der Agent es täte.
fn feed_hook(dir: &Path, payload: &str) {
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir)
        .args(["hook", "--agent", "claude-code"]);
    let mut child = without_user_config(&mut cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
    }
    child.wait().unwrap();
}

#[test]
fn the_bundle_proves_a_rejected_session_without_leaking_its_content() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();

    // Eine zurückgewiesene Session dazu: Der Deny-Begriff steckt im eigenen
    // Platzhalter — der Verify-Pass der Redaction bricht fail-closed ab.
    std::fs::create_dir_all(dir.join(".minds")).unwrap();
    std::fs::write(
        dir.join(".minds/redact.json"),
        r#"{"deny_pii":["redacted"]}"#,
    )
    .unwrap();
    let payload = format!(
        r#"{{"session_id":"sess-blocked","cwd":"{}","hook_event_name":"UserPromptSubmit","prompt":"redacted geheimnis"}}"#,
        dir.display()
    );
    feed_hook(dir, &payload);
    minds(dir, &["checkpoint"]);

    let out = minds(dir, &["audit", "--export"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let bundle: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("gültiges JSON");

    // Die gespeicherte Session trägt ihren Seal, byte-genau.
    let sessions = &bundle["changes"][0]["sessions"];
    let seals = sessions[0]["seals"].as_array().expect("seals");
    assert_eq!(seals.len(), 1, "{sessions}");
    let text = seals[0]["text"].as_str().unwrap();
    assert!(text.starts_with("minds-seal-v1\n"), "{text}");
    assert!(text.contains("outcome=stored"), "{text}");

    // Der Block-Seal beweist die Existenz der zurückgewiesenen Session —
    // ohne ein Wort ihres Inhalts.
    let rejected = bundle["rejected_seals"].as_array().expect("rejected_seals");
    assert_eq!(rejected.len(), 1, "{bundle}");
    let text = rejected[0]["text"].as_str().unwrap();
    assert!(
        text.contains("outcome=storage_policy_rejected_payload"),
        "{text}"
    );
    assert!(text.contains("session=-"), "{text}");
    for verboten in ["redacted", "geheimnis", "prompt"] {
        assert!(
            !text.contains(verboten),
            "Block-Seal leakt {verboten:?}: {text}"
        );
    }

    // Und die neuen Zusagen stehen im Artefakt — samt der Grenze: Der Seal
    // committed auf Root und Coverage, die Chain selbst bleibt nur lokal
    // reproduzierbar. Das Bündel darf nie mehr behaupten.
    let proves = bundle["proves"].as_array().unwrap();
    assert!(
        proves.iter().any(|l| l["text"].as_str().is_some_and(|t| t
            .contains("commits cryptographically to chain root and coverage")
            && t.contains("only with the local journal and session salt"))),
        "{proves:?}"
    );
    let limits = bundle["does_not_prove"].as_array().unwrap();
    assert!(
        limits.iter().any(|l| l["text"]
            .as_str()
            .is_some_and(|t| t.contains("between append and seal"))),
        "{limits:?}"
    );
}

#[test]
fn proof_mode_keeps_the_skeleton_and_drops_the_content() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();
    let change = change_id_of_head(dir);
    minds(
        dir,
        &[
            "review",
            &change,
            "--approve",
            "--summary",
            "geprüft und gut",
        ],
    );
    minds(
        dir,
        &[
            "comment",
            &change,
            "--on",
            "a.txt:1",
            "vertraulicher hinweis",
        ],
    );

    let out = minds(dir, &["audit", "--export", "--mode", "proof"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let text = stdout(&out);
    let bundle: serde_json::Value = serde_json::from_str(&text).expect("gültiges JSON");
    assert_eq!(bundle["mode"], "proof");

    // Das Beweisgerüst bleibt: Ids, Payload-Texte, Seals, Verdict-Metadaten.
    let session = &bundle["changes"][0]["sessions"][0];
    assert!(session["id"].as_str().unwrap().starts_with("b3-"));
    assert!(
        session["attestation_payload"]
            .as_str()
            .unwrap()
            .starts_with("minds-attestation-v1\n")
    );
    assert!(session["seals"].as_array().is_some_and(|s| !s.is_empty()));
    let verdict = &bundle["changes"][0]["verdicts"][0];
    assert_eq!(verdict["decision"], "approve");

    // Der Inhalt ist weg — nicht nur die Felder, auch als Text im Artefakt.
    for verboten in [
        "Retry-Test reparieren",
        "geprüft und gut",
        "vertraulicher hinweis",
    ] {
        assert!(!text.contains(verboten), "proof leakt {verboten:?}");
    }
    assert!(
        bundle["changes"][0]["comments"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Und ein erfundener Modus wird abgelehnt, mit Begründung.
    let out = minds(dir, &["audit", "--export", "--mode", "full"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("there is deliberately no full"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_bundle_never_carries_remote_credentials() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();
    // Eine Remote-URL mit eingebettetem Token — der CI-Klassiker.
    git(
        dir,
        &[
            "remote",
            "add",
            "origin",
            concat!(
                "https://oauth2:glpat",
                "-AbCdEf123456789012@gitlab.example.com/group/repo.git"
            ),
        ],
    );

    let out = minds(dir, &["audit", "--export"]);
    let text = stdout(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        !text.contains(concat!("glpat", "-AbCdEf123456789012")),
        "das Bundle trägt den Remote-Token: {text}"
    );
    assert!(text.contains("gitlab.example.com"), "{text}");

    // Und im Proof-Modus entfällt das Feld ganz.
    let proof = stdout(&minds(dir, &["audit", "--export", "--mode", "proof"]));
    let bundle: serde_json::Value = serde_json::from_str(&proof).unwrap();
    assert!(bundle["repository"]["origin"].is_null(), "{proof}");
}

/// EA-13: Das Bündel nennt seine Stufe und genau die Sätze dieser Stufe. Ein
/// Repo, dessen Session nur die Hooks des Agenten versiegelt haben, trägt A1
/// — und damit die Lücke zwischen Anhängen und Versiegeln. Auch im
/// Proof-Zuschnitt: Stufe und Grenzen sind Beweisgerüst, kein Inhalt.
#[test]
fn an_a1_bundle_carries_its_level_and_the_append_window() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();
    minds(dir, &["checkpoint"]);

    for mode in ["redacted", "proof"] {
        let out = minds(dir, &["audit", "--export", "--mode", mode]);
        assert!(out.status.success(), "{}", stdout(&out));
        let bundle: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("gültiges JSON");

        assert_eq!(bundle["assurance"]["level"], "A1", "{mode}: {bundle}");
        assert_eq!(bundle["assurance"]["name"], "A1 observed");
        assert!(bundle["assurance"]["trusted_signers_available"].is_boolean());
        assert!(bundle["assurance"].get("reason").is_none(), "{bundle}");

        let ids = |key: &str| -> Vec<String> {
            bundle[key]
                .as_array()
                .unwrap_or_else(|| panic!("{key}"))
                .iter()
                .map(|s| s["id"].as_str().expect("id").to_owned())
                .collect()
        };
        let limits = ids("does_not_prove");
        assert!(
            limits.iter().any(|id| id == "append_to_seal_window"),
            "{limits:?}"
        );
        assert!(!limits.iter().any(|id| id == "only_actor_witnessed"));
        let proves = ids("proves");
        assert!(
            proves.iter().any(|id| id == "seal_verifiable"),
            "{proves:?}"
        );
        assert!(!proves.iter().any(|id| id == "witness_chaining"));

        // Je Session ihre Stufe samt Grund — derselbe Wortlaut wie `verify`.
        let session = &bundle["changes"][0]["sessions"][0];
        assert_eq!(session["assurance"]["level"], "A1", "{session}");
        assert!(
            session["assurance"]["reason"]
                .as_str()
                .is_some_and(|r| r.contains("agent's hooks only")),
            "{session}"
        );
    }
}

/// Das Bündel verspricht nie mehr als sein schwächster Teil: eine Session
/// ohne Seal-Material (A0) neben einer versiegelten (A1) ⇒ Bündel A0 — mit
/// den Sätzen von A0, also ohne Seal-Zusagen.
#[test]
fn the_bundle_level_is_its_weakest_session() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();
    // Die erste Session verliert ihr Seal-Material — kein Seal, A0 …
    let refs = stdout(&git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/minds/evidence/",
        ],
    ));
    assert!(!refs.trim().is_empty(), "kein Seal angelegt");
    // Hängt am Ref-Layout des Stores (`refs/minds/evidence/<seal-id>`):
    // Ohne diese Refs lässt sich kein Seal mehr lesen.
    for name in refs.lines() {
        assert!(git(dir, &["update-ref", "-d", name]).status.success());
    }
    // … eine zweite wird beim Commit versiegelt.
    let payload = format!(
        r#"{{"session_id":"sess-second","cwd":"{}","hook_event_name":"UserPromptSubmit","prompt":"Zweiter Schritt"}}"#,
        dir.display()
    );
    feed_hook(dir, &payload);
    std::fs::write(dir.join("a.txt"), "drei\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "fix: second"]);

    let out = minds(dir, &["audit", "--export"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let bundle: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("gültiges JSON");
    let mut levels: Vec<&str> = bundle["changes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|change| change["sessions"].as_array().unwrap())
        .map(|session| session["assurance"]["level"].as_str().unwrap())
        .collect();
    levels.sort_unstable();
    assert_eq!(levels, ["A0", "A1"], "{bundle}");
    let reasons: Vec<&str> = bundle["changes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|change| change["sessions"].as_array().unwrap())
        .filter(|session| session["assurance"]["level"] == "A0")
        .filter_map(|session| session["assurance"]["reason"].as_str())
        .collect();
    assert_eq!(reasons, ["no seal"], "{bundle}");
    assert_eq!(bundle["assurance"]["level"], "A0", "{bundle}");
    let proves: Vec<&str> = bundle["proves"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert!(!proves.contains(&"seal_verifiable"), "{proves:?}");
    assert!(proves.contains(&"session_content_addressed"), "{proves:?}");
}

/// Ein Bereich ohne Agent-Session: A0, mit ausdrücklichem Grund — ein leeres
/// Bündel darf nicht wie ein schwach belegtes aussehen.
#[test]
fn an_empty_range_says_why_it_is_a0() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();
    let out = minds(dir, &["audit", "--export", "--base", "HEAD"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let bundle: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("gültiges JSON");
    assert_eq!(bundle["changes"], serde_json::json!([]), "{bundle}");
    assert_eq!(bundle["assurance"]["level"], "A0", "{bundle}");
    assert_eq!(
        bundle["assurance"]["reason"], "no agent session in the exported range",
        "{bundle}"
    );
}

/// Ein ausdrücklich genanntes `--signers`, das fehlt, ist ein Fehler — kein
/// stiller Rückfall auf „ungeprüft".
#[test]
fn a_missing_signers_file_is_an_error() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();
    let out = minds(
        dir,
        &[
            "audit",
            "--export",
            "--signers",
            "/does/not/exist/allowed_signers",
        ],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--signers"), "{err}");
    assert!(
        stdout(&out).trim().is_empty(),
        "kein Bündel: {}",
        stdout(&out)
    );
}

/// Dieselbe Grenze für eine zu große Signer-Datei: Was die Witness-Prüfung
/// verwürfe, ist bei ausdrücklichem `--signers` ein Fehler.
#[test]
fn an_oversized_signers_file_is_an_error() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();
    let big = dir.join("big_allowed_signers");
    std::fs::write(&big, vec![b'#'; 1024 * 1024 + 1]).unwrap();
    let out = minds(
        dir,
        &["audit", "--export", "--signers", big.to_str().unwrap()],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).trim().is_empty(), "kein Bündel");
}

/// Ein lesbares `--signers` wird angenommen: Das Bündel entsteht, und
/// `trusted_signers_available` sagt, ob mit `ssh-keygen` geprüft werden
/// könnte. Die Stufe bleibt A1 — Agent-Hook-Seals trägt keine Signatur
/// höher (und A2 ist ohne gebundenen Intent ohnehin nicht erreichbar).
#[test]
fn a_readable_signers_file_is_accepted() {
    let Some(dir) = repo_with_session() else {
        return;
    };
    let dir = dir.path();
    let signers = dir.join("trusted_allowed_signers");
    std::fs::write(&signers, "# keine Principals\n").unwrap();
    let out = minds(
        dir,
        &["audit", "--export", "--signers", signers.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bundle: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("gültiges JSON");
    assert_eq!(
        bundle["assurance"]["trusted_signers_available"],
        minds_attest::ssh_keygen_available(),
        "{bundle}"
    );
    assert_eq!(bundle["assurance"]["level"], "A1", "{bundle}");
}
