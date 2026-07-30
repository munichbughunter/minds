//! `minds sync` — der Kontext reist mit dem Code, in **einer** Verbindung.
//!
//! Der `pre-push`-Hook rief bisher ein zweites `git push` auf. Das kostete auf
//! jedem Push den vollen Verbindungsaufbau — gegen gitlab.com gemessen ~2,7 s,
//! und zwar unabhängig davon, ob es überhaupt etwas zu schicken gab. Bei einer
//! Agent-Flotte, die im Minutentakt pusht, ist das der teuerste Teil des
//! Vorgangs.
//!
//! Dieses Modul ersetzt den Hook durch drei Regeln:
//!
//! 1. **Kein Netz, wenn nichts neu ist.** Was am Remote steht, wissen wir aus
//!    eigenen Tracking-Refs (siehe unten) — der Soll-Ist-Vergleich ist rein
//!    lokal. Ein Push ohne neuen Kontext kostet damit einen Prozessstart, keine
//!    Verbindung.
//! 2. **Ein Push für alle Refs.** Kontext, Reviews und Session-Refs gehen als
//!    Refspec-Liste in *einen* `git push`. N Refs kosten eine Verbindung, nicht
//!    N.
//! 3. **Nie `--force`.** Wird ein Ref abgewiesen, weil zwei Maschinen ihn
//!    fortgeschrieben haben, wird der fremde Stand geholt und **vereinigt**
//!    (der Thread-Log ist konfliktfrei mergebar, siehe
//!    [`ReviewStore::merge_from`]), dann erneut gepusht — wieder fast-forward.
//!    Was sich nicht vereinigen lässt, bleibt liegen. Der Remote wird nie
//!    überschrieben.
//!
//! # Warum synchron und nicht im Hintergrund
//!
//! Ein abgelöster Hintergrundprozess wäre schneller — er hat aber **kein
//! Terminal**. Credential-Helper, SSH-Passphrase und der Touch eines
//! Security-Keys brauchen genau das. Ein Sync, der im Hintergrund still an der
//! Authentifizierung scheitert, ist schlimmer als einer, der zwei Sekunden
//! braucht und es sagt. Deshalb läuft der Push im Vordergrund, meldet seinen
//! Fortschritt auf stderr (Git zeigt die Ausgabe des Hooks) — und tut in dem
//! häufigen Fall, dass nichts neu ist, gar nichts.
//!
//! # Die Tracking-Refs
//!
//! Für `refs/heads/*` führt Git selbst Buch (`refs/remotes/origin/*`); für
//! `refs/minds/*` tut es das nicht. Also führen wir es selbst: Nach einem
//! bestätigten Push steht der gepushte Stand unter
//! `refs/minds/remotes/<remote>/<rest>`. Das ist bewusst **kein** Zustand neben
//! Git — es sind Refs, sie überleben `git gc`, sie lassen sich mit `git
//! for-each-ref` ansehen, und geht einer verloren, ist die Folge ein
//! überflüssiger, idempotenter Push. Kein Journal, das kaputtgehen kann.
//!
//! # Fail-soft
//!
//! Nichts hier darf den Push des Nutzers scheitern lassen. Jeder Fehler wird
//! gemeldet und geschluckt; der Rückgabewert ist auch dann 0. Wer den Sync von
//! Hand aufruft und den Fehler sehen will, nimmt `-v`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime};

use minds_git::{MINDS_REF_NAMESPACE, Repo};
use minds_store::{Backend, DEFAULT_REVIEW_REF, ReviewStore};

use crate::config;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Unterhalb dieses Präfix liegen die eigenen Tracking-Refs. Sie werden nie
/// gepusht — sie sind die Buchhaltung *über* das Pushen.
const TRACKING_PREFIX: &str = "refs/minds/remotes/";

/// Bricht die Rekursion: Der Push dieses Moduls löst im selben Repo erneut
/// `pre-push` aus. `--no-verify` verhindert das bereits; die Umgebungsvariable
/// ist der Gürtel zum Hosenträger (und greift auch, wenn jemand den Hook von
/// Hand aufruft).
const GUARD_ENV: &str = "MINDS_SYNCING";

/// Schlüssel in `.git/config`: `false` schaltet den Sync ab.
const KEY_SYNC: &str = "minds.sync";

/// Nach dieser Zeit gilt ein Lock als verwaist (ein abgestürzter Sync soll den
/// nächsten nicht dauerhaft blockieren).
const LOCK_STALE: Duration = Duration::from_secs(300);

