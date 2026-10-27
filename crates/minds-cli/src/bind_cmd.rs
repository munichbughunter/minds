//! `minds bind` (Spec S3-MVP §6.1, ADR-S3-005): fragt den Witness, die
//! Commits eines Pushes an seine eigenen, inhaltsgebundenen Seals zu binden.
//!
//! Die CLI bestimmt nur, **welche** Commits gebunden werden — mit genau der
//! Menge, die der Server bewertet ([`minds_witness_git::access::commit_set`],
//! S1 „neue Commits“) — und schickt je Commit `{commit, expected_project}`.
//! Seals, Bindungsbytes und Schlüssel kennt sie nie: Der Witness wählt aus
//! seinem Ledger, baut, signiert und legt ab, und nur für das Projekt aus
//! seiner `witness.json`. `expected_project` stammt aus `git config
//! minds.project` und ist nur die Erwartung, die der Witness prüft.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use minds_git::quarantine::{Limits, ObjectKind, ReceiveEnv, ServerRepo};

/// Höchstzahl der Commits, die `bind` je Ref-Zeile bindet (Spec S3 §6.1):
/// oberhalb von `max_commits_per_push` bewertet der Server ohnehin nicht
/// mehr je Commit (S1 `truncated`).
pub(crate) const MAX_COMMITS_BIND: usize = 500;

/// So lange bindet der pre-push-Hook höchstens (Spec S3 §6.2: minds hält
/// den Push nicht auf).
const HOOK_BIND_TIME: Duration = Duration::from_secs(30);

/// Längste Laufzeit von `rev-list` je Ref-Zeile.
const REV_LIST_TIME: Duration = Duration::from_secs(30);

/// Die Null-Id (Löschung bzw. Anlage in einer pre-push-Zeile).
fn is_null(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b == b'0')
}

/// Eine Zeile der pre-push-Eingabe: `<local-ref> <local-sha> <remote-ref>
/// <remote-sha>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PushLine {
    pub(crate) local_ref: String,
    pub(crate) local_sha: String,
    pub(crate) remote_ref: String,
    pub(crate) remote_sha: String,
}

/// Liest die pre-push-Eingabe; Zeilen, die nicht genau vier Felder mit zwei
/// vollen Ids haben, zählen nicht.
pub(crate) fn parse_push_lines(text: &str) -> Vec<PushLine> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (Some(local_ref), Some(local_sha), Some(remote_ref), Some(remote_sha), None) = (
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
            ) else {
                return None;
            };
            let id = |s: &str| minds_witness_git::binding::is_commit_id(s);
            (id(local_sha) && id(remote_sha)).then(|| PushLine {
                local_ref: local_ref.to_owned(),
                local_sha: local_sha.to_owned(),
                remote_ref: remote_ref.to_owned(),
                remote_sha: remote_sha.to_owned(),
            })
        })
        .collect()
}

/// Die Lesesicht des Servers über dem lokalen Git-Verzeichnis.
pub(crate) fn open_view(git_dir: &Path) -> Result<ServerRepo, String> {
    ServerRepo::open_for_receive(
        &ReceiveEnv {
            git_dir: git_dir.to_path_buf(),
            quarantine: None,
            namespace: false,
        },
        Limits::default(),
    )
    .map_err(|err| format!("repository not readable for binding ({err})"))
}

/// Die zu bindenden Commits (Spec S3 §6.1, Tabelle „Commit-Menge“) in der
/// Reihenfolge des Servers, ohne Doppelte:
///
/// - Löschung (`local-sha` Null): keine;
/// - Update (`remote-sha` bekannt): `rev-list <local> --not <remote>`;
/// - Anlage (`remote-sha` Null) und unbekannte Basis (`remote-sha` fehlt
///   lokal): `rev-list <local>`, alle erreichbaren wie im Server, ohne
///   `--not --remotes`; je Zeile höchstens [`MAX_COMMITS_BIND`].
pub(crate) fn commits_for_push(
    repo: &ServerRepo,
    git: &Path,
    lines: &[PushLine],
) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in lines {
        // Löschung — und wie der Server (S2 §7.0 Regel 8): Ein Ziel, das kein
        // Commit ist (annotierter Tag), wird nicht je Commit bewertet.
        if is_null(&line.local_sha)
            || !matches!(repo.kind(&line.local_sha), Ok(Some(ObjectKind::Commit)))
        {
            continue;
        }
        let old = (!is_null(&line.remote_sha)
            && matches!(repo.kind(&line.remote_sha), Ok(Some(ObjectKind::Commit))))
        .then_some(line.remote_sha.as_str());
        let (commits, _) = minds_witness_git::access::commit_set(
            repo,
            git,
            old,
            &line.local_sha,
            MAX_COMMITS_BIND,
            REV_LIST_TIME,
        )
        .map_err(|err| format!("commits not listable ({err:?})"))?;
        for c in commits {
            if seen.insert(c.clone()) {
                out.push(c);
            }
        }
    }
    Ok(out)
}

