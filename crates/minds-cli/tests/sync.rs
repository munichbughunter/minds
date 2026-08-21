//! `minds sync` gegen ein echtes Remote — die drei Zusagen des Push-Pfades.
//!
//! Der `pre-push`-Hook rief bis v0.2 selbst `git push`. Das kostete auf jedem
//! Push den vollen Verbindungsaufbau (gegen gitlab.com ~1,9 s gemessen), auch
//! wenn es gar nichts Neues gab. Die drei Zusagen, die das ablösen, stehen hier
//! als Tests — gegen ein bares Repo nebenan, ohne Netz:
//!
//! 1. Alle fälligen Refs gehen in **einem** Aufruf mit.
//! 2. Ist nichts neu, passiert **nichts** (kein Push, keine Ausgabe).
//! 3. Divergiert der Review-Log, wird **vereinigt** statt überschrieben.
//!
//! Braucht `git` im Pfad; fehlt es, überspringen sich die Tests selbst.

use std::path::Path;
use std::process::{Command, Output};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

/// Eine Change-Id, wie `minds review` sie als Subjekt annimmt.
fn change_id(fill: &str) -> String {
    format!("I{}", fill.repeat(20))
}

/// Ein Arbeits-Repo mit einem Commit und einem baren Remote daneben.
///
/// Gibt `None` zurück, wenn kein `git` da ist — dann überspringt sich der Test,
/// statt falsch-rot zu werden.
fn repo_with_remote() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let remote = dir.path().join("remote.git");
    std::fs::create_dir_all(&work).unwrap();

    if !Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&remote)
        .status()
        .ok()?
        .success()
    {
        return None;
    }
    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "test@minds.invalid"]);
    git(&work, &["config", "user.name", "Minds Test"]);
    git(
        &work,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    std::fs::write(work.join("a.txt"), "hallo\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "erster Commit"]);

    Some((dir, work))
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
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

fn minds(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(dir).args(args);
    without_user_config(&mut cmd).output().expect("minds läuft")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Die Refs, die am Remote liegen.
fn remote_refs(work: &Path) -> String {
    String::from_utf8_lossy(&git(work, &["ls-remote", "origin", "refs/minds/*"]).stdout)
        .into_owned()
}

#[test]
fn all_due_refs_travel_in_one_call() {
    let Some((_dir, work)) = repo_with_remote() else {
        return;
    };

    // Zwei Minds-Refs: ein Verdict und ein Kontext-Stand.
    minds(&work, &["review", &change_id("ab"), "--approve"]);
    let head = String::from_utf8_lossy(&git(&work, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git(&work, &["update-ref", "refs/minds/context", &head]);

    let out = minds(&work, &["sync", "--remote", "origin"]);
    assert!(out.status.success(), "{}", text(&out));

    // Ein Aufruf, beide Refs — und die Meldung nennt beide.
    assert!(
        text(&out).contains("2 Ref(s)"),
        "beide Refs müssen in einem Push gehen: {}",
        text(&out)
    );
    let refs = remote_refs(&work);
    assert!(refs.contains("refs/minds/context"), "{refs}");
    assert!(
        refs.contains("refs/minds/reviews"),
        "Reviews müssen am Remote ankommen: {refs}"
    );

    // Und der Erfolg ist lokal vermerkt — das ist die Buchhaltung, die den
    // nächsten Lauf ohne Netz auskommen lässt.
    let tracking =
        String::from_utf8_lossy(&git(&work, &["for-each-ref", "refs/minds/remotes/"]).stdout)
            .into_owned();
    assert!(
        tracking.contains("refs/minds/remotes/origin/context"),
        "{tracking}"
    );
    assert!(
        tracking.contains("refs/minds/remotes/origin/reviews"),
        "{tracking}"
    );
}

#[test]
fn nothing_new_means_nothing_happens() {
    // Die eigentliche Beschleunigung: Der zweite Lauf darf das Remote nicht
    // anfassen. Beobachtbar ist das an der Ausgabe — ohne fällige Refs wird
    // nicht einmal die Fortschrittszeile gedruckt.
    let Some((_dir, work)) = repo_with_remote() else {
        return;
    };
    minds(&work, &["review", &change_id("cd"), "--approve"]);

    let first = minds(&work, &["sync", "--remote", "origin"]);
    assert!(text(&first).contains("Ref(s)"), "{}", text(&first));

    let second = minds(&work, &["sync", "--remote", "origin", "-v"]);
    assert!(second.status.success());
    assert!(
        !text(&second).contains("Ref(s) →"),
        "ohne neue Refs darf kein Push laufen: {}",
        text(&second)
    );
    assert!(text(&second).contains("nichts Neues"), "{}", text(&second));
}

#[test]
fn a_diverged_review_log_is_merged_not_overwritten() {
    // Zwei Reviewer, zwei Maschinen, dasselbe Remote. Der zweite Push wird
    // abgewiesen (non-fast-forward) — und darf den fremden Verdict weder
    // überschreiben noch verlieren.
    let Some((_dir, work)) = repo_with_remote() else {
        return;
    };
    let Some((_dir_b, other)) = repo_with_remote() else {
        return;
    };
    // Beide zeigen auf dasselbe Remote.
    let remote_url = String::from_utf8_lossy(&git(&work, &["remote", "get-url", "origin"]).stdout)
        .trim()
        .to_string();
    git(&other, &["remote", "set-url", "origin", &remote_url]);

    // Maschine A legt ein Verdict ab und pusht.
    minds(
        &work,
        &[
            "review",
            &change_id("ab"),
            "--approve",
            "--summary",
            "von A",
        ],
    );
    assert!(
        minds(&work, &["sync", "--remote", "origin"])
            .status
            .success()
    );

    // Maschine B kennt das nicht, legt ihr eigenes Verdict ab und pusht.
    minds(
        &other,
        &["review", &change_id("cd"), "--reject", "--summary", "von B"],
    );
    let out = minds(&other, &["sync", "--remote", "origin"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("vereinige"),
        "die Divergenz muss über den Merge laufen: {}",
        text(&out)
    );

    // Beide Verdicts sind am Remote — keines wurde überschrieben.
    minds(&work, &["sync", "--remote", "origin"]);
    let fetched = minds(&other, &["reviews", &change_id("ab")]);
    assert!(
        text(&fetched).contains("von A"),
        "das fremde Verdict muss erhalten bleiben: {}",
        text(&fetched)
    );
    let own = minds(&other, &["reviews", &change_id("cd")]);
    assert!(text(&own).contains("von B"), "{}", text(&own));
}

/// Ein Git-Aufruf mit stdin — für Plumbing, das seine Eingabe liest
/// (`hash-object --stdin`, `mktree`).
fn git_stdin(dir: &Path, args: &[&str], input: &[u8]) -> String {
    use std::io::Write;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = without_user_config(&mut cmd).spawn().expect("git startet");
    child
        .stdin
        .take()
        .expect("stdin ist piped")
        .write_all(input)
        .unwrap();
    let out = child.wait_with_output().expect("git läuft");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Legt den Store-Ref einer Session (`refs/minds/store/<hex>`) als elternlosen
/// Commit mit `session.json` an — so, wie `put` ihn schreibt — und gibt den
/// Commit zurück.
fn session_store_ref(work: &Path, hex: &str, payload: &[u8]) -> String {
    let blob = git_stdin(work, &["hash-object", "-w", "--stdin"], payload);
    let tree = git_stdin(
        work,
        &["mktree"],
        format!("100644 blob {blob}\tsession.json\n").as_bytes(),
    );
    let commit =
        String::from_utf8_lossy(&git(work, &["commit-tree", &tree, "-m", "session"]).stdout)
            .trim()
            .to_string();
    git(
        work,
        &["update-ref", &format!("refs/minds/store/{hex}"), &commit],
    );
    commit
}

#[test]
fn a_forgotten_pushed_session_reaches_the_forge_as_tombstone() {
    // #102: Ein bereits gepushter Session-Ref, der danach vergessen wird, ist
    // non-fast-forward — bis hier behielt die Forge den Klartext als aktuelle,
    // browsbare Ref-Spitze, obwohl `forget` Erfolg meldete. `minds sync` muss
    // genau diesen Ref per gezieltem Force-Push nachziehen.
    let Some((_dir, work)) = repo_with_remote() else {
        return;
    };
    let hex = "ab".repeat(32);
    let plain = session_store_ref(&work, &hex, br#"{"agent":{"name":"x"}}"#);

    // Der Klartext liegt auf der Forge.
    let pushed = minds(&work, &["sync", "--remote", "origin"]);
    assert!(pushed.status.success(), "{}", text(&pushed));
    assert!(
        remote_refs(&work).contains(&plain),
        "Testaufbau: Klartext nicht am Remote"
    );

    // Lokal vergessen …
    let forgotten = minds(
        &work,
        &["forget", &format!("b3-{hex}"), "--reason", "DSGVO"],
    );
    assert!(forgotten.status.success(), "{}", text(&forgotten));

    // … und der nächste Sync überträgt die Löschung — sichtbar gemeldet.
    let sync = minds(&work, &["sync", "--remote", "origin"]);
    assert!(sync.status.success(), "{}", text(&sync));
    assert!(
        text(&sync).contains("Force-Push übertragen"),
        "die Übertragung der Löschung muss gemeldet werden: {}",
        text(&sync)
    );

    // Die Spitze am Remote ist jetzt der Tombstone, der Klartext-Commit fort.
    let refs = remote_refs(&work);
    assert!(
        !refs.contains(&plain),
        "der Klartext ist noch die Ref-Spitze am Remote: {refs}"
    );
    let reference = format!("refs/minds/store/{hex}");
    let local = String::from_utf8_lossy(&git(&work, &["rev-parse", &reference]).stdout)
        .trim()
        .to_string();
    assert!(refs.contains(&local), "Tombstone nicht am Remote: {refs}");
    let content = String::from_utf8_lossy(
        &git(&work, &["show", &format!("{reference}:session.json")]).stdout,
    )
    .into_owned();
    assert!(
        content.contains("minds_tombstone"),
        "die übertragene Spitze ist kein Tombstone: {content}"
    );

    // Und der dritte Lauf hat nichts mehr zu tun — die Buchhaltung stimmt wieder.
    let third = minds(&work, &["sync", "--remote", "origin", "-v"]);
    assert!(text(&third).contains("nichts Neues"), "{}", text(&third));

    // Ein zweiter `forget` derselben Session ist ein No-op: nichts neu zu
    // tilgen, kein Ref-Delete — und der nächste Sync bleibt still.
    let again = minds(
        &work,
        &["forget", &format!("b3-{hex}"), "--reason", "DSGVO"],
    );
    assert!(again.status.success(), "{}", text(&again));
    assert!(
        text(&again).contains("nichts zu vergessen"),
        "{}",
        text(&again)
    );
    let fourth = minds(&work, &["sync", "--remote", "origin", "-v"]);
    assert!(text(&fourth).contains("nichts Neues"), "{}", text(&fourth));
}

#[cfg(unix)]
#[test]
fn a_denied_erasure_push_is_reported_loudly() {
    // Weist die Forge die `+`-Refspec ab (hier: ein pre-receive-Hook, in freier
    // Wildbahn ein Protected Branch auf `minds/session/*` — Gits
    // `receive.denyNonFastForwards` greift nur für `refs/heads/*`), darf die
    // Löschung nicht lautlos ausbleiben: `forget` hat sie bereits zugesagt.
    // Die Meldung muss bei jedem Sync wiederkommen, bis der Tombstone durch ist.
    use std::os::unix::fs::PermissionsExt;

    let Some((_dir, work)) = repo_with_remote() else {
        return;
    };
    let hex = "ef".repeat(32);
    let plain = session_store_ref(&work, &hex, br#"{"agent":{"name":"x"}}"#);
    let pushed = minds(&work, &["sync", "--remote", "origin"]);
    assert!(pushed.status.success(), "{}", text(&pushed));

    // Ab jetzt weist das Remote jeden weiteren Update ab.
    let remote_url = String::from_utf8_lossy(&git(&work, &["remote", "get-url", "origin"]).stdout)
        .trim()
        .to_string();
    let hook = Path::new(&remote_url).join("hooks/pre-receive");
    std::fs::write(&hook, "#!/bin/sh\necho 'rejected by policy' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    let forgotten = minds(
        &work,
        &["forget", &format!("b3-{hex}"), "--reason", "DSGVO"],
    );
    assert!(forgotten.status.success(), "{}", text(&forgotten));

    let sync = minds(&work, &["sync", "--remote", "origin"]);
    assert!(
        text(&sync).contains("NICHT bestätigt"),
        "die abgewiesene Löschung muss gemeldet werden: {}",
        text(&sync)
    );
    // Der Klartext steht noch als Ref-Spitze am Remote — genau das soll die
    // Meldung sichtbar halten. Und der nächste Lauf meldet es wieder.
    assert!(
        remote_refs(&work).contains(&plain),
        "{}",
        remote_refs(&work)
    );
    let next = minds(&work, &["sync", "--remote", "origin"]);
    assert!(
        text(&next).contains("NICHT bestätigt"),
        "die Meldung muss wiederkommen, bis die Löschung durch ist: {}",
        text(&next)
    );
}

#[test]
fn a_forgotten_session_branch_in_the_child_repo_reaches_its_forge_as_tombstone() {
    // Der Pfad, um den es in #102 eigentlich geht: Im Child-Backend erscheint
    // die Session als browsbarer Branch `minds/session/<hex>` auf der Forge —
    // mit voller `session.md`. Nach `forget` muss der nächste Sync genau diesen
    // Branch (und den Store-Ref) per Force-Push auf den Tombstone ziehen.
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let child = dir.path().join("kontext");
    let child_remote = dir.path().join("kontext-remote.git");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    if !Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&child_remote)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        return;
    }
    for repo in [&work, &child] {
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "test@minds.invalid"]);
        git(repo, &["config", "user.name", "Minds Test"]);
    }
    std::fs::write(work.join("a.txt"), "hallo\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "erster Commit"]);
    git(
        &child,
        &["remote", "add", "origin", &child_remote.to_string_lossy()],
    );
    git(&work, &["config", "minds.backend", "child-repo"]);
    git(&work, &["config", "minds.childPath", "../kontext"]);

    // Die Session liegt im Child: Store-Ref und browsbarer Branch. Die ID ist
    // der blake3 der Bytes — wie beim echten `put`: Seit #100 prüft `forget`
    // die Identität der Branch-Nutzlast; unter einer frei erfundenen ID bliebe
    // der Branch als vermeintlich fremder stehen.
    let payload = br#"{"agent":{"name":"x"}}"#;
    let id = minds_core::SessionId::from_canonical_bytes(payload).to_string();
    let hex = id
        .strip_prefix(minds_core::SESSION_ID_PREFIX)
        .unwrap()
        .to_owned();
    let hex16 = &hex[..16];
    session_store_ref(&child, &hex, payload);
    let json = git_stdin(&child, &["hash-object", "-w", "--stdin"], payload);
    let md = git_stdin(
        &child,
        &["hash-object", "-w", "--stdin"],
        b"# Session\n\nstreng geheim\n",
    );
    let tree = git_stdin(
        &child,
        &["mktree"],
        format!("100644 blob {json}\tsession.json\n100644 blob {md}\tsession.md\n").as_bytes(),
    );
    let commit =
        String::from_utf8_lossy(&git(&child, &["commit-tree", &tree, "-m", "session"]).stdout)
            .trim()
            .to_string();
    git(
        &child,
        &[
            "update-ref",
            &format!("refs/minds/sessions/{hex16}"),
            &commit,
        ],
    );

    // Erster Sync: Der Branch steht mit Klartext auf der Child-Forge.
    let pushed = minds(&work, &["sync"]);
    assert!(pushed.status.success(), "{}", text(&pushed));
    let branch = format!("refs/heads/minds/session/{hex16}");
    let at_remote = String::from_utf8_lossy(&git(&child, &["ls-remote", "origin", &branch]).stdout)
        .into_owned();
    assert!(
        at_remote.contains(&commit),
        "Testaufbau: Branch nicht am Child-Remote: {at_remote}"
    );

    // forget im Arbeitsrepo tilgt im Child …
    let forgotten = minds(&work, &["forget", &id, "--reason", "DSGVO"]);
    assert!(forgotten.status.success(), "{}", text(&forgotten));

    // … und der nächste Sync zieht den Branch per Force-Push nach.
    let sync = minds(&work, &["sync"]);
    assert!(sync.status.success(), "{}", text(&sync));
    assert!(
        text(&sync).contains("Force-Push übertragen"),
        "{}",
        text(&sync)
    );
    let tomb = String::from_utf8_lossy(
        &git(
            &child,
            &["rev-parse", &format!("refs/minds/sessions/{hex16}")],
        )
        .stdout,
    )
    .trim()
    .to_string();
    let at_remote = String::from_utf8_lossy(&git(&child, &["ls-remote", "origin", &branch]).stdout)
        .into_owned();
    assert!(
        at_remote.contains(&tomb) && !at_remote.contains(&commit),
        "die Branch-Spitze am Child-Remote muss der Tombstone sein: {at_remote}"
    );
    let md_now = String::from_utf8_lossy(
        &git(
            &child,
            &["show", &format!("refs/minds/sessions/{hex16}:session.md")],
        )
        .stdout,
    )
    .into_owned();
    assert!(
        md_now.contains("minds_tombstone"),
        "session.md muss der Tombstone sein: {md_now}"
    );
}

#[test]
fn a_diverged_plaintext_ref_is_still_not_force_pushed() {
    // Die Gegenprobe zu #102: Die Force-Ausnahme gilt **nur** für Tombstones.
    // Ein regulär divergierter Klartext-Ref bleibt zurückgestellt und wird
    // gemeldet — der Remote-Stand wird nie mit Klartext überschrieben.
    let Some((_dir, work)) = repo_with_remote() else {
        return;
    };
    let hex = "cd".repeat(32);
    let first = session_store_ref(&work, &hex, br#"{"agent":{"name":"a"}}"#);
    let pushed = minds(&work, &["sync", "--remote", "origin"]);
    assert!(pushed.status.success(), "{}", text(&pushed));

    // Divergenz von Hand: ein anderer Klartext-Orphan ersetzt den Ref.
    let second = session_store_ref(&work, &hex, br#"{"agent":{"name":"b"}}"#);
    assert_ne!(first, second, "Testaufbau: keine Divergenz");

    let sync = minds(&work, &["sync", "--remote", "origin"]);
    assert!(sync.status.success(), "{}", text(&sync));
    assert!(
        text(&sync).contains("nicht übertragen"),
        "der zurückgestellte Ref muss gemeldet werden: {}",
        text(&sync)
    );
    let refs = remote_refs(&work);
    assert!(
        refs.contains(&first) && !refs.contains(&second),
        "der Remote-Stand wurde überschrieben: {refs}"
    );
}
