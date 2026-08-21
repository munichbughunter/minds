//! Die Kommandos des Pilot-Zuschnitts, gegen das echte Binary (#51, v0.1.3).
//!
//! Nicht alle zwölf ungedeckten Kommandos — genau die Pfade, die beim
//! Pilot-Partner nicht selbst debuggbar sind: die Erfassungs-Vertragsflächen
//! (`prepare-commit-msg` über einen echten `git commit`, das
//! `brief --hook`-Envelope, das Claude Code parst) und die Lese-Kommandos
//! des Piloten (`blame`, `recap`, `search`) samt der GitLab-Spiegelung
//! (`gitlab mirror` gegen einen lokalen HTTP-Stub).
//!
//! Braucht `git`; der Spiegel-Teil zusätzlich `curl`. Fehlt eines, überspringt
//! sich der jeweilige Test, statt falsch-rot zu werden.

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
/// Hook-Verzeichnis, `commit.gpgsign` verlangt eine Signatur. Beides machte
/// den Lauf von der Maschine abhängig. `/dev/null` schaltet die Config-Ebene ab.
fn without_user_config(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
}

/// Der `PATH` für Git-Aufrufe: vorneweg das Verzeichnis des Test-Binaries.
///
/// Die von `minds enable` installierten Hooks rufen `minds` **ohne Pfad** auf.
/// Ohne diesen Eintrag greift der Aufruf ins Leere, `|| true` schluckt ihn,
/// und der Commit bekäme keine Change-Id. Das Verzeichnis steht **vorn**,
/// damit auch eine global installierte `minds` den Lauf nicht verfälscht.
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

fn minds(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    minds_with_env(dir, args, stdin, &[])
}

/// Wie [`minds`], aber mit zusätzlichen Umgebungsvariablen — der Weg, auf dem
/// der GitLab-Token ins Kommando kommt (nie als Argument).
fn minds_with_env(dir: &Path, args: &[&str], stdin: Option<&str>, envs: &[(&str, &str)]) -> Output {
    use std::io::Write;
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir).args(args);
    without_user_config(&mut cmd);
    cmd.envs(envs.iter().copied());
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("minds startet");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().expect("minds endet")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn text(output: &Output) -> String {
    format!("{}{}", stdout(output), stderr(output))
}

/// Ein Repo mit Minds-Hooks und einem Commit auf `main`.
///
/// Der Branchname steht **explizit** da: Ohne `-b` entscheidet ihn
/// `init.defaultBranch`, und der Lauf hinge an der Maschine.
fn repo() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    if !git(dir.path(), &["init", "-q", "-b", "main"])
        .status
        .success()
    {
        return None;
    }
    git(dir.path(), &["config", "user.email", "anna@example.org"]);
    git(dir.path(), &["config", "user.name", "Anna"]);
    let enable = minds(dir.path(), &["enable"], None);
    assert!(enable.status.success(), "{}", text(&enable));
    std::fs::write(dir.path().join("a.txt"), "eins\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "erster Commit"]);
    Some(dir)
}

/// Alle `Minds-Change-Id`-Trailer aus der Message von HEAD.
fn change_ids_of_head(dir: &Path) -> Vec<String> {
    let message = String::from_utf8_lossy(&git(dir, &["show", "-s", "--format=%B", "HEAD"]).stdout)
        .into_owned();
    message
        .lines()
        .filter_map(|line| line.strip_prefix("Minds-Change-Id: "))
        .map(str::trim)
        .map(str::to_owned)
        .collect()
}

/// Die `Minds-Change-Id` von HEAD — genau eine, sonst Panik: Auch ein
/// **zweiter** Trailer wäre ein Defekt, kein Detail.
fn change_id_of_head(dir: &Path) -> String {
    match change_ids_of_head(dir).as_slice() {
        [id] => id.clone(),
        other => panic!("erwartet genau einen Change-Id-Trailer an HEAD: {other:?}"),
    }
}

/// Ein Hook-Event, wie es der Agent auf stdin schickt.
fn event(dir: &Path, body: &str) {
    let payload = format!(
        r#"{{"session_id":"sess-pilot","cwd":"{}",{body}}}"#,
        dir.display()
    );
    let out = minds(dir, &["hook", "--agent", "claude-code"], Some(&payload));
    assert!(out.status.success(), "hook endet immer mit 0");
}

