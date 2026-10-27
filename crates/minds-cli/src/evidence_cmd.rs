//! Evidence vor Code (Spec S3-MVP §6.2, ADR-S3-006): die Bindungen der
//! Commits eines Pushes, die darin genannten Seals und deren Observations —
//! **synchron** übertragen, bevor Gits eigener Push läuft, damit der Server
//! sie beim Code-Push vorfindet.
//!
//! Gemeinsam für den pre-push-Orchestrator ([`crate::prepush`]) und `minds
//! sync --evidence`. Der interne Push läuft mit `--no-verify` und
//! `MINDS_SYNCING`: Er löst keinen weiteren Orchestrator aus. Er
//! überschreibt nie (kein `+`), überspringt am Remote inhaltsgleich
//! vorhandene Refs und kennt eine Frist.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use minds_git::quarantine::{RefTarget, ServerRepo};
use minds_witness_git::binding::{CommitBinding, DEFAULT_MAX_SEALS};
use minds_witness_git::view::{EvidenceView, MODE_BLOB};

/// Frist für Abgleich und Übertragung zusammen (Spec S3 §6.2 Schritt 5).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Überschreibt die Frist in Millisekunden (Tests, langsame Leitungen).
pub(crate) const TIMEOUT_ENV: &str = "MINDS_EVIDENCE_TIMEOUT_MS";
/// Die Wächtervariable: Ein Push mit ihr löst keinen minds-Ablauf aus.
pub(crate) const GUARD_ENV: &str = "MINDS_SYNCING";

/// Größte Bindung, die gelesen wird (Spec S2 §9).
const MAX_BINDING_BYTES: u64 = 64 * 1024;
/// Größter Seal-Text (Spec S2 §8).
const MAX_SEAL_BYTES: u64 = 64 * 1024;

/// Ein zu übertragender Ref: Name und lokales Ziel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceRef {
    pub(crate) name: String,
    pub(crate) target: String,
}

/// Die Evidence-Menge der `commits` aus dem lokalen Repository, in der
/// Reihenfolge der Übertragung: erst Observations, dann Seals, dann
/// Bindungen (Spec S3 §6.2 Schritt 4 und 5) — jede Gruppe nach Namen, ohne
/// Doppelte. Nur Refs, die lokal liegen; Unlesbares wird übergangen (der
/// Server prüft ohnehin jedes Objekt).
pub(crate) fn evidence_refs(repo: &ServerRepo, commits: &[String]) -> Vec<EvidenceRef> {
    let view = minds_witness_git::access::RepoView::new(repo);
    let direct = |name: &str| match repo.prestate().read_ref(name) {
        Ok(Some(RefTarget::Direct(id))) => Some(id),
        _ => None,
    };
    let mut bindings: BTreeMap<String, String> = BTreeMap::new();
    let mut seal_ids: BTreeSet<String> = BTreeSet::new();
    for commit in commits {
        let prefix = minds_witness_git::candidates::prefix(commit);
        let Ok(found) = repo.prestate().read_prefix(&prefix) else {
            continue;
        };
        for (name, target) in found {
            if let Some(binding) = read_binding(&view, &target) {
                seal_ids.extend(binding.seals().iter().cloned());
            }
            bindings.insert(name, target);
        }
    }
    let mut seals: BTreeMap<String, String> = BTreeMap::new();
    let mut observations: BTreeMap<String, String> = BTreeMap::new();
    for id in &seal_ids {
        let name = format!("refs/minds/evidence/{}", &id[3..]);
        let Some(target) = direct(&name) else {
            continue;
        };
        if let Some(obs) = seal_observations(&view, &target, id) {
            let obs_name = format!("refs/minds/observations/{}", &obs[3..]);
            if let Some(obs_target) = direct(&obs_name) {
                observations.insert(obs_name, obs_target);
            }
        }
        seals.insert(name, target);
    }
    observations
        .into_iter()
        .chain(seals)
        .chain(bindings)
        .map(|(name, target)| EvidenceRef { name, target })
        .collect()
}

fn read_binding(view: &dyn EvidenceView, target: &str) -> Option<CommitBinding> {
    let tree = view.tree(target).ok()?;
    let entry = tree
        .iter()
        .find(|e| e.name == "binding" && e.mode == MODE_BLOB)?;
    let bytes = view.blob(&entry.oid, MAX_BINDING_BYTES).ok()??;
    CommitBinding::parse(&bytes, DEFAULT_MAX_SEALS).ok()
}

/// Die Observation-Id eines Seals (`b3-…`), sofern sein Text lesbar ist und
/// zu `id` passt.
fn seal_observations(view: &dyn EvidenceView, target: &str, id: &str) -> Option<String> {
    let tree = view.tree(target).ok()?;
    let entry = tree
        .iter()
        .find(|e| e.name == "seal" && e.mode == MODE_BLOB)?;
    let bytes = view.blob(&entry.oid, MAX_SEAL_BYTES).ok()??;
    let text = String::from_utf8(bytes).ok()?;
    let (_, obs) = minds_witness_git::selection::seal_observations(id, &text)?;
    obs.filter(|o| minds_witness_git::binding::is_seal_id(o))
}

/// Ergebnis einer Übertragung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Transfer {
    /// Nichts zu übertragen (keine Evidence oder alles schon dort).
    Nothing,
    /// So viele Refs übertragen.
    Sent(usize),
    /// Nicht übertragen; der feste Grund.
    Failed(&'static str),
}