/// `git rev-parse` in `root`, ohne geerbtes `GIT_DIR`.
fn rev_parse(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .args(args)
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !value.is_empty()).then_some(value)
}

fn commit_of(root: &Path, rev: &str) -> Result<String, String> {
    rev_parse(
        root,
        &[
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .ok_or_else(|| format!("{} is not a commit", crate::text::sanitize(rev)))
}

/// Die Ref-Zeilen ohne pre-push: ein Bereich `<a>..<b>` wie ein Update,
/// ein einzelner Rev wie eine Anlage (alle erreichbaren, begrenzt); ohne
/// Angabe HEAD gegen den Remote-Tracking-Ref (`--remote`) bzw. den Upstream,
/// sonst wie eine Anlage.
pub(crate) fn lines_for(
    root: &Path,
    range: Option<&str>,
    remote: Option<&str>,
) -> Result<Vec<PushLine>, String> {
    let null = "0".repeat(40);
    let line = |old: Option<String>, new: String| PushLine {
        local_ref: String::new(),
        local_sha: new,
        remote_ref: String::new(),
        remote_sha: old.unwrap_or_else(|| null.clone()),
    };
    if let Some(range) = range {
        if let Some((a, b)) = range.split_once("..") {
            if b.starts_with('.') {
                return Err("symmetric ranges (a...b) are not supported".into());
            }
            let b = if b.is_empty() { "HEAD" } else { b };
            return Ok(vec![line(Some(commit_of(root, a)?), commit_of(root, b)?)]);
        }
        return Ok(vec![line(None, commit_of(root, range)?)]);
    }
    let head = commit_of(root, "HEAD")?;
    let base = match remote {
        Some(remote) => rev_parse(root, &["--abbrev-ref", "HEAD"]).and_then(|branch| {
            rev_parse(
                root,
                &[
                    "--verify",
                    "--quiet",
                    &format!("refs/remotes/{remote}/{branch}^{{commit}}"),
                ],
            )
        }),
        None => rev_parse(root, &["--verify", "--quiet", "@{upstream}^{commit}"]),
    };
    Ok(vec![line(base, head)])
}

/// Das `git` aus `PATH`, absolut: `rev-list` läuft mit geleerter Umgebung.
pub(crate) fn which_git() -> Option<PathBuf> {
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|path| std::fs::metadata(path).is_ok_and(|m| m.is_file() && executable(&m)))
}

#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_: &std::fs::Metadata) -> bool {
    true
}

/// Was der Witness je Commit geantwortet hat, für die Ausgabe.
fn describe(commit: &str, answer: Result<&str, &str>) -> (String, bool) {
    let short = &commit[..12.min(commit.len())];
    let files = |part: Option<&str>| {
        part.and_then(|p| p.split_once('/'))
            .map(|(covered, total)| format!("{covered}/{total} files covered"))
            .unwrap_or_default()
    };
    match answer {
        Ok(status) => {
            let parts: Vec<&str> = status.split(' ').collect();
            let line = match parts.as_slice() {
                ["bound", id, cover] => format!(
                    "minds: {short} bound: {} (binding {})",
                    files(Some(cover)),
                    id.chars().take(15).collect::<String>()
                ),
                ["unchanged", _, cover] => {
                    format!("minds: {short} already bound: {}", files(Some(cover)))
                }
                ["selected", seals, cover] => format!(
                    "minds: {short} would bind {seals} seal(s): {} (dry run, nothing signed)",
                    files(Some(cover))
                ),
                ["no-evidence", _] => format!("minds: {short} no evidence found, no binding"),
                _ => format!("minds: {short} unexpected answer from the witness"),
            };
            (crate::text::sanitize(&line), true)
        }
        Err(reason) => {
            let why = match reason {
                "project_mismatch" => {
                    "the witness binds for another project than `git config minds.project` \
                     (project_mismatch)"
                }
                "project_unpinned" => {
                    "the witness has no pinned project (project_unpinned) — pin it with \
                     `minds witness init --project <instance>/project-<id>` as the witness \
                     user, or `minds enable --project …`"
                }
                "unknown commit" => "the witness does not have this commit",
                "binding cap reached" => "4 bindings exist for this commit already",
                "the in-repo store is required" => "the witness needs the in-repo store",
                "binding ref occupied" => {
                    "something else lies under the name of the new binding (not this witness's)"
                }
                "rate limited" => "the witness is rate limiting, try again in a second",
                "bind failed" => "the witness failed (see its log)",
                _ => "the witness refused",
            };
            (format!("minds: {short} not bound: {why}"), false)
        }
    }
}

