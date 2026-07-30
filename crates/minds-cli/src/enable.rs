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
//! | Git         | `.git/hooks/post-commit`, `prepare-commit-msg`   |
//!
//! Alles projekt-lokal, relativ zur Repo-Wurzel — der Kontext gehört zum Repo,
//! nicht zum Benutzerkonto.
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
const MARK_BEGIN: &str = "# >>> minds >>>";
/// Ende unseres Blocks.
const MARK_END: &str = "# <<< minds <<<";

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

    let post = enable_git_hook(&paths.git_dir, "post-commit", POST_COMMIT_BODY)?;
    report(verbose, ".git/hooks/post-commit", post);
    let prepare = enable_git_hook(&paths.git_dir, "prepare-commit-msg", PREPARE_MSG_BODY)?;
    report(verbose, ".git/hooks/prepare-commit-msg", prepare);

    // Der Kontext soll mit dem Code reisen — aber *woher* er reist, hängt am
    // Backend. In-Repo liegt er im selben Repo; beim Child-Repo in einem
    // separaten, das erst angelegt (oder geklont) werden muss.
    configure_sync(paths, store, child_remote, verbose)?;

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

// ---------------------------------------------------------------------------
// Sync des Kontext-Refs — je Backend anders
// ---------------------------------------------------------------------------

/// Richtet den Push/Fetch des Kontext-Refs ein — für In-Repo im selben
/// Repository, für das Child-Repo im separaten, das dafür erst existieren muss.
fn configure_sync(
    paths: &RepoPaths,
    store: &StoreConfig,
    child_remote: Option<&str>,
    verbose: bool,
) -> std::io::Result<()> {
    // Der Hook ist für beide Backends derselbe; die Unterscheidung trifft
    // `minds sync` anhand der Store-Config.
    let pre_push = enable_git_hook(&paths.git_dir, "pre-push", PRE_PUSH_BODY)?;
    report(verbose, ".git/hooks/pre-push", pre_push);

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

/// post-commit: der Checkpoint-Auslöser. `minds checkpoint` (M6) nimmt Journal +
/// Transkript, redigiert und legt die Session ab. Non-blocking — ein Rekorder
/// darf einen Commit nie scheitern lassen.
const POST_COMMIT_BODY: &str =
    "minds checkpoint --commit \"$(git rev-parse HEAD)\" >/dev/null 2>&1 || true";

/// prepare-commit-msg: reserviert für den Trailer (M6). Heute ein sicherer
/// No-op — der Aufruf schlägt fehl, `|| true` fängt ihn, die Nachricht bleibt
/// unangetastet.
const PREPARE_MSG_BODY: &str = "minds prepare-commit-msg \"$1\" >/dev/null 2>&1 || true";

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
const PRE_PUSH_BODY: &str = "minds sync --remote \"$1\" || true";

/// Fügt einen markierten Block in einen Git-Hook ein oder aktualisiert ihn.
///
/// Fremde Zeilen in derselben Datei (die eigenen Hooks des Nutzers) bleiben; nur
/// der Block zwischen [`MARK_BEGIN`] und [`MARK_END`] gehört uns und wird
/// ersetzt. Eine neue Datei bekommt eine `#!/bin/sh`-Zeile und `chmod +x`.
fn enable_git_hook(git_dir: &Path, name: &str, body: &str) -> std::io::Result<Change> {
    let path = git_dir.join("hooks").join(name);
    let existed = path.exists();
    let current = if existed {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };

    let block = format!("{MARK_BEGIN}\n{body}\n{MARK_END}");
    let next = replace_block(&current, &block);
    if next == current && existed {
        return Ok(Change::Unchanged);
    }

    create_parent(&path)?;
    fs::write(&path, &next)?;
    make_executable(&path)?;
    Ok(if existed {
        Change::Updated
    } else {
        Change::Created
    })
}

/// Ersetzt einen vorhandenen minds-Block durch `block` oder hängt ihn an. Eine
/// leere Datei bekommt zusätzlich den Shebang.
fn replace_block(current: &str, block: &str) -> String {
    if let (Some(start), Some(end)) = (current.find(MARK_BEGIN), current.find(MARK_END)) {
        let end = end + MARK_END.len();
        let mut out = String::with_capacity(current.len());
        out.push_str(&current[..start]);
        out.push_str(block);
        out.push_str(&current[end..]);
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
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
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
}

/// Sucht von der aktuellen Position aufwärts das Repository. Eigene, dumme
/// Suche wie im Journal — `enable` braucht kein `minds-git`, nur zwei
/// Verzeichnisse.
fn locate() -> std::io::Result<RepoPaths> {
    let start = std::env::current_dir()?;
    for dir in start.ancestors() {
        let candidate = dir.join(".git");
        match fs::metadata(&candidate) {
            Ok(m) if m.is_dir() => {
                return Ok(RepoPaths {
                    root: dir.to_path_buf(),
                    git_dir: candidate,
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
        assert!(PRE_PUSH_BODY.contains("minds sync"));
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
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();

        let change = enable_git_hook(&git_dir, "post-commit", POST_COMMIT_BODY).unwrap();
        assert_eq!(change, Change::Created);

        let path = git_dir.join("hooks/post-commit");
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
            enable_git_hook(&git_dir, "post-commit", POST_COMMIT_BODY).unwrap(),
            Change::Unchanged
        );
    }

    #[test]
    fn git_hook_preserves_the_users_own_lines() {
        let dir = tmp();
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(git_dir.join("hooks")).unwrap();
        let path = git_dir.join("hooks/post-commit");
        fs::write(&path, "#!/bin/sh\necho meins\n").unwrap();

        enable_git_hook(&git_dir, "post-commit", POST_COMMIT_BODY).unwrap();
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
