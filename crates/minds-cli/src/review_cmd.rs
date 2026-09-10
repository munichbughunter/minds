//! `minds review <subject> --approve|--reject|--needs-work [--summary]` und
//! `minds reviews <subject>` — Reviews als Git-Objekte (Schicht 3).
//!
//! Das Verdict zu einer Änderung landet content-adressiert und signierbar unter
//! `refs/minds/reviews/` — im Repo, nicht in einer Plattform-Datenbank. Es hängt
//! an der **Change-Id** (oder ersatzweise an einer Session-Id), damit es den
//! Rebase überlebt.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use minds_core::{Anchor, ChangeId, Comment, Decision, Review, SessionId, Subject, review_payload};
use minds_git::Repo;
use minds_store::ReviewStore;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds review` aus — legt ein Verdict an.
pub fn run_review(
    subject: Option<&str>,
    decision: Option<Decision>,
    summary: Option<&str>,
    sign: bool,
    key: Option<&str>,
) -> ExitCode {
    let Some(subject) = subject else {
        eprintln!("minds review: expected <subject> (Change-Id I… or Session-Id b3…)");
        return ExitCode::FAILURE;
    };
    let Some(decision) = decision else {
        eprintln!("minds review: specify a decision: --approve | --reject | --needs-work");
        return ExitCode::FAILURE;
    };
    match review(subject, decision, summary.unwrap_or(""), sign, key) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds review: {err}");
            ExitCode::FAILURE
        }
    }
}

fn review(
    subject: &str,
    decision: Decision,
    summary: &str,
    sign: bool,
    key: Option<&str>,
) -> Fallible<()> {
    let subject = parse_subject(subject)?;
    let (repo, root) = open()?;
    let reviewer =
        git_config(&root, "user.email").ok_or("no identity: set `git config user.email`")?;

    // Der Zeitstempel kommt von hier, nicht aus dem Modell — `minds-core` ruft
    // nie `now()`. Er macht „das jüngste Verdict" zu einer beantwortbaren Frage
    // (siehe `minds stack`).
    let (at, _) = minds_capture::clock::now();
    let review = Review::new(subject, decision, reviewer, summary, Some(at));
    let store = ReviewStore::new(repo);
    let hash = store.put(&review)?;

    println!("Review {hash}");
    println!(
        "  {} · {} · {}",
        review.decision.as_str(),
        review.reviewer,
        review.subject.id()
    );

    if sign {
        // Erst ablegen, dann signieren: Die Signatur geht über den Hash, und den
        // gibt es erst, wenn das Review steht. Scheitert das Signieren, bleibt
        // ein gültiges, unsigniertes Verdict zurück — kein halber Zustand.
        let key = resolve_key(key, &root)?;
        let signature = minds_attest::ssh_sign(&review_payload(&hash, &review)?, Path::new(&key))?;
        store.put_signature(&hash, &signature)?;
        println!("  signed with {key}");
    }
    Ok(())
}

/// Der Signaturschlüssel: `--key`, sonst `git config user.signingkey`.
fn resolve_key(key: Option<&str>, root: &Path) -> Fallible<String> {
    if let Some(key) = key {
        return Ok(key.to_string());
    }
    git_config(root, "user.signingkey")
        .ok_or_else(|| "no key: --key <path> or `git config user.signingkey`".into())
}

/// Führt `minds reviews` aus — listet die Verdicts zu einem Subjekt.
pub fn run_reviews(
    subject: Option<&str>,
    signers: Option<&str>,
    identity: Option<&str>,
) -> ExitCode {
    let Some(subject) = subject else {
        eprintln!("minds reviews: expected <subject>");
        return ExitCode::FAILURE;
    };
    match reviews(subject, signers, identity) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds reviews: {err}");
            ExitCode::FAILURE
        }
    }
}

fn reviews(subject: &str, signers: Option<&str>, identity: Option<&str>) -> Fallible<()> {
    let subject = parse_subject(subject)?;
    let (repo, _root) = open()?;
    let store = ReviewStore::new(repo);
    let found = store.for_subject(subject.id())?;

    if found.is_empty() {
        println!("no verdicts for {}", subject.id());
    } else {
        println!("{} review(s) for {}:\n", found.len(), subject.id());
    }
    for review in &found {
        println!("▸ {} · {}", review.decision.as_str(), review.reviewer);
        if !review.summary.is_empty() {
            println!("  {}", review.summary);
        }
        println!("  {}", signature_state(&store, review, signers, identity));
    }

    let thread = store.thread(subject.id())?;
    if !thread.is_empty() {
        println!("\n{} comment(s):\n", thread.len());
        for comment in &thread {
            println!("▸ {} · {}", comment.anchor.as_text(), comment.author);
            for line in comment.body.lines() {
                println!("  {line}");
            }
        }
    }
    Ok(())
}

/// Was über die Signatur eines Verdicts zu sagen ist.
///
/// Ohne `--signers` wird **nicht** geprüft, sondern nur gemeldet, dass eine
/// Signatur da ist. Das ist Absicht: „signiert" ohne bekannte Schlüssel ist eine
/// Behauptung, keine Prüfung, und die beiden dürfen nicht gleich aussehen.
fn signature_state(
    store: &ReviewStore,
    review: &Review,
    signers: Option<&str>,
    identity: Option<&str>,
) -> String {
    let Ok(hash) = review.content_hash() else {
        return "· hash not computable".into();
    };
    let signature = match store.signature(&hash) {
        Ok(Some(signature)) => signature,
        Ok(None) => return "· not signed".into(),
        Err(err) => return format!("· signature unreadable: {err}"),
    };

    let Some(signers) = signers else {
        return "· signed (unverified — check with --signers <file>)".into();
    };
    let identity = identity.unwrap_or(&review.reviewer);
    // Fail-closed (#12): Ein Review, dessen Felder eine Zeile fälschen könnten,
    // bekommt keinen Payload — und damit hier kein „gültig".
    let payload = match review_payload(&hash, review) {
        Ok(payload) => payload,
        Err(err) => return format!("· signature not verifiable: {err}"),
    };
    match minds_attest::ssh_verify(&payload, &signature, Path::new(signers), identity) {
        Ok(true) => format!("· signature valid ({identity})"),
        Ok(false) => format!("· SIGNATURE INVALID ({identity})"),
        Err(err) => format!("· signature not verifiable: {err}"),
    }
}

/// Führt `minds comment` aus — hängt eine Anmerkung an den Thread.
pub fn run_comment(subject: Option<&str>, on: Option<&str>, body: Option<&str>) -> ExitCode {
    let Some(subject) = subject else {
        eprintln!("minds comment: expected <subject> (Change-Id I… or Session-Id b3…)");
        return ExitCode::FAILURE;
    };
    let Some(body) = body else {
        eprintln!("minds comment: expected a comment body");
        return ExitCode::FAILURE;
    };
    match comment(subject, on, body) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds comment: {err}");
            ExitCode::FAILURE
        }
    }
}

fn comment(subject: &str, on: Option<&str>, body: &str) -> Fallible<()> {
    let subject = parse_subject(subject)?;
    let anchor = parse_anchor(on)?;
    let (repo, root) = open()?;
    let author =
        git_config(&root, "user.email").ok_or("no identity: set `git config user.email`")?;

    // Der Zeitstempel kommt von hier, nicht aus dem Modell — `minds-core` ruft
    // nie `now()`, damit dieselbe Eingabe immer denselben Hash ergibt.
    let (at, _) = minds_capture::clock::now();
    let comment = Comment::new(subject, anchor, author, body, Some(at));
    let hash = ReviewStore::new(repo).put_comment(&comment)?;

    println!("Comment {hash}");
    println!(
        "  {} · {} · {}",
        comment.anchor.as_text(),
        comment.author,
        comment.subject.id()
    );
    Ok(())
}

/// Deutet `--on` als `<datei>:<zeile>` oder `turn:<n>`; ohne Angabe gilt der
/// Kommentar dem Change als Ganzem.
fn parse_anchor(on: Option<&str>) -> Fallible<Anchor> {
    let Some(on) = on else {
        return Ok(Anchor::Whole);
    };
    if let Some(index) = on.strip_prefix("turn:") {
        let index = index
            .parse()
            .map_err(|_| format!("not a turn number: {index:?}"))?;
        return Ok(Anchor::Turn { index });
    }
    // Von rechts trennen: Ein Windows-Pfad trägt selbst einen Doppelpunkt.
    let (path, line) = on
        .rsplit_once(':')
        .ok_or_else(|| format!("expected <file>:<line> or turn:<n>, got {on:?}"))?;
    let line = line
        .parse()
        .map_err(|_| format!("not a line number: {line:?}"))?;
    if path.is_empty() {
        return Err(format!("no file path in {on:?}").into());
    }
    Ok(Anchor::File {
        path: path.to_owned(),
        line,
    })
}

/// Deutet `<subject>` als Session-Id (`b3-…`) oder Change-Id (`I…`).
fn parse_subject(subject: &str) -> Fallible<Subject> {
    if let Ok(id) = subject.parse::<SessionId>() {
        return Ok(Subject::Session(id.to_string()));
    }
    let id: ChangeId = subject
        .parse()
        .map_err(|err| format!("neither a Change-Id nor a Session-Id: {subject:?} ({err})"))?;
    Ok(Subject::Change(id.to_string()))
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
        .args(["config", key])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #12: Ein gespeichertes Review mit gefälschtem Reviewer-Feld darf in der
    /// Statuszeile nie als „gültig" erscheinen — der Payload-Bau schlägt fehl,
    /// bevor eine Signatur überhaupt geprüft wird, und genau das steht dann da.
    #[test]
    fn a_forged_review_is_unverifiable_not_valid() {
        let dir = tempfile::tempdir().unwrap();
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("git not on PATH — test skipped");
            return;
        }
        let store = ReviewStore::new(Repo::discover(dir.path()).unwrap());
        let forged = Review::new(
            Subject::Change(format!("I{}", "ab".repeat(20))),
            Decision::Approve,
            "anna@example.org\ndecision=approve",
            "",
            None,
        );
        let hash = forged.content_hash().unwrap();
        store.put(&forged).unwrap();
        store.put_signature(&hash, "keine-echte-signatur").unwrap();

        let state = signature_state(&store, &forged, Some("irrelevant"), None);
        assert!(state.contains("signature not verifiable"), "{state}");
        assert!(!state.contains("signature valid"), "{state}");
        assert!(!state.contains("INVALID"), "{state}");
    }
}
