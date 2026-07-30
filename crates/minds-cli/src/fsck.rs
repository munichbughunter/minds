//! `minds fsck` — hält der Record, was der Trailer verspricht?
//!
//! Der heiße Pfad ist fail-open: `minds hook` darf ein Event verlieren, statt
//! die Sitzung zu stören. Der Preis dafür sind mögliche Lücken — und Lücken, die
//! niemand sieht, sind schlimmer als keine. `fsck` macht sie sichtbar. Es prüft
//! zwei Zusagen:
//!
//! 1. **Jeder Trailer ist auflösbar.** Zu jeder `Minds-Session-Id` in der
//!    Historie muss die Session im Store liegen. Ein Trailer, der ins Leere
//!    zeigt, ist eine Waise — der eine Integritätsbruch, den `fsck` mit einem
//!    Rückgabewert ≠ 0 quittiert.
//! 2. **Das Journal ist heil.** Angesammelte, noch nicht eingecheckte Sessions
//!    werden gemeldet; Sequenzlücken (ein fail-open verlorenes Event) und
//!    beschädigte Dateien (ein abgestürzter Schreibvorgang) ebenso. Das sind
//!    Warnungen, kein Bruch: Sie erzählen, was der heiße Pfad gekostet hat.
//!
//! Ehrlich lückenhaft schlägt still vollständig — diese Datei ist die Einlösung
//! dieses Satzes aus dem ganzen Entwurf.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, ExitCode};

use minds_capture::Journal;
use minds_core::{ChangeId, Decision, SessionId, Trailer};
use minds_git::{CommitId, Repo};
use minds_store::ReviewStore;

