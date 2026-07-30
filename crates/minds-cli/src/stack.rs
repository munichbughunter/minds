//! `minds stack` — die abhängigen Changes und ihr jeweiliger Review-Stand
//! (Schicht 3, R3).
//!
//! # Die Gerrit-Lehre
//!
//! Ein Branch ist die falsche Einheit für einen Review. Wer fünf aufeinander
//! aufbauende Änderungen schickt, bekommt in einem branch-zentrierten Werkzeug
//! *einen* Review über alle fünf — und nach jedem Force-Push fängt er von vorn
//! an, weil sich sämtliche Commit-Hashes geändert haben.
//!
//! Minds hängt das Verdict an die **Change-Id** (ADR-0006). Damit ist jeder
//! Change im Stapel einzeln reviewbar, und ein Rebase oder Force-Push lässt die
//! Verdicts stehen: Der Hash wechselt, die Identität nicht. `minds stack` macht
//! genau das sichtbar — welche Änderungen übereinanderliegen, und wie es um jede
//! einzeln steht.
//!
//! # Woher die Basis kommt
//!
//! `--base` schlägt alles. Sonst der Upstream des aktuellen Branches
//! (`@{upstream}`), sonst der erste vorhandene aus `main`/`master`. Gefragt ist
//! immer der **Merge-Base**, nicht die Spitze: Was auf `main` liegt und hier
//! auch, ist nicht Teil dieses Stapels.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use minds_core::{Decision, Review, Trailer};
use minds_git::Repo;
use minds_store::ReviewStore;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Kandidaten für die Basis, wenn weder `--base` noch ein Upstream da ist.
const FALLBACK_BASES: [&str; 2] = ["main", "master"];

/// Führt `minds stack` aus.
pub fn run(base: Option<&str>) -> ExitCode {
    match stack(base) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds stack: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Ein Change im Stapel.
struct Entry {
    commit: String,
    subject: String,
    change: Option<String>,
    verdict: Option<Decision>,
    reviewer: Option<String>,
    comments: usize,
}

fn stack(base: Option<&str>) -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);

    let base = resolve_base(&root, base)?;
    let commits = commits_since(&root, &base)?;
    if commits.is_empty() {
        println!("Stapel auf {base}: leer — HEAD ist nicht voraus.");
        return Ok(());
    }

    let store = ReviewStore::new(Repo::open(&root)?);
    // Einmal alle Verdicts holen und nach Subjekt bündeln, statt je Change
    // erneut den ganzen Log zu lesen.
    let mut by_subject: BTreeMap<String, Vec<Review>> = BTreeMap::new();
    for review in store.list()? {
        by_subject
            .entry(review.subject.id().to_string())
            .or_default()
            .push(review);
    }

    let mut entries = Vec::new();
    for commit in &commits {
        let change = change_id_of(&root, commit);
        let (verdict, reviewer) = match change.as_deref().and_then(|id| by_subject.get(id)) {
            Some(reviews) => {
                let latest = newest(reviews);
                (Some(latest.decision), Some(latest.reviewer.clone()))
            }
            None => (None, None),
        };
        let comments = match change.as_deref() {
            Some(id) => store.thread(id)?.len(),
            None => 0,
        };
        entries.push(Entry {
            commit: commit.clone(),
            subject: subject_of(&root, commit),
            change,
            verdict,
            reviewer,
            comments,
        });
    }

    println!("Stapel auf {base} — {} Change(s):\n", entries.len());
    for (position, entry) in entries.iter().enumerate() {
        let change = entry
            .change
            .as_deref()
            .map(short)
            .unwrap_or_else(|| "ohne Change-Id".to_string());
        println!(
            "{:>2}. {}  {}",
            position + 1,
            change,
            truncate(&entry.subject, 56)
        );
        println!(
            "    {}  {}{}",
            &entry.commit[..entry.commit.len().min(8)],
            state(entry),
            match entry.comments {
                0 => String::new(),
                1 => " · 1 Kommentar".to_string(),
                n => format!(" · {n} Kommentare"),
            }
        );
    }

    let offen = entries
        .iter()
        .filter(|entry| entry.verdict != Some(Decision::Approve))
        .count();
    println!(
        "\n{} von {} approbiert.",
        entries.len() - offen,
        entries.len()
    );
    Ok(())
}

/// Der Review-Stand eines Changes, in einer Zeile.
fn state(entry: &Entry) -> String {
    match (&entry.verdict, &entry.reviewer) {
        (Some(decision), Some(reviewer)) => {
            let mark = match decision {
                Decision::Approve => "✓",
                Decision::Reject => "✗",
                Decision::NeedsWork => "!",
            };
            format!("{mark} {} · {reviewer}", decision.as_str())
        }
        _ if entry.change.is_none() => "– kein Trailer (nicht reviewbar)".to_string(),
        _ => "– kein Verdict".to_string(),
    }
}