/// `minds bind [<rev-range>] [--remote <r>] [--dry-run]`.
pub(crate) fn run(range: Option<&str>, remote: Option<&str>, dry_run: bool) -> ExitCode {
    match bind(range, remote, dry_run) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("minds bind: {}", crate::text::sanitize(&err));
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn bind(_: Option<&str>, _: Option<&str>, _: bool) -> Result<bool, String> {
    Err("minds bind needs a witness, which runs on macOS and Linux only".into())
}

#[cfg(unix)]
fn bind(range: Option<&str>, remote: Option<&str>, dry_run: bool) -> Result<bool, String> {
    let here = std::env::current_dir().map_err(|err| err.to_string())?;
    let root = crate::hook::witness::repo_root(&here).ok_or("not inside a Git worktree")?;
    if let Some(remote) = remote {
        if remote.is_empty() || remote.starts_with('-') || remote.contains(['/', ' ', ':']) {
            return Err("--remote must name a configured remote".into());
        }
    }
    let project = expected_project(&root)?.ok_or(
        "no project set — `git config minds.project <instance>/project-<id>` names the project \
         the witness should bind for",
    )?;
    let git_dir = git_dir(&root).ok_or("no git directory")?;
    let view = open_view(&git_dir)?;
    let lines = lines_for(&root, range, remote)?;
    let git = which_git().ok_or("git not found on PATH")?;
    let commits = commits_for_push(&view, &git, &lines)?;
    if commits.is_empty() {
        println!("minds: nothing to bind");
        return Ok(true);
    }
    bind_commits(&root, &project, &commits, dry_run, false)
}

/// Das erwartete Projekt aus `git config minds.project` — `None`, wenn
/// keines gesetzt ist (dann bindet `minds` nichts); eine ungültige Form ist
/// ein Fehler.
pub(crate) fn expected_project(root: &Path) -> Result<Option<String>, String> {
    match crate::sign_cmd::git_config_value(root, "minds.project") {
        None => Ok(None),
        Some(p) if minds_witness_git::binding::is_project_id(&p) => Ok(Some(p)),
        Some(_) => Err("minds.project must have the form <instance>/project-<id>".into()),
    }
}

/// Das absolute Git-Verzeichnis von `root`.
pub(crate) fn git_dir(root: &Path) -> Option<PathBuf> {
    rev_parse(root, &["--absolute-git-dir"]).map(PathBuf::from)
}

/// Fragt den Witness je Commit (Spec S3 §6.1) und druckt je Commit eine
/// Zeile. `Ok(false)`, wenn ein Commit nicht gebunden werden konnte;
/// `Err`, wenn gar nicht gefragt werden konnte (Store, Witness). Aus dem
/// pre-push-Hook (`from_hook`) ohne den Hinweis zur Anrechnung, der bei
/// jedem Push nur Lärm wäre.
#[cfg(unix)]
pub(crate) fn bind_commits(
    root: &Path,
    expected: &str,
    commits: &[String],
    dry_run: bool,
    from_hook: bool,
) -> Result<bool, String> {
    use minds_capture::witness_proto::Frame;

    if !matches!(
        crate::config::load(root).backend(),
        minds_store::Backend::InRepo
    ) {
        return Err("bindings need the in-repo store (minds.backend in-repo)".into());
    }
    let found = crate::hook::witness::discovered_socket(root)
        .ok_or("no witness found for this repository — minds enable sets one up")?;
    // SAFETY: geteuid hat keine Vorbedingungen.
    let euid = unsafe { libc::geteuid() };
    if !from_hook && found.root.is_some() && found.peer == Some(euid) {
        crate::text::warn(
            "the witness runs under your own user ID — its observations are not separated, so \
             the server credits none of them; bindings stay empty",
        );
    }
    let mut all_ok = true;
    let mut covered_any = false;
    // Aus dem Hook bindet minds höchstens so lange, harte Grenze: Der Push des
    // Nutzers wartet darauf (ein neuer Branch kann Hunderte Commits tragen).
    // Die jüngsten zuerst — sie brauchen die Evidence, ältere sind meist
    // schon gebunden.
    let stop = from_hook.then(|| std::time::Instant::now() + HOOK_BIND_TIME);
    let ordered: Vec<&String> = if from_hook {
        commits.iter().rev().collect()
    } else {
        commits.iter().collect()
    };
    let mut counts = [0usize; 3]; // gebunden, unverändert, ohne Evidence
    for (done, commit) in ordered.iter().enumerate() {
        let left = stop.map(|stop| stop.saturating_duration_since(std::time::Instant::now()));
        if left.is_some_and(|left| left < Duration::from_millis(500)) {
            println!(
                "minds: warning: binding stopped after {}s; {} commit(s) not bound — run \
                 `minds bind` and `minds sync --evidence` to finish",
                HOOK_BIND_TIME.as_secs(),
                ordered.len() - done
            );
            return Ok(false);
        }
        let per_request = left.map_or(crate::checkpoint::delegate::timeout(), |left| {
            left.min(crate::checkpoint::delegate::timeout())
        });
        let commit: &String = commit;
        let request = Frame::Bind {
            commit: commit.clone(),
            expected_project: expected.to_owned(),
            dry_run,
            request_id: crate::checkpoint::delegate::request_id(),
        };
        let ask = || {
            let deadline = std::time::Instant::now() + per_request;
            crate::hook::witness::request(&found.socket, found.peer, &request, deadline)
        };
        let mut answer = ask();
        // Der Witness lässt je Commit (und nach einem Fehlschlag) eine
        // Sekunde Abstand; einmal nachfragen genügt — aus dem Hook nur, wenn
        // die Zeit dafür reicht.
        let time_for_retry = stop.is_none_or(|stop| {
            std::time::Instant::now() + Duration::from_millis(1100) + per_request <= stop
        });
        if time_for_retry
            && matches!(&answer, Ok(Frame::Nack { reason, .. }) if reason == "rate limited")
        {
            std::thread::sleep(Duration::from_millis(1100));
            answer = ask();
        }
        let (line, ok) = match &answer {
            Ok(Frame::Ack { status, .. }) => describe(commit, Ok(status)),
            Ok(Frame::Nack { reason, .. }) => describe(commit, Err(reason)),
            Ok(_) => describe(commit, Err("unexpected")),
            Err(kind) => (
                format!(
                    "minds: {} not bound: witness unavailable ({kind})",
                    &commit[..12]
                ),
                false,
            ),
        };
        // Aus dem Hook: Erfolge nur gezählt (eine Zeile am Ende), jede
        // Störung als eigene Zeile.
        match &answer {
            Ok(Frame::Ack { status, .. }) if from_hook => {
                let at = if status.starts_with("bound") {
                    0
                } else if status.starts_with("unchanged") {
                    1
                } else {
                    2
                };
                counts[at] += 1;
            }
            _ => println!("{line}"),
        }
        all_ok &= ok;
        covered_any |= matches!(&answer, Ok(Frame::Ack { status, .. })
            if !status.starts_with("no-evidence"));
        // Ein abweichendes oder fehlendes Projekt gilt für jeden Commit;
        // ein unerreichbarer Witness ebenso.
        if matches!(&answer, Ok(Frame::Nack { reason, .. })
            if reason == "project_mismatch" || reason == "project_unpinned")
            || answer.is_err()
        {
            return Ok(false);
        }
    }
    if from_hook && counts.iter().sum::<usize>() > 0 {
        println!(
            "minds: {} commit(s) bound, {} already bound, {} without evidence",
            counts[0], counts[1], counts[2]
        );
    }
    if covered_any && !from_hook {
        // Spec S3 §6.1: Die Vorschau ist die Abdeckung des Servers nur, wenn
        // er diese Seals anerkennt.
        println!(
            "minds: the server counts this coverage only if it trusts this witness's key and \
             has the seals and observations (pushed with minds sync)"
        );
    }
    Ok(all_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_described_with_fixed_words() {
        let c = "a".repeat(40);
        assert_eq!(
            describe(&c, Ok("bound b3-0123456789abcdef 2/3")).0,
            "minds: aaaaaaaaaaaa bound: 2/3 files covered (binding b3-0123456789ab)"
        );
        assert_eq!(
            describe(&c, Ok("no-evidence 0/3")).0,
            "minds: aaaaaaaaaaaa no evidence found, no binding"
        );
        let (line, ok) = describe(&c, Err("\u{1b}[31mevil"));
        assert!(!ok && line.ends_with("the witness refused"), "{line}");
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_DATE", "2026-10-27T10:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-10-27T10:00:00Z")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit(root: &Path, file: &str) -> String {
        std::fs::write(root.join(file), file).unwrap();
        git(root, &["add", file]);
        git(root, &["commit", "-qm", file]);
        git(root, &["rev-parse", "HEAD"])
    }

    fn rev_list(root: &Path, args: &[&str]) -> std::collections::BTreeSet<String> {
        let mut all = vec!["rev-list"];
        all.extend_from_slice(args);
        git(root, &all).lines().map(str::to_owned).collect()
    }

    /// Spec S3 §6.1, s3_16 **[R6]**: Die Commit-Menge des Clients ist die des
    /// Servers (S1 „neue Commits“, `GitObjectAccess::new_commits` über
    /// dieselbe Funktion) — in jedem Fall der Tabelle.
    #[cfg(unix)]
    #[test]
    fn s3_16_bind_commit_set_matches_server() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "--template=", "-b", "main"]);
        for (k, v) in [
            ("user.name", "t"),
            ("user.email", "t@t"),
            ("commit.gpgsign", "false"),
        ] {
            git(root, &["config", k, v]);
        }
        let a = commit(root, "a");
        let b = commit(root, "b");
        git(root, &["update-ref", "refs/remotes/origin/main", &b]);
        git(root, &["checkout", "-q", "-b", "feature", &a]);
        let c = commit(root, "c");
        let d = commit(root, "d");
        git(root, &["checkout", "-q", "main"]);
        let e = commit(root, "e");
        git(root, &["merge", "-q", "--no-edit", "--no-ff", "feature"]);
        let m = git(root, &["rev-parse", "HEAD"]);
        let repo = open_view(&root.join(".git")).unwrap();
        let git_bin = which_git().unwrap();
        let null = "0".repeat(40);
        let line = |local: &str, remote: &str| PushLine {
            local_ref: "refs/heads/x".into(),
            local_sha: local.into(),
            remote_ref: "refs/heads/x".into(),
            remote_sha: remote.into(),
        };
        let set = |lines: &[PushLine]| -> std::collections::BTreeSet<String> {
            commits_for_push(&repo, &git_bin, lines)
                .unwrap()
                .into_iter()
                .collect()
        };
        // (a) Neuer Branch auf Historie, die schon unter Remote-Tracking-Refs
        // liegt: alle erreichbaren, wie der Server — ohne `--not --remotes`.
        assert_eq!(set(&[line(&d, &null)]), rev_list(root, &[&d]));
        assert!(set(&[line(&d, &null)]).contains(&a));
        // (b) Update.
        assert_eq!(set(&[line(&e, &b)]), [e.clone()].into_iter().collect());
        // (c) Force-Push: kein Nachfahre.
        assert_eq!(set(&[line(&d, &b)]), rev_list(root, &[&d, "--not", &b]));
        assert_eq!(
            set(&[line(&d, &b)]),
            [c.clone(), d.clone()].into_iter().collect()
        );
        // (d) Merge.
        assert_eq!(set(&[line(&m, &b)]), rev_list(root, &[&m, "--not", &b]));
        // (e) Unbekannte Basis: Regel „Anlage“.
        assert_eq!(set(&[line(&e, &"7".repeat(40))]), rev_list(root, &[&e]));
        // Löschung: nichts; mehrere Zeilen: Vereinigung ohne Doppelte.
        assert!(set(&[line(&null, &b)]).is_empty());
        let both = commits_for_push(&repo, &git_bin, &[line(&e, &b), line(&m, &b)]).unwrap();
        let unique: std::collections::BTreeSet<&String> = both.iter().collect();
        assert_eq!(unique.len(), both.len());
        // Die Reihenfolge ist die des Servers: Eltern vor Kindern.
        let (order, truncated) = minds_witness_git::access::commit_set(
            &repo,
            &git_bin,
            Some(&b),
            &m,
            MAX_COMMITS_BIND,
            REV_LIST_TIME,
        )
        .unwrap();
        assert!(!truncated);
        assert_eq!(
            commits_for_push(&repo, &git_bin, &[line(&m, &b)]).unwrap(),
            order
        );
        let pos = |x: &str| order.iter().position(|o| o == x).unwrap();
        assert!(pos(&c) < pos(&d) && pos(&d) < pos(&m) && pos(&e) < pos(&m));
    }
}
