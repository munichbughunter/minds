//! `minds gitlab mirror` und `minds gitlab webhook` — die Plattform als Cache
//! (Schicht 3, R4).
//!
//! Die Richtung, die zählt, ist **hinaus**: Was im Repo steht, wird in GitLab
//! sichtbar. Die Gegenrichtung ist opt-in und tut nichts von selbst — siehe
//! [`minds_gitlab::webhook`].
//!
//! # Konfiguration
//!
//! Projekt und Instanz stehen in `.git/config` (`minds.gitlabUrl`,
//! `minds.gitlabProject`), damit die CI-Zeile kurz bleibt; `--url`/`--project`
//! schlagen sie. Der Token kommt **nur** aus einer Umgebungsvariablen
//! (`MINDS_GITLAB_TOKEN`, oder was `--token-env` nennt) — nie aus einem
//! Argument, das in `ps` steht.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use minds_core::{Decision, Trailer};
use minds_git::Repo;
use minds_gitlab::{Project, webhook};
use minds_store::ReviewStore;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Die Umgebungsvariable, aus der der Token kommt, wenn keine genannt ist.
const DEFAULT_TOKEN_ENV: &str = "MINDS_GITLAB_TOKEN";

/// Was `minds gitlab` tun soll.
pub struct Options<'a> {
    pub subject: Option<&'a str>,
    pub merge_request: Option<&'a str>,
    pub url: Option<&'a str>,
    pub project: Option<&'a str>,
    pub token_env: Option<&'a str>,
    pub approve: bool,
    pub write: bool,
}

/// Führt `minds gitlab <unterkommando>` aus.
pub fn run(command: Option<&str>, options: Options<'_>) -> ExitCode {
    let result = match command {
        Some("mirror") => mirror(&options),
        Some("webhook") => incoming(&options),
        Some(other) => Err(format!("unbekannt: gitlab {other} (mirror | webhook)").into()),
        None => Err("erwartet: minds gitlab mirror|webhook".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds gitlab: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Spiegelt die Verdicts eines Subjekts an einen Merge Request.
fn mirror(options: &Options<'_>) -> Fallible<()> {
    let subject = options.subject.ok_or("erwartet <subject> (Change-Id I…)")?;
    let mr: u64 = options
        .merge_request
        .ok_or("erwartet --mr <nummer>")?
        .parse()
        .map_err(|_| "--mr erwartet eine Zahl")?;

    let (repo, root) = open()?;
    let project = project(&root, options)?;
    let store = ReviewStore::new(repo);

    let reviews = store.for_subject(subject)?;
    if reviews.is_empty() {
        println!("keine Verdicts für {subject} — nichts zu spiegeln");
        return Ok(());
    }

    let mut mirrored = 0usize;
    for review in &reviews {
        let hash = review.content_hash()?;
        if project.mirror(mr, &hash, review)? {
            println!("  gespiegelt: {} · {hash}", review.decision.as_str());
            mirrored += 1;
        } else {
            println!("  steht schon: {} · {hash}", review.decision.as_str());
        }
        if options.approve && review.decision == Decision::Approve {
            project.approve(mr)?;
            println!("  Approval gesetzt");
        }
    }
    println!(
        "{mirrored} von {} Verdict(s) neu an MR !{mr} gespiegelt.",
        reviews.len()
    );
    Ok(())
}

/// Liest eine Webhook-Nutzlast von stdin und macht daraus ein Verdict.
///
/// Ohne `--write` wird nur gezeigt, was entstünde. Das ist der Default, weil ein
/// Kommando, das aus einer Netz-Nutzlast ungefragt einen Audit-Record schreibt,
/// die falsche Voreinstellung hätte.
fn incoming(options: &Options<'_>) -> Fallible<()> {
    let mut payload = Vec::new();
    std::io::stdin().read_to_end(&mut payload)?;

    let Some(incoming) = webhook::parse(&payload) else {
        // Der Normalfall: irgendein anderes Ereignis. Kein Fehler.
        println!("kein Verdict in dieser Nutzlast");
        return Ok(());
    };

    let (repo, root) = open()?;
    // Die Change-Id aus dem Kommentar gewinnt; sonst der Commit des MR, lokal
    // aufgelöst. Das ist der Grund, warum dieses Kommando in einem Checkout
    // läuft und nicht als Dienst irgendwo.
    let resolved = incoming
        .commit
        .as_deref()
        .and_then(|commit| change_id_of(&root, commit));

    let (at, _) = minds_capture::clock::now();
    let Some(review) = incoming.into_review(resolved.as_deref(), Some(at)) else {
        return Err("kein Subjekt: weder eine Change-Id im Kommentar noch am Commit des MR".into());
    };

    let hash = review.content_hash()?;
    println!(
        "{} · {} · {}",
        review.decision.as_str(),
        review.reviewer,
        review.subject.id()
    );
    if !review.summary.is_empty() {
        println!("  {}", review.summary);
    }

    if !options.write {
        println!("\n(nichts geschrieben — mit --write anlegen; Hash wäre {hash})");
        return Ok(());
    }
    // Ingest-Validierung (#12): Eine Netz-Nutzlast, deren Felder keinen
    // signierbaren Payload ergäben (Zeilen-/Steuer-/Versteckzeichen in
    // Reviewer oder Subjekt), kommt gar nicht erst in den Store — sonst
    // stünde dort ein Verdict, das audit nur degradiert ausweisen und
    // niemand je signieren könnte.
    minds_core::review_payload(&hash, &review)?;
    let written = ReviewStore::new(repo).put(&review)?;
    println!("\nReview {written} angelegt.");
    Ok(())
}

/// Der Projektzugang aus Flags und `.git/config`.
fn project(root: &Path, options: &Options<'_>) -> Fallible<Project> {
    let url = options
        .url
        .map(str::to_owned)
        .or_else(|| git_config(root, "minds.gitlabUrl"))
        .ok_or("keine Instanz: --url <basis> oder `git config minds.gitlabUrl`")?;
    let project = options
        .project
        .map(str::to_owned)
        .or_else(|| git_config(root, "minds.gitlabProject"))
        .ok_or("kein Projekt: --project <id|pfad> oder `git config minds.gitlabProject`")?;
    let token_env = options.token_env.unwrap_or(DEFAULT_TOKEN_ENV);
    Ok(Project::new(&url, &project, token_env)?)
}

/// Die `Minds-Change-Id` eines Commits — die Brücke von dem, was GitLab kennt
/// (ein Hash), zu dem, woran ein Verdict hängt.
fn change_id_of(root: &Path, commit: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", "-s", "--format=%B", commit])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Trailer::change_id(&String::from_utf8_lossy(&output.stdout)).map(|id| id.to_string())
}

fn open() -> Fallible<(Repo, PathBuf)> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo
        .git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf();
    Ok((repo, root))
}

fn git_config(root: &Path, key: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get", key])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !value.is_empty()).then_some(value)
}