/// Das jüngste Verdict einer Liste.
///
/// Ohne Zeitstempel entscheidet der Hash — nicht, weil er etwas über die Zeit
/// aussagt, sondern damit die Auswahl **total und auf jeder Maschine gleich**
/// ist. Eine Anzeige, die von der Lesereihenfolge abhinge, wäre schlimmer als
/// eine, die willkürlich, aber stabil wählt.
fn newest(reviews: &[Review]) -> &Review {
    reviews
        .iter()
        .max_by_key(|review| {
            (
                review.at.clone().unwrap_or_default(),
                review
                    .content_hash()
                    .map(|hash| hash.to_string())
                    .unwrap_or_default(),
            )
        })
        .expect("die Liste entsteht nur mit mindestens einem Eintrag")
}

/// Die Basis des Stapels: `--base`, sonst Upstream, sonst `main`/`master`.
fn resolve_base(root: &Path, base: Option<&str>) -> Fallible<String> {
    if let Some(base) = base {
        return Ok(base.to_string());
    }
    if let Some(upstream) = git(root, &["rev-parse", "--abbrev-ref", "@{upstream}"]) {
        return Ok(upstream);
    }
    for candidate in FALLBACK_BASES {
        if git(root, &["rev-parse", "--verify", "--quiet", candidate]).is_some() {
            return Ok(candidate.to_string());
        }
    }
    Err("keine Basis gefunden — mit --base <ref> angeben".into())
}

/// Die Commits von der Basis bis HEAD, ältester zuerst.
///
/// Über `<base>..HEAD` mit Merge-Base-Semantik: Was auf der Basis liegt, gehört
/// nicht zum Stapel, auch wenn die Basis inzwischen weitergelaufen ist.
fn commits_since(root: &Path, base: &str) -> Fallible<Vec<String>> {
    let range = format!("{base}..HEAD");
    let out = git(root, &["rev-list", "--reverse", &range])
        .ok_or_else(|| format!("{range} lässt sich nicht auflösen"))?;
    Ok(out.lines().map(str::to_owned).collect())
}

/// Die `Minds-Change-Id` aus der Message eines Commits.
fn change_id_of(root: &Path, commit: &str) -> Option<String> {
    let message = git(root, &["show", "-s", "--format=%B", commit])?;
    Trailer::change_id(&message).map(|id| id.to_string())
}

/// Die Betreffzeile eines Commits.
fn subject_of(root: &Path, commit: &str) -> String {
    git(root, &["show", "-s", "--format=%s", commit]).unwrap_or_default()
}

/// Die Kurzform einer Change-Id — genug zum Wiedererkennen, kurz genug für eine
/// Zeile.
fn short(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…", &id[..12])
    } else {
        id.to_string()
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Ruft `git` auf und gibt die getrimmte Ausgabe zurück — `None` bei
/// Nicht-Null-Exit oder leerer Ausgabe.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn repo_root(repo: &Repo) -> PathBuf {
    repo.git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_id_is_shortened_but_recognisable() {
        let id = format!("I{}", "ab".repeat(20));
        assert_eq!(short(&id), "Iabababababa…");
        assert_eq!(short("Ikurz"), "Ikurz");
    }

    #[test]
    fn a_long_subject_is_cut_at_a_char_boundary() {
        // Nicht an Bytes schneiden: Ein Umlaut in der Betreffzeile darf nicht
        // zu einem Panic führen.
        let subject = "füge größere Änderungen hinzu ".repeat(5);
        let cut = truncate(&subject, 20);
        assert_eq!(cut.chars().count(), 20);
        assert!(cut.ends_with('…'));
        assert_eq!(truncate("kurz", 20), "kurz");
    }

    #[test]
    fn the_state_line_tells_the_three_cases_apart() {
        let entry = |change: Option<&str>, verdict: Option<Decision>| Entry {
            commit: "abcdef1234".into(),
            subject: "feat: x".into(),
            change: change.map(str::to_owned),
            verdict,
            reviewer: verdict.map(|_| "anna@example.org".to_string()),
            comments: 0,
        };

        assert!(state(&entry(Some("I1"), Some(Decision::Approve))).contains("approve"));
        assert!(state(&entry(Some("I1"), Some(Decision::NeedsWork))).contains("needs-work"));
        assert!(state(&entry(Some("I1"), None)).contains("kein Verdict"));
        // Ohne Trailer ist der Change gar nicht adressierbar — das ist ein
        // anderer Zustand als „noch niemand hat geschaut".
        assert!(state(&entry(None, None)).contains("kein Trailer"));
    }
}
