//! `minds doctor` — weist nach, dass die Einrichtung trägt (EA-10).
//!
//! `minds enable --witness` richtet ein; `doctor` prüft, ob es hält — vor
//! allem das eine, was sich nicht aus Konfigurationsdateien ablesen lässt:
//! ob die Agent-Seite das Witness-Home tatsächlich **nicht** erreicht.
//!
//! Jede Prüfung ist eine Zeile `ok` / `warn` / `fail` mit einem Satz Grund.
//! Exit 0 ohne `fail`, sonst 1.
//!
//! # Zwei Seiten
//!
//! - **Agent-Seite** — `MINDS_WITNESS_SOCKET` ist gesetzt oder
//!   `--probe-home` angegeben: Socket gesetzt, Witness antwortet auf `ping`,
//!   und die **Isolationsprobe**: Lässt sich das Witness-Home öffnen, ist das
//!   `fail`. `EACCES` ist `ok` — ein Nachweis. `ENOENT` ist nur `warn`:
//!   Abwesenheit in einem anderen Mount-Namensraum beweist nichts. Eine
//!   Einbindung des Homes außer `run/` (Mount-Tabelle, eine Heuristik) ist
//!   `fail`. Gelesen wird dabei nichts — ein erfolgreich geöffneter Pfad wird
//!   sofort wieder geschlossen (W1).
//! - **Host-Seite** — `witness.json` liegt im Witness-Home: Profil, Pins,
//!   läuft der Witness, Schlüssel vorhanden mit 0600.
//!
//! Hooks, Git-Hooks und Store-Config prüft `doctor` auf beiden Seiten, mit
//! demselben Wortlaut wie `minds fsck`.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::ExitCode;

/// Der Ausgang einer Prüfung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

/// Eine Zeile des Berichts.
#[derive(Debug)]
pub(crate) struct Check {
    pub(crate) status: Status,
    pub(crate) name: &'static str,
    pub(crate) reason: String,
}

impl Check {
    fn new(status: Status, name: &'static str, reason: impl Into<String>) -> Self {
        Self {
            status,
            name,
            reason: reason.into(),
        }
    }

    fn line(&self) -> String {
        format!(
            "{:<4}  {}: {}",
            self.status.word(),
            self.name,
            crate::text::sanitize(&self.reason)
        )
    }
}