impl Transfer {
    /// Die Zeile für den Nutzer (Spec S3 §6.2 Schritt 7; Englisch wie die
    /// übrige CLI). `None`, wenn es nichts zu sagen gibt.
    pub(crate) fn line(&self) -> Option<String> {
        match self {
            Self::Nothing => None,
            Self::Sent(n) => Some(format!("minds: evidence transferred ({n} refs)")),
            Self::Failed(reason) => Some(format!(
                "minds: warning: evidence not transferred ({reason}); the code push continues"
            )),
        }
    }
}

/// Die Frist aus [`TIMEOUT_ENV`], sonst [`DEFAULT_TIMEOUT`].
fn timeout() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(DEFAULT_TIMEOUT, Duration::from_millis)
        .min(Duration::from_secs(600))
}

/// Ein `git`-Kindprozess bis `deadline`; stdout gesammelt, stderr
/// durchgereicht (die Ablehnung des Servers bleibt sichtbar), oder
/// verworfen. `Err(true)` heißt Frist abgelaufen, `Err(false)` nicht
/// startbar.
fn run_git(
    root: &Path,
    args: &[String],
    deadline: Instant,
    show_stderr: bool,
) -> Result<(bool, String), bool> {
    let mut command = Command::new("git");
    // Keine Mitschnitte des Transports (Zugangsdaten in URLs, Header).
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_TRACE") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env(GUARD_ENV, "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if show_stderr {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .spawn()
        .map_err(|_| false)?;
    let mut stdout = child.stdout.take().expect("stdout ist eine Pipe");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(true);
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    Ok((status.success(), String::from_utf8_lossy(&out).into_owned()))
}

/// Ob `remote` als Argument taugt: ein Remote-Name oder eine URL, nie eine
/// Option.
fn plain_remote(remote: &str) -> bool {
    !remote.is_empty() && !remote.starts_with('-') && !remote.chars().any(char::is_control)
}

/// Überträgt `refs` an `remote`: erst `git ls-remote` für genau diese Namen,
/// inhaltsgleich vorhandene werden übersprungen; dann ein `git push
/// --no-verify --porcelain` mit den übrigen, in der gegebenen Reihenfolge,
/// ohne `--force` und ohne `--atomic`.
pub(crate) fn transfer(root: &Path, remote: &str, refs: &[EvidenceRef]) -> Transfer {
    if refs.is_empty() {
        return Transfer::Nothing;
    }
    if !plain_remote(remote) {
        return Transfer::Failed("unreachable");
    }
    let deadline = Instant::now() + timeout();
    let mut args = vec!["ls-remote".to_owned(), remote.to_owned()];
    args.extend(refs.iter().map(|r| r.name.clone()));
    let present: BTreeMap<String, String> = match run_git(root, &args, deadline, false) {
        Ok((true, out)) => out
            .lines()
            .filter_map(|line| {
                let (oid, name) = line.split_once('\t')?;
                Some((name.to_owned(), oid.to_owned()))
            })
            .collect(),
        Ok((false, _)) => return Transfer::Failed("unreachable"),
        Err(true) => return Transfer::Failed("timeout"),
        Err(false) => return Transfer::Failed("unreachable"),
    };
    let missing: Vec<&EvidenceRef> = refs
        .iter()
        .filter(|r| present.get(&r.name) != Some(&r.target))
        .collect();
    if missing.is_empty() {
        return Transfer::Nothing;
    }
    let mut args = vec![
        "push".to_owned(),
        "--no-verify".to_owned(),
        "--porcelain".to_owned(),
        remote.to_owned(),
    ];
    args.extend(missing.iter().map(|r| format!("{}:{}", r.target, r.name)));
    match run_git(root, &args, deadline, true) {
        // `!` heißt abgelehnt, auch wenn git mit 0 endete.
        Ok((_, out)) if out.lines().any(|l| l.starts_with('!')) => Transfer::Failed("rejected"),
        Ok((true, _)) => Transfer::Sent(missing.len()),
        Ok((false, _)) => Transfer::Failed("unreachable"),
        Err(true) => Transfer::Failed("timeout"),
        Err(false) => Transfer::Failed("unreachable"),
    }
}

/// `minds sync --evidence [<rev-range>] [--remote <r>]`: Schritt 4 und 5 als
/// eigener Befehl, zum Wiederholen von Hand.
pub(crate) fn run(range: Option<&str>, remote: &str) -> ExitCode {
    let result = (|| -> Result<Transfer, String> {
        let here = std::env::current_dir().map_err(|e| e.to_string())?;
        let root = crate::hook::witness::repo_root(&here).ok_or("not inside a Git worktree")?;
        let git_dir = crate::bind_cmd::git_dir(&root).ok_or("no git directory")?;
        let repo = crate::bind_cmd::open_view(&git_dir)?;
        let lines = crate::bind_cmd::lines_for(&root, range, Some(remote))?;
        let git = crate::bind_cmd::which_git().ok_or("git not found on PATH")?;
        let commits = crate::bind_cmd::commits_for_push(&repo, &git, &lines)?;
        Ok(transfer(&root, remote, &evidence_refs(&repo, &commits)))
    })();
    match result {
        Ok(Transfer::Failed(reason)) => {
            println!("{}", Transfer::Failed(reason).line().unwrap_or_default());
            ExitCode::FAILURE
        }
        Ok(done) => {
            println!(
                "{}",
                done.line()
                    .unwrap_or_else(|| "minds: no evidence to transfer".to_owned())
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("minds sync --evidence: {}", crate::text::sanitize(&err));
            ExitCode::FAILURE
        }
    }
}
