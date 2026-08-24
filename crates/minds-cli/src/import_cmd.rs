//! Der Backfill: bestehende Transkripte in den Store, verlinkt über den Index.
//!
//! Ausgelöst wird das **automatisch** von `minds enable` — als losgelöster
//! Hintergrundprozess, der sich selbst mit dem versteckten Flag aufruft. Von
//! Hand aufrufbar ist es trotzdem (`minds enable --__background-import`) — für
//! einen erneuten Lauf oder zum Nachsehen.
//!
//! # Wohin die Ausgabe geht
//!
//! Der Hintergrundprozess hat kein Terminal, und `enable` wartet nicht auf ihn.
//! Damit ist er ein Hook-Pfad wie `checkpoint` oder `sync`, und er folgt deren
//! Regeln: **Fehler** — ein Store, der sich nicht öffnen lässt, ein Transkript
//! ohne Leserechte, eine Session, die die Redaction verweigert — gehen über
//! [`crate::hooklog`] in `hook.log` (entschärft, gedeckelt, rotiert, `0600`)
//! und zugleich auf stderr, für den, der von Hand aufruft. Der **Gutfall**
//! schreibt nur auf stdout, also ins Terminal des Hand-Aufrufers und sonst
//! nirgendwohin: Stünde „3 Session(s) gespeichert" oder „codex: kein Importer"
//! im Log, zeigte `minds fsck` nach jedem `enable` einen Hinweis, und ein
//! Hinweis, den man nicht loswird, wird überlesen — mitsamt den echten.
//!
//! Bis #69 landete beides roh in einer eigenen Datei daneben (`import.log`),
//! ohne eine der Zusagen von `hook.log` — und `fsck` wusste nichts von ihr.
//!
//! ```text
//!   Transkripte ──import──► Sessions ──redact──► Store
//!                                │
//!                     match(Dateien+Zeit) mit git log ──► Index (inferred)
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use minds_capture::{CommitInfo, SessionInfo, match_sessions};
use minds_core::{EvidenceMark, EvidenceSource};
use minds_git::Repo;
use minds_redact::RedactionConfig;

use crate::config;
use crate::hooklog::{self, Source};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt den Backfill aus.
///
/// In der Panic-Klammer wie jeder Hook-Pfad: Der Prozess hat kein stderr, das
/// jemand liest — ein Panic verschwände sonst spurlos. Und weil hier die
/// **rohen** Transkripte im Speicher liegen, hält das Log vom Panic nur den
/// Ort fest, nicht die Meldung.
pub fn run() -> ExitCode {
    hooklog::guarded(Source::Import, || match import() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            hooklog::report(Source::Import, &err.to_string());
            ExitCode::FAILURE
        }
    })
}

fn import() -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);
    let git_dir = repo.git_dir().to_path_buf();
    let store = config::load(&root).open(&root)?;
    let home = home_dir()?;

    // 1. Transkripte aller bekannten Agents lesen.
    let reports = minds_capture::for_repo(&root, &home);

    // 2. Jede Session redigieren und ablegen; dabei den Fingerabdruck fürs
    //    Matching aus der *redigierten* Session ziehen (ihre Id und ihre
    //    repo-relativen Pfade sind das, was im Store liegt).
    let pipeline = RedactionConfig::default().pipeline()?;
    let mut infos: Vec<SessionInfo> = Vec::new();
    let mut stored = 0usize;

    for report in &reports {
        for session in &report.sessions {
            match pipeline.redact_session(session.clone()) {
                Ok(redacted) => {
                    let put = store.put(&redacted)?;
                    // Eine vergessene Session wird nicht reanimiert (#6) — also
                    // auch nicht als importiert gezählt oder für das
                    // Commit-Matching verwendet. Im Store liegt nur ihr
                    // Tombstone, nicht die Session.
                    if put.was_forgotten() {
                        // Anders als der Redaction-Skip darunter kein Befund:
                        // Das Tombstone ist gewollt, die Session soll fehlen.
                        // Ein Hinweis für den Hand-Aufrufer, nichts fürs Log.
                        eprintln!("  Session {} bleibt vergessen (nicht reanimiert)", put.id());
                        continue;
                    }
                    infos.push(SessionInfo::of(put.id(), redacted.session()));
                    stored += 1;
                }
                // Eine übersprungene Session ist ein Befund, kein Fortschritt:
                // Sie fehlt danach im Store, und ohne Eintrag wüsste niemand,
                // dass sie je da war — genau der stille Ausfall aus #10.
                Err(err) => hooklog::report_at(
                    &git_dir,
                    Source::Import,
                    &format!("Session übersprungen (Redaction): {err}"),
                ),
            }
        }
        // Was nicht lesbar war, ist ein Befund: Die Session fehlt danach im
        // Store. Die Notiz darunter ist Information — „kein Importer" ist für
        // vier von fünf Agents der Dauerzustand, und ein Log, das bei jedem
        // `enable` wächst, würde `fsck` zum Dauer-Hinweis.
        for error in &report.errors {
            hooklog::report_at(
                &git_dir,
                Source::Import,
                &format!("{}: {error}", report.agent),
            );
        }
        let count = report.sessions.len();
        match &report.note {
            Some(note) => println!("  {}: {note}", report.agent),
            None => println!("  {}: {count} Transkript(e)", report.agent),
        }
    }

    if stored == 0 {
        println!("  nichts zu importieren");
        return Ok(());
    }

    // 3. Commits aus der Historie holen und zuordnen.
    let commits = gather_commits(&root)?;
    let links = match_sessions(&infos, &commits);

    // 4. Den Store-Index um die (vermuteten) Kanten ergänzen — mergend, nicht
    //    ersetzend: eine zweite Ausführung darf bestehende Kanten nicht
    //    verlieren.
    let mut index = store.index()?;
    for link in &links {
        index.link(
            &link.commit,
            link.session,
            EvidenceMark::of(EvidenceSource::Heuristic),
        );
    }
    store.set_index(&index)?;

    println!(
        "  {stored} Session(s) gespeichert, {} Verknüpfung(en) vermutet",
        links.len()
    );
    Ok(())
}

