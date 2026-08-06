//! `minds enable` — Hooks im Agenten registrieren, idempotent und fremd-schonend.
//!
//! Das ist der Unterschied, der den Hook-Ansatz überhaupt trägt: Damit *jeder*
//! Agent-Hook dasselbe `minds hook` aufruft, muss Minds sich in jedem Agenten
//! eintragen. Fünf Agents, fünf Formate — die Fleißarbeit, die schon die Vision
//! als schwerste Last benennt. Dieses Modul kapselt sie an *einer* Stelle.
//!
//! # Zwei eiserne Regeln
//!
//! 1. **Idempotent.** Ein zweites `minds enable` ändert nichts. Erkennbar an
//!    unserer Marke (`minds hook` in der Kommandozeile, ein Block zwischen
//!    [`MARK_BEGIN`]/[`MARK_END`] in Shell-Dateien).
//! 2. **Fremdes bleibt.** Eine schon vorhandene Konfiguration in denselben
//!    Dateien — die eigenen Hooks des Nutzers, andere Tools — wird nie
//!    überschrieben. Wir fügen hinzu, wir ersetzen nicht.
//!
//! # Wohin was geschrieben wird
//!
//! | Agent       | Datei                                            |
//! |-------------|--------------------------------------------------|
//! | Claude Code | `.claude/settings.json`                          |
//! | Codex       | `.codex/hooks.json` + `codex_hooks` in `config.toml` |
//! | Cursor      | `.cursor/hooks.json`                             |
//! | Gemini      | `.gemini/settings.json`                          |
//! | OpenCode    | `.opencode/plugin/minds.ts`                      |
//! | Git         | `post-commit`, `prepare-commit-msg`, `pre-push` im **effektiven** Hook-Verzeichnis |
//!
//! Die Agent-Dateien liegen projekt-lokal, relativ zur Repo-Wurzel — der Kontext
//! gehört zum Repo, nicht zum Benutzerkonto. Die Git-Hooks gehen dorthin, wo Git
//! sie tatsächlich ausführt: `<git-dir>/hooks`, sofern `core.hooksPath` nichts
//! anderes sagt. Das kann auch außerhalb des Repos liegen — siehe
//! [`effective_hooks_dir`].
//!
//! # Ehrlich zu den Formaten
//!
//! Die JSON-Struktur der Claude-Code-Hooks ist belastbar. Für Codex, Cursor,
//! Gemini und OpenCode folgt dieses Modul der dokumentierten Form zum Zeitpunkt
//! des Umbaus (ADR-0003); ändert ein Agent sein Format, ist genau *diese* Stelle
//! nachzuziehen — nicht der Rest des Systems. Deshalb steht das Format-Wissen je
//! Agent gebündelt und benannt beisammen und nirgends sonst.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use minds_store::{Backend, StoreConfig};
use serde_json::{Map, Value, json};

use crate::config;

/// Verstecktes Flag, mit dem sich `minds enable` selbst als Hintergrund-Import
/// aufruft. Nicht in USAGE — der Nutzer soll den Backfill nicht als Kommando
/// kennen müssen.
pub(crate) const BACKGROUND_IMPORT_FLAG: &str = "--__background-import";

/// Anfang unseres Blocks in Shell-Hooks — die Marke für idempotentes Ersetzen.
/// `fsck` erkennt an derselben Marke, ob ein Hook von uns stammt.
pub(crate) const MARK_BEGIN: &str = "# >>> minds >>>";
/// Ende unseres Blocks.
pub(crate) const MARK_END: &str = "# <<< minds <<<";

/// Was mit einer Datei geschah — für den Bericht an den Nutzer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Created,
    Updated,
    Unchanged,
}

impl Change {
    fn word(self) -> &'static str {
        match self {
            Change::Created => "angelegt",
            Change::Updated => "ergänzt",
            Change::Unchanged => "unverändert",
        }
    }
}

/// Führt `minds enable` aus: Hooks registrieren **und** die Store-Config
/// schreiben — das eine Setup-Kommando, das ein Repo Minds-fähig macht. Fehler
/// melden über den Rückgabewert; anders als `minds hook` ist das hier ein
/// bewusst aufgerufenes Setup, kein heißer Pfad.
pub fn run(
    agent: Option<&str>,
    store: &StoreConfig,
    child_remote: Option<&str>,
    verbose: bool,
    recall: bool,
) -> ExitCode {
    let paths = match locate() {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("minds enable: {err}");
            return ExitCode::FAILURE;
        }
    };

    let which = agent.unwrap_or("all");
    let agents: &[Which] = match which {
        "claude-code" => &[Which::ClaudeCode],
        "codex" => &[Which::Codex],
        "cursor" => &[Which::Cursor],
        "gemini" => &[Which::Gemini],
        "opencode" => &[Which::OpenCode],
        "all" => Which::ALL,
        other => {
            eprintln!(
                "minds enable: unbekannter Agent {other:?}\n\
                 bekannt: claude-code, codex, cursor, gemini, opencode, all"
            );
            return ExitCode::FAILURE;
        }
    };

    match enable_agents(&paths, agents, store, child_remote, verbose, recall) {
        // Im Regelfall still — den Nutzer interessiert das Setup nicht. `-v`
        // zeigt jeden Schritt; nur echte Fehler kommen immer durch.
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds enable: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Die Agents, die `enable` kennt.
#[derive(Debug, Clone, Copy)]
enum Which {
    ClaudeCode,
    Codex,
    Cursor,
    Gemini,
    OpenCode,
}

impl Which {
    const ALL: &'static [Which] = &[
        Which::ClaudeCode,
        Which::Codex,
        Which::Cursor,
        Which::Gemini,
        Which::OpenCode,
    ];
}

/// Registriert die gewählten Agents, immer die Git-Hooks (ein Checkpoint soll
/// entstehen, wenn committet wird — unabhängig davon, welcher Agent lief) und
/// schreibt die Store-Config.
fn enable_agents(
    paths: &RepoPaths,
    agents: &[Which],
    store: &StoreConfig,
    child_remote: Option<&str>,
    verbose: bool,
    recall: bool,
) -> std::io::Result<()> {
    // Zuerst, vor jedem Schreibzugriff: Gibt es überhaupt einen Ort, aus dem
    // Git Hooks ausführt, und lässt sich dort schreiben? Wenn nicht, soll der
    // Nutzer nichts halb Eingerichtetes zurückbehalten — ein Repo mit
    // Agent-Konfiguration, aber ohne Hooks und ohne Store-Config journaliert,
    // checkt aber nie ein.
    let hooks_dir = paths.hooks.require()?.to_path_buf();

    // Der Hinweis gehört vor die erste Änderung am Dateisystem — auch vor das
    // Anlegen des Verzeichnisses. Wer abbricht, soll wissen, wohin es gegangen
    // wäre, und nicht ein leeres Verzeichnis an fremder Stelle zurücklassen.
    if let Some(note) = moved_hooks_note(&paths.root, &paths.git_dir, &hooks_dir) {
        println!("{note}");
    }

    // Die Hooks selbst einmal ansehen, bevor irgendetwas entsteht — auch vor
    // dem Anlegen des Verzeichnisses: Liegt dort ein Symlink oder eine Datei,
    // die keine ist, scheiterte der Schreibvorgang sonst mitten in der Reihe,
    // mit Agent-Konfiguration und erstem Hook auf der Platte, aber ohne
    // Store-Config. Der Agent journalierte dann, und nichts checkte je ein.
    // (`read_existing_hook` verträgt ein fehlendes Elternverzeichnis.)
    for name in hook_names() {
        read_existing_hook(&hooks_dir.join(name))?;
    }
    ensure_writable(&hooks_dir)?;

    // Vor der ersten Datei: den Ort dieses Binaries festhalten, den die
    // Hook-Rümpfe auflösen (#25). Ohne den Eintrag suchen die Hooks im PATH —
    // der Stand vor #25, der in GUI-Clients still ausfällt. Deshalb ist ein
    // Scheitern von `current_exe` hier kein stiller Fall, sondern ein Hinweis.
    match config::record_binary(&paths.root)? {
        Some(_) => vln(verbose, "  .git/config: minds.binary gesetzt"),
        None => println!(
            "Hinweis: der Ort dieses Binaries ließ sich nicht ermitteln — \
             die Hooks suchen minds über den PATH"
        ),
    }

    for &agent in agents {
        let change = match agent {
            Which::ClaudeCode => claude_style(&paths.root, ".claude/settings.json", "claude-code")?,
            Which::Codex => enable_codex(&paths.root)?,
            Which::Cursor => claude_style(&paths.root, ".cursor/hooks.json", "cursor")?,
            Which::Gemini => claude_style(&paths.root, ".gemini/settings.json", "gemini")?,
            Which::OpenCode => enable_opencode(&paths.root)?,
        };
        report(verbose, agent_label(agent), change);
    }

    // Opt-in Kontext-Rückführung: ein SessionStart-Hook, der `minds brief --hook`
    // ausgibt und dessen additionalContext der neuen Session vorangeht. Nur für
    // Claude Code (der Envelope-Vertrag ist agent-spezifisch).
    if recall && agents.iter().any(|a| matches!(a, Which::ClaudeCode)) {
        let change = enable_recall_hook(&paths.root)?;
        report(verbose, ".claude/settings.json (recall)", change);
    }

    for &(name, body) in commit_hooks() {
        let change = enable_git_hook(&hooks_dir, name, body)?;
        report(
            verbose,
            &label_for(&paths.root, &hooks_dir.join(name)),
            change,
        );
    }

    // Der Kontext soll mit dem Code reisen — aber *woher* er reist, hängt am
    // Backend. In-Repo liegt er im selben Repo; beim Child-Repo in einem
    // separaten, das erst angelegt (oder geklont) werden muss.
    configure_sync(paths, &hooks_dir, store, child_remote, verbose)?;

    // Die Store-Config gehört zum Setup: Ohne sie wüssten checkpoint/show/why
    // nicht, wo der Kontext liegt. `git config` setzt idempotent.
    config::write(&paths.root, store)?;
    vln(verbose, "  .git/config: minds.backend/contextRef gesetzt");

    // Der Backfill läuft losgelöst im Hintergrund: Wer Minds spät einrichtet,
    // soll rückwirkend Kontext bekommen, ohne auf das Lesen (womöglich großer)
    // Transkripte zu warten. Ausgabe und Fehler landen im Log, nicht im
    // Terminal des Nutzers.
    spawn_background_import(&paths.git_dir);
    vln(
        verbose,
        "  Backfill läuft im Hintergrund → .git/minds/import.log",
    );

    Ok(())
}

/// Gibt `line` nur bei `-v` aus. So bleibt `minds enable` im Regelfall still.
fn vln(verbose: bool, line: &str) {
    if verbose {
        println!("{line}");
    }
}

/// Startet den Backfill als **losgelösten** Prozess: dasselbe Binary, mit dem
/// versteckten Flag, stdout/stderr in eine Log-Datei, kein `wait`.
///
/// Best effort: Lässt sich der Prozess nicht starten (kein `current_exe`, kein
/// Log), bleibt das Setup trotzdem erfolgreich — der Backfill ist eine
/// Zugabe, kein Kernschritt. Er kann jederzeit durch ein erneutes `minds
/// enable` angestoßen werden.
fn spawn_background_import(git_dir: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };

    let log_dir = git_dir.join("minds");
    let _ = fs::create_dir_all(&log_dir);
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("import.log"));

    let mut cmd = Command::new(exe);
    cmd.arg("enable")
        .arg(BACKGROUND_IMPORT_FLAG)
        .stdin(Stdio::null());

    match log {
        Ok(file) => {
            let err = file.try_clone().ok();
            cmd.stdout(Stdio::from(file));
            if let Some(err) = err {
                cmd.stderr(Stdio::from(err));
            }
        }
        // Ohne Log lieber still als lärmend im Terminal.
        Err(_) => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }

    // Absichtlich kein `wait`: Der Prozess überlebt `minds enable` und wird von
    // init adoptiert.
    let _ = cmd.spawn();
}

/// Die Fetch-Refspecs, die `git fetch` den Minds-Namensraum mitziehen lassen.
///
/// Zwei Refs, zwei **verschiedene** Regeln — und der Unterschied ist der Punkt:
///
/// - **Kontext** wird direkt geholt. Er ist eine append-only Orphan-Kette; wer
///   fetcht, will den neuesten Stand sehen.
/// - **Reviews** landen im **Tracking-Namensraum** und überschreiben den lokalen
///   Log nie. Ein Verdict, das hier entstand und noch nicht gepusht ist, darf
///   ein `git fetch` nicht wegräumen. Zusammengeführt wird es von `minds sync`
///   — durch Vereinigung, konfliktfrei (siehe `ReviewStore::merge_from`).
///
/// Das ist derselbe Gedanke, den Git für Branches selbst anwendet: fremdes
/// kommt nach `refs/remotes/…`, gemergt wird bewusst.
const FETCH_REFSPECS: [&str; 3] = [
    // Die Nutzlast: ein Ref je Session. Content-adressiert, also entsteht jeder
    // dieser Refs genau einmal und ändert sich nie — ein Fetch kann hier nichts
    // überschreiben, was jemand anders meinte.
    "+refs/minds/store/*:refs/minds/store/*",
    "+refs/minds/context:refs/minds/context",
    "+refs/minds/reviews:refs/minds/remotes/origin/reviews",
];