/// Ein Repo mit Hooks, einer erfassten Session und einem eingecheckten
/// Commit — die Vorbedingung aller Lese-Kommandos des Piloten.
fn captured_repo() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    if !git(dir.path(), &["init", "-q", "-b", "main"])
        .status
        .success()
    {
        return None;
    }
    git(dir.path(), &["config", "user.email", "anna@example.org"]);
    git(dir.path(), &["config", "user.name", "Anna"]);
    let enable = minds(dir.path(), &["enable", "--agent", "claude-code"], None);
    assert!(enable.status.success(), "{}", text(&enable));

    event(
        dir.path(),
        r#""hook_event_name":"UserPromptSubmit","prompt":"Schreibe eine Grußfunktion""#,
    );
    event(
        dir.path(),
        r#""hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"greet.rs"}"#,
    );
    event(dir.path(), r#""hook_event_name":"Stop""#);

    std::fs::write(
        dir.path().join("greet.rs"),
        "fn greet() {\n    println!(\"hallo\");\n}\n",
    )
    .unwrap();
    git(dir.path(), &["add", "greet.rs"]);
    assert!(
        git(dir.path(), &["commit", "-q", "-m", "feat: Grußfunktion"])
            .status
            .success()
    );
    // Der post-commit-Hook checkpointet bereits selbst; der Aufruf von Hand
    // ist ein No-op und hält die Vorbedingung unabhängig vom Hook-Netz.
    let head = String::from_utf8_lossy(&git(dir.path(), &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();
    let checkpoint = minds(dir.path(), &["checkpoint", "--commit", &head], None);
    assert!(checkpoint.status.success(), "{}", text(&checkpoint));
    Some(dir)
}

// --- prepare-commit-msg: die Erfassungs-Vertragsfläche am Commit ------------

#[test]
fn a_real_commit_gains_a_change_id_and_an_amend_keeps_it() {
    // Der Hook läuft hier so, wie er beim Partner läuft: von Git gestartet,
    // nicht direkt aufgerufen. Die Unit-Tests prüfen die Message-Chirurgie,
    // dieser Test die Verdrahtung — das ist der Teil, der beim Partner
    // ausfallen kann, ohne dass es jemand sieht.
    let Some(dir) = repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();

    let id = change_id_of_head(dir);
    assert!(id.starts_with('I'), "{id}");
    assert_eq!(id.len(), 41, "{id}");
    assert!(
        id[1..].chars().all(|c| c.is_ascii_hexdigit()),
        "keine Hex-Id: {id}"
    );

    // Ein Amend darf weder eine neue Id vergeben noch eine **zweite**
    // anhängen — sonst verlöre das Verdict an der alten Id seinen Anker.
    assert!(
        git(dir, &["commit", "-q", "--amend", "--no-edit"])
            .status
            .success()
    );
    assert_eq!(
        change_ids_of_head(dir),
        vec![id],
        "Amend muss genau die eine Change-Id halten"
    );
}

// --- Die Lese-Kommandos des Piloten -----------------------------------------

#[test]
fn blame_names_the_session_behind_each_line() {
    let Some(dir) = captured_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();

    let out = minds(dir, &["blame", "greet.rs"], None);
    assert!(out.status.success(), "{}", text(&out));
    let shown = stdout(&out);
    // Alle drei Zeilen stammen aus dem Commit mit erfasster Session — die
    // vollständige Zeile, damit auch ein Teilverlust (2 von 3) rot wird.
    assert!(
        shown.contains("greet.rs — 3 Zeilen, 3 mit erfasstem Kontext (100%)"),
        "{shown}"
    );
    assert!(shown.contains("▸ "), "keine Session benannt: {shown}");
}

#[test]
fn blame_refuses_what_head_does_not_know() {
    let Some(dir) = repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();

    let out = minds(dir, &["blame", "gibt-es-nicht.txt"], None);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("nicht auflösbar"), "{}", text(&out));
}

#[test]
fn recap_lists_the_captured_session() {
    let Some(dir) = captured_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();

    let out = minds(dir, &["recap"], None);
    assert!(out.status.success(), "{}", text(&out));
    let shown = stdout(&out);
    assert!(
        shown.contains("Die 1 jüngsten von 1 Session(s):"),
        "{shown}"
    );
}

#[test]
fn recap_rejects_a_limit_below_one() {
    let Some(dir) = repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();

    let out = minds(dir, &["recap", "--limit", "0"], None);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("--limit erwartet eine Zahl ≥ 1"),
        "{}",
        text(&out)
    );
}