pub fn run(probe_home: Option<&str>) -> ExitCode {
    let checks = checks(probe_home.map(Path::new));
    for check in &checks {
        println!("{}", check.line());
    }
    if checks.iter().any(|check| check.status == Status::Fail) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn checks(probe_home: Option<&Path>) -> Vec<Check> {
    let mut checks = Vec::new();
    let root = match crate::enable::locate() {
        Ok(paths) => {
            repo_checks(&paths, &mut checks);
            Some(paths.root().to_path_buf())
        }
        Err(err) => {
            checks.push(Check::new(
                Status::Fail,
                "repository",
                format!("not usable here — {err}"),
            ));
            None
        }
    };
    witness_checks(root.as_deref(), probe_home, &mut checks);
    checks
}

/// Hooks der Agents, Git-Hooks, Store-Config.
fn repo_checks(paths: &crate::enable::RepoPaths, checks: &mut Vec<Check>) {
    let root = paths.root();
    checks.push(match crate::fsck::agents_verdict(root) {
        None => Check::new(
            Status::Warn,
            "agent hooks",
            "no agent configuration found — `minds enable` registers the hooks",
        ),
        Some(verdict) => Check::new(
            if verdict.ok { Status::Ok } else { Status::Fail },
            "agent hooks",
            verdict.reason,
        ),
    });
    let hooks = crate::fsck::hooks_verdict(root, paths.git_dir());
    checks.push(Check::new(
        if hooks.ok { Status::Ok } else { Status::Fail },
        "git hooks",
        hooks.reason,
    ));
    let store = crate::config::load(root);
    let described = match store.backend() {
        minds_store::Backend::InRepo => format!("in-repo, {}", store.reference()),
        minds_store::Backend::ChildRepo { path } => {
            format!("child repo \"{}\", {}", path.display(), store.reference())
        }
    };
    checks.push(match store.open(root) {
        Ok(_) => Check::new(Status::Ok, "store", described),
        Err(err) => Check::new(
            Status::Fail,
            "store",
            format!("{described} — cannot be opened: {err}"),
        ),
    });
}

#[cfg(not(unix))]
fn witness_checks(_root: Option<&Path>, _probe_home: Option<&Path>, checks: &mut Vec<Check>) {
    checks.push(Check::new(
        Status::Warn,
        "witness",
        "not supported on this platform",
    ));
}

#[cfg(unix)]
fn witness_checks(root: Option<&Path>, probe_home: Option<&Path>, checks: &mut Vec<Check>) {
    let socket = crate::hook::witness::socket_path();
    // Wo das Home des Witness läge, wäre dies die Host-Seite — für die
    // Agent-Seite der Kandidat, den die Probe ohne `--probe-home` versucht.
    let default_home =
        root.and_then(|root| crate::witness_cmd::resolve_home_for(None, Some(root.to_str()?)).ok());
    if socket.is_some() || probe_home.is_some() {
        agent_side(socket.as_deref(), probe_home, default_home, checks);
    } else if let Some(home) =
        default_home.filter(|home| std::fs::symlink_metadata(home.join("witness.json")).is_ok())
    {
        host_side(&home, root, checks);
    } else if root.is_some_and(|root| {
        crate::enable_witness::read_regular(&root.join(".devcontainer/compose.yaml"))
            .ok()
            .flatten()
            .is_some_and(|text| text.contains("MINDS_WITNESS_SOCKET"))
    }) {
        // Ein Witness-Container ist eingerichtet, aber kein Home dafür zu
        // finden: Entweder fehlt der Witness, oder der Weg zum Home wurde
        // umgelenkt — nie ein stilles „nicht eingerichtet".
        checks.push(Check::new(
            Status::Fail,
            "witness",
            "a witness dev container is set up, but no witness home was found for this \
             repository — check MINDS_WITNESS_HOME and rerun `minds enable --witness container`",
        ));
    } else {
        checks.push(Check::new(
            Status::Warn,
            "witness",
            "not configured — sessions are recorded as A1 (`minds enable --witness container`)",
        ));
    }
}

/// Die Agent-Seite: Socket, `ping`, Isolationsprobe.
#[cfg(unix)]
fn agent_side(
    socket: Option<&Path>,
    probe_home: Option<&Path>,
    default_home: Option<PathBuf>,
    checks: &mut Vec<Check>,
) {
    match socket {
        None => checks.push(Check::new(
            Status::Fail,
            "witness socket",
            "MINDS_WITNESS_SOCKET is not set — hooks journal locally, nothing is witnessed",
        )),
        Some(path) => {
            checks.push(Check::new(
                Status::Ok,
                "witness socket",
                format!("MINDS_WITNESS_SOCKET={}", path.display()),
            ));
            checks.push(if crate::witness_cmd::ping(path) {
                Check::new(Status::Ok, "witness", "answers ping")
            } else {
                Check::new(
                    Status::Fail,
                    "witness",
                    format!("does not answer ping at \"{}\"", path.display()),
                )
            });
        }
    }
    // Ohne `--probe-home`: wo die Agent-Seite ein Home vermuten könnte —
    // eine gesetzte Variable oder der Standardpfad. Ist dort nichts, beweist
    // das im `container`-Profil wenig (dort heißt der Host-Pfad anders):
    // höchstens `warn`. Lässt sich dort etwas öffnen, ist das trotzdem `fail`.
    let explicit = probe_home.is_some();
    if probe_home.is_some_and(|home| home.as_os_str().is_empty()) {
        checks.push(Check::new(
            Status::Fail,
            "isolation",
            "--probe-home is empty — pass the host's witness home",
        ));
        return;
    }
    // `default_home` nennt `MINDS_WITNESS_HOME` schon zuerst, wenn es gesetzt ist.
    let candidates: Vec<PathBuf> = match probe_home {
        Some(home) => vec![home.to_path_buf()],
        None => default_home.into_iter().collect(),
    };
    if candidates.is_empty() {
        checks.push(Check::new(
            Status::Warn,
            "isolation",
            "not probed — pass --probe-home <dir> with the host's witness home",
        ));
        return;
    }
    for home in candidates {
        let check = match probe(&home) {
            Probe::Reachable(path) => Check::new(
                Status::Fail,
                "isolation",
                format!(
                    "agent can reach the witness home — \"{}\" opens",
                    path.display()
                ),
            ),
            // Im Container heißt der Host-Pfad nicht so — „fehlt" beweist dort
            // nur etwas, wenn auch keine Einbindung das Home an anderer Stelle
            // zeigt. Die Mount-Tabelle (Linux) nennt die Quelle jeder
            // Einbindung; erlaubt ist allein `run/`.
            Probe::Unreachable(why) if explicit => match foreign_home_mounts(&home) {
                Some(mounts) if !mounts.is_empty() => Check::new(
                    Status::Fail,
                    "isolation",
                    format!(
                        "a mount exposes the witness home at {} — only its run/ may be mounted",
                        mounts.join(", ")
                    ),
                ),
                // „Fehlt hier" ist im Container kein Nachweis: Die
                // Mount-Tabelle ist eine Heuristik — sie erkennt keine
                // Einbindung eines übergeordneten Ordners und keine Quelle
                // auf einer eigenen Partition. Nur ein vorhandenes, aber
                // verweigertes Home (EACCES) ist ein Nachweis.
                Some(_) if why == "absent" => Check::new(
                    Status::Warn,
                    "isolation",
                    format!(
                        "\"{}\" is absent here and the mount table shows no mount of it — a \
                         heuristic, not proof (a mount of a parent directory or of another \
                         partition is not recognised); check the container's mounts",
                        home.display()
                    ),
                ),
                None if why == "absent" => Check::new(
                    Status::Warn,
                    "isolation",
                    format!(
                        "\"{}\" is absent here, but without a mount table that proves nothing \
                         about another mount namespace",
                        home.display()
                    ),
                ),
                _ => Check::new(
                    Status::Ok,
                    "isolation",
                    format!("agent cannot reach \"{}\" ({why})", home.display()),
                ),
            },
            Probe::Unreachable(why) => Check::new(
                Status::Warn,
                "isolation",
                format!(
                    "nothing reachable at the default location \"{}\" ({why}) — that proves little; \
                     pass --probe-home <dir> with the host's witness home",
                    home.display()
                ),
            ),
            // Mit ausdrücklichem `--probe-home` ist „nicht feststellbar" kein
            // Nachweis der Trennung: fail-closed.
            Probe::Unknown(err) => Check::new(
                if explicit { Status::Fail } else { Status::Warn },
                "isolation",
                format!(
                    "\"{}\" could not be probed: {err} — isolation not proven",
                    home.display()
                ),
            ),
        };
        checks.push(check);
    }
}

/// Code, den der Agent in `.git` hinterlegen kann und den ein `git` auf dem
/// **Host** ausführte — etwa der Editor vor „Reopen in Container", oder ein
/// `git status` des Menschen: fremde Hooks, Zusatzzeilen in den
/// minds-Hooks, und Konfigschlüssel, die Befehle starten. Damit liefe Code
/// des Agenten unter der Kennung, der der Witness-Schlüssel gehört (W1).
#[cfg(unix)]
fn host_code_check(root: &Path, checks: &mut Vec<Check>) {
    let git_dir = root.join(".git");
    let mut problems = Vec::new();
    match std::fs::read_dir(git_dir.join("hooks")) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        problems.push(format!(".git/hooks cannot be listed completely: {err}"));
                        continue;
                    }
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".sample") {
                    continue;
                }
                if crate::enable::hook_names().any(|hook| hook == name) {
                    match crate::enable::read_existing_hook(&entry.path()) {
                        Ok(Some(text)) if only_minds_block(&text) => {}
                        _ => problems
                            .push(format!("hook {name} carries lines besides the minds block")),
                    }
                } else {
                    problems.push(format!("unexpected hook {name}"));
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => problems.push(format!(".git/hooks cannot be listed: {err}")),
    }
    // Die Datei selbst lesen — gewöhnlich, ohne Symlink, begrenzt, ohne zu
    // blockieren (ein FIFO oder eine riesige Datei des Agenten hielte sonst
    // `doctor` an) — und `git` nur als Parser über stdin nutzen: `--file -`
    // folgt keinem `include.path`, und `git` läuft außerhalb des Baums.
    let parsed = crate::enable_witness::read_regular(&git_dir.join("config"))
        .ok()
        .flatten()
        .and_then(|text| {
            use std::io::Write;
            let mut child = std::process::Command::new("git")
                .current_dir("/")
                .env_remove("GIT_DIR")
                .env_remove("GIT_CONFIG_PARAMETERS")
                .env_remove("GIT_CONFIG_COUNT")
                .args(["config", "--file", "-", "--null", "--list"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            child.stdin.take()?.write_all(text.as_bytes()).ok()?;
            child.wait_with_output().ok()
        });
    match parsed {
        Some(out) if out.status.success() => {
            for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
                let entry = String::from_utf8_lossy(entry);
                let (key, value) = entry.split_once('\n').unwrap_or((&entry, ""));
                if executes(&key.to_ascii_lowercase(), value) {
                    problems.push(format!("git config {key} can run commands"));
                }
            }
        }
        _ => problems.push(".git/config cannot be read".to_owned()),
    }
    // Eine Heuristik, kein Nachweis: Eine Liste bekannter Schlüssel und
    // Hook-Orte kann in einem `.git`, das der Agent schreiben darf, nicht
    // vollständig sein (`commondir`, `config.worktree`, neue git-Schlüssel,
    // verschachtelte Repos). Funde sind deshalb `warn` — es können auch die
    // eigenen Hooks des Menschen sein —, und ein leerer Befund sagt nur, dass
    // nichts Bekanntes gefunden wurde.
    checks.push(if problems.is_empty() {
        Check::new(
            Status::Ok,
            "host code in .git",
            "no known foreign hooks or command-running git config (a heuristic, not proof — \
             do not run git on the host in a tree the agent wrote)",
        )
    } else {
        Check::new(
            Status::Warn,
            "host code in .git",
            format!(
                "{} — the agent can write .git; a git on the host would run it with the \
                 witness key in reach (a heuristic: review these)",
                problems.join("; ")
            ),
        )
    });
}