/// Fügt die **additiven** Fetch-Refspecs an `origin` an. Anders als beim Push
/// ist das gefahrlos: Fetch-Refspecs sind eine Menge, kein Ersatz.
///
/// Nur wenn `origin` existiert; ein Repo ohne Remote bekommt sie beim nächsten
/// `minds enable` nach `git remote add`. Idempotent: Was schon dasteht, wird
/// nicht doppelt eingetragen. Gibt die neu gesetzten Refspecs zurück.
fn ensure_fetch_refspecs(root: &Path) -> std::io::Result<Vec<&'static str>> {
    if !git_output(root, &["remote"])?
        .lines()
        .any(|r| r.trim() == "origin")
    {
        return Ok(Vec::new());
    }

    let existing = git_output(root, &["config", "--get-all", "remote.origin.fetch"])?;
    let mut added = Vec::new();
    for spec in FETCH_REFSPECS {
        if existing.lines().any(|line| line.trim() == spec) {
            continue;
        }
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", "--add", "remote.origin.fetch", spec])
            .status()?;
        if status.success() {
            added.push(spec);
        }
    }
    Ok(added)
}

/// Liest die stdout eines `git`-Aufrufs. Ein Nicht-Null-Exit (z. B. ein
/// fehlender Config-Key) ist kein Fehler — dann ist die Ausgabe eben leer.
fn git_output(root: &Path, args: &[&str]) -> std::io::Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Der Wert eines Config-Schlüssels — `None`, wenn er **nicht gesetzt** ist.
///
/// Der Unterschied zwischen „nicht gesetzt" und „gesetzt, aber leer" lässt sich
/// an der Ausgabe allein nicht ablesen: Beides ist eine leere Zeile. Erst der
/// Exit-Status trennt sie — `git config --get` endet mit 1, wenn der Schlüssel
/// fehlt. Für `core.hooksPath` ist das keine Feinheit, sondern der ganze
/// Unterschied: leer heißt, dass Git **gar keine** Hooks ausführt (siehe
/// [`effective_hooks_dir`]). Deshalb hier ein eigener Aufruf statt
/// [`git_output`], das den Status wegwirft.
fn git_config_value(root: &Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Nur den Zeilenumbruch von `git config` abschneiden, nicht den Wert
    // trimmen: `core.hooksPath = "  "` ist ein zulässiger Verzeichnisname, und
    // wer ihn gesetzt hat, soll nicht das Verhalten von „leer" bekommen.
    let value = String::from_utf8_lossy(&out.stdout);
    Some(value.trim_end_matches(['\n', '\r']).to_owned())
}

// ---------------------------------------------------------------------------
// Sync des Kontext-Refs — je Backend anders
// ---------------------------------------------------------------------------

/// Richtet den Push/Fetch des Kontext-Refs ein — für In-Repo im selben
/// Repository, für das Child-Repo im separaten, das dafür erst existieren muss.
fn configure_sync(
    paths: &RepoPaths,
    hooks_dir: &Path,
    store: &StoreConfig,
    child_remote: Option<&str>,
    verbose: bool,
) -> std::io::Result<()> {
    // Der Hook ist für beide Backends derselbe; die Unterscheidung trifft
    // `minds sync` anhand der Store-Config.
    let (name, body) = push_hook();
    let pre_push = enable_git_hook(hooks_dir, name, body)?;
    report(
        verbose,
        &label_for(&paths.root, &hooks_dir.join(name)),
        pre_push,
    );

    if let Backend::ChildRepo { path } = store.backend() {
        let child = resolve_against(&paths.root, path);
        match ensure_child_repo(&paths.root, &child, child_remote) {
            Ok(change) => report(verbose, &format!("Child-Repo {}", child.display()), change),
            // Ein Fehler beim Child-Repo kommt immer durch — ohne das Repo
            // kann checkpoint nicht speichern.
            Err(err) => eprintln!("  Child-Repo {}: {err}", child.display()),
        }
    }

    for spec in ensure_fetch_refspecs(&paths.root)? {
        vln(
            verbose,
            &format!("  .git/config: Fetch-Refspec {spec} gesetzt"),
        );
    }
    Ok(())
}

/// Löst einen (womöglich relativen) Child-Pfad gegen die Repo-Wurzel auf — genau
/// wie `minds-store` beim Öffnen, damit `enable` und `checkpoint` denselben Ort
/// meinen.
fn resolve_against(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Legt das Child-Repo an oder klont es — oder lässt es, wenn es schon existiert.
///
/// - Mit `--child-remote`: `git clone --bare <url>`; scheitert das (unerreichbar,
///   noch leer angelegt), wird ein bares Repo initialisiert und `origin` gesetzt,
///   damit der spätere Push trotzdem ein Ziel hat.
/// - Ohne Remote: ein bares Repo, rein lokal.
///
/// In beiden Fällen bekommt das Child-Repo eine Committer-Identität (vom
/// Code-Repo geerbt, sonst ein Default) — ohne die kann der Store dort keinen
/// Commit schreiben.
fn ensure_child_repo(root: &Path, child: &Path, remote: Option<&str>) -> std::io::Result<Change> {
    if is_git_repo(child) {
        if let Some(url) = remote {
            ensure_child_origin(child, url)?;
        }
        return Ok(Change::Unchanged);
    }

    if let Some(parent) = child.parent() {
        fs::create_dir_all(parent)?;
    }

    match remote {
        Some(url) if git_ok(&["clone", "--bare", "--quiet", url, &child.to_string_lossy()]) => {}
        Some(url) => {
            git_init_bare(child)?;
            let _ = run_git(child, &["remote", "add", "origin", url]);
        }
        None => git_init_bare(child)?,
    }

    set_child_identity(root, child);
    Ok(Change::Created)
}

/// Ist `path` ein Git-Repository (bare oder nicht)?
fn is_git_repo(path: &Path) -> bool {
    path.exists()
        && Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--git-dir"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

fn git_init_bare(child: &Path) -> std::io::Result<()> {
    let status = Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(child)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("`git init --bare` schlug fehl"))
    }
}

/// Sorgt dafür, dass `origin` des Child-Repos auf `url` zeigt — hinzufügen oder
/// umbiegen, idempotent.
fn ensure_child_origin(child: &Path, url: &str) -> std::io::Result<()> {
    let current = git_output(child, &["remote", "get-url", "origin"]).unwrap_or_default();
    let current = current.trim();
    if current == url {
        return Ok(());
    }
    if current.is_empty() {
        let _ = run_git(child, &["remote", "add", "origin", url]);
    } else {
        let _ = run_git(child, &["remote", "set-url", "origin", url]);
    }
    Ok(())
}