/// Der Remote, wenn der Hook keinen nennt.
const DEFAULT_REMOTE: &str = "origin";

/// Führt `minds sync` aus. Endet **immer** mit 0, wenn es aus dem Hook kommt —
/// siehe Modul-Doku.
pub fn run(remote: Option<&str>, verbose: bool) -> ExitCode {
    if std::env::var_os(GUARD_ENV).is_some() {
        return ExitCode::SUCCESS;
    }
    match sync(remote.unwrap_or(DEFAULT_REMOTE), verbose) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds sync: {err}");
            ExitCode::FAILURE
        }
    }
}

fn sync(remote: &str, verbose: bool) -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let git_dir = repo.git_dir().to_path_buf();
    let root = repo_root(&repo);

    if !enabled(&root) {
        vln(verbose, "minds sync: durch minds.sync abgeschaltet");
        return Ok(());
    }

    // Was ein `git fetch` an fremden Verdicts mitgebracht hat, liegt im
    // Tracking-Namensraum und wird hier vereinigt — lokal, ohne Netz, idempotent.
    // Erst danach steht fest, was zu pushen ist.
    merge_incoming(&root, remote, verbose);

    let jobs = plan(&root, &repo, remote)?;
    if jobs.iter().all(|job| job.updates.is_empty()) {
        // Der häufige Fall. Bis hierher wurde keine Verbindung geöffnet.
        vln(verbose, "minds sync: nichts Neues");
        return Ok(());
    }

    let Some(_lock) = Lock::acquire(&git_dir)? else {
        // Ein zweiter Agent pusht gerade. Der ist entweder schon weiter als wir
        // oder wird gleich neu planen; ein zweiter Verbindungsaufbau brächte
        // nichts.
        vln(verbose, "minds sync: läuft bereits");
        return Ok(());
    };

    for job in jobs {
        if job.updates.is_empty() {
            continue;
        }
        if let Err(err) = job.execute(verbose) {
            // Fail-soft: Der Push des Nutzers läuft weiter, die Refs bleiben
            // ungetrackt und werden beim nächsten Mal erneut angeboten.
            eprintln!("minds: Kontext-Sync ({}) nicht möglich: {err}", job.label);
        }
    }
    Ok(())
}

/// Ob der Sync eingeschaltet ist (`minds.sync`, Default `true`).
fn enabled(root: &Path) -> bool {
    !matches!(
        git_output(root, &["config", "--local", "--get", KEY_SYNC])
            .unwrap_or_default()
            .trim(),
        "false" | "no" | "0" | "off"
    )
}

// ---------------------------------------------------------------------------
// Der Plan — rein lokal
// ---------------------------------------------------------------------------

/// Ein Ref, der zu schicken ist: was, wohin, und wo der Erfolg vermerkt wird.
#[derive(Debug, PartialEq, Eq)]
struct Update {
    /// Der lokale Ref (nur für die Anzeige).
    local: String,
    /// Der Commit, der geschickt wird. Bewusst der Hash und nicht der Ref-Name:
    /// So landet am Remote genau das, was hier geplant wurde, auch wenn ein
    /// paralleler Checkpoint den Ref inzwischen weiterschiebt.
    oid: String,
    /// Der Ziel-Ref am Remote.
    destination: String,
    /// Der Tracking-Ref, der nach dem Erfolg auf `oid` gesetzt wird.
    tracking: String,
}

/// Ein Push: ein Repository, ein Remote, alle dorthin fälligen Refs.
#[derive(Debug)]
struct Job {
    dir: PathBuf,
    remote: String,
    label: String,
    updates: Vec<Update>,
}

/// Vereinigt den vom letzten `git fetch` mitgebrachten Review-Stand in den
/// lokalen Log.
///
/// `minds enable` setzt dafür den Fetch-Refspec
/// `+refs/minds/reviews:refs/minds/remotes/<remote>/reviews`: Fremde Verdicts
/// landen im Tracking-Namensraum und **überschreiben nie** den lokalen Log.
/// Zusammengeführt wird hier — durch Vereinigung, konfliktfrei (siehe
/// [`ReviewStore::merge_from`]). Damit konvergieren zwei Maschinen ohne
/// Zutun: fetch bringt es her, sync führt es zusammen, der nächste Push
/// schickt den vereinigten Stand zurück.
fn merge_incoming(root: &Path, remote: &str, verbose: bool) {
    let incoming = format!("{}reviews", tracking_prefix(remote));
    let merged = Repo::open(root)
        .map_err(|err| err.to_string())
        .and_then(|repo| {
            ReviewStore::new(repo)
                .merge_from(&incoming)
                .map_err(|err| err.to_string())
        });
    match merged {
        Ok(0) => {}
        Ok(count) => eprintln!("minds: {count} fremde(s) Verdict(s) übernommen"),
        Err(err) => vln(verbose, &format!("minds sync: Merge übersprungen: {err}")),
    }
}

