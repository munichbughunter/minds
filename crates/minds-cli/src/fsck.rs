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
//! 3. **Die Hooks liegen dort, wo Git sie liest.** Ein Repo mit `core.hooksPath`
//!    (husky, lefthook) führt `.git/hooks` nie aus; ein Hook, der dort liegt,
//!    ist kein Hook. Auch das ist eine Warnung — nicht jedes Repo *will*
//!    Hooks —, aber eine, die den stillsten Ausfall des Produkts benennt.
//!
//! Ehrlich lückenhaft schlägt still vollständig — diese Datei ist die Einlösung
//! dieses Satzes aus dem ganzen Entwurf.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use minds_capture::Journal;
use minds_core::{ChangeId, Decision, SessionId, Trailer};
use minds_git::{CommitId, Repo};
use minds_store::ReviewStore;

use crate::config;
use crate::enable::NoHooksDir;

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
    let hints = report_hooks(&root, &hook_state(&root, repo.git_dir()));

    let mut total = orphans + index_orphans;
    if require_review {
        let reviews =
            ReviewStore::new(Repo::open(&root).map_err(minds_store::StoreError::backend)?);
        total += check_reviews(&repo, &root, &reviews)?;
    }

    if total == 0 {
        // Ein Hinweis bricht die Integrität nicht — verschweigen darf ihn die
        // Schlusszeile trotzdem nicht, sonst liest sich „in Ordnung" über einen
        // Hook hinweg, der nie feuert.
        match hints {
            0 => println!("fsck: in Ordnung"),
            n => println!("fsck: in Ordnung, {n} Hinweis(e)"),
        }
        Ok(true)
    } else {
        match hints {
            0 => println!("fsck: {total} Befund(e)"),
            n => println!("fsck: {total} Befund(e), {n} Hinweis(e)"),
        }
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

/// Die Hooks, ohne die nichts erfasst wird.
///
/// `pre-push` fehlt bewusst — nicht, weil `enable` ihn nicht schriebe (das tut
/// es immer), sondern weil sein Fehlen nichts kostet: Ohne ihn geht kein
/// Kontext verloren, er reist nur später mit dem nächsten `minds sync`.
const REQUIRED_HOOKS: [&str; 2] = ["post-commit", "prepare-commit-msg"];

/// Was `fsck` über die Hooks herausfindet.
#[derive(Debug, PartialEq, Eq)]
enum HookState {
    /// `core.hooksPath` führt auf kein brauchbares Verzeichnis. Dann gibt es
    /// nichts zu suchen: Kein Ort wäre der richtige.
    Unusable(NoHooksDir),
    /// Git führt Hooks aus einem Verzeichnis aus; hier steht, was dort liegt.
    Checked {
        /// Das Verzeichnis, aus dem Git die Hooks **tatsächlich** ausführt.
        hooks_dir: PathBuf,
        /// Hooks aus [`REQUIRED_HOOKS`], die dort keinen minds-Block tragen.
        missing: Vec<&'static str>,
        /// Hooks, die keine sind: Symlink, Verzeichnis, zu groß, kein Text.
        /// Getrennt von `missing`, weil der Rat ein anderer ist — `minds enable`
        /// würde diese Datei nicht anlegen, sondern ablehnen.
        refused: Vec<(&'static str, String)>,
        /// Wo unser Block stattdessen liegt: das ignorierte `<git-dir>/hooks` —
        /// der Fingerabdruck eines `enable` aus der Zeit vor #9.
        stray: Option<PathBuf>,
    },
}

/// Prüft die Hooks vom **effektiven** Hook-Verzeichnis aus (`core.hooksPath`),
/// nicht von `<git-dir>/hooks`.
fn hook_state(root: &Path, git_dir: &Path) -> HookState {
    let hooks_dir = match crate::enable::effective_hooks_dir(root, git_dir) {
        crate::enable::HooksDir::Unusable(why) => return HookState::Unusable(why),
        crate::enable::HooksDir::At(dir) => dir,
    };
    // `missing` nur für die Hooks, ohne die nichts erfasst wird — `abgelehnt`
    // dagegen für **alle**, die `enable` anfasst: Ein Symlink auf `pre-push`
    // lässt `enable` abbrechen, und ein `fsck`, das im selben Repo „in Ordnung"
    // meldet, ist genau die Divergenz, um die es in #9 geht.
    let mut missing = Vec::new();
    let mut refused = Vec::new();
    for name in crate::enable::hook_names() {
        match inspect_hook(&hooks_dir.join(name)) {
            Hook::Installed => {}
            Hook::Absent if REQUIRED_HOOKS.contains(&name) => missing.push(name),
            Hook::Absent => {}
            Hook::Refused(reason) => refused.push((name, reason)),
        }
    }

    let default_dir = git_dir.join("hooks");
    let stray = (!crate::enable::same_location(&hooks_dir, &default_dir)
        && missing
            .iter()
            .any(|name| matches!(inspect_hook(&default_dir.join(name)), Hook::Installed)))
    .then_some(default_dir);

    HookState::Checked {
        hooks_dir,
        missing,
        refused,
        stray,
    }
}

/// Was an einem Hook-Pfad liegt.
enum Hook {
    /// Eine Hook-Datei mit unserem Block.
    Installed,
    /// Nichts, oder ein fremder Hook ohne unseren Block.
    Absent,
    /// Etwas, das `enable` nicht ergänzen würde — mit dem Grund.
    Refused(String),
}

/// Sieht nach, was an diesem Pfad liegt.
///
/// Bewusst über dieselbe Lesefunktion wie `enable`: Das Hook-Verzeichnis kann in
/// der versionierten Arbeitskopie liegen, sein Inhalt ist also fremdbestimmt.
/// Ein eingecheckter Symlink auf `/dev/zero` ließe ein nacktes `read_to_string`
/// hier bis zum Speicherende laufen — bei jedem Kollegen und in der CI.
///
/// Der Ablehnungsgrund wird **behalten**, nicht verschluckt: „fehlt" mit dem Rat
/// `minds enable` wäre für einen Symlink ein Rat, der garantiert scheitert.
/// `fsck` bricht deshalb trotzdem nicht ab — es ist ein Bericht.
fn inspect_hook(path: &Path) -> Hook {
    match crate::enable::read_existing_hook(path) {
        Ok(Some(body)) if body.contains(crate::enable::MARK_BEGIN) => Hook::Installed,
        Ok(_) => Hook::Absent,
        Err(err) => Hook::Refused(err.to_string()),
    }
}

/// Der Hook-Abschnitt des Berichts, Zeile für Zeile.
///
/// Als reine Funktion, damit der Wortlaut prüfbar ist statt nur sichtbar: Die
/// Ausgabe ist hier die ganze Leistung des Kommandos — wer sie nur druckt,
/// testet sie nie.
///
/// Die erste Zeile fasst zusammen, weitere Zeilen erklären. Pfade stehen in
/// Anführungszeichen: Ein Pfad mit Leerzeichen ist sonst nicht als einer zu
/// erkennen, und ein leerer Pfad ließe den Satz mitten im Nichts enden.
fn hook_report_lines(root: &Path, state: &HookState) -> Vec<String> {
    let (hooks_dir, missing, refused, stray) = match state {
        HookState::Unusable(why) => {
            let first = match why {
                NoHooksDir::Empty => {
                    "Hooks: core.hooksPath ist leer — Git führt gar keine Hooks aus"
                }
                NoHooksDir::WorktreeRoot => {
                    "Hooks: core.hooksPath zeigt auf die Repo-Wurzel — dort führt Git sie nicht aus"
                }
                NoHooksDir::Unanswered => {
                    "Hooks: core.hooksPath ist gesetzt, aber Git nennt kein Hook-Verzeichnis"
                }
            };
            return vec![
                first.to_owned(),
                "  kein Commit erzeugt einen Checkpoint, solange das so ist".to_owned(),
            ];
        }
        HookState::Checked {
            hooks_dir,
            missing,
            refused,
            stray,
        } => (hooks_dir, missing, refused, stray),
    };

    let dir = short(root, hooks_dir);
    if missing.is_empty() && refused.is_empty() {
        return vec![format!("Hooks: installiert in „{dir}“")];
    }

    let mut lines = Vec::new();
    if !missing.is_empty() {
        // Ein fehlender Hook ist der häufigste Fall — der Satz muss auch im
        // Singular stimmen, in beiden Zeilen.
        let (verb, object) = if missing.len() == 1 {
            ("fehlt", "ihn")
        } else {
            ("fehlen", "sie")
        };
        lines.push(format!("Hooks: {} {verb} in „{dir}“", missing.join(", ")));
        if let Some(stray) = stray {
            lines.push(format!(
                "  der minds-Block liegt in „{}“, aber core.hooksPath verweist auf „{dir}“ — \
                 Git liest ihn dort nie",
                short(root, stray)
            ));
        }
        lines.push(format!("  `minds enable` installiert {object} (neu)"));
    }

    // Abgelehnte Hooks bekommen den Grund statt des Rats: `minds enable` würde
    // hier nicht installieren, sondern mit derselben Begründung abbrechen.
    for (name, reason) in refused {
        lines.push(format!(
            "Hooks: {name} in „{dir}“ ist kein Hook, den minds ergänzt"
        ));
        lines.push(format!("  {reason}"));
    }
    lines
}

/// Meldet den Hook-Zustand und gibt zurück, ob das ein Hinweis war. Kein
/// Befund: Ein Repo ohne Hooks ist nicht kaputt, es erfasst nur nichts.
fn report_hooks(root: &Path, state: &HookState) -> usize {
    for line in hook_report_lines(root, state) {
        println!("{line}");
    }
    usize::from(!matches!(
        state,
        HookState::Checked { missing, refused, .. } if missing.is_empty() && refused.is_empty()
    ))
}

/// Ein Pfad, wie er im Bericht erscheint: relativ zur Repo-Wurzel, wo das geht,
/// und mit entschärften Steuerzeichen (siehe [`crate::enable::display_path`]).
fn short(root: &Path, path: &Path) -> String {
    crate::enable::display_path(path.strip_prefix(root).unwrap_or(path))
}

fn repo_root(repo: &Repo) -> std::path::PathBuf {
    repo.git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enable::MARK_BEGIN;

    /// Ein frisch initialisiertes Repo, oder `None` ohne `git` im Pfad.
    fn repo() -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().unwrap();
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["init", "--quiet"])
            .status()
            .ok()?
            .success();
        ok.then_some(dir)
    }

    fn set_hooks_path(root: &Path, value: &str) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", "--local", "core.hooksPath", value])
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    fn write_hook(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn install_ours(dir: &Path) {
        for name in REQUIRED_HOOKS {
            write_hook(dir, name, &format!("#!/bin/sh\n{MARK_BEGIN}\nminds …\n"));
        }
    }

    /// Zerlegt einen `Checked`-Zustand für die Prüfung; `Unusable` ist hier ein
    /// Testfehler, kein stiller Sonderfall.
    #[allow(clippy::type_complexity)]
    fn checked(
        state: &HookState,
    ) -> (
        &PathBuf,
        &Vec<&'static str>,
        &Vec<(&'static str, String)>,
        &Option<PathBuf>,
    ) {
        match state {
            HookState::Checked {
                hooks_dir,
                missing,
                refused,
                stray,
            } => (hooks_dir, missing, refused, stray),
            HookState::Unusable(why) => {
                panic!("erwartet: geprüftes Hook-Verzeichnis, nicht Unusable({why:?})")
            }
        }
    }

    #[test]
    fn installed_hooks_at_the_effective_path_are_in_order() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        set_hooks_path(root, ".husky");
        install_ours(&root.join(".husky"));

        let state = hook_state(root, &root.join(".git"));
        let (hooks_dir, missing, _, stray) = checked(&state);
        assert_eq!(hooks_dir, &root.join(".husky"));
        assert!(missing.is_empty(), "{state:?}");
        assert_eq!(stray, &None);
    }

    /// Der Befund aus #9: `enable` schrieb nach `.git/hooks`, Git liest
    /// `.husky`. Beides einzeln sieht heil aus — erst der Vergleich zeigt den
    /// Ausfall, und `fsck` muss ihn benennen.
    #[test]
    fn hooks_in_the_ignored_directory_are_reported_as_stray() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        set_hooks_path(root, ".husky");
        install_ours(&root.join(".git/hooks"));

        let state = hook_state(root, &root.join(".git"));
        let (_, missing, _, stray) = checked(&state);
        assert_eq!(missing, &REQUIRED_HOOKS.to_vec());
        assert_eq!(stray, &Some(root.join(".git/hooks")));
    }

    /// Ein fremder Hook (husky ohne minds) ist kein installierter minds-Hook —
    /// und auch kein verirrter Block.
    #[test]
    fn a_foreign_hook_counts_as_missing_not_as_stray() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        set_hooks_path(root, ".husky");
        write_hook(&root.join(".husky"), "post-commit", "#!/bin/sh\nnpm test\n");

        let state = hook_state(root, &root.join(".git"));
        let (_, missing, _, stray) = checked(&state);
        assert_eq!(missing, &REQUIRED_HOOKS.to_vec());
        assert_eq!(stray, &None);
    }

    /// Zeigt `core.hooksPath` auf das Standardverzeichnis, ist nichts verirrt —
    /// dann fehlt der Block einfach. Der Pfad steht hier **explizit** in der
    /// lokalen Config, weil ein global gesetztes `core.hooksPath` den Fall sonst
    /// verfälschte; „gar nicht gesetzt" lässt sich von außen nicht erzwingen.
    #[test]
    fn a_hookspath_at_the_default_leaves_nothing_stray() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let git_dir = root.join(".git");
        set_hooks_path(root, &git_dir.join("hooks").display().to_string());

        let state = hook_state(root, &git_dir);
        let (hooks_dir, missing, _, stray) = checked(&state);
        assert_eq!(hooks_dir, &git_dir.join("hooks"));
        assert_eq!(missing, &REQUIRED_HOOKS.to_vec());
        assert_eq!(stray, &None);
    }

    /// Ein leeres `core.hooksPath` schaltet Git ab — dann gibt es kein
    /// Verzeichnis zu prüfen, sondern etwas zu sagen.
    #[test]
    fn an_empty_hookspath_is_reported_as_unusable() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        set_hooks_path(root, "");

        assert_eq!(
            hook_state(root, &root.join(".git")),
            HookState::Unusable(NoHooksDir::Empty)
        );
    }

    /// `fsck` läuft bei jedem Kollegen und in der CI. Ein eingecheckter Symlink
    /// im (versionierten) Hook-Verzeichnis darf es weder hängen lassen noch den
    /// Speicher füllen — und er ist nicht einfach „fehlend": `minds enable`
    /// würde ihn ablehnen, der Rat wäre also falsch.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_hook_is_refused_not_reported_as_missing() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        set_hooks_path(root, ".husky");
        let hooks = root.join(".husky");
        std::fs::create_dir_all(&hooks).unwrap();
        std::os::unix::fs::symlink("/dev/zero", hooks.join("post-commit")).unwrap();

        let state = hook_state(root, &root.join(".git"));
        let (_, missing, refused, _) = checked(&state);
        assert!(!missing.contains(&"post-commit"), "{state:?}");
        assert!(
            refused
                .iter()
                .any(|(name, reason)| *name == "post-commit" && reason.contains("Symlink")),
            "{state:?}"
        );

        // Und der Bericht rät nicht zu etwas, das scheitern würde.
        let lines = hook_report_lines(root, &state).join("\n");
        assert!(lines.contains("kein Hook, den minds ergänzt"), "{lines}");
    }

    /// Dasselbe für die schlichte große Datei — die braucht keinen Symlink.
    #[test]
    fn an_oversized_hook_is_refused() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        set_hooks_path(root, ".husky");
        let hooks = root.join(".husky");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("post-commit"),
            vec![b'x'; crate::enable::MAX_HOOK_BYTES as usize + 1],
        )
        .unwrap();

        let state = hook_state(root, &root.join(".git"));
        let (_, _, refused, _) = checked(&state);
        assert!(
            refused.iter().any(|(name, _)| *name == "post-commit"),
            "{state:?}"
        );
    }

    /// Der eingefrorene Wortlaut für einen abgelehnten Hook.
    #[test]
    fn golden_a_refused_hook_gets_the_reason_not_the_advice() {
        let state = HookState::Checked {
            hooks_dir: PathBuf::from("/repo/.husky"),
            missing: vec![],
            refused: vec![("post-commit", "…/post-commit ist ein Symlink".to_owned())],
            stray: None,
        };
        assert_eq!(
            lines(&state),
            [
                "Hooks: post-commit in „.husky“ ist kein Hook, den minds ergänzt",
                "  …/post-commit ist ein Symlink",
            ]
        );
    }

    /// `core.hooksPath = .` löst auf die Arbeitskopie auf. Auch das ist kein
    /// Verzeichnis, in dem ein Hook etwas bewirkt.
    #[test]
    fn a_hookspath_at_the_worktree_root_is_reported_as_unusable() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        set_hooks_path(root, ".");

        assert_eq!(
            hook_state(root, &root.join(".git")),
            HookState::Unusable(NoHooksDir::WorktreeRoot)
        );
    }

    // -----------------------------------------------------------------------
    // Golden: der eingefrorene Wortlaut
    //
    // Die Ausgabe *ist* die Leistung von `fsck` — ändert sie sich, soll das
    // eine bewusste Entscheidung sein und kein Nebeneffekt. Die Pfade sind
    // relativ zur Wurzel, damit die Zeilen unabhängig vom Temp-Verzeichnis
    // gleich bleiben.
    // -----------------------------------------------------------------------

    /// Baut die Zeilen zu einem Zustand, wie ihn `fsck` gedruckt hätte.
    fn lines(state: &HookState) -> Vec<String> {
        hook_report_lines(Path::new("/repo"), state)
    }

    #[test]
    fn golden_all_hooks_installed() {
        let state = HookState::Checked {
            refused: vec![],
            hooks_dir: PathBuf::from("/repo/.husky"),
            missing: vec![],
            stray: None,
        };
        assert_eq!(lines(&state), ["Hooks: installiert in „.husky“"]);
    }

    /// Genau ein fehlender Hook — der Satz muss im Singular stimmen.
    #[test]
    fn golden_one_hook_missing_reads_as_singular() {
        let state = HookState::Checked {
            refused: vec![],
            hooks_dir: PathBuf::from("/repo/.husky"),
            missing: vec!["prepare-commit-msg"],
            stray: None,
        };
        assert_eq!(
            lines(&state),
            [
                "Hooks: prepare-commit-msg fehlt in „.husky“",
                "  `minds enable` installiert ihn (neu)",
            ]
        );
    }

    #[test]
    fn golden_two_hooks_missing_with_a_stray_block() {
        let state = HookState::Checked {
            refused: vec![],
            hooks_dir: PathBuf::from("/repo/.husky"),
            missing: vec!["post-commit", "prepare-commit-msg"],
            stray: Some(PathBuf::from("/repo/.git/hooks")),
        };
        assert_eq!(
            lines(&state),
            [
                "Hooks: post-commit, prepare-commit-msg fehlen in „.husky“",
                "  der minds-Block liegt in „.git/hooks“, aber core.hooksPath verweist auf \
                 „.husky“ — Git liest ihn dort nie",
                "  `minds enable` installiert sie (neu)",
            ]
        );
    }

    #[test]
    fn golden_unusable_because_empty() {
        assert_eq!(
            lines(&HookState::Unusable(NoHooksDir::Empty)),
            [
                "Hooks: core.hooksPath ist leer — Git führt gar keine Hooks aus",
                "  kein Commit erzeugt einen Checkpoint, solange das so ist",
            ]
        );
    }

    #[test]
    fn golden_unusable_because_worktree_root() {
        assert_eq!(
            lines(&HookState::Unusable(NoHooksDir::WorktreeRoot)),
            [
                "Hooks: core.hooksPath zeigt auf die Repo-Wurzel — dort führt Git sie nicht aus",
                "  kein Commit erzeugt einen Checkpoint, solange das so ist",
            ]
        );
    }

    /// Determinismus, wo er nicht selbstverständlich ist: Die Reihenfolge der
    /// fehlenden Hooks kommt aus [`REQUIRED_HOOKS`] — nicht daraus, in welcher
    /// Reihenfolge die Dateien im Verzeichnis liegen. Deshalb legt der Test sie
    /// **rückwärts** an und erwartet den Bericht trotzdem vorwärts.
    #[test]
    fn the_order_of_missing_hooks_follows_the_constant_not_the_directory() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        set_hooks_path(root, ".husky");
        let hooks = root.join(".husky");
        for name in REQUIRED_HOOKS.iter().rev() {
            write_hook(&hooks, name, "#!/bin/sh\nfremd\n");
        }

        let state = hook_state(root, &root.join(".git"));
        let (_, missing, _, _) = checked(&state);
        assert_eq!(missing, &REQUIRED_HOOKS.to_vec());
        assert!(
            hook_report_lines(root, &state)[0].contains("post-commit, prepare-commit-msg"),
            "{:?}",
            hook_report_lines(root, &state)
        );
    }

    /// Ein Pfad mit Leerzeichen bleibt als *ein* Pfad erkennbar.
    #[test]
    fn a_path_with_spaces_stays_readable() {
        let state = HookState::Checked {
            refused: vec![],
            hooks_dir: PathBuf::from("/repo/mein ordner/hooks"),
            missing: vec!["post-commit"],
            stray: None,
        };
        assert_eq!(
            lines(&state)[0],
            "Hooks: post-commit fehlt in „mein ordner/hooks“"
        );
    }
}