/// Vererbt die Committer-Identität des Code-Repos an das Child-Repo; fehlt sie,
/// ein neutraler Default. Best effort — schlägt das fehl, meldet der Store das
/// später deutlich.
fn set_child_identity(root: &Path, child: &Path) {
    let name = git_output(root, &["config", "user.name"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "minds".to_string());
    let email = git_output(root, &["config", "user.email"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "minds@localhost".to_string());
    let _ = run_git(child, &["config", "user.name", &name]);
    let _ = run_git(child, &["config", "user.email", &email]);
}

/// Führt `git -C <dir> <args>` aus und meldet nur Erfolg/Misserfolg.
fn run_git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Führt `git <args>` (ohne `-C`) aus — für `clone`, das sein Zielverzeichnis
/// selbst anlegt.
fn git_ok(args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn agent_label(agent: Which) -> &'static str {
    match agent {
        Which::ClaudeCode => ".claude/settings.json",
        Which::Codex => ".codex/hooks.json",
        Which::Cursor => ".cursor/hooks.json",
        Which::Gemini => ".gemini/settings.json",
        Which::OpenCode => ".opencode/plugin/minds.ts",
    }
}

fn report(verbose: bool, label: &str, change: Change) {
    vln(verbose, &format!("  {label}: {}", change.word()));
}

/// Der Hinweis auf ein verschobenes Hook-Verzeichnis — `None`, wenn die Hooks
/// im üblichen `<git-dir>/hooks` liegen und es nichts zu sagen gibt.
///
/// Zwei Fälle, zwei Nachrichten, weil die Folgen verschieden sind: Liegt das
/// Verzeichnis **im Repo** (husky, lefthook), teilt es das Schicksal der
/// Arbeitskopie — je nachdem, ob es eingecheckt oder ignoriert ist, reist unser
/// Block zu den Kollegen oder verschwindet beim nächsten Aufräumen. Liegt es
/// **außerhalb** (global gesetztes `core.hooksPath`, `init.templateDir`),
/// schaltet ein `enable` in *einem* Repo die Erfassung für **alle** Repos des
/// Nutzers ein. Das ist die Nachricht, die niemand aus einem Pfad allein liest.
///
/// Bewusst offen gelassen: ob das Verzeichnis versioniert ist. `husky` ≥ 9 setzt
/// `core.hooksPath` auf `.husky/_`, und das ist per `.gitignore` ausgenommen und
/// wird bei jedem `husky install` neu erzeugt — unser Block wäre dort nach dem
/// nächsten `npm install` weg. Diese Unterscheidung braucht `git check-ignore`
/// und einen eigenen Satz; sie steht als Folgearbeit im Issue-Tracker, und
/// solange behauptet der Hinweis nichts, was er nicht weiß.
fn moved_hooks_note(root: &Path, git_dir: &Path, hooks_dir: &Path) -> Option<String> {
    if same_location(hooks_dir, &git_dir.join("hooks")) {
        return None;
    }
    let resolved = canonical_prefix(hooks_dir);
    let inside = resolved.starts_with(canonical_prefix(root));

    // Normalerweise steht der Pfad da, den der Nutzer gesetzt hat — der
    // aufgelöste wäre nur Lärm (auf macOS wird aus `/home/anna` schnell
    // `/System/Volumes/Data/home/anna`). Nur wenn der Pfad *im Repo* aussieht,
    // aber woandershin auflöst, muss das Ziel dastehen: Sonst stünde „zeigt aus
    // dem Repo heraus" neben einem harmlosen `.husky`, und der Satz widerspräche
    // dem Pfad, den er erklärt.
    let misleading = !inside && hooks_dir.starts_with(root);
    let where_ = label_for(root, if misleading { &resolved } else { hooks_dir });

    Some(if inside {
        format!("Hinweis: core.hooksPath ist gesetzt — die Git-Hooks gehen nach „{where_}“")
    } else {
        format!(
            "Hinweis: core.hooksPath zeigt aus dem Repo heraus („{where_}“) — \
                 die Hooks gelten damit für alle deine Repositories"
        )
    })
}

/// Ein Pfad, wie er im Bericht erscheint: relativ zur Repo-Wurzel, damit die
/// Ausgabe kurz bleibt und trotzdem den *tatsächlichen* Ort nennt statt eines
/// fest verdrahteten `.git/hooks/…`.
fn label_for(root: &Path, path: &Path) -> String {
    display_path(path.strip_prefix(root).unwrap_or(path))
}

/// Ein Pfad für die Ausgabe, mit entschärften Steuerzeichen.
///
/// `git config` speichert beliebige Bytes. Ein `core.hooksPath` mit
/// ANSI-Sequenzen (`\e[2K\e[A`) könnte sonst Zeilen des Berichts überschreiben
/// — ausgerechnet den Hinweis, der bei einem Hook-Verzeichnis außerhalb des
/// Repos die einzige Warnung ist.
///
/// Entschärft wird über [`crate::text::sanitize`] — dieselbe Stelle, durch die
/// auch die Log-Zeilen gehen. Zwei Fassungen davon wären eine zu viel: Die
/// Zeichenklassen sind subtil (Bidi-Marken sind `Cf`, also **nicht**
/// `is_control`, drehen die Zeile im Terminal aber trotzdem um), und die eine,
/// die man beim Nachbessern vergisst, ist die Lücke.
pub(crate) fn display_path(path: &Path) -> String {
    crate::text::sanitize_path(&path.display().to_string())
}

/// Entfernt `.` und löst `..` **textuell** auf.
///
/// Nur für Entscheidungen, nie für einen Schreibpfad: Führt eine Komponente vor
/// dem `..` über einen Symlink, weicht das Ergebnis vom Dateisystem ab — und
/// damit von dem, was Git tut. Für die Frage „ist das die Arbeitskopie-Wurzel?"
/// braucht es die Antwort aber auch dann, wenn Teile des Pfades noch gar nicht
/// existieren und [`fs::canonicalize`] deshalb schweigt.
fn lexically_normalized(path: PathBuf) -> PathBuf {
    use std::path::Component;

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            // Ein `..` hebt die vorige Komponente auf — außer am Anfang eines
            // relativen Pfades, wo es nichts aufzuheben gibt.
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Meinen zwei Pfade denselben Ort?
///
/// Ein reiner Textvergleich reicht nicht: Auf macOS ist `/tmp` ein Symlink auf
/// `/private/tmp`, unter Linux tun Bind-Mounts und ein verlinktes `$HOME`
/// dasselbe. Wer `core.hooksPath` über einen solchen Alias auf die Repo-Wurzel
/// setzt, käme sonst an der Prüfung vorbei, die genau das verhindern soll.
///
/// [`fs::canonicalize`] antwortet nur für existierende Pfade; für alles andere
/// bleibt der Textvergleich das Beste, was wir haben.
pub(crate) fn same_location(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (fs::canonicalize(a), fs::canonicalize(b)),
        (Ok(left), Ok(right)) if left == right
    )
}

/// Kanonisiert so viel vom Pfad, wie es auf der Platte schon gibt, und hängt den
/// Rest unverändert an.
///
/// Nötig, weil das Hook-Verzeichnis oft noch nicht existiert — `enable` legt es
/// erst an —, die Frage „liegt es innerhalb des Repos?" aber jetzt schon
/// beantwortet werden muss, und zwar über Symlinks hinweg.
fn canonical_prefix(path: &Path) -> PathBuf {
    let mut trailing = Vec::new();
    let mut head = path;
    loop {
        if let Ok(real) = fs::canonicalize(head) {
            let mut out = real;
            out.extend(trailing.iter().rev());
            return out;
        }
        match (head.parent(), head.file_name()) {
            (Some(parent), Some(name)) => {
                trailing.push(name.to_owned());
                head = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Legt das Hook-Verzeichnis an und prüft, dass sich dort schreiben lässt —
/// **bevor** `enable` irgendetwas anderes anfasst.
///
/// Ohne diese Probe scheitert erst der Hook-Schreibvorgang, und zwar nachdem die
/// Agent-Konfigurationen schon geschrieben sind und bevor die Store-Config
/// geschrieben wird: Der Agent journaliert dann, aber kein Commit checkt je ein.
/// Ein Verzeichnis ohne Schreibrecht oder ein `core.hooksPath`, das auf eine
/// Datei zeigt, sind keine exotischen Fälle — sie liegen einen Tippfehler
/// entfernt.
///
/// # Warum hier kein Symlink geprüft wird
///
/// Naheliegend wäre, ein Hook-**Verzeichnis**, das über einen eingecheckten
/// Symlink woandershin zeigt, abzulehnen. Ein Versuch dazu stand hier und ist
/// wieder entfernt worden: Pfad-für-Pfad-Prüfungen lassen sich zu leicht
/// umgehen — ein nachgestellter Schrägstrich genügt, weil POSIX `lstat("link/")`
/// dem Link folgen lässt, und ein Pfad-Alias führt an der Zuständigkeitsgrenze
/// vorbei. Umgekehrt lehnte die Prüfung legitime Setups ab (ein symlinktes
/// `.git` oder ein geteiltes `.git/hooks`, beides von Git unterstützt).
///
/// Was gegen den eingecheckten Symlink trägt, sitzt eine Ebene tiefer und ist
/// nicht von Schreibweisen abhängig: [`read_existing_hook`] und [`write_hook`]
/// arbeiten am Blatt und schreiben nie durch einen Link hindurch. Offen bleibt,
/// dass ein umgelenktes Verzeichnis unsere Dateien woanders entstehen lässt —
/// das ist eine Frage des *Ortes*, nicht des Links, und wird dort beantwortet,
/// wo `enable` künftig vor dem Schreiben außerhalb des Repos zurückfragt.
fn ensure_writable(hooks_dir: &Path) -> std::io::Result<()> {
    let explain = |err: std::io::Error| {
        std::io::Error::new(
            err.kind(),
            format!(
                "das Hook-Verzeichnis {} lässt sich nicht anlegen oder beschreiben \
                 (core.hooksPath): {err}",
                display_path(hooks_dir)
            ),
        )
    };

    // Das Verzeichnis bleibt stehen, wenn ein späterer Schritt scheitert. Es
    // wieder abzuräumen hieße zu wissen, ob es vorher schon da war — und ein
    // leeres Hook-Verzeichnis schadet niemandem.
    fs::create_dir_all(hooks_dir).map_err(explain)?;
    let probe = temp_name(hooks_dir, "schreibprobe");
    create_new_file(&probe).map_err(explain)?;
    let _ = fs::remove_file(&probe);
    Ok(())
}

/// Ein Name für eine kurzlebige Nachbardatei, kollisionsarm über PID und Zeit.
///
/// Ausdrücklich **kein Zufall** und keine Sicherheitszusage — die trägt allein
/// [`create_new_file`]. PID und Nanosekunden verhindern, dass ein Rest aus einem
/// abgestürzten Lauf den nächsten blockiert; mehr sollen sie nicht.
fn temp_name(dir: &Path, purpose: &str) -> PathBuf {
    // Der volle Nanosekunden-Wert, nicht nur der Bruchteil: Nach einer
    // wiederverwendeten PID kollidierte der Name sonst mit einem Rest aus einem
    // abgestürzten Lauf, und `create_new` ließe den neuen Lauf scheitern.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    dir.join(format!(".minds-{purpose}-{}-{nanos}", std::process::id()))
}

/// Legt eine Datei an, die es vorher **nicht** gab.
///
/// `create_new` bedeutet `O_CREAT | O_EXCL`; das scheitert an einem bestehenden
/// Namen *einschließlich* eines Symlinks — auch eines, dessen Ziel gar nicht
/// existiert. Ein `fs::write` folgte dem Link stattdessen und schriebe in die
/// fremde Datei. Fail-closed ohne `O_NOFOLLOW` und damit ohne neue Dependency.
fn create_new_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

// ---------------------------------------------------------------------------
// Claude-Style: eine hooks-Map in JSON (Claude, Cursor, Gemini)
// ---------------------------------------------------------------------------

/// Die Lebenszyklus-Events und ob sie einen Tool-Matcher tragen. Claude Code
/// definiert diese Menge; die anderen JSON-Agents folgen derselben Form.
const HOOK_EVENTS: &[(&str, bool)] = &[
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("UserPromptSubmit", false),
    ("Stop", false),
    ("SubagentStop", false),
    ("SessionStart", false),
    ("SessionEnd", false),
];

/// Trägt in eine Claude-artige `hooks`-Map je Event einen Command-Hook ein, der
/// `minds hook --agent <agent>` aufruft — idempotent und ohne Fremdes zu
/// verwerfen.
fn claude_style(root: &Path, rel: &str, agent: &str) -> std::io::Result<Change> {
    let path = root.join(rel);
    let mut root_value = read_json(&path)?;
    let existed = path.exists();

    let obj = as_object(&mut root_value);
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = match hooks.as_object_mut() {
        Some(map) => map,
        // Eine fremde, nicht-Objekt-`hooks` würden wir sonst zertrümmern.
        None => return Ok(Change::Unchanged),
    };

    let mut changed = false;
    for &(event, matcher) in HOOK_EVENTS {
        let command = command_for(agent, event, agent != "claude-code");
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        if array_has_minds_hook(groups) {
            continue;
        }
        groups.push(hook_group(matcher, &command));
        changed = true;
    }

    if !changed && existed {
        return Ok(Change::Unchanged);
    }
    write_json(&path, &root_value)?;
    Ok(if existed {
        Change::Updated
    } else {
        Change::Created
    })
}

/// Registriert einen zusätzlichen SessionStart-Hook, der `minds brief --hook`
/// ausgibt — dessen `additionalContext` stellt Claude Code der neuen Session
/// voran. So lernt jede Session aus den vorigen (Vision-Problem #3). Idempotent,
/// erkennbar an `minds brief` in der Kommandozeile; fremde SessionStart-Hooks
/// (auch der Capture-Hook `minds hook`) bleiben unangetastet.
fn enable_recall_hook(root: &Path) -> std::io::Result<Change> {
    let path = root.join(".claude/settings.json");
    let existed = path.exists();
    let mut root_value = read_json(&path)?;

    let obj = as_object(&mut root_value);
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        return Ok(Change::Unchanged);
    };
    let groups = hooks
        .entry("SessionStart")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(groups) = groups.as_array_mut() else {
        return Ok(Change::Unchanged);
    };

    let already = groups
        .iter()
        .filter_map(|g| g.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(|h| h.get("command").and_then(Value::as_str))
        .any(|cmd| cmd.contains("minds brief"));
    if already {
        return Ok(Change::Unchanged);
    }

    groups.push(json!({
        "hooks": [{ "type": "command", "command": "minds brief --hook 2>/dev/null || true" }]
    }));
    write_json(&path, &root_value)?;
    Ok(if existed {
        Change::Updated
    } else {
        Change::Created
    })
}

/// `{"type":"command","command":"minds hook …"}`, ggf. in eine Matcher-Gruppe
/// verpackt.
fn hook_group(matcher: bool, command: &str) -> Value {
    let entry = json!({ "type": "command", "command": command });
    if matcher {
        json!({ "matcher": "*", "hooks": [entry] })
    } else {
        json!({ "hooks": [entry] })
    }
}

/// `true`, wenn irgendeine Gruppe schon einen `minds hook`-Command trägt — die
/// Idempotenz-Prüfung. Bewusst über die Teilzeichenkette, damit auch eine
/// ältere, leicht anders geschriebene Registrierung als „schon da" gilt.
fn array_has_minds_hook(groups: &[Value]) -> bool {
    groups.iter().any(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|h| h.get("command").and_then(Value::as_str))
            .any(|cmd| cmd.contains("minds hook"))
    })
}

fn command_for(agent: &str, event: &str, with_event: bool) -> String {
    if with_event {
        // Agents, deren Payload den Eventnamen nicht mitschickt, bekommen ihn
        // über die Registrierung (siehe hook_event::parse, event_override).
        format!("minds hook --agent {agent} --event {event}")
    } else {
        format!("minds hook --agent {agent}")
    }
}

// ---------------------------------------------------------------------------
// Codex: hooks.json + codex_hooks in config.toml
// ---------------------------------------------------------------------------

/// Codex braucht zweierlei: die Hook-Registrierung *und* den Schalter
/// `codex_hooks = true`, ohne den Codex die `hooks.json` gar nicht liest.
fn enable_codex(root: &Path) -> std::io::Result<Change> {
    let hooks_change = claude_style(root, ".codex/hooks.json", "codex")?;
    let toml_change = ensure_codex_hooks_flag(&root.join(".codex/config.toml"))?;
    // Der interessantere der beiden Änderungsstände bestimmt den Bericht.
    Ok(match (hooks_change, toml_change) {
        (Change::Created, _) | (_, Change::Created) => Change::Created,
        (Change::Updated, _) | (_, Change::Updated) => Change::Updated,
        _ => Change::Unchanged,
    })
}

/// Setzt `codex_hooks = true` in `config.toml` — zeilenbasiert, ohne
/// TOML-Abhängigkeit.
///
/// Bewusst schlicht: Steht die Zeile schon (auf `true`), passiert nichts. Steht
/// sie auf `false` oder fehlt sie, wird sie gesetzt bzw. angehängt. Das deckt
/// den Alltag; eine `codex_hooks` tief in einer `[table]` verschachtelt käme in
/// echtem TOML vor, ist hier aber nicht der Fall (der Schalter ist top-level).
fn ensure_codex_hooks_flag(path: &Path) -> std::io::Result<Change> {
    let existed = path.exists();
    let current = if existed {
        fs::read_to_string(path)?
    } else {
        String::new()
    };

    let mut lines: Vec<String> = current.lines().map(str::to_owned).collect();
    let mut found = false;
    let mut changed = false;
    for line in &mut lines {
        if line.trim_start().starts_with("codex_hooks") {
            found = true;
            if line.trim() != "codex_hooks = true" {
                *line = "codex_hooks = true".to_string();
                changed = true;
            }
            break;
        }
    }
    if !found {
        lines.push("codex_hooks = true".to_string());
        changed = true;
    }

    if !changed && existed {
        return Ok(Change::Unchanged);
    }
    create_parent(path)?;
    let mut out = lines.join("\n");
    out.push('\n');
    fs::write(path, out)?;
    Ok(if existed {
        Change::Updated
    } else {
        Change::Created
    })
}

// ---------------------------------------------------------------------------
// OpenCode: ein TypeScript-Plugin
// ---------------------------------------------------------------------------

/// Das Plugin, das OpenCode-Events an `minds hook` reicht. Trägt unsere Marke,
/// damit `enable` es wiedererkennt.
const OPENCODE_PLUGIN: &str = r#"// >>> minds >>>
// Von `minds enable` erzeugt. Reicht OpenCode-Events an das lokale Journal.
import { spawnSync } from "node:child_process";

export const minds = async () => ({
  event: async ({ event }: { event: unknown }) => {
    spawnSync("minds", ["hook", "--agent", "opencode"], {
      input: JSON.stringify(event),
    });
  },
});
// <<< minds <<<
"#;

/// Schreibt das OpenCode-Plugin. Existiert bereits ein *fremdes* `minds.ts`
/// (ohne unsere Marke), bleibt es unangetastet — wir überschreiben nichts, was
/// wir nicht selbst geschrieben haben.
fn enable_opencode(root: &Path) -> std::io::Result<Change> {
    let path = root.join(".opencode/plugin/minds.ts");
    if path.exists() {
        let current = fs::read_to_string(&path)?;
        if current == OPENCODE_PLUGIN {
            return Ok(Change::Unchanged);
        }
        if !current.contains(MARK_BEGIN) {
            // Fremde Datei mit unserem Namen — nicht anrühren.
            return Ok(Change::Unchanged);
        }
    }
    create_parent(&path)?;
    let existed = path.exists();
    fs::write(&path, OPENCODE_PLUGIN)?;
    Ok(if existed {
        Change::Updated
    } else {
        Change::Created
    })
}

// ---------------------------------------------------------------------------
// Git-Hooks
// ---------------------------------------------------------------------------

/// Die Zeilen, mit denen jeder Hook-Rumpf beginnt: erst den bei `enable`
/// gemerkten Binary-Ort auflösen (`minds.binary`, siehe
/// [`crate::config::record_binary`], #25), dann — wenn dort keiner (mehr)
/// liegt — auf die PATH-Suche zurückfallen.
///
/// **Warum nicht das nackte `minds`:** GUI-Clients (VS Code, Fork, Tower) und
/// minimale CI-Shells starten Git ohne das Profil der Shell. `~/.local/bin`
/// fehlt dort im `PATH`, der Aufruf lief ins Leere, und `|| true` machte
/// daraus einen stillen Totalausfall — committen ging, erfasst wurde nichts.
///
/// **Warum kein absoluter Pfad im Hook-Text:** Seit #9 kann die Hook-Datei in
/// der Arbeitskopie liegen und ist dann **versioniert**. Ein Home-Pfad darin
/// würde eingecheckt, bräche auf jeder anderen Maschine und machte die Datei
/// bei jedem `enable` schmutzig. Der maschinenlokale Wert steht deshalb in der
/// maschinenlokalen `.git/config` — der Hook-Text bleibt überall derselbe,
/// und der `fsck`-Vergleich (exakter Rumpf-Vergleich) bleibt aussagekräftig.
///
/// **Der gemerkte Ort gewinnt gegen den `PATH`:** So kann eine veraltete
/// globale `minds` die Hooks nicht mehr beschatten — es läuft die Version,
/// deren `enable` die Hooks geschrieben hat. `[ -f … ] && [ -x … ]` fängt den
/// umgezogenen Binary ab (und ein Verzeichnis mit x-Bit, das `[ -x ]` allein
/// durchließe); dann greift der `PATH`, bis ein erneutes `minds enable` den
/// Eintrag erneuert (`minds fsck` weist darauf hin).
///
/// **`--local`, nicht der effektive Wert:** Der Ort ist per Entwurf repo- und
/// maschinenlokal. Ohne `--local` läse der Hook auch `~/.gitconfig`,
/// `GIT_CONFIG_*`-Umgebung und `git -c`-Parameter (Git vererbt sie in den
/// Hook-Prozess) — eine breitere Auflösungsfläche als dokumentiert, und
/// [`crate::config::record_binary`] wie `fsck` fragen nur die lokale Ebene.
/// Alle drei stellen so dieselbe Frage an dieselbe Quelle.
///
/// Jede Zeile ist für sich `set -e`-fest (`|| true` bzw. `|| MINDS_BIN=minds`):
/// Der Block kann in einer fremden Hook-Datei stehen, deren Kopf `set -e`
/// setzt. Und `"$MINDS_BIN"` ist überall gequotet — ein Pfad mit Leerzeichen
/// bleibt ein Wort.
macro_rules! hook_body {
    ($command:literal) => {
        concat!(
            "MINDS_BIN=$(git config --local --get minds.binary 2>/dev/null) || true\n",
            "[ -f \"$MINDS_BIN\" ] && [ -x \"$MINDS_BIN\" ] || MINDS_BIN=minds\n",
            $command
        )
    };
}

/// post-commit: der Checkpoint-Auslöser. `minds checkpoint` (M6) nimmt Journal +
/// Transkript, redigiert und legt die Session ab. Non-blocking — ein Rekorder
/// darf einen Commit nie scheitern lassen.
const POST_COMMIT_BODY: &str = hook_body!(
    "\"$MINDS_BIN\" checkpoint --commit \"$(git rev-parse HEAD)\" >/dev/null 2>&1 || true"
);

/// prepare-commit-msg: reserviert für den Trailer (M6). Heute ein sicherer
/// No-op — der Aufruf schlägt fehl, `|| true` fängt ihn, die Nachricht bleibt
/// unangetastet.
const PREPARE_MSG_BODY: &str =
    hook_body!("\"$MINDS_BIN\" prepare-commit-msg \"$1\" >/dev/null 2>&1 || true");

/// pre-push: `minds sync` schickt den Kontext beim `git push` mit — an dasselbe
/// Remote, an das gerade gepusht wird (`$1`).
///
/// Warum ein Hook und kein `remote.push`-Refspec: Sobald `remote.<name>.push`
/// gesetzt ist, pusht `git push` **nur noch** diese Refspecs — der Branch des
/// Nutzers bliebe liegen. Der Hook lässt den normalen Push unangetastet und
/// legt den Kontext daneben.
///
/// # Warum hier keine Git-Kommandos mehr stehen
///
/// Bis v0.2 rief dieser Hook selbst `git push` auf. Das kostete auf **jedem**
/// Push den vollen Verbindungsaufbau (gegen gitlab.com ~2,7 s gemessen), auch
/// wenn es gar nichts Neues gab — die Shell kann nicht wissen, was am Remote
/// schon steht. `minds sync` weiß es aus den Tracking-Refs, entscheidet ohne
/// Netz und schickt alle fälligen Refs in *einer* Verbindung. Die Logik gehört
/// ins Binary, wo sie testbar ist; im Hook bleibt der Aufruf.
///
/// `|| true` hält es non-blocking: Ein Sync-Fehler darf den Push des Nutzers nie
/// verhindern. Beide Backends benutzen denselben Body — welches Repo wohin
/// gepusht wird, entscheidet `minds sync` anhand der Store-Config.
///
/// # Warum stderr hier weggeht
///
/// Als einziger der drei Hooks schrieb dieser seine Fehler roh in den
/// Push-Output — „minds sync: …" mitten zwischen den Zeilen von `git push`, bei
/// jedem Push, für einen Vorgang, den der Nutzer gar nicht angestoßen hat. Seit
/// [`crate::hooklog`] geht der Wortlaut in eine Datei, auf die `minds fsck`
/// verweist; damit ist die Umleitung kein Verschweigen mehr, sondern die
/// Verlagerung an einen Ort, an dem sie auch morgen noch steht.
///
/// **stdout bleibt**: Was `minds sync` dort meldet, ist die Erfolgsmeldung, und
/// die gehört an den Push, zu dem sie gehört.
const PRE_PUSH_BODY: &str = hook_body!("\"$MINDS_BIN\" sync --remote \"$1\" 2>/dev/null || true");

/// **Die** Liste der Git-Hooks, die `minds` schreibt — samt ihrer Rümpfe.
///
/// Eine Quelle, aus der sich alles andere ableitet: `enable_agents` schreibt die
/// Commit-Hooks, `configure_sync` den letzten, und die Vorprüfung läuft über
/// alle Namen. Wer hier einen Hook ergänzt, bekommt die Vorprüfung geschenkt —
/// zählte man die Namen an zwei Stellen auf, fiele der neue Hook still aus ihr
/// heraus, und genau dann bräche die Zusage „nichts halb Eingerichtetes".
///
/// **`pre-push` steht zuletzt**; darauf verlässt sich [`push_hook`], und ein
/// Test hält es fest.
const ALL_HOOKS: [(&str, &str); 3] = [
    ("post-commit", POST_COMMIT_BODY),
    ("prepare-commit-msg", PREPARE_MSG_BODY),
    ("pre-push", PRE_PUSH_BODY),
];

/// Die Hooks, die [`enable_agents`] selbst schreibt.
fn commit_hooks() -> &'static [(&'static str, &'static str)] {
    &ALL_HOOKS[..ALL_HOOKS.len() - 1]
}

/// Der Hook, den [`configure_sync`] schreibt.
fn push_hook() -> (&'static str, &'static str) {
    ALL_HOOKS[ALL_HOOKS.len() - 1]
}

/// Alle Hook-Namen — für die Vorprüfung und für `fsck`.
pub(crate) fn hook_names() -> impl Iterator<Item = &'static str> {
    ALL_HOOKS.iter().map(|(name, _)| *name)
}

/// Der Rumpf, den `minds enable` für diesen Hook schreibt.
///
/// `fsck` braucht ihn, um „unser Block liegt da" von „unser Block liegt da,
/// aber in einer alten Fassung" zu unterscheiden. Ohne diesen Vergleich meldete
/// `fsck` einen Hook als installiert, dessen Rumpf aus einer Version stammt, die
/// den Fehler noch hatte — und der Nutzer erführe nie, dass ein `minds enable`
/// fällig ist.
pub(crate) fn expected_body(name: &str) -> Option<&'static str> {
    ALL_HOOKS
        .iter()
        .find(|(hook, _)| *hook == name)
        .map(|(_, body)| *body)
}

/// Wo unser Block in dieser Datei steht, samt Marken.
///
/// `MARK_END` wird **hinter** `MARK_BEGIN` gesucht, nicht global: Eine Datei mit
/// einem Schlussmarker vor dem Anfang (von Hand zusammengestückelt) ergäbe sonst
/// eine negative Spanne. `None`, wenn keine vollständige Klammer da ist — ein
/// angefangener Block ohne Ende ist keiner, den wir wiedererkennen.
///
/// Eine Quelle für beide Leser: [`block_body`] entnimmt hier den Rumpf,
/// [`replace_block`] ersetzt genau diese Spanne. Zwei Auffassungen davon, was
/// „unser Block" ist, hießen: `fsck` meldet etwas, das `enable` nicht repariert.
fn block_span(text: &str) -> Option<std::ops::Range<usize>> {
    let start = text.find(MARK_BEGIN)?;
    let end = text[start..].find(MARK_END)? + start + MARK_END.len();
    Some(start..end)
}

/// Was zwischen [`MARK_BEGIN`] und [`MARK_END`] steht, ohne die Marken.
///
/// Zeilenenden werden normalisiert — `\r\n` wird zu `\n`, an den Rändern fällt
/// beides weg. Sonst gälte eine Hook-Datei mit CRLF (Windows-Editor,
/// `core.autocrlf`, ein `.gitattributes` mit `eol=crlf` auf dem
/// `.husky`-Verzeichnis aus #9) **dauerhaft** als veraltet, auch direkt nach
/// `minds enable` — und ein Hinweis, den man nicht loswerden kann, wird
/// überlesen, mitsamt den echten daneben. Seit die Rümpfe mehrzeilig sind
/// (#25), reicht das Abschneiden an den Rändern dafür nicht mehr: CRLF steht
/// dann auch **zwischen** den Zeilen.
pub(crate) fn block_body(text: &str) -> Option<String> {
    let span = block_span(text)?;
    let inner = &text[span.start + MARK_BEGIN.len()..span.end - MARK_END.len()];
    Some(
        inner
            .trim_matches(|c| c == '\n' || c == '\r')
            .replace("\r\n", "\n"),
    )
}

/// Fügt einen markierten Block in einen Git-Hook ein oder aktualisiert ihn.
///
/// `hooks_dir` ist das **effektive** Hook-Verzeichnis (siehe
/// [`effective_hooks_dir`]), nicht blind `<git-dir>/hooks`.
///
/// Fremde Zeilen in derselben Datei (die eigenen Hooks des Nutzers) bleiben; nur
/// der Block zwischen [`MARK_BEGIN`] und [`MARK_END`] gehört uns und wird
/// ersetzt. Eine neue Datei bekommt eine `#!/bin/sh`-Zeile und `chmod +x`.
fn enable_git_hook(hooks_dir: &Path, name: &str, body: &str) -> std::io::Result<Change> {
    let path = hooks_dir.join(name);
    let current = read_existing_hook(&path)?;
    let existed = current.is_some();
    let current = current.unwrap_or_default();

    let block = format!("{MARK_BEGIN}\n{body}\n{MARK_END}");
    let next = replace_block(&current, &block);
    if next == current && existed {
        return Ok(Change::Unchanged);
    }

    create_parent(&path)?;
    write_hook(&path, &next)?;
    Ok(if existed {
        Change::Updated
    } else {
        Change::Created
    })
}

/// Größter Hook, den wir noch einlesen. Alles darüber ist keiner.
///
/// Seit das Zielverzeichnis am `core.hooksPath` hängt, kann es in der
/// Arbeitskopie liegen — und ist damit **versioniert und fremdbestückbar**.
/// Symlinks und Gerätedateien fängt [`read_existing_hook`] schon vorher ab;
/// diese Grenze gilt der schlichten **großen regulären Datei**, die jemand unter
/// dem Namen `post-commit` eincheckt. Ohne sie zöge ein `read_to_string` sie
/// vollständig in den Speicher. Ein echter Hook ist ein Shell-Skript.
pub(crate) const MAX_HOOK_BYTES: u64 = 1024 * 1024;

/// Liest einen vorhandenen Hook — oder `None`, wenn dort keiner liegt.
///
/// # Warum das nicht `fs::read_to_string` ist
///
/// Vor dieser Änderung war das Ziel immer `<git-dir>/hooks/…`, ein Verzeichnis,
/// in das ein Checkout nichts legen kann. Mit `core.hooksPath = .husky` liegt es
/// **in der Arbeitskopie** — ein gemergter PR kann dort also eine Datei
/// platzieren, und im Diff sieht man davon nur einen Moduswechsel.
///
/// Ist diese Datei ein **Symlink**, würde `read_to_string` das Ziel lesen,
/// `fs::write` durch den Link hindurchschreiben und `set_permissions` dessen
/// Rechte ändern — aus `~/.aws/credentials` mit `0600` würde eine Datei mit
/// `0755` und angehängtem minds-Block. Git *liest* Hooks nur; die Schreib- und
/// die chmod-Primitive entstehen erst hier. Deshalb fail-closed: Ein Hook, der
/// ein Symlink ist, ist nichts, was wir ergänzen.
///
/// **Was offen bleibt:** Zwischen diesem `symlink_metadata` und dem Lesen liegt
/// ein Zeitfenster, und ein **Hardlink** ist von einer regulären Datei nicht zu
/// unterscheiden. Beides setzt einen Angreifer mit Schreibrechten im
/// Hook-Verzeichnis unter derselben Kennung voraus — der könnte die Datei
/// ohnehin lesen und schreiben. Gegen den eingecheckten Symlink, der aus einem
/// PR kommt, trägt die Prüfung; mehr behauptet sie nicht.
pub(crate) fn read_existing_hook(path: &Path) -> std::io::Result<Option<String>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(with_path(path, err)),
    };

    if meta.file_type().is_symlink() {
        return Err(std::io::Error::other(format!(
            "{} ist ein Symlink — minds ergänzt keinen Hook, der woandershin zeigt. \
             Entferne den Link oder wähle ein anderes core.hooksPath",
            display_path(path)
        )));
    }
    if !meta.is_file() {
        return Err(std::io::Error::other(format!(
            "{} ist keine reguläre Datei",
            display_path(path)
        )));
    }
    if meta.len() > MAX_HOOK_BYTES {
        return Err(std::io::Error::other(format!(
            "{} ist {} Bytes groß — das ist kein Hook-Skript",
            display_path(path),
            meta.len()
        )));
    }

    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
            Err(std::io::Error::other(format!(
                "{} ist kein Text — minds ergänzt nur Shell-Hooks",
                display_path(path)
            )))
        }
        Err(err) => Err(with_path(path, err)),
    }
}