/// Ob ein Konfigschlüssel einen Befehl startet (oder Konfiguration von
/// anderswo nachlädt).
#[cfg(unix)]
fn executes(key: &str, value: &str) -> bool {
    const EXACT: &[&str] = &[
        "core.hookspath",
        "core.fsmonitor",
        "core.sshcommand",
        "core.pager",
        "core.editor",
        "core.askpass",
        "core.gitproxy",
        "sequence.editor",
        "diff.external",
        "gpg.program",
        "gpg.ssh.program",
        "gpg.x509.program",
        "credential.helper",
        "include.path",
        "uploadpack.packobjectshook",
    ];
    let parts: Vec<&str> = key.split('.').collect();
    let last = parts.last().copied().unwrap_or_default();
    EXACT.contains(&key)
        || (key.starts_with("filter.") && matches!(last, "clean" | "smudge" | "process"))
        || (key.starts_with("diff.") && matches!(last, "textconv" | "command"))
        || (key.starts_with("merge.") && last == "driver")
        || (key.starts_with("credential.") && last == "helper")
        || (key.starts_with("includeif.") && last == "path")
        || (key.starts_with("alias.") && value.trim_start().starts_with('!'))
}

/// Ob ein Hook außer dem minds-Block nur Shebang, Leerzeilen und Kommentare
/// trägt.
#[cfg(unix)]
fn only_minds_block(text: &str) -> bool {
    let (Some(start), Some(end)) = (
        text.find(crate::enable::MARK_BEGIN),
        text.find(crate::enable::MARK_END),
    ) else {
        return false;
    };
    if end < start {
        return false;
    }
    let outside = format!("{}{}", &text[..start], &text[end..]);
    outside.lines().all(|line| {
        let line = line.trim();
        line.is_empty() || line.starts_with('#')
    })
}