/// Was zu tun ist — ohne eine einzige Netzoperation.
fn plan(root: &Path, repo: &Repo, remote: &str) -> Fallible<Vec<Job>> {
    let store = config::load(root);
    let mut jobs = Vec::new();

    // 1. Das Repo des Codes → das Remote, auf das der Nutzer gerade pusht.
    //    In-Repo-Backend: Kontext, Reviews und Session-Refs. Child-Backend:
    //    nur die Reviews (die liegen immer beim Code).
    if has_remote(root, remote) {
        jobs.push(Job {
            dir: root.to_path_buf(),
            remote: remote.to_string(),
            label: remote.to_string(),
            updates: due(repo, remote, MINDS_REF_NAMESPACE, identity)?,
        });
    }

    // 2. Das Child-Repo → dessen eigenes `origin`. Dort werden die Session-Refs
    //    bewusst auf Branches gemappt: Das Kontext-Repo *soll* sie in der
    //    Forge-Oberfläche als auswählbare Branches zeigen (siehe `enable.rs`).
    if let Backend::ChildRepo { path } = store.backend() {
        let child = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        if child.is_dir() && has_remote(&child, DEFAULT_REMOTE) {
            let child_repo = Repo::open(&child)?;
            jobs.push(Job {
                dir: child.clone(),
                remote: DEFAULT_REMOTE.to_string(),
                label: format!("Child-Repo {}", child.display()),
                // Alles unter refs/minds/ — die Nutzlast (`store/…`) identisch,
                // die Browsing-Refs (`sessions/…`) als Branches. Welches was
                // ist, entscheidet die Abbildung.
                updates: due(
                    &child_repo,
                    DEFAULT_REMOTE,
                    MINDS_REF_NAMESPACE,
                    session_branch,
                )?,
            });
        }
    }

    Ok(jobs)
}

/// Alle Refs unter `prefix`, deren Stand noch nicht am `remote` vermerkt ist.
fn due(
    repo: &Repo,
    remote: &str,
    prefix: &str,
    destination: fn(&str) -> String,
) -> Fallible<Vec<Update>> {
    let tracked: BTreeMap<String, String> = repo
        .refs_under(&tracking_prefix(remote))?
        .into_iter()
        .map(|(name, commit)| (name, commit.to_string()))
        .collect();

    let mut updates = Vec::new();
    for (name, commit) in repo.refs_under(prefix)? {
        // Die eigene Buchhaltung wird nie gepusht.
        if name.starts_with(TRACKING_PREFIX) {
            continue;
        }
        let oid = commit.to_string();
        let tracking = tracking_ref(remote, &name);
        if tracked.get(&tracking) == Some(&oid) {
            continue;
        }
        updates.push(Update {
            local: name.clone(),
            oid,
            destination: destination(&name),
            tracking,
        });
    }
    Ok(updates)
}

/// Der Ziel-Ref beim identischen Mapping: derselbe Name am Remote.
///
/// Dass das geht, ist keine Selbstverständlichkeit — aber überprüft: GitLab
/// nimmt `refs/minds/context` an. Und weil der Ref nicht unter `refs/heads/`
/// liegt, kann eine Forge ihn weder als Default-Branch wählen noch in die
/// Branch-Liste des Nutzers stellen.
fn identity(name: &str) -> String {
    name.to_string()
}

/// Der Ziel-Ref im Child-Repo: `refs/minds/sessions/<hex>` → Branch
/// `minds/session/<hex>`, damit die Forge jede Session als eigenen,
/// anklickbaren Branch zeigt.
fn session_branch(name: &str) -> String {
    match name.strip_prefix("refs/minds/sessions/") {
        Some(hex) => format!("refs/heads/minds/session/{hex}"),
        None => name.to_string(),
    }
}

/// `refs/minds/remotes/<remote>/`.
fn tracking_prefix(remote: &str) -> String {
    format!("{TRACKING_PREFIX}{remote}/")
}

/// Der Tracking-Ref zu einem lokalen Minds-Ref.
fn tracking_ref(remote: &str, local: &str) -> String {
    let rest = local.strip_prefix(MINDS_REF_NAMESPACE).unwrap_or(local);
    format!("{}{rest}", tracking_prefix(remote))
}