/// Schreibt den Hook **ohne** einem Symlink zu folgen: erst eine Nachbardatei,
/// dann [`fs::rename`] darüber.
///
/// `rename` ersetzt den *Namen* im Verzeichnis; selbst wenn zwischen Prüfung und
/// Schreiben jemand einen Symlink an diese Stelle legt, wird der Link ersetzt
/// statt durch ihn hindurchgeschrieben. Zusammen mit `create_new` (also
/// `O_CREAT | O_EXCL`) für die Nachbardatei braucht es dafür kein `O_NOFOLLOW`
/// und damit keine neue Dependency für das statische Binary.
///
/// **Was offen bleibt:** Wird das *Verzeichnis* zwischen Prüfung und Schreiben
/// gegen einen Symlink getauscht, landet auch dieses `rename` am neuen Ziel.
/// Lückenlos wäre das nur mit einem offenen Verzeichnis-Deskriptor und
/// `openat`/`renameat`, also mit `libc`. Wer das ausnutzen will, braucht
/// Schreibrechte im Hook-Verzeichnis unter derselben Kennung — und könnte den
/// Hook dann ohnehin selbst schreiben. Keine Vertrauensgrenze wird überschritten;
/// gegen den eingecheckten Symlink, um den es hier geht, trägt die Prüfung.
///
/// Die Rechte werden auf der Temp-Datei gesetzt, nicht über den Zielpfad: Ein
/// `chmod` auf einen Pfad folgte wieder dem Link.
fn write_hook(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;

    let dir = path.parent().unwrap_or(Path::new("."));
    let temp = temp_name(dir, "hook");

    // Erst anlegen, dann der Rest — damit das Aufräumen unten nur Dateien
    // trifft, die von uns stammen. Scheitert `create_new_file` (etwa an einem
    // Symlink, der schon so heißt), gehört uns dieser Name nicht, und wir
    // löschen ihn auch nicht.
    let mut file = create_new_file(&temp).map_err(|err| with_path(&temp, err))?;

    let result = (|| {
        file.write_all(content.as_bytes())
            .map_err(|err| with_path(&temp, err))?;
        // Die Rechte über das offene Handle, nicht über den Pfad: Ein `chmod`
        // auf einen Pfad folgte einem Link, der inzwischen dort liegen könnte.
        make_executable(&file).map_err(|err| with_path(&temp, err))?;
        fs::rename(&temp, path).map_err(|err| with_path(path, err))
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Hängt den Pfad an einen I/O-Fehler. „Permission denied (os error 13)" allein
/// lässt den Nutzer raten, welche Datei gemeint ist — und bei einem Ziel aus
/// `core.hooksPath` ist genau das die Frage.
fn with_path(path: &Path, err: std::io::Error) -> std::io::Error {
    std::io::Error::new(err.kind(), format!("{}: {err}", display_path(path)))
}

/// Ersetzt einen vorhandenen minds-Block durch `block` oder hängt ihn an. Eine
/// leere Datei bekommt zusätzlich den Shebang.
fn replace_block(current: &str, block: &str) -> String {
    if let Some(span) = block_span(current) {
        let mut out = String::with_capacity(current.len());
        out.push_str(&current[..span.start]);
        out.push_str(block);
        out.push_str(&current[span.end..]);
        return out;
    }

    let mut out = if current.is_empty() {
        String::from("#!/bin/sh\n")
    } else {
        let mut s = current.to_string();
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s
    };
    out.push_str(block);
    out.push('\n');
    out
}

#[cfg(unix)]
fn make_executable(file: &fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_file: &fs::File) -> std::io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Gemeinsames Dateisystem- und JSON-Kleinzeug
// ---------------------------------------------------------------------------

/// Wo Konfiguration hingehört und wo die Git-Hooks liegen.
struct RepoPaths {
    /// Die Repo-Wurzel (Elternverzeichnis von `.git`).
    root: PathBuf,
    /// Das Git-Verzeichnis selbst.
    git_dir: PathBuf,
    /// Woher Git die Hooks **tatsächlich** ausführt — oder dass es das nicht tut.
    hooks: HooksDir,
}

/// Das Verzeichnis, aus dem Git die Hooks dieses Repositories ausführt.
///
/// Normalerweise `<git-dir>/hooks` — aber `core.hooksPath` verschiebt es, und
/// zwar aus *jeder* Config-Ebene: husky setzt es lokal auf `.husky`, lefthook
/// und pre-commit tun Ähnliches, über `init.templateDir` kann es global gesetzt
/// sein. Schrieben wir dann nach `<git-dir>/hooks`, meldete `enable` Erfolg und
/// Git läse die Datei **nie** — der stillste denkbare Ausfall, und er trifft
/// genau die JS-Monorepos, in denen Agent-Teams arbeiten.
///
/// Deshalb fragen wir Git selbst, statt die Config-Ebenen nachzubauen:
/// `rev-parse --git-path hooks` löst `core.hooksPath` mit auf. Die Ausgabe ist
/// entweder relativ zum Arbeitsverzeichnis des Aufrufs — das ist hier `root` —
/// oder absolut; `join` behandelt beides richtig. Antwortet Git nicht (kein
/// `git` im Pfad, kaputte Config), bleibt es beim bisherigen Verhalten.
///
/// # Der leere Wert
///
/// Ein **gesetztes, aber leeres** `core.hooksPath` schaltet die Hook-Ausführung
/// ganz ab; gemessen mit git 2.51:
///
/// | `core.hooksPath` | `rev-parse --git-path hooks` | beim Commit |
/// |---|---|---|
/// | ungesetzt | `.git/hooks` | Hook feuert |
/// | `""` | `./` | **kein Hook, keine Meldung** |
/// | `.` | `.` | `error: cannot run post-commit: …` |
/// | absoluter Pfad | absolut | Hook feuert |
///
/// Die `rev-parse`-Antwort taugt hier nicht als Unterscheidung — `./` und `.`
/// liegen einen Schrägstrich auseinander und meinen Verschiedenes. Deshalb
/// fragen wir zusätzlich den Config-Wert selbst ab. Ohne diese Abfrage würden
/// wir bei leerem Wert nach `<root>/.` schreiben, also **ausführbare Dateien in
/// die Arbeitskopie** legen, die Git nie liest.
pub(crate) fn effective_hooks_dir(root: &Path, git_dir: &Path) -> HooksDir {
    let configured = git_config_value(root, "core.hooksPath");
    if configured.as_ref().is_some_and(|value| value.is_empty()) {
        return HooksDir::Unusable(NoHooksDir::Empty);
    }

    let default = git_dir.join("hooks");
    let answer = git_output(root, &["rev-parse", "--git-path", "hooks"]).unwrap_or_default();
    // Nur den Zeilenumbruch abschneiden. Ein `trim()` fräße Pfadbestandteile:
    // `core.hooksPath = ".husky "` ist zulässig, Git bewahrt das Leerzeichen und
    // führt die Hooks aus `.husky ` aus. Wer hier trimmt, schreibt nach `.husky`
    // und meldet Erfolg — der Ausfall aus #9, nur eine Ecke weiter.
    let answer = answer.trim_end_matches(['\n', '\r']);
    let dir = if answer.is_empty() {
        // Schweigt `rev-parse`, obwohl `core.hooksPath` gesetzt ist, wäre der
        // Rückfall auf `<git-dir>/hooks` geraten — und zwar in die Richtung, die
        // #9 gerade geschlossen hat: schreiben, wo Git nie liest. Lieber
        // abbrechen und den Nutzer nachsehen lassen.
        if configured.is_some() {
            return HooksDir::Unusable(NoHooksDir::Unanswered);
        }
        default
    } else {
        // **Unnormalisiert.** Git löst `..` physisch auf, nicht textuell: Führt
        // eine Komponente davor über einen Symlink, meint `links/../hooks`
        // etwas anderes als die textuelle Vereinfachung `hooks`. Geschrieben
        // wird deshalb über genau den Pfad, den Git genannt hat — das Auflösen
        // überlassen wir demselben Betriebssystem, das auch Git antwortet.
        root.join(answer)
    };

    // Für die *Entscheidung* zusätzlich textuell normalisieren: `canonicalize`
    // antwortet nur für existierende Pfade, und `gibtsnicht/..` zeigt auf die
    // Wurzel, ohne dass es `gibtsnicht` gäbe. Ohne diesen Schritt liefe der
    // Riegel ins Leere und drei ausführbare Dateien landeten im Quellcode.
    //
    // Geschrieben wird weiter über `dir`, den **unnormalisierten** Pfad: Git
    // löst `..` physisch auf, und über einen Symlink meint `links/../hooks`
    // etwas anderes als die textuelle Vereinfachung.
    if same_location(&dir, root) || same_location(&lexically_normalized(dir.clone()), root) {
        return HooksDir::Unusable(NoHooksDir::WorktreeRoot);
    }
    HooksDir::At(dir)
}

/// Wohin Git die Hooks dieses Repositories legt — oder dass es keine ausführt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HooksDir {
    /// Aus diesem Verzeichnis führt Git die Hooks aus.
    At(PathBuf),
    /// Es gibt keinen Ort, an dem ein Hook etwas bewirken würde.
    Unusable(NoHooksDir),
}

/// Warum `core.hooksPath` auf kein brauchbares Hook-Verzeichnis führt.
///
/// Beide Fälle enden gleich — wir schreiben nichts —, aber aus verschiedenen
/// Gründen, und der Nutzer braucht den Grund, um es zu beheben.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoHooksDir {
    /// `core.hooksPath` ist gesetzt, aber leer: Git führt **keine** Hooks aus.
    /// Jede Datei, die wir schrieben, wäre tot.
    Empty,
    /// `core.hooksPath` ist gesetzt, aber `git rev-parse --git-path hooks` gibt
    /// keine Antwort (defekte Config, sehr altes Git). Dann wüssten wir nicht,
    /// wohin — und zu raten hieße, womöglich dorthin zu schreiben, wo Git nie
    /// liest.
    Unanswered,
    /// Das effektive Verzeichnis ist die Arbeitskopie selbst — so kommt es bei
    /// `core.hooksPath = .` zustande.
    ///
    /// Git selbst kommt damit nicht zurecht (gemessen mit git 2.51: `error:
    /// cannot run post-commit: No such file or directory`, weil es den nackten
    /// Namen ohne `./` auszuführen versucht). Selbst wenn es liefe, wäre das
    /// Ergebnis falsch: Drei ausführbare, unversionierte Dateien lägen zwischen
    /// dem Quellcode des Nutzers — und `git add -A` nähme sie mit.
    WorktreeRoot,
}

impl HooksDir {
    /// Das Verzeichnis, oder ein Fehler, der den Grund benennt.
    ///
    /// Fail-closed: Lieber ein Abbruch mit Begründung als ein `enable`, das
    /// Erfolg meldet und nichts bewirkt — das ist derselbe stille Ausfall, den
    /// dieses Modul gerade erst geschlossen hat.
    pub(crate) fn require(&self) -> std::io::Result<&Path> {
        match self {
            Self::At(dir) => Ok(dir),
            Self::Unusable(NoHooksDir::Empty) => Err(std::io::Error::other(
                "core.hooksPath ist gesetzt, aber leer — Git führt dann gar keine Hooks aus. \
                 Setze einen Pfad oder entferne den Schlüssel: git config --unset core.hooksPath",
            )),
            Self::Unusable(NoHooksDir::Unanswered) => Err(std::io::Error::other(
                "core.hooksPath ist gesetzt, aber `git rev-parse --git-path hooks` antwortet \
                 nicht — dann lässt sich nicht sagen, aus welchem Verzeichnis Git die Hooks \
                 ausführt. Prüfe die Git-Konfiguration",
            )),
            Self::Unusable(NoHooksDir::WorktreeRoot) => Err(std::io::Error::other(
                "core.hooksPath zeigt auf die Repo-Wurzel — dort würden die Hooks als \
                 ausführbare Dateien zwischen deinem Quellcode liegen, und Git führt sie von \
                 dort nicht aus. Setze core.hooksPath auf ein eigenes Verzeichnis, etwa .husky",
            )),
        }
    }
}

/// Sucht von der aktuellen Position aufwärts das Repository. Eigene, dumme
/// Suche wie im Journal — `enable` braucht kein `minds-git`, nur die
/// Verzeichnisse.
fn locate() -> std::io::Result<RepoPaths> {
    let start = std::env::current_dir()?;
    for dir in start.ancestors() {
        let candidate = dir.join(".git");
        match fs::metadata(&candidate) {
            Ok(m) if m.is_dir() => {
                let hooks = effective_hooks_dir(dir, &candidate);
                return Ok(RepoPaths {
                    root: dir.to_path_buf(),
                    git_dir: candidate,
                    hooks,
                });
            }
            // `.git` als Datei (verlinkte Worktrees) wird hier bewusst nicht
            // aufgelöst: `enable` ist ein Setup-Schritt im Hauptbaum.
            _ => continue,
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "kein Git-Repository gefunden, ausgehend von {}",
            start.display()
        ),
    ))
}