/// Einbindungen, deren Quelle im Witness-Home liegt, außer `run/` — aus
/// `/proc/self/mountinfo`. `None`, wo es keine Mount-Tabelle gibt.
#[cfg(unix)]
fn foreign_home_mounts(home: &Path) -> Option<Vec<String>> {
    let table = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    Some(foreign_mounts_in(&table, home))
}

/// Die Prüfung über einem Text im Format von `mountinfo`: Feld 4 ist die
/// Quelle (der Pfad im eingebundenen Dateisystem), Feld 5 der Einhängepunkt.
/// Docker Desktop stellt Host-Pfaden ein Präfix voran (`/host_mnt/…`) —
/// gesucht wird deshalb das Home als Pfad-Teil, an Komponentengrenzen.
#[cfg(unix)]
fn foreign_mounts_in(table: &str, home: &Path) -> Vec<String> {
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    let unescape = |field: &str| {
        field
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\")
    };
    let mut found = Vec::new();
    for line in table.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let (Some(source), Some(target)) = (fields.get(3), fields.get(4)) else {
            continue;
        };
        let source = unescape(source);
        // Jedes Vorkommen prüfen: Das erste kann mitten in einer Komponente
        // liegen (`/a/bc/…/a/b/key` bei Home `/a/b`), ein späteres an einer
        // Grenze.
        let exposed = source.match_indices(home).any(|(at, _)| {
            let rest = &source[at + home.len()..];
            let boundary = rest.is_empty() || rest.starts_with('/');
            let run_only = rest == "/run" || rest.starts_with("/run/");
            boundary && !run_only
        });
        if exposed {
            found.push(format!("\"{}\"", unescape(target)));
        }
    }
    found
}