#[test]
fn search_finds_the_prompt_and_names_misses() {
    let Some(dir) = captured_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();

    // Case-insensitive Substring über den erfassten Prompt.
    let hit = minds(dir, &["search", "grußfunktion"], None);
    assert!(hit.status.success(), "{}", text(&hit));
    assert!(
        stdout(&hit).contains(r#"1 Treffer für "grußfunktion":"#),
        "{}",
        stdout(&hit)
    );
    assert!(stdout(&hit).contains("▸ "), "{}", stdout(&hit));

    // Kein Treffer ist ein Ergebnis, kein Fehler.
    let miss = minds(dir, &["search", "gibtsnicht"], None);
    assert!(miss.status.success(), "{}", text(&miss));
    assert!(
        stdout(&miss).contains(r#"Keine Treffer für "gibtsnicht"."#),
        "{}",
        stdout(&miss)
    );
}

// --- brief --hook: das Envelope, das Claude Code parst ----------------------

#[test]
fn brief_hook_emits_the_envelope_claude_code_parses() {
    // Die Fehlerpfade (hook.log statt stdout/stderr) stehen in end_to_end.rs.
    // Hier steht der Erfolgsfall: Das JSON ist die Vertragsfläche, die Claude
    // Code als SessionStart-Hook-Output parst — bricht seine Form, fällt es
    // erst beim Partner auf, und zwar stumm.
    let Some(dir) = captured_repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();

    let out = minds(dir, &["brief", "--hook"], None);
    assert!(out.status.success(), "{}", text(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));

    let shown = stdout(&out);
    let doc: serde_json::Value = serde_json::from_str(shown.trim())
        .unwrap_or_else(|err| panic!("kein JSON-Envelope ({err}): {shown:?}"));
    let hook_output = &doc["hookSpecificOutput"];
    assert_eq!(hook_output["hookEventName"], "SessionStart", "{doc}");
    let context = hook_output["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("additionalContext ist kein String: {doc}"));
    assert!(context.contains("Kontext für den Agenten"), "{context}");
    // Nicht nur die Form, auch der Inhalt: Die erfasste Session muss im
    // Brief auftauchen — ein leerer Store lieferte dieselbe Überschrift.
    assert!(
        context.contains("Grußfunktion"),
        "erfasste Session fehlt im Brief: {context}"
    );
}

// --- gitlab mirror: die Spiegelung, CLI-Ebene gegen einen lokalen Stub ------
//
// Der Stub ist dasselbe Muster wie in den minds-gitlab-Unit-Tests (#7): ein
// echter `curl`-Prozess gegen einen `TcpListener`. Hier läuft zusätzlich die
// ganze CLI-Strecke mit: Konfiguration über Flags, Reviews aus
// `refs/minds/reviews`, Token aus der Umgebungsvariablen.

struct Received {
    method: String,
    path: String,
    headers: Vec<String>,
    body: String,
}

/// Liefert die `responses` der Reihe nach aus und legt jeden empfangenen
/// Request in den Kanal. Nach der letzten Antwort endet der Thread.
fn stub_server(responses: Vec<(u16, String)>) -> (String, std::sync::mpsc::Receiver<Received>) {
    use std::io::Write as _;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let request = read_request(&mut stream);
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = sender.send(request);
        }
    });
    (format!("http://{address}"), receiver)
}