/// Liest eine JSON-Datei oder liefert ein leeres Objekt. Ungültiges JSON wird
/// **nicht** stillschweigend überschrieben, sondern ist ein Fehler — sonst
/// verlöre der Nutzer seine Konfiguration.
fn read_json(path: &Path) -> std::io::Result<Value> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Value::Object(Map::new())),
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} ist kein gültiges JSON: {e}", path.display()),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(e) => Err(e),
    }
}

fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    create_parent(path)?;
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    fs::write(path, text)
}

/// Sorgt dafür, dass `value` ein Objekt ist, und gibt es aus. Ein fremder
/// Nicht-Objekt-Wert würde ersetzt — aber `read_json` liefert für Dateien und
/// Leeres immer ein Objekt, sodass das nur bei absichtlich kaputter Eingabe
/// greift.
fn as_object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().expect("gerade zum Objekt gemacht")
}

fn create_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Ein frisch initialisiertes Repository. `None`, wenn kein `git` im Pfad
    /// liegt — dort soll der Test nicht falsch-rot werden.
    fn init_repo() -> Option<tempfile::TempDir> {
        let dir = tmp();
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["init", "--quiet"])
            .status()
            .ok()?
            .success();
        ok.then_some(dir)
    }

    fn git_config(root: &Path, key: &str, value: &str) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", "--local", key, value])
            .status()
            .unwrap()
            .success();
        assert!(ok, "`git config {key}` schlug fehl");
    }

    /// Die Regression aus #9: In einem Repo mit `core.hooksPath` (husky,
    /// lefthook, pre-commit) las Git unsere Hooks **nie** — `enable` meldete
    /// trotzdem Erfolg. Vorher landete der Block in `.git/hooks`, hier liegt er
    /// da, wo Git ihn tatsächlich ausführt.
    #[test]
    fn git_hooks_follow_a_relative_core_hookspath() {
        let Some(dir) = init_repo() else { return };
        let root = dir.path();
        git_config(root, "core.hooksPath", ".husky");

        let hooks = effective_hooks_dir(root, &root.join(".git"));
        assert_eq!(hooks, HooksDir::At(root.join(".husky")));
        let hooks = hooks.require().unwrap().to_path_buf();

        let change = enable_git_hook(&hooks, "post-commit", POST_COMMIT_BODY).unwrap();
        assert_eq!(change, Change::Created);

        let written = fs::read_to_string(root.join(".husky/post-commit")).unwrap();
        assert!(written.contains(MARK_BEGIN), "unser Block fehlt: {written}");
        assert!(written.contains(POST_COMMIT_BODY));
        assert!(
            !root.join(".git/hooks/post-commit").exists(),
            "der Hook darf nicht im ignorierten Verzeichnis landen"
        );
    }

    /// `core.hooksPath` darf auch absolut sein — dann ist die Repo-Wurzel als
    /// Basis gerade nicht gemeint.
    #[test]
    fn git_hooks_follow_an_absolute_core_hookspath() {
        let Some(dir) = init_repo() else { return };
        let root = dir.path();
        let elsewhere = root.join("anderswo");
        git_config(root, "core.hooksPath", &elsewhere.display().to_string());

        assert_eq!(
            effective_hooks_dir(root, &root.join(".git")),
            HooksDir::At(elsewhere),
            "ein absoluter Pfad darf nicht an die Wurzel gehängt werden"
        );
    }

    /// Ohne `core.hooksPath` bleibt alles wie bisher.
    #[test]
    fn without_core_hookspath_the_hooks_stay_in_the_git_dir() {
        let Some(dir) = init_repo() else { return };
        let root = dir.path();
        // Ein global gesetztes core.hooksPath in der Entwickler-Umgebung würde
        // diesen Fall verfälschen; dann sagt der Test nichts aus. Der Skip wird
        // gemeldet, sonst wäre er ausgerechnet auf den Maschinen unsichtbar, für
        // die dieses Feature gebaut ist. (Die Integrationstests decken den Fall
        // hermetisch ab — dort lässt sich die Config der Kindprozesse setzen.)
        if git_config_value(root, "core.hooksPath").is_some() {
            eprintln!("global gesetztes core.hooksPath — Test übersprungen");
            return;
        }

        let git_dir = root.join(".git");
        assert_eq!(
            effective_hooks_dir(root, &git_dir),
            HooksDir::At(git_dir.join("hooks"))
        );
    }

    /// Der Fall, der `enable` dazu brachte, ausführbare Dateien in die
    /// Arbeitskopie zu legen: `core.hooksPath` ist **gesetzt, aber leer**.
    ///
    /// Git führt dann gar keine Hooks aus und meldet dabei nichts —
    /// `rev-parse --git-path hooks` antwortet trotzdem mit `./`. Wer dieser
    /// Antwort folgt, schreibt `post-commit` & Co. in die Repo-Wurzel, wo sie
    /// als unversionierte Dateien liegen bleiben und nie laufen.
    #[test]
    fn an_empty_core_hookspath_disables_hooks_instead_of_moving_them() {
        let Some(dir) = init_repo() else { return };
        let root = dir.path();
        git_config(root, "core.hooksPath", "");

        assert_eq!(
            effective_hooks_dir(root, &root.join(".git")),
            HooksDir::Unusable(NoHooksDir::Empty),
            "ein leerer Wert ist kein Verzeichnis"
        );
    }

    /// Der zweite Weg in dieselbe Sackgasse: `core.hooksPath = .` löst auf die
    /// Arbeitskopie selbst auf.
    ///
    /// Er kommt anders zustande als der leere Wert — der Wert *ist* gesetzt und
    /// *ist* nicht leer, erst die Auflösung landet auf der Wurzel —, und er
    /// endet gleich: Dort würden drei ausführbare, unversionierte Dateien
    /// zwischen dem Quellcode liegen, und Git führt sie von dort nicht aus.
    #[test]
    fn a_hookspath_that_resolves_to_the_worktree_root_is_refused() {
        let Some(dir) = init_repo() else { return };
        let root = dir.path();
        git_config(root, "core.hooksPath", ".");

        assert_eq!(
            effective_hooks_dir(root, &root.join(".git")),
            HooksDir::Unusable(NoHooksDir::WorktreeRoot)
        );
    }

    /// Und die Folge davon: `enable` bricht ab, statt Erfolg zu melden — mit
    /// einer Begründung, die den Schlüssel nennt und sagt, was zu tun ist.
    #[test]
    fn an_unusable_hooks_dir_is_an_error_that_names_the_key() {
        let empty = HooksDir::Unusable(NoHooksDir::Empty)
            .require()
            .unwrap_err()
            .to_string();
        assert!(empty.contains("core.hooksPath"), "{empty}");
        assert!(empty.contains("keine Hooks"), "{empty}");
        assert!(empty.contains("--unset"), "die Abhilfe fehlt: {empty}");

        let root = HooksDir::Unusable(NoHooksDir::WorktreeRoot)
            .require()
            .unwrap_err()
            .to_string();
        assert!(root.contains("core.hooksPath"), "{root}");
        assert!(root.contains("Repo-Wurzel"), "{root}");
        assert!(root.contains(".husky"), "die Abhilfe fehlt: {root}");
    }

    /// „Nicht gesetzt" und „gesetzt, aber leer" sehen in der Ausgabe gleich aus
    /// — der Exit-Status trennt sie. Genau darauf steht die Fallunterscheidung.
    #[test]
    fn an_unset_key_and_an_empty_value_are_told_apart() {
        let Some(dir) = init_repo() else { return };
        let root = dir.path();
        if git_config_value(root, "core.hooksPath").is_some() {
            // Global gesetzt — „nicht gesetzt" lässt sich hier nicht herstellen.
            eprintln!("global gesetztes core.hooksPath — Test übersprungen");
            return;
        }

        assert_eq!(git_config_value(root, "core.hooksPath"), None);
        git_config(root, "core.hooksPath", "");
        assert_eq!(
            git_config_value(root, "core.hooksPath"),
            Some(String::new())
        );
    }

    // -----------------------------------------------------------------------
    // Golden: der Wortlaut des Hinweises auf ein verschobenes Verzeichnis
    // -----------------------------------------------------------------------

    #[test]
    fn golden_no_note_for_the_default_directory() {
        let root = Path::new("/repo");
        let git_dir = Path::new("/repo/.git");
        assert_eq!(
            moved_hooks_note(root, git_dir, &git_dir.join("hooks")),
            None
        );
    }

    #[test]
    fn golden_note_for_a_directory_inside_the_repo() {
        assert_eq!(
            moved_hooks_note(
                Path::new("/repo"),
                Path::new("/repo/.git"),
                Path::new("/repo/.husky")
            ),
            Some(
                "Hinweis: core.hooksPath ist gesetzt — die Git-Hooks gehen nach „.husky“"
                    .to_owned()
            )
        );
    }

    /// Der Angriff, gegen den [`read_existing_hook`] fail-closed ist: Ein
    /// eingecheckter Symlink im Hook-Verzeichnis (bei `core.hooksPath = .husky`
    /// ist das die Arbeitskopie) zeigt auf eine private Datei. Ohne die Prüfung
    /// schriebe `fs::write` durch den Link und `chmod` machte `0600` zu `0755`.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_hook_is_refused_instead_of_followed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmp();
        let victim = dir.path().join("credentials");
        fs::write(&victim, "AWS_SECRET_ACCESS_KEY=geheim\n").unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o600)).unwrap();

        let hooks = dir.path().join(".husky");
        fs::create_dir_all(&hooks).unwrap();
        std::os::unix::fs::symlink(&victim, hooks.join("post-commit")).unwrap();

        let err = enable_git_hook(&hooks, "post-commit", POST_COMMIT_BODY).unwrap_err();
        assert!(err.to_string().contains("Symlink"), "{err}");

        // Und das Opfer ist unangetastet — Inhalt wie Rechte.
        assert_eq!(
            fs::read_to_string(&victim).unwrap(),
            "AWS_SECRET_ACCESS_KEY=geheim\n"
        );
        assert_eq!(
            fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    /// Die Kehrseite der entfernten Verzeichnis-Sperre, als Regressionsschutz:
    /// Ein symlinktes `.git` (oder ein geteiltes `.git/hooks`) ist ein Setup,
    /// das Git unterstützt — `enable` darf daran nicht scheitern. Ein Versuch,
    /// Symlinks auf dem Weg abzulehnen, hat genau das getan.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_hooks_directory_is_not_refused() {
        let dir = tmp();
        let real = dir.path().join("wirklich-hier");
        fs::create_dir_all(&real).unwrap();
        let hooks = dir.path().join("verlinkt");
        std::os::unix::fs::symlink(&real, &hooks).unwrap();

        assert!(ensure_writable(&hooks).is_ok());
        assert_eq!(
            enable_git_hook(&hooks, "post-commit", POST_COMMIT_BODY).unwrap(),
            Change::Created
        );
        // Geschrieben wird am Ziel — das ist die bewusst getragene Folge.
        assert!(real.join("post-commit").is_file());
    }

    /// Die Nachbardatei darf keinem vorgelegten Symlink folgen. Der Name ist
    /// zwar zufällig, aber `create_new` ist die Zusage — hier direkt geprüft,
    /// indem der Name vorher belegt wird.
    #[cfg(unix)]
    #[test]
    fn a_taken_temp_name_is_refused_instead_of_followed() {
        let dir = tmp();
        let victim = dir.path().join("opfer.txt");
        fs::write(&victim, "unberührt\n").unwrap();

        let taken = dir.path().join("belegt");
        std::os::unix::fs::symlink(&victim, &taken).unwrap();

        let err = create_new_file(&taken).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(fs::read_to_string(&victim).unwrap(), "unberührt\n");
    }

    /// Der Ersatz für den Symlink beim Schreiben: Selbst wenn zwischen Prüfung
    /// und Schreibzugriff ein Link an die Stelle käme, ersetzt `rename` den
    /// Namen. Hier direkt geprüft — ein bestehender Hook wird ersetzt, ohne dass
    /// eine Temp-Datei zurückbleibt.
    #[test]
    fn writing_a_hook_leaves_no_temp_file_behind() {
        let dir = tmp();
        let hooks = dir.path().join("hooks");
        enable_git_hook(&hooks, "post-commit", POST_COMMIT_BODY).unwrap();
        enable_git_hook(&hooks, "post-commit", "minds etwas-anderes").unwrap();

        let leftovers: Vec<_> = fs::read_dir(&hooks)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "post-commit")
            .collect();
        assert!(leftovers.is_empty(), "übrig geblieben: {leftovers:?}");
    }

    /// Eine Datei jenseits jeder Hook-Größe wird nicht eingelesen. Ohne die
    /// Grenze liefe ein eingecheckter Symlink auf `/dev/zero` bis zum
    /// Speicherende.
    #[test]
    fn an_oversized_file_is_refused_before_it_is_read() {
        let dir = tmp();
        let hooks = dir.path().join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        fs::write(
            hooks.join("post-commit"),
            vec![b'x'; MAX_HOOK_BYTES as usize + 1],
        )
        .unwrap();

        let err = enable_git_hook(&hooks, "post-commit", POST_COMMIT_BODY).unwrap_err();
        assert!(err.to_string().contains("kein Hook-Skript"), "{err}");
    }

    /// Steuerzeichen aus der Config dürfen die Ausgabe nicht umschreiben.
    #[test]
    fn control_characters_in_a_path_are_defused() {
        let shown = display_path(Path::new("\u{1b}[2K\u{1b}[Aböse"));
        assert!(
            !shown.contains('\u{1b}'),
            "roher Escape blieb stehen: {shown}"
        );
        assert!(shown.contains("böse"), "{shown}");
    }

    /// Ein Wert mit Leerzeichen ist ein zulässiger Verzeichnisname, und Git
    /// führt die Hooks von dort aus. Wer die `rev-parse`-Antwort trimmt,
    /// schreibt nach `.git/hooks` und meldet Erfolg — der Ausfall aus #9, nur
    /// eine Ecke weiter.
    #[test]
    fn a_hookspath_with_trailing_space_keeps_its_space() {
        let Some(dir) = init_repo() else { return };
        let root = dir.path();
        git_config(root, "core.hooksPath", ".husky ");

        assert_eq!(
            effective_hooks_dir(root, &root.join(".git")),
            HooksDir::At(root.join(".husky "))
        );
    }

    /// `gibtsnicht/..` zeigt auf die Arbeitskopie-Wurzel — auch wenn es
    /// `gibtsnicht` nicht gibt. `canonicalize` schweigt dann, der Riegel muss
    /// trotzdem greifen; sonst landen drei ausführbare Dateien im Quellcode.
    #[test]
    fn a_parent_hop_to_the_root_is_refused_even_if_the_hop_does_not_exist() {
        let Some(dir) = init_repo() else { return };
        let root = dir.path();
        git_config(root, "core.hooksPath", "gibtsnicht/..");

        assert_eq!(
            effective_hooks_dir(root, &root.join(".git")),
            HooksDir::Unusable(NoHooksDir::WorktreeRoot)
        );
    }

    /// Und der reine Whitespace-Pfad ist nicht „leer": Git legt die Hooks in ein
    /// Verzeichnis, das so heißt, und führt sie von dort aus.
    #[test]
    fn a_whitespace_hookspath_is_a_directory_not_an_empty_value() {
        let Some(dir) = init_repo() else { return };
        let root = dir.path();
        git_config(root, "core.hooksPath", "  ");

        assert_eq!(
            effective_hooks_dir(root, &root.join(".git")),
            HooksDir::At(root.join("  "))
        );
    }

    /// `..` im Pfad darf die Einordnung „innen oder außen" nicht kippen —
    /// lexikalisch verglichen sähe `<root>/../global-hooks` wie ein Pfad
    /// *innerhalb* der Wurzel aus.
    #[test]
    fn a_parent_dir_hop_leaves_the_repo() {
        let dir = tmp();
        let root = dir.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        // Die Erwartung selbst kanonisieren: Das Temp-Verzeichnis liegt auf
        // macOS hinter `/var -> /private/var`, sonst verglichen wir zwei
        // Schreibweisen desselben Ortes.
        let outside = canonical_prefix(&dir.path().join("global-hooks"));

        // `<root>/../global-hooks` ist derselbe Ort wie `<tmp>/global-hooks` —
        // und liegt außerhalb. Ein Vergleich Komponente für Komponente hielte
        // ihn für einen Pfad *innerhalb* von `<root>`.
        assert_eq!(canonical_prefix(&root.join("../global-hooks")), outside);

        assert!(
            moved_hooks_note(&root, &root.join(".git"), &root.join("../global-hooks"))
                .is_some_and(|note| note.contains("aus dem Repo heraus")),
            "ein Pfad über `..` liegt außerhalb"
        );
        // Und innerhalb bleibt innerhalb.
        assert!(
            moved_hooks_note(&root, &root.join(".git"), &root.join(".husky"))
                .is_some_and(|note| !note.contains("aus dem Repo heraus")),
            ".husky liegt im Repo"
        );
    }

    /// `canonical_prefix` muss auch dann antworten, wenn das Verzeichnis noch
    /// gar nicht existiert — `enable` legt es erst an, die Einordnung braucht
    /// aber vorher eine Aussage.
    #[test]
    fn canonical_prefix_resolves_what_exists_and_keeps_the_rest() {
        let dir = tmp();
        let real = fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            canonical_prefix(&dir.path().join("gibt/es/noch/nicht")),
            real.join("gibt/es/noch/nicht")
        );
    }

    /// Außerhalb des Repos ist die Folge eine andere — und die muss dastehen:
    /// Ein `enable` in *einem* Repo schaltet dann alle Repos scharf.
    #[test]
    fn golden_note_for_a_directory_outside_the_repo() {
        assert_eq!(
            moved_hooks_note(
                Path::new("/repo"),
                Path::new("/repo/.git"),
                Path::new("/home/anna/git-hooks")
            ),
            Some(
                "Hinweis: core.hooksPath zeigt aus dem Repo heraus („/home/anna/git-hooks“) — \
                 die Hooks gelten damit für alle deine Repositories"
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_child_repo_is_created_bare_and_is_idempotent() {
        let dir = tmp();
        let child = dir.path().join("ctx");

        let created = ensure_child_repo(dir.path(), &child, None).unwrap();
        assert_eq!(created, Change::Created);
        assert!(is_git_repo(&child), "muss ein Git-Repo sein");
        // Bare: HEAD liegt direkt im Verzeichnis, es gibt kein .git/.
        assert!(child.join("HEAD").exists());
        assert!(!child.join(".git").exists());

        // Zweiter Lauf ändert nichts.
        let again = ensure_child_repo(dir.path(), &child, None).unwrap();
        assert_eq!(again, Change::Unchanged);
    }

    #[test]
    fn a_child_repo_gets_an_origin_when_a_remote_is_given() {
        let dir = tmp();
        let child = dir.path().join("ctx");
        // Eine unerreichbare URL: clone scheitert, der Fallback init+remote greift.
        ensure_child_repo(dir.path(), &child, Some("/nicht/erreichbar.git")).unwrap();
        assert!(is_git_repo(&child));
        let origin = git_output(&child, &["remote", "get-url", "origin"]).unwrap();
        assert_eq!(origin.trim(), "/nicht/erreichbar.git");
    }

    #[test]
    fn the_pre_push_hook_only_delegates() {
        // Die Regression, gegen die dieser Test steht: Solange der Hook selbst
        // `git push` rief, kostete *jeder* Push den vollen Verbindungsaufbau —
        // auch wenn es nichts Neues gab. Ob etwas fällig ist, kann nur das
        // Binary entscheiden (Tracking-Refs), nicht die Shell.
        assert!(PRE_PUSH_BODY.contains("\"$MINDS_BIN\" sync"));
        assert!(
            !PRE_PUSH_BODY.contains("git push"),
            "der Hook darf nicht selbst pushen: {PRE_PUSH_BODY}"
        );
        // Ein Sync-Fehler darf den Push des Nutzers nie scheitern lassen.
        assert!(PRE_PUSH_BODY.contains("|| true"));
        // Das Remote, an das gerade gepusht wird, muss durchgereicht werden.
        assert!(PRE_PUSH_BODY.contains("\"$1\""));
    }

    #[test]
    fn what_enable_writes_is_what_fsck_recognizes() {
        // Die Brücke zwischen den beiden Kommandos, und der Test, ohne den die
        // ganze `Outdated`-Erkennung wertlos wäre: Er geht über den
        // *Produktionspfad* (`enable_git_hook` → Datei → `block_body`), statt
        // das Blockformat im Test ein zweites Mal nachzubauen.
        //
        // Ohne ihn bliebe eine Änderung am Format (eine Kommentarzeile im Block,
        // ein anderer Marker) grün — und `fsck` meldete danach in **jedem**
        // frisch eingerichteten Repo „stammt aus einer älteren minds-Version".
        // Falsch-rot ist hier schlimmer als der Fehler, den die Erkennung behebt.
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks");

        for name in hook_names() {
            let body = expected_body(name).expect("jeder Name hat einen Rumpf");
            enable_git_hook(&hooks, name, body).unwrap();

            let written = fs::read_to_string(hooks.join(name)).unwrap();
            assert_eq!(block_body(&written).as_deref(), Some(body), "{name}");
        }
    }

    #[test]
    fn a_block_with_crlf_line_endings_is_still_recognized() {
        // Seit #9 kann die Hook-Datei in der Arbeitskopie liegen — und dort
        // schreibt ein `.gitattributes` mit `eol=crlf` sie bei jedem Checkout
        // um. Der Block ist derselbe, nur die Zeilenenden sind es nicht.
        let body = expected_body("pre-push").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks");
        enable_git_hook(&hooks, "pre-push", body).unwrap();

        let written = fs::read_to_string(hooks.join("pre-push")).unwrap();
        let crlf = written.replace('\n', "\r\n");
        assert_eq!(block_body(&crlf).as_deref(), Some(body));
    }

    #[test]
    fn a_stray_end_marker_before_the_block_does_not_confuse_the_span() {
        // Von Hand zusammengestückelt: Der Schlussmarker steht *vor* dem
        // Anfang. Global gesucht ergäbe das eine Spanne, die rückwärts läuft.
        let body = expected_body("post-commit").unwrap();
        let text = format!("{MARK_END}\nfremd\n{MARK_BEGIN}\n{body}\n{MARK_END}\n");
        assert_eq!(block_body(&text).as_deref(), Some(body));

        // Und `replace_block` fasst dieselbe Spanne an — sonst meldete `fsck`
        // etwas, das `enable` nicht repariert.
        let replaced = replace_block(&text, &format!("{MARK_BEGIN}\nneu\n{MARK_END}"));
        assert_eq!(block_body(&replaced).as_deref(), Some("neu"));
        assert!(replaced.contains("fremd"), "Fremdes bleibt: {replaced}");
    }

    #[test]
    fn the_pre_push_hook_redirects_its_stderr_but_keeps_stdout() {
        // stderr geht weg, weil sie sonst roh im Push-Output landet — der
        // Wortlaut steht seit #10 in `<git-dir>/minds/hook.log`. Geprüft wird
        // die **Aufruf-Zeile** (die letzte): Seit der Prelude trägt auch die
        // `git config`-Zeile ein `2>/dev/null` — ein `contains` über den ganzen
        // Rumpf bliebe grün, wenn ausgerechnet der sync-Aufruf seins verlöre.
        let call = PRE_PUSH_BODY.lines().last().unwrap();
        assert!(
            call.contains("2>/dev/null"),
            "stderr gehört ins Log, nicht in den Push-Output: {call}"
        );
        // stdout bleibt: Dort steht die Erfolgsmeldung, und die gehört zu dem
        // Push, bei dem sie entsteht. `>/dev/null` wäre hier ein Verlust.
        assert!(
            !PRE_PUSH_BODY.contains(">/dev/null 2>&1") && !PRE_PUSH_BODY.contains("1>/dev/null"),
            "stdout darf nicht mit umgeleitet werden: {PRE_PUSH_BODY}"
        );
    }

    #[test]
    fn the_commit_hooks_discard_their_output_because_the_log_carries_it() {
        // Die Umkehrung: Diese beiden dürfen still sein — sie schreiben in
        // dieselbe Datei. Fiele die Umleitung weg, redete der Rekorder in jeden
        // Commit hinein; fiele das Log weg, wäre die Umleitung ein Verschweigen.
        for body in [POST_COMMIT_BODY, PREPARE_MSG_BODY] {
            assert!(body.contains(">/dev/null 2>&1"), "{body}");
            assert!(body.contains("|| true"), "{body}");
        }
    }

    #[test]
    fn every_hook_resolves_the_recorded_binary_before_searching_the_path() {
        // #25: Der Commit aus VS Code, Fork oder Tower kommt ohne das Profil
        // der Shell — `minds` nackt aufzurufen hieß dort: stiller Totalausfall.
        // Jeder Rumpf löst deshalb zuerst `minds.binary` auf; der PATH ist nur
        // noch die Rückfallebene für den umgezogenen Binary.
        for (name, body) in ALL_HOOKS {
            // `--local`: Env, `git -c` und globale Ebenen dürfen den Ort nicht
            // stellen — Hook, `record_binary` und `fsck` fragen dieselbe Quelle.
            assert!(
                body.starts_with("MINDS_BIN=$(git config --local --get minds.binary"),
                "{name} löst den gemerkten Ort nicht lokal auf: {body}"
            );
            // `-f` zusätzlich zu `-x`: ein Verzeichnis mit x-Bit bestünde
            // `[ -x ]` — der Hook liefe dann gegen ein Verzeichnis statt in
            // die PATH-Rückfallebene.
            assert!(
                body.contains("[ -f \"$MINDS_BIN\" ] && [ -x \"$MINDS_BIN\" ] || MINDS_BIN=minds"),
                "{name} hat keine PATH-Rückfallebene: {body}"
            );
            // Der Aufruf geht über die Variable, gequotet — ein Pfad mit
            // Leerzeichen bleibt ein Wort.
            assert!(
                body.contains("\"$MINDS_BIN\" "),
                "{name} ruft nicht über die Variable auf: {body}"
            );
            // Der Block kann in einer fremden Hook-Datei mit `set -e` stehen —
            // keine Zeile darf den Hook dann abbrechen.
            for line in body.lines() {
                assert!(
                    line.ends_with("|| true") || line.ends_with("|| MINDS_BIN=minds"),
                    "{name}: Zeile bricht unter set -e ab: {line}"
                );
            }
        }
    }

    #[test]
    fn the_fetch_refspecs_never_clobber_local_reviews() {
        // Kontext direkt, Reviews in den Tracking-Namensraum: Ein `git fetch`
        // darf ein lokal entstandenes, noch nicht gepushtes Verdict nicht
        // wegräumen.
        assert!(FETCH_REFSPECS.contains(&"+refs/minds/context:refs/minds/context"));
        let reviews = FETCH_REFSPECS
            .iter()
            .find(|spec| spec.contains("reviews"))
            .expect("ein Refspec für die Reviews");
        assert!(
            reviews.ends_with(":refs/minds/remotes/origin/reviews"),
            "Reviews müssen ins Tracking-Ziel: {reviews}"
        );
    }

    #[test]
    fn claude_creates_all_events_once() {
        let dir = tmp();
        let change = claude_style(dir.path(), ".claude/settings.json", "claude-code").unwrap();
        assert_eq!(change, Change::Created);

        let value = read_json(&dir.path().join(".claude/settings.json")).unwrap();
        let hooks = value["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), HOOK_EVENTS.len());
        let pre = hooks["PreToolUse"].as_array().unwrap();
        assert_eq!(pre[0]["matcher"], "*");
        assert!(
            pre[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("minds hook --agent claude-code")
        );
        // Ohne Matcher bei den Nicht-Tool-Events.
        assert!(hooks["Stop"][0].get("matcher").is_none());
    }

    #[test]
    fn recall_adds_a_session_start_brief_hook_and_is_idempotent() {
        let dir = tmp();
        // Erst der reguläre Capture-Hook, dann die Rückführung obendrauf.
        claude_style(dir.path(), ".claude/settings.json", "claude-code").unwrap();

        let change = enable_recall_hook(dir.path()).unwrap();
        assert_eq!(change, Change::Updated);

        let value = read_json(&dir.path().join(".claude/settings.json")).unwrap();
        let groups = value["hooks"]["SessionStart"].as_array().unwrap();
        // Zwei Gruppen: der Capture-Hook (minds hook) und der Recall-Hook (minds brief).
        assert_eq!(groups.len(), 2);
        let commands: Vec<&str> = groups
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap())
            .map(|h| h["command"].as_str().unwrap())
            .collect();
        assert!(commands.iter().any(|c| c.contains("minds hook")));
        assert!(commands.iter().any(|c| c.contains("minds brief --hook")));

        // Zweiter Lauf ändert nichts.
        assert_eq!(enable_recall_hook(dir.path()).unwrap(), Change::Unchanged);
    }

    #[test]
    fn claude_is_idempotent() {
        let dir = tmp();
        claude_style(dir.path(), ".claude/settings.json", "claude-code").unwrap();
        let again = claude_style(dir.path(), ".claude/settings.json", "claude-code").unwrap();
        assert_eq!(again, Change::Unchanged);

        let value = read_json(&dir.path().join(".claude/settings.json")).unwrap();
        // Kein zweiter Eintrag pro Event.
        assert_eq!(value["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn foreign_config_is_preserved() {
        let dir = tmp();
        let path = dir.path().join(".claude/settings.json");
        create_parent(&path).unwrap();
        fs::write(
            &path,
            r#"{"model":"opus","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo eigen"}]}]}}"#,
        )
        .unwrap();

        let change = claude_style(dir.path(), ".claude/settings.json", "claude-code").unwrap();
        assert_eq!(change, Change::Updated);

        let value = read_json(&path).unwrap();
        assert_eq!(value["model"], "opus", "fremde Keys bleiben");
        let pre = value["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(
            pre.len(),
            2,
            "der eigene Hook des Nutzers bleibt, unserer kommt dazu"
        );
        assert_eq!(pre[0]["hooks"][0]["command"], "echo eigen");
    }

    #[test]
    fn non_claude_agents_carry_the_event_override() {
        let dir = tmp();
        claude_style(dir.path(), ".cursor/hooks.json", "cursor").unwrap();
        let value = read_json(&dir.path().join(".cursor/hooks.json")).unwrap();
        let cmd = value["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(cmd.contains("--agent cursor"));
        assert!(cmd.contains("--event Stop"));
    }

    #[test]
    fn codex_writes_the_flag_and_is_idempotent() {
        let dir = tmp();
        let created = enable_codex(dir.path()).unwrap();
        assert_eq!(created, Change::Created);
        let toml = fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap();
        assert!(toml.contains("codex_hooks = true"));

        let again = enable_codex(dir.path()).unwrap();
        assert_eq!(again, Change::Unchanged);
    }

    #[test]
    fn codex_flips_a_false_flag_to_true() {
        let dir = tmp();
        let path = dir.path().join(".codex/config.toml");
        create_parent(&path).unwrap();
        fs::write(&path, "model = \"o3\"\ncodex_hooks = false\n").unwrap();
        ensure_codex_hooks_flag(&path).unwrap();
        let toml = fs::read_to_string(&path).unwrap();
        assert!(toml.contains("codex_hooks = true"));
        assert!(!toml.contains("false"));
        assert!(toml.contains("model = \"o3\""), "fremde Zeile bleibt");
    }

    #[test]
    fn opencode_plugin_is_written_and_idempotent() {
        let dir = tmp();
        assert_eq!(enable_opencode(dir.path()).unwrap(), Change::Created);
        assert_eq!(enable_opencode(dir.path()).unwrap(), Change::Unchanged);
        let ts = fs::read_to_string(dir.path().join(".opencode/plugin/minds.ts")).unwrap();
        assert!(ts.contains("minds"));
        assert!(ts.contains("--agent"));
    }

    #[test]
    fn git_hook_is_created_marked_and_executable() {
        let dir = tmp();
        let hooks_dir = dir.path().join(".git/hooks");

        // Das Verzeichnis gibt es absichtlich noch nicht: Ein verschobenes
        // `core.hooksPath` zeigt oft auf ein Verzeichnis, das erst entstehen
        // muss.
        let change = enable_git_hook(&hooks_dir, "post-commit", POST_COMMIT_BODY).unwrap();
        assert_eq!(change, Change::Created);

        let path = hooks_dir.join("post-commit");
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("#!/bin/sh"));
        assert!(body.contains(MARK_BEGIN) && body.contains(MARK_END));
        assert!(body.contains("git rev-parse HEAD"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755);
        }

        assert_eq!(
            enable_git_hook(&hooks_dir, "post-commit", POST_COMMIT_BODY).unwrap(),
            Change::Unchanged
        );
    }

    #[test]
    fn git_hook_preserves_the_users_own_lines() {
        let dir = tmp();
        let hooks_dir = dir.path().join(".git/hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        let path = hooks_dir.join("post-commit");
        fs::write(&path, "#!/bin/sh\necho meins\n").unwrap();

        enable_git_hook(&hooks_dir, "post-commit", POST_COMMIT_BODY).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("echo meins"), "fremde Zeile bleibt");
        assert!(body.contains(MARK_BEGIN));
    }

    #[test]
    fn invalid_json_is_an_error_not_an_overwrite() {
        let dir = tmp();
        let path = dir.path().join(".claude/settings.json");
        create_parent(&path).unwrap();
        fs::write(&path, "{ kaputt ").unwrap();
        assert!(claude_style(dir.path(), ".claude/settings.json", "claude-code").is_err());
        // Die kaputte Datei ist noch da, nicht ersetzt.
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ kaputt ");
    }
}