/// Was die Isolationsprobe fand.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    /// Dieser Pfad ließ sich öffnen.
    Reachable(PathBuf),
    /// Nichts ließ sich öffnen; der Grund des Homes selbst.
    Unreachable(&'static str),
    /// Ein Fehler, der weder „fehlt" noch „verweigert" heißt.
    Unknown(String),
}

/// Versucht, das Home und die Dateien darin zu öffnen, die nur der Witness
/// sehen darf. Gelesen wird nichts: Ein geöffneter Pfad ist schon der Befund.
/// Geöffnet wird ohne zu blockieren und ohne Terminal-Übernahme — ein FIFO
/// oder Gerät an einem der Pfade hält `doctor` nicht an.
#[cfg(unix)]
fn probe(home: &Path) -> Probe {
    use std::os::unix::fs::OpenOptionsExt;
    let targets = [
        home.to_path_buf(),
        home.join("witness.json"),
        home.join("key/witness_ed25519"),
        home.join("journal"),
        home.join("ledger"),
        home.join("evidence"),
        home.join("evidence/state"),
        home.join("log"),
    ];
    let mut first_reason = None;
    let mut unknown = None;
    // Jedes Ziel wird versucht: Ein unklarer Fehler am ersten darf nicht
    // verdecken, dass sich ein späteres öffnen lässt.
    for path in targets {
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
            .open(&path)
            .map(drop);
        let reason = match opened {
            Ok(()) => return Probe::Reachable(path),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => "absent",
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => "permission denied",
            Err(err) if err.raw_os_error() == Some(libc::ENOTDIR) => "absent",
            Err(err) => {
                unknown.get_or_insert(err.to_string());
                continue;
            }
        };
        first_reason.get_or_insert(reason);
    }
    match unknown {
        Some(err) => Probe::Unknown(err),
        None => Probe::Unreachable(first_reason.unwrap_or("absent")),
    }
}