fn read_request(stream: &mut std::net::TcpStream) -> Received {
    use std::io::{BufRead, BufReader, Read};

    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
        headers.push(line);
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).unwrap();
    Received {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn curl_available() -> bool {
    Command::new("curl")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[test]
fn gitlab_mirror_posts_the_note_through_the_cli() {
    let Some(dir) = repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    if !curl_available() {
        eprintln!("kein curl im Pfad — Test übersprungen");
        return;
    }
    let dir = dir.path();

    let change = change_id_of_head(dir);
    let review = minds(
        dir,
        &["review", &change, "--approve", "--summary", "geprüft"],
        None,
    );
    assert!(review.status.success(), "{}", text(&review));

    let (url, received) = stub_server(vec![
        (200, "[]".into()),          // has_note: noch keine Notes am MR
        (201, r#"{"id":1}"#.into()), // die angelegte Note
    ]);
    let out = minds_with_env(
        dir,
        &[
            "gitlab",
            "mirror",
            &change,
            "--mr",
            "4",
            "--url",
            &url,
            "--project",
            "1",
            "--token-env",
            "MINDS_PILOT_TOKEN",
        ],
        None,
        &[("MINDS_PILOT_TOKEN", "geheim123")],
    );
    assert!(out.status.success(), "{}", text(&out));
    let shown = stdout(&out);
    assert!(shown.contains("gespiegelt: approve"), "{shown}");
    assert!(shown.contains("neu an MR !4 gespiegelt"), "{shown}");

    // Die Pfade prüfen die Verdrahtung der Flags: `--project 1` und `--mr 4`
    // müssen im richtigen URL-Segment ankommen — das assertierte stdout allein
    // formatiert nur dieselben geparsten Variablen zurück.
    let get = received.recv().unwrap();
    assert_eq!(get.method, "GET");
    assert!(
        get.path
            .ends_with("/projects/1/merge_requests/4/notes?per_page=100"),
        "{}",
        get.path
    );
    let post = received.recv().unwrap();
    assert_eq!(post.method, "POST");
    assert!(
        post.path.ends_with("/projects/1/merge_requests/4/notes"),
        "{}",
        post.path
    );
    assert!(
        post.headers.iter().any(|h| h == "PRIVATE-TOKEN: geheim123"),
        "{:?}",
        post.headers
    );
    // Der Kern von #7, hier über die ganze CLI-Strecke: Die Note steht im
    // Body — als JSON mit Marker und Summary — und in keinem Header.
    let payload: serde_json::Value = serde_json::from_str(&post.body)
        .unwrap_or_else(|err| panic!("POST-Body ist kein JSON ({err}): {:?}", post.body));
    let note = payload["body"].as_str().unwrap();
    assert!(note.contains("minds:review:"), "{note}");
    assert!(note.contains("geprüft"), "{note}");
    assert!(
        !post.headers.iter().any(|h| h.contains("minds:review")),
        "Note im Header statt im Body: {:?}",
        post.headers
    );
}

// --- gitlab webhook: der verifizierte Pfad (#8) ------------------------------

/// Eine Note-Nutzlast, wie GitLab sie schickt — Autor laut Payload „anna".
fn webhook_payload(note: &str) -> String {
    serde_json::json!({
        "object_kind": "note",
        "user": { "username": "anna", "email": "anna@example.org" },
        "object_attributes": { "note": note, "noteable_type": "MergeRequest" }
    })
    .to_string()
}

#[test]
fn gitlab_webhook_rejects_a_wrong_or_missing_token() {
    // Der Kern von #8: Wer das Secret nicht kennt, erzeugt kein Audit-Objekt —
    // auch nicht mit --write und einer ansonsten perfekten Nutzlast.
    let Some(dir) = repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();
    let payload = webhook_payload(&format!("/minds approve I{} passt", "ab".repeat(20)));

    let wrong = minds_with_env(
        dir,
        &["gitlab", "webhook", "--write"],
        Some(&payload),
        &[
            ("MINDS_GITLAB_WEBHOOK_SECRET", "streng-geheim"),
            ("MINDS_GITLAB_WEBHOOK_TOKEN", "geraten"),
        ],
    );
    assert!(!wrong.status.success(), "{}", text(&wrong));
    assert!(
        stderr(&wrong).contains("Token-Verifikation"),
        "{}",
        text(&wrong)
    );
    assert!(!stdout(&wrong).contains("angelegt"), "{}", text(&wrong));

    let missing = minds_with_env(
        dir,
        &["gitlab", "webhook", "--write"],
        Some(&payload),
        &[("MINDS_GITLAB_WEBHOOK_SECRET", "streng-geheim")],
    );
    assert!(!missing.status.success(), "{}", text(&missing));
    assert!(
        stderr(&missing).contains("MINDS_GITLAB_WEBHOOK_TOKEN"),
        "der Empfänger muss erfahren, wie der Header durchzureichen ist: {}",
        text(&missing)
    );
}

#[test]
fn gitlab_webhook_accepts_the_matching_token() {
    // Über --secret-env, damit auch die Flag-Verdrahtung geprüft ist. Der
    // Autor erscheint als Behauptung der Nutzlast, nicht als Faktum.
    let Some(dir) = repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();
    let payload = webhook_payload(&format!("/minds approve I{} passt", "ab".repeat(20)));

    let out = minds_with_env(
        dir,
        &[
            "gitlab",
            "webhook",
            "--secret-env",
            "MINDS_PILOT_HOOK_SECRET",
        ],
        Some(&payload),
        &[
            ("MINDS_PILOT_HOOK_SECRET", "streng-geheim"),
            ("MINDS_GITLAB_WEBHOOK_TOKEN", "streng-geheim"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let shown = stdout(&out);
    assert!(shown.contains("approve"), "{shown}");
    assert!(shown.contains("(laut Payload)"), "{shown}");
}

#[test]
fn gitlab_webhook_commit_id_never_reaches_git_as_an_option() {
    // Der Kern von #23: `merge_request.last_commit.id` kommt verbatim aus der
    // Nutzlast. Ein Wert wie `--output=<pfad>` parste `git show` als Option
    // und legte die Datei an — er muss an der Hex-Validierung scheitern,
    // bevor git ihn je sieht.
    let Some(dir) = repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();
    let target = dir.join("injiziert.txt");
    // Neben der Options-Injection auch Kurzhash und Ref-Syntax: hex, aber
    // nicht voll bzw. auflösbar statt wörtlich — beides darf git nie erreichen
    // und muss ohne Change-Id im Kommentar in „kein Subjekt" enden.
    let rejected = [
        format!("--output={}", target.display()),
        "-O/tmp/injiziert".to_string(),
        "deadbeef".to_string(),
        "HEAD".to_string(),
    ];
    for id in &rejected {
        let payload = serde_json::json!({
            "object_kind": "note",
            "user": { "username": "anna" },
            "object_attributes": { "note": "/minds approve passt", "noteable_type": "MergeRequest" },
            "merge_request": { "iid": 4, "last_commit": { "id": id } }
        })
        .to_string();

        // Das Secret ausdrücklich leer: Eine in der Entwickler-Umgebung
        // gesetzte Variable schöbe den Lauf sonst auf den verifizierten Pfad.
        let out = minds_with_env(
            dir,
            &["gitlab", "webhook", "--write"],
            Some(&payload),
            &[("MINDS_GITLAB_WEBHOOK_SECRET", "")],
        );
        assert!(!out.status.success(), "{id}: {}", text(&out));
        assert!(
            stderr(&out).contains("kein Subjekt"),
            "{id}: {}",
            text(&out)
        );
    }
    assert!(
        !target.exists(),
        "die Commit-Id aus der Nutzlast hat git als Option erreicht"
    );
}

#[test]
fn gitlab_webhook_resolves_the_change_id_from_the_mr_commit() {
    // Die Rückfallebene: keine Change-Id im Kommentar, aber der Commit des MR
    // ist lokal bekannt — seine Change-Id wird das Subjekt. Der volle
    // Hex-Hash ist zugleich der einzige Wert, den die Validierung aus #23
    // durchlässt; der Test hält fest, dass sie den Gutfall nicht mitnimmt.
    let Some(dir) = repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();
    let head = String::from_utf8_lossy(&git(dir, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let change = change_id_of_head(dir);
    let payload = serde_json::json!({
        "object_kind": "note",
        "user": { "username": "anna" },
        "object_attributes": { "note": "/minds approve passt", "noteable_type": "MergeRequest" },
        "merge_request": { "iid": 4, "last_commit": { "id": head } }
    })
    .to_string();

    let out = minds_with_env(
        dir,
        &["gitlab", "webhook"],
        Some(&payload),
        &[("MINDS_GITLAB_WEBHOOK_SECRET", "")],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(stdout(&out).contains(&change), "{}", text(&out));
}

#[test]
fn gitlab_mirror_names_the_missing_token_variable() {
    // Der häufigste Konfigurationsfehler beim Partner — er muss die Variable
    // benennen, statt sich als HTTP-Fehler oder Stille zu zeigen. Kein Stub
    // nötig: Der Fehler fällt vor dem ersten Netz-Aufruf.
    let Some(dir) = repo() else {
        eprintln!("kein git im Pfad — Test übersprungen");
        return;
    };
    let dir = dir.path();

    let change = change_id_of_head(dir);
    let review = minds(dir, &["review", &change, "--approve"], None);
    assert!(review.status.success(), "{}", text(&review));

    let out = minds(
        dir,
        &[
            "gitlab",
            "mirror",
            &change,
            "--mr",
            "4",
            "--url",
            "http://127.0.0.1:9",
            "--project",
            "1",
            "--token-env",
            "MINDS_PILOT_TOKEN_GIBT_ES_NICHT",
        ],
        None,
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("MINDS_PILOT_TOKEN_GIBT_ES_NICHT"),
        "{}",
        text(&out)
    );
}