// ---------------------------------------------------------------------------
// Die Ausführung — hier wird das Netz angefasst
// ---------------------------------------------------------------------------

impl Job {
    fn execute(&self, verbose: bool) -> Fallible<()> {
        // Fortschritt auf stderr: Git zeigt die Ausgabe des Hooks. Ein Push,
        // der zehn Sekunden schweigt, sieht aus wie ein hängender Push.
        eprint!(
            "minds: {} Ref(s) → {} …",
            self.updates.len(),
            display(&self.label)
        );

        match self.push(&self.updates) {
            Ok(()) => {
                eprintln!(" fertig");
                self.record(&self.updates, verbose);
                Ok(())
            }
            Err(err) => {
                eprintln!();
                // Divergenz ist der eine Fehler, aus dem wir uns selbst
                // befreien können — aber nur dort, wo der Inhalt vereinigbar
                // ist. Für alles andere gilt: melden, nichts überschreiben.
                if !is_rejected(&err.to_string()) {
                    return Err(err);
                }
                self.reconcile(verbose)
            }
        }
    }

    /// Ein einziger `git push` für alle Refspecs.
    fn push(&self, updates: &[Update]) -> Fallible<()> {
        let mut args: Vec<String> = vec![
            "push".into(),
            // Der eigene Push darf den pre-push-Hook nicht erneut auslösen.
            "--no-verify".into(),
            "--porcelain".into(),
            self.remote.clone(),
        ];
        args.extend(
            updates
                .iter()
                .map(|update| format!("{}:{}", update.oid, update.destination)),
        );

        let output = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(&args)
            // Kein Terminal-Prompt aus dem Hook heraus: lieber schnell
            // scheitern als den Push des Nutzers an einer Passwortfrage
            // hängen lassen, die er nicht sieht.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env(GUARD_ENV, "1")
            .output()?;

        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim()
        )
        .into())
    }

    /// Vermerkt den gepushten Stand in den Tracking-Refs.
    fn record(&self, updates: &[Update], verbose: bool) {
        for update in updates {
            let done = Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(["update-ref", &update.tracking, &update.oid])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if !done {
                // Kein Drama: Ohne Vermerk wird derselbe Ref beim nächsten Mal
                // erneut angeboten — der Push ist idempotent.
                vln(
                    verbose,
                    &format!("  Tracking-Ref {} nicht gesetzt", update.tracking),
                );
            }
        }
    }

    /// Ein Ref wurde abgewiesen — jemand anderes hat ihn fortgeschrieben.
    ///
    /// Für den Review-/Thread-Log ist das lösbar: Sein Inhalt ist ein Log aus
    /// content-adressierten Einträgen, zwei Stände lassen sich **vereinigen**
    /// (kommutativ, konfliktfrei). Also: fremden Stand holen, vereinigen,
    /// erneut pushen — und der zweite Push ist wieder fast-forward.
    ///
    /// Für alles andere bleibt es beim Melden. Ein `--force` würde hier den
    /// Kontext einer anderen Maschine löschen, und das ist der eine Fehler, den
    /// ein Audit-Record nicht machen darf.
    fn reconcile(&self, verbose: bool) -> Fallible<()> {
        let Some(review) = self
            .updates
            .iter()
            .find(|update| update.local == DEFAULT_REVIEW_REF)
        else {
            return Err("Remote ist weiter als wir — bitte `git fetch` und erneut pushen".into());
        };

        eprint!("minds: Review-Log divergiert, vereinige …");
        let incoming = format!("{}incoming", tracking_prefix(&self.remote));
        let fetched = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args([
                "fetch",
                "--quiet",
                &self.remote,
                &format!("+{}:{incoming}", review.destination),
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env(GUARD_ENV, "1")
            .status()?;
        if !fetched.success() {
            eprintln!();
            return Err("fremder Review-Stand nicht abrufbar".into());
        }

        let repo = Repo::open(&self.dir)?;
        let merged = ReviewStore::new(repo).merge_from(&incoming)?;

        // Nach dem Merge zeigt der Ref woanders hin — der Plan von vorhin ist
        // veraltet, also neu nachsehen.
        let repo = Repo::open(&self.dir)?;
        let Some(commit) = repo.commit_at(DEFAULT_REVIEW_REF)? else {
            eprintln!();
            return Err("Review-Ref ist nach dem Merge verschwunden".into());
        };
        let retry = vec![Update {
            local: DEFAULT_REVIEW_REF.to_string(),
            oid: commit.to_string(),
            destination: review.destination.clone(),
            tracking: review.tracking.clone(),
        }];
        self.push(&retry)?;
        eprintln!(" {merged} übernommen, fertig");
        self.record(&retry, verbose);
        Ok(())
    }
}

/// Ob die Ausgabe von `git push` eine Abweisung meldet (statt eines
/// Netz-/Auth-Fehlers).
fn is_rejected(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("non-fast-forward")
        || text.contains("rejected")
        || text.contains("fetch first")
        || text.contains("stale info")
}

// ---------------------------------------------------------------------------
// Lock — damit eine Agent-Flotte nicht N Verbindungen gleichzeitig öffnet
// ---------------------------------------------------------------------------

/// Ein Lock, das sich beim Fallenlassen selbst entfernt.
///
/// Absichtlich primitiv: `create_new` ist auf jedem Dateisystem atomar, und
/// mehr als „einer nach dem anderen" wird hier nicht gebraucht. Ein Lock, das
/// älter ist als [`LOCK_STALE`], stammt von einem abgestürzten Lauf und wird
/// übergangen — ein Rekorder darf sich nicht selbst dauerhaft aussperren.
struct Lock {
    path: PathBuf,
}

impl Lock {
    fn acquire(git_dir: &Path) -> std::io::Result<Option<Self>> {
        let dir = git_dir.join("minds");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("sync.lock");

        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Some(Self { path })),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !is_stale(&path) {
                        return Ok(None);
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(err) => return Err(err),
            }
        }
        Ok(None)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn is_stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .map(|age| age > LOCK_STALE)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Kleinkram
// ---------------------------------------------------------------------------

fn repo_root(repo: &Repo) -> PathBuf {
    let git_dir = repo.git_dir();
    if git_dir.file_name().is_some_and(|name| name == ".git") {
        git_dir.parent().unwrap_or(git_dir).to_path_buf()
    } else {
        git_dir.to_path_buf()
    }
}

/// Ob `dir` einen Remote dieses Namens kennt. Rein lokal (`git remote`), kein
/// `ls-remote`.
fn has_remote(dir: &Path, remote: &str) -> bool {
    git_output(dir, &["remote"])
        .unwrap_or_default()
        .lines()
        .any(|line| line.trim() == remote)
}

fn git_output(dir: &Path, args: &[&str]) -> std::io::Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Eine URL im Label würde ein Token mitdrucken, das jemand in die Remote-URL
/// geschrieben hat. Nur Namen anzeigen.
fn display(label: &str) -> &str {
    if label.contains("://") || label.contains('@') {
        "Remote"
    } else {
        label
    }
}

fn vln(verbose: bool, line: &str) {
    if verbose {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tracking_ref_mirrors_the_local_name() {
        assert_eq!(
            tracking_ref("origin", "refs/minds/context"),
            "refs/minds/remotes/origin/context"
        );
        assert_eq!(
            tracking_ref("origin", "refs/minds/sessions/abc"),
            "refs/minds/remotes/origin/sessions/abc"
        );
    }

    #[test]
    fn tracking_refs_are_never_pushed() {
        // Sonst spiegelte sich die eigene Buchhaltung ins Remote — und beim
        // nächsten Lauf hielte sie sich selbst für zu pushenden Kontext.
        assert!(tracking_ref("origin", "refs/minds/context").starts_with(TRACKING_PREFIX));
    }

    #[test]
    fn the_identity_mapping_keeps_the_name() {
        assert_eq!(identity("refs/minds/context"), "refs/minds/context");
    }

    #[test]
    fn a_session_ref_becomes_a_branch_in_the_child_repo() {
        assert_eq!(
            session_branch("refs/minds/sessions/ab12"),
            "refs/heads/minds/session/ab12"
        );
        // Fremdes bleibt unangetastet.
        assert_eq!(session_branch("refs/minds/context"), "refs/minds/context");
    }

    #[test]
    fn a_rejection_is_told_apart_from_a_network_error() {
        assert!(is_rejected(
            "! [rejected] refs/minds/reviews -> refs/minds/reviews (non-fast-forward)"
        ));
        assert!(is_rejected("Updates were rejected because the remote…"));
        assert!(!is_rejected(
            "ssh: connect to host gitlab.com port 22: timeout"
        ));
        assert!(!is_rejected("Permission denied (publickey)."));
    }

    #[test]
    fn a_url_label_is_not_printed() {
        assert_eq!(display("origin"), "origin");
        assert_eq!(display("https://token@example.org/x.git"), "Remote");
        assert_eq!(display("git@example.org:x.git"), "Remote");
    }
}