/// Die Host-Seite: Profil und Pins aus `witness.json`, läuft der Witness,
/// Schlüssel privat, und — im `container`-Profil — die Isolationsgrenze in
/// `.devcontainer/` unverändert.
#[cfg(unix)]
fn host_side(home: &Path, root: Option<&Path>, checks: &mut Vec<Check>) {
    use std::os::unix::fs::MetadataExt;

    match crate::witness_cmd::load(home) {
        Ok(config) => {
            checks.push(Check::new(
                Status::Ok,
                "witness profile",
                format!(
                    "{} (witness.json in \"{}\")",
                    config.profile().name(),
                    home.display()
                ),
            ));
            if !config.pinned() {
                checks.push(Check::new(
                    Status::Fail,
                    "witness pins",
                    crate::witness_cmd::UNPINNED,
                ));
            }
            if config.profile().name() == "container" {
                if let Some(root) = root {
                    devcontainer_check(root, checks);
                    host_code_check(root, checks);
                }
            }
        }
        Err(err) => checks.push(Check::new(
            Status::Fail,
            "witness profile",
            format!("witness.json cannot be loaded: {err}"),
        )),
    }
    let socket = home.join("run/witness.sock");
    checks.push(if crate::witness_cmd::ping(&socket) {
        Check::new(Status::Ok, "witness", "running, answers ping")
    } else {
        Check::new(
            Status::Fail,
            "witness",
            "not running — start its service (see `minds enable --witness`)",
        )
    });
    let key = home.join("key/witness_ed25519");
    checks.push(match std::fs::symlink_metadata(&key) {
        // SAFETY: geteuid hat keine Vorbedingungen.
        Ok(meta)
            if meta.is_file()
                && meta.mode() & 0o077 == 0
                && meta.uid() == unsafe { libc::geteuid() } =>
        {
            Check::new(
                Status::Ok,
                "witness key",
                format!("present, {:04o}", meta.mode() & 0o777),
            )
        }
        Ok(meta) => Check::new(
            Status::Fail,
            "witness key",
            format!(
                "not a private regular file of this user (mode {:04o}) — the witness refuses it",
                meta.mode() & 0o777
            ),
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Check::new(
            Status::Fail,
            "witness key",
            "missing — `minds witness keygen`",
        ),
        Err(err) => Check::new(Status::Fail, "witness key", err.to_string()),
    });
}

/// Die Isolationsgrenze des `container`-Profils steht in Dateien, die der
/// Agent ändern kann — und er kann neue dazulegen: eine `.env`, die die
/// `${…:?}`-Schutzwerte von Compose erfüllt (und damit das ganze Home
/// mountet), eine weitere `devcontainer.json` in einem Unterordner, die der
/// Editor zur Auswahl anbietet, eine `.devcontainer.json` an der Wurzel.
/// Deshalb gilt: In `.devcontainer/` liegen genau die beiden Vorlagen
/// (unverändert) und höchstens Vorschläge (`*.minds-proposed`), sonst nichts;
/// an der Wurzel keine `.devcontainer.json`. Alles andere ist `fail`, bis ein
/// Mensch die Änderung geprüft hat.
#[cfg(unix)]
fn devcontainer_check(root: &Path, checks: &mut Vec<Check>) {
    let templates = crate::enable_witness::devcontainer_templates();
    let mut problems = Vec::new();
    let dir = root.join(".devcontainer");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => match std::fs::read_dir(&dir) {
            Ok(entries) => {
                // Zulässig sind genau die beiden Vorlagen und ihre beiden
                // Vorschläge — als gewöhnliche Dateien. Ein Verzeichnis, das
                // nur wie ein Vorschlag heißt, trüge eine eigene
                // `devcontainer.json`, die der Editor zur Auswahl anböte.
                let allowed: Vec<String> = templates
                    .iter()
                    .flat_map(|(file, _)| {
                        [
                            (*file).to_owned(),
                            format!("{file}{}", crate::enable_witness::PROPOSED_SUFFIX),
                        ]
                    })
                    .collect();
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(err) => {
                            problems
                                .push(format!(".devcontainer/ cannot be listed completely: {err}"));
                            continue;
                        }
                    };
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let regular = entry.file_type().is_ok_and(|kind| kind.is_file());
                    if !allowed.contains(&name) || !regular {
                        problems.push(format!(".devcontainer/{name} is not a template"));
                    }
                }
            }
            Err(err) => problems.push(format!(".devcontainer/ cannot be listed: {err}")),
        },
        Ok(_) => problems.push(".devcontainer is not a plain directory".to_owned()),
        Err(_) => problems.push(".devcontainer/ is missing".to_owned()),
    }
    for (file, template) in templates {
        let path = dir.join(file);
        match crate::enable_witness::read_regular(&path) {
            Ok(Some(text)) if text == template => {}
            Ok(Some(_)) => problems.push(format!(".devcontainer/{file} differs from the template")),
            Ok(None) => problems.push(format!(".devcontainer/{file} is missing")),
            Err(err) => problems.push(err.to_string()),
        }
    }
    // An der Wurzel ist eine `.devcontainer.json` eine weitere Konfiguration.
    // Nur „gibt es nicht" ist in Ordnung; jeder andere Fehler beweist nichts.
    match std::fs::symlink_metadata(root.join(".devcontainer.json")) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => problems
            .push(".devcontainer.json at the repository root can widen the container".to_owned()),
        Err(err) => problems.push(format!(
            ".devcontainer.json at the repository root cannot be checked: {err}"
        )),
    }
    // Eine `.env` an der Wurzel ist in vielen Projekten App-Konfiguration;
    // je nach Compose-Version und Aufruf kann sie aber auch die Variablen der
    // Container-Vorlage füllen. Kein stilles `ok`, aber auch kein Dauer-`fail`.
    if std::fs::symlink_metadata(root.join(".env")).is_ok() {
        checks.push(Check::new(
            Status::Warn,
            "isolation boundary (.env)",
            ".env at the repository root may feed the dev container's variables, depending \
             on the Compose version — check it does not set MINDS_*",
        ));
    }
    checks.push(if problems.is_empty() {
        Check::new(
            Status::Ok,
            "isolation boundary",
            ".devcontainer/ holds exactly the EA-S2 templates",
        )
    } else {
        Check::new(
            Status::Fail,
            "isolation boundary",
            format!(
                "{} — review the change; `minds enable --witness container` shows the \
                 templates as *.minds-proposed",
                problems.join("; ")
            ),
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_status_name_and_reason() {
        let check = Check::new(Status::Fail, "isolation", "agent can reach");
        assert_eq!(check.line(), "fail  isolation: agent can reach");
        assert_eq!(
            Check::new(Status::Ok, "store", "in-repo").line(),
            "ok    store: in-repo"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_tells_reachable_from_absent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        assert_eq!(probe(&home), Probe::Unreachable("absent"));
        std::fs::create_dir(&home).unwrap();
        assert_eq!(probe(&home), Probe::Reachable(home.clone()));
    }

    /// Die Mount-Tabelle: Quelle im Home außer `run/` ist eine Freilegung —
    /// auch mit Docker-Desktop-Präfix; `run/` und fremde Pfade nicht.
    #[cfg(unix)]
    #[test]
    fn mounts_exposing_the_home_are_found() {
        let home = Path::new("/home/alice/.local/state/minds-witness/abc");
        let table = "\
36 35 98:0 /home/alice/.local/state/minds-witness/abc/run /run/minds-witness rw - ext4 /dev/x rw
37 35 98:0 /home/alice/projects/demo /workspaces/demo rw - ext4 /dev/x rw
38 35 0:5 /host_mnt/home/alice/.local/state/minds-witness/abc /x rw - fakeowner fake rw
39 35 98:0 /home/alice/.local/state/minds-witness/abcdef /y rw - ext4 /dev/x rw
40 35 98:0 /home/alice/.local/state/minds-witness/abc/key /with\\040space rw - ext4 /dev/x rw
41 35 98:0 /home/alice/.local/state/minds-witness/abcd/home/alice/.local/state/minds-witness/abc/key /z rw - ext4 /dev/x rw";
        assert_eq!(
            foreign_mounts_in(table, home),
            [
                "\"/x\"".to_owned(),
                "\"/with space\"".to_owned(),
                "\"/z\"".to_owned()
            ]
        );
    }

    /// Ein unklarer Fehler (hier eine Symlink-Schleife, `ELOOP`) ist kein
    /// Nachweis der Trennung — und verdeckt nicht, dass sich ein späteres
    /// Ziel öffnen lässt.
    #[cfg(unix)]
    #[test]
    fn an_unclear_probe_error_is_unknown_and_does_not_stop_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let looped = dir.path().join("loop");
        std::os::unix::fs::symlink(&looped, &looped).unwrap();
        assert!(matches!(probe(&looped), Probe::Unknown(_)));

        // SAFETY: geteuid hat keine Vorbedingungen.
        if unsafe { libc::geteuid() } == 0 {
            return; // root liest auch das 0300-Verzeichnis.
        }
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let loop_json = home.join("witness.json");
        std::os::unix::fs::symlink(&loop_json, &loop_json).unwrap();
        std::fs::write(home.join("ledger"), "").unwrap();
        std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o300))
            .unwrap();
        let result = probe(&home);
        std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        assert_eq!(result, Probe::Reachable(home.join("ledger")));

        // Mit ausdrücklichem `--probe-home` ist „unklar" ein fail.
        let mut checks = Vec::new();
        agent_side(None, Some(&looped), None, &mut checks);
        let isolation = checks.iter().find(|c| c.name == "isolation").unwrap();
        assert_eq!(isolation.status, Status::Fail);
    }

    /// Ein FIFO an einem Probe-Pfad hält `doctor` nicht an.
    #[cfg(unix)]
    #[test]
    fn the_probe_does_not_block_on_a_fifo() {
        // SAFETY: geteuid hat keine Vorbedingungen.
        if unsafe { libc::geteuid() } == 0 {
            return; // root liest auch das 0300-Verzeichnis.
        }
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o300))
            .unwrap();
        let fifo = home.join("witness.json");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: nul-terminierter Pfad, keine weiteren Vorbedingungen.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        let result = probe(&home);
        std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        // Das Verzeichnis selbst ist nicht lesbar (0300), das FIFO öffnet
        // ohne zu warten — und ist damit erreichbar.
        assert_eq!(result, Probe::Reachable(fifo));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_sees_permission_denied_as_unreachable() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid hat keine Vorbedingungen.
        if unsafe { libc::geteuid() } == 0 {
            return; // root öffnet alles; die Probe gilt der Agent-Kennung.
        }
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("witness.json"), "{}").unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = probe(&home);
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result, Probe::Unreachable("permission denied"));
    }
}