/// Sammelt die erreichbare Historie ab HEAD als [`CommitInfo`] — Hash, Autor-Zeit
/// und geänderte Dateien.
///
/// Ein einziger `git log`-Lauf: `%x01` markiert die Kopfzeile jedes Commits,
/// darunter stehen seine Dateien (`--name-only`). `--no-renames`, damit ein Pfad
/// so heißt wie im Baum. Merges tragen ohne Diff keine Dateien und fallen damit
/// ohnehin aus dem Matching.
fn gather_commits(root: &Path) -> Fallible<Vec<CommitInfo>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "log",
            "--no-renames",
            "--name-only",
            "--format=%x01%H %at",
            "--end-of-options",
            "HEAD",
        ])
        .output()?;
    if !output.status.success() {
        // Kein HEAD (leeres Repo) o. Ä. — dann gibt es nichts zuzuordnen.
        return Ok(Vec::new());
    }
    Ok(parse_log(&String::from_utf8_lossy(&output.stdout)))
}

/// Zerlegt die `git log`-Ausgabe in [`CommitInfo`]s.
fn parse_log(text: &str) -> Vec<CommitInfo> {
    let mut commits: Vec<CommitInfo> = Vec::new();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix('\u{1}') {
            let (hex, epoch) = header.split_once(' ').unwrap_or((header, "0"));
            commits.push(CommitInfo {
                hex: hex.to_string(),
                epoch: epoch.trim().parse().unwrap_or(0),
                files: Vec::new(),
            });
        } else if !line.trim().is_empty() {
            if let Some(current) = commits.last_mut() {
                current.files.push(line.to_string());
            }
        }
    }
    commits
}

fn home_dir() -> Fallible<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME ist nicht gesetzt".into())
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
    fn parse_log_reads_commits_and_their_files() {
        let text = "\u{1}aaaa 1000\nsrc/a.rs\nsrc/b.rs\n\n\u{1}bbbb 2000\ndocs/x.md\n";
        let commits = parse_log(text);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hex, "aaaa");
        assert_eq!(commits[0].epoch, 1000);
        assert_eq!(commits[0].files, vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(commits[1].hex, "bbbb");
        assert_eq!(commits[1].files, vec!["docs/x.md"]);
    }

    #[test]
    fn a_merge_without_files_is_kept_but_empty() {
        let text = "\u{1}mmmm 3000\n\n\u{1}aaaa 1000\nsrc/a.rs\n";
        let commits = parse_log(text);
        assert_eq!(commits.len(), 2);
        assert!(commits[0].files.is_empty());
        assert_eq!(commits[1].files, vec!["src/a.rs"]);
    }

    #[test]
    fn empty_output_yields_no_commits() {
        assert!(parse_log("").is_empty());
    }
}