use crate::config;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds fsck` aus. Rückgabewert ≠ 0 genau dann, wenn ein Trailer nicht
/// auflösbar ist — oder, mit `require_review`, ein agent-authored Change kein
/// Approve trägt (Policy-Gate, R5).
pub fn run(require_review: bool) -> ExitCode {
    match fsck(require_review) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("minds fsck: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Gibt `true` zurück, wenn kein Trailer verwaist ist (und, falls verlangt, jeder
/// agent-authored Change ein Approve trägt).
fn fsck(require_review: bool) -> Fallible<bool> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);
    let store = config::load(&root).open(&root)?;

    let orphans = check_trailers(&repo, store.as_ref())?;
    let index_orphans = check_index(store.as_ref())?;
    check_journal(&repo);

    let mut total = orphans + index_orphans;
    if require_review {
        let reviews =
            ReviewStore::new(Repo::open(&root).map_err(minds_store::StoreError::backend)?);
        total += check_reviews(&repo, &root, &reviews)?;
    }

    if total == 0 {
        println!("fsck: in Ordnung");
        Ok(true)
    } else {
        println!("fsck: {total} Befund(e)");
        Ok(false)
    }
}

/// Das Policy-Gate (R5): Jeder erreichbare, agent-authored Commit (trägt ≥1
/// `Minds-Session-Id`) muss ein **Approve** tragen — an seiner Change-Id oder
/// einer seiner Session-Ids. Gibt die Zahl der ungereviewten Commits zurück.
fn check_reviews(repo: &Repo, root: &Path, reviews: &ReviewStore) -> Fallible<usize> {
    let Some(head) = repo.head()?.commit() else {
        println!("Reviews: HEAD hat noch keinen Commit — nichts zu prüfen");
        return Ok(0);
    };

    // Alle approbierten Subjekte einmal einsammeln.
    let approved: BTreeSet<String> = reviews
        .list()?
        .into_iter()
        .filter(|review| review.decision == Decision::Approve)
        .map(|review| review.subject.id().to_string())
        .collect();

    let mut unreviewed = 0usize;
    let mut checked = 0usize;
    for commit in repo.revwalk(head)? {
        let commit = commit?;
        let sessions = repo.session_ids_of(commit)?;
        if sessions.is_empty() {
            continue; // nicht agent-authored — kein Review verlangt
        }
        checked += 1;

        let mut subjects: Vec<String> = sessions.iter().map(SessionId::to_string).collect();
        if let Some(change) = commit_change_id(root, commit) {
            subjects.push(change.to_string());
        }
        if !subjects.iter().any(|subject| approved.contains(subject)) {
            println!(
                "  ungereviewt: {commit} ({} Session(s), kein Approve)",
                sessions.len()
            );
            unreviewed += 1;
        }
    }

    println!("Reviews: {checked} agent-authored Commit(s), {unreviewed} ohne Approve");
    Ok(unreviewed)
}

/// Die `Minds-Change-Id` aus der Commit-Message.
fn commit_change_id(root: &Path, commit: CommitId) -> Option<ChangeId> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", "-s", "--format=%B", &commit.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Trailer::change_id(&String::from_utf8_lossy(&output.stdout))
}

/// Prüft die Kanten des Store-Index: jede benannte Session muss im Store liegen.
/// Gibt die Zahl der Waisen zurück.
///
/// Die Index-Kanten sind heuristisch ([`Evidence::Inferred`](minds_core::Evidence::Inferred)),
/// aber die Session, auf die sie zeigen, muss trotzdem da sein — sonst ist der
/// Verweis so tot wie ein verwaister Trailer.
fn check_index(store: &dyn minds_store::ContextStore) -> Fallible<usize> {
    let index = store.index()?;
    if index.is_empty() {
        println!("Index: leer");
        return Ok(0);
    }

    let mut seen: BTreeSet<SessionId> = BTreeSet::new();
    let mut orphans = 0usize;
    let mut links = 0usize;

    for (commit, entries) in index.iter() {
        for entry in entries {
            links += 1;
            if !seen.insert(entry.session) {
                continue;
            }
            if !store.exists(entry.session)? {
                println!("  Waise: {commit} → {} (nicht im Store)", entry.session);
                orphans += 1;
            }
        }
    }

    println!(
        "Index: {links} vermutete Verknüpfung(en), {} eindeutig, {orphans} verwaist",
        seen.len()
    );
    Ok(orphans)
}

/// Läuft die Historie ab HEAD ab und prüft jede eindeutige `Minds-Session-Id`
/// gegen den Store. Gibt die Zahl der Waisen zurück.
fn check_trailers(repo: &Repo, store: &dyn minds_store::ContextStore) -> Fallible<usize> {
    let Some(head) = repo.head()?.commit() else {
        println!("Trailer: HEAD hat noch keinen Commit — nichts zu prüfen");
        return Ok(0);
    };

    // Eine Session-Id kann an mehreren Commits stehen (Rebase kopiert den
    // Trailer). Jede nur einmal prüfen — der Store-Zugriff ist der teure Teil.
    let mut seen: BTreeSet<SessionId> = BTreeSet::new();
    let mut orphans = 0usize;
    let mut total = 0usize;

    for commit in repo.revwalk(head)? {
        let commit = commit?;
        for id in repo.session_ids_of(commit)? {
            total += 1;
            if !seen.insert(id) {
                continue;
            }
            if !store.exists(id)? {
                println!("  Waise: {commit} → {id} (nicht im Store)");
                orphans += 1;
            }
        }
    }

    println!(
        "Trailer: {total} Verweis(e), {} eindeutig, {orphans} verwaist",
        seen.len()
    );
    Ok(orphans)
}

/// Meldet den Zustand des Journals: was noch aussteht, was fehlt, was beschädigt
/// ist. Nur Warnungen — ein volles Journal ist der Normalfall zwischen zwei
/// Commits.
fn check_journal(repo: &Repo) {
    let journal = Journal::open(repo.git_dir());
    let Ok(sessions) = journal.sessions() else {
        return;
    };

    if sessions.is_empty() {
        println!("Journal: leer");
        return;
    }

    println!(
        "Journal: {} Session(s) noch nicht eingecheckt",
        sessions.len()
    );
    for key in sessions {
        let Ok(outcome) = journal.read(&key) else {
            continue;
        };
        let mut notes = Vec::new();
        if !outcome.gaps.is_empty() {
            notes.push(format!("{} Lücke(n)", outcome.gaps.len()));
        }
        if !outcome.damaged.is_empty() {
            notes.push(format!("{} beschädigt", outcome.damaged.len()));
        }
        let suffix = if notes.is_empty() {
            String::new()
        } else {
            format!(" — {}", notes.join(", "))
        };
        println!(
            "  {}/{}: {} Event(s){suffix}",
            key.agent(),
            key.local_id(),
            outcome.events.len()
        );
    }
}

fn repo_root(repo: &Repo) -> std::path::PathBuf {
    repo.git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}
