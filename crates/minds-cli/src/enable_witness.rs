//! `minds enable --witness <profile>` — die bezeugte Konfiguration (EA-10).
//!
//! Ein Kommando richtet ein, was ein Witness braucht; `minds doctor` weist
//! danach nach, dass die Trennung hält. Drei Profile (ADR-0012):
//!
//! | Profil      | Was `enable` tut                                                   |
//! |-------------|--------------------------------------------------------------------|
//! | `container` | Witness-Home + Schlüssel, `.devcontainer/` aus den EA-S2-Vorlagen, Nutzerdienst (systemd/launchd), `allowed_signers`-Zeile |
//! | `user`      | druckt die Schritte für einen eigenen OS-Nutzer samt Service-Unit  |
//! | `managed`   | schreibt nur `managed-settings.minds-proposed.json` (bleibt A1, EA-S1) |
//!
//! # Was nie geschieht
//!
//! - **Kein privilegierter Befehl.** Was Administratorrechte braucht, steht
//!   als Text da (`templates/witness/user-profile.txt`); dieses Modul startet
//!   außer `git` und `ssh-keygen` (über `witness init`/`keygen`) keinen
//!   Prozess. Ein Test sucht den Quelltext danach ab.
//! - **Kein Überschreiben.** Liegt eine Datei schon da und unterscheidet
//!   sich, entsteht daneben `<name>.minds-proposed`, und der Bericht sagt es.
//! - **Kein stiller Start.** Der Dienst wird geschrieben, nicht gestartet; der
//!   Befehl dazu steht im Bericht.

// Außerhalb von Unix gibt es kein Witness-Profil; die Hilfen bleiben ungenutzt.
#![cfg_attr(not(unix), allow(dead_code))]

use std::path::{Path, PathBuf};

/// Die Vorlagen aus EA-S2, unverändert.
const DEVCONTAINER: &str = include_str!("../templates/witness/devcontainer.json");
const COMPOSE: &str = include_str!("../templates/witness/compose.yaml");
/// Der Nutzerdienst des Witness (`container`-Profil).
const SYSTEMD_UNIT: &str = include_str!("../templates/witness/minds-witness.service");
const LAUNCHD_PLIST: &str = include_str!("../templates/witness/minds-witness.plist");
/// Die Schritte und die Service-Unit des `user`-Profils — nur zum Drucken.
const USER_STEPS: &str = include_str!("../templates/witness/user-profile.txt");
const USER_UNIT: &str = include_str!("../templates/witness/minds-witness-user.service");

/// Die beiden Dateien in `.devcontainer/`, die die Isolationsgrenze des
/// `container`-Profils festlegen — `doctor` vergleicht gegen sie.
pub(crate) fn devcontainer_templates() -> [(&'static str, &'static str); 2] {
    [
        ("devcontainer.json", DEVCONTAINER),
        ("compose.yaml", COMPOSE),
    ]
}

/// Der Dateiname des `managed`-Vorschlags an der Repo-Wurzel.
pub(crate) const MANAGED_PROPOSAL: &str = "managed-settings.minds-proposed.json";

/// Die Endung eines Vorschlags neben einer vorhandenen Datei.
pub(crate) const PROPOSED_SUFFIX: &str = ".minds-proposed";

/// Das gewählte Isolationsprofil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Profile {
    Container,
    User,
    Managed,
}

impl Profile {
    pub(crate) fn parse(name: &str) -> std::io::Result<Self> {
        match name {
            "container" => Ok(Self::Container),
            "user" => Ok(Self::User),
            "managed" => Ok(Self::Managed),
            other => Err(std::io::Error::other(format!(
                "unknown witness profile {:?} — known: container, user, managed",
                crate::text::sanitize(other)
            ))),
        }
    }
}

/// Richtet das Profil ein — nach den gewöhnlichen `enable`-Schritten.
///
/// `store` ist die Store-Konfiguration, die der Mensch `enable` gegeben hat:
/// Ein Child-Repo darf der Witness nur von dort übernehmen, nie aus der
/// `.git/config`, die der Agent schreiben kann.
pub(crate) fn run(
    root: &Path,
    profile: Profile,
    store: &minds_store::StoreConfig,
) -> std::io::Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (root, profile, store);
        Err(std::io::Error::other(
            "--witness is not supported on this platform",
        ))
    }
    #[cfg(unix)]
    {
        let root = root.canonicalize()?;
        let child_repo = match store.backend() {
            minds_store::Backend::ChildRepo { path } => Some(path.as_path()),
            minds_store::Backend::InRepo => None,
        };
        match profile {
            Profile::Container => container(&root, child_repo),
            Profile::User => user(&root),
            Profile::Managed => managed(&root),
        }
    }
}

/// Ob `--witness` auf dieser Plattform überhaupt geht — geprüft, bevor
/// `enable` etwas schreibt.
pub(crate) fn supported() -> bool {
    cfg!(unix)
}

/// Was mit einer Datei geschah.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Placed {
    Created,
    Unchanged,
    /// Die vorhandene Datei weicht ab; der Vorschlag liegt hier.
    Proposed(PathBuf),
}

impl Placed {
    fn describe(&self, root: &Path) -> String {
        match self {
            Placed::Created => "created".to_owned(),
            Placed::Unchanged => "unchanged".to_owned(),
            Placed::Proposed(path) => format!(
                "exists and differs — left untouched, proposal in {}",
                shown(root, path)
            ),
        }
    }
}

fn shown(root: &Path, path: &Path) -> String {
    crate::text::sanitize(
        &path
            .strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string(),
    )
}

fn other(err: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(err.to_string())
}

#[cfg(unix)]
fn container(root: &Path, child_repo: Option<&Path>) -> std::io::Result<()> {
    use crate::witness_cmd::{InitRequest, Initialized};

    let root_text = utf8(root)?;
    let name = workspace_name(root)?;
    // Das Binary, das der Dienst startet, darf nicht dort liegen, wo der
    // Agent schreibt — sonst liefe beim nächsten Start Agent-Code auf dem
    // Host, mit Schlüssel und Journal des Witness.
    let exe = service_binary(root)?;
    let home = witness_home(root)?;
    refuse_home_inside(root, &home)?;
    // Alle Ziele vor dem ersten Schreibzugriff prüfen: Ein Symlink in
    // `.devcontainer/` soll nicht erst nach Home und Schlüssel auffallen.
    let devcontainer = root.join(".devcontainer");
    for (file, _) in devcontainer_templates() {
        let path = devcontainer.join(file);
        crate::enable::check_agent_path(root, &path)?;
        crate::enable::check_agent_path(root, &proposal_path(&path))?;
    }
    let service_target = service_path(root, &repo_id(root))?;
    let mapping = format!("/workspaces/{name}={root_text}");
    let initialized = crate::witness_cmd::init_config(
        &home,
        &InitRequest {
            repo: root_text,
            mapping: Some(&mapping),
            profile: Some("container"),
            socket_group: None,
            child_repo,
            policy_rev: None,
        },
    )
    .map_err(other)?;
    // Ab hier kanonisch: Compose bindet genau diesen Pfad (EA-S2), und der
    // Dienst startet den Witness mit ihm.
    let home = home.canonicalize()?;
    refuse_home_inside(root, &home)?;
    let (signers, key) = match crate::witness_cmd::existing_signers_line(&home).map_err(other)? {
        Some(line) => (line, "present"),
        None => (crate::witness_cmd::keygen(&home).map_err(other)?, "created"),
    };

    let mut placed = Vec::new();
    for (file, content) in devcontainer_templates() {
        let path = devcontainer.join(file);
        placed.push((path.clone(), place_in_repo(root, &path, content)?));
    }
    let id = repo_id(root);
    let service = match service_target {
        Some(target) => Some(write_service(&target, &home, &exe, &id)?),
        None => None,
    };

    println!("Witness (container profile)");
    let (state, pins) = match &initialized {
        Initialized::Created(pins) => ("created", Some(pins)),
        Initialized::Pinned(pins) => ("pins added", Some(pins)),
        Initialized::Unchanged(pins) => ("unchanged", Some(pins)),
    };
    println!("  home       {} ({state})", quoted_display(&home));
    for line in pins.map(|pins| pins.lines()).unwrap_or_default() {
        println!("             {}", crate::text::sanitize(&line));
    }
    println!("  key        {key}");
    for (path, outcome) in &placed {
        println!("  {}  {}", shown(root, path), outcome.describe(root));
    }
    match &service {
        Some((path, outcome, _)) => {
            println!(
                "  service    {} {}",
                quoted_display(path),
                outcome.describe(root)
            );
        }
        None => println!("  service    no user service manager known on this platform"),
    }
    println!();
    match &service {
        // Liegt dort eine andere Unit, startete der Befehl sie — womöglich
        // eine alte mit einem Binary, das der Agent ersetzen kann. Erst prüfen
        // und den Vorschlag übernehmen.
        Some((path, Placed::Proposed(proposal), start)) => {
            println!(
                "The existing service file {} differs from what minds would write. Review \
                 {} and move it into place yourself; then start the witness:",
                quoted_display(path),
                quoted_display(proposal)
            );
            for line in start {
                println!("  {line}");
            }
        }
        Some((_, _, start)) => {
            println!("Start the witness yourself — minds does not start it:");
            for line in start {
                println!("  {line}");
            }
        }
        None => println!(
            "Run the witness yourself:\n  {} witness run --home {}",
            shell_quote(utf8(&exe)?),
            shell_quote(utf8(&home)?)
        ),
    }
    if let Some(note) = unstable_binary_note(&exe) {
        println!();
        println!("{note}");
    }
    println!();
    println!(
        "The isolation boundary lives in .devcontainer/ — the agent can edit those files \
         and add new ones (.env, another devcontainer.json). Run `minds doctor` on the host \
         right before every reopen or rebuild of the container (it fails when \
         .devcontainer/ holds anything but the two templates), also after moving a \
         *.minds-proposed file into place."
    );
    if cfg!(target_os = "macos") {
        println!(
            "Note: launchd loads every plist in ~/Library/LaunchAgents at the next login — \
             the witness then starts with it (RunAtLoad)."
        );
    }
    println!();
    println!("Set these on the host before starting the dev container:");
    // SAFETY: getuid/getgid haben keine Vorbedingungen.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    println!("  export MINDS_HOST_UID={uid} MINDS_HOST_GID={gid}");
    println!(
        "  export MINDS_WORKSPACE_ROOT={} MINDS_WORKSPACE_NAME={}",
        shell_quote(root_text),
        shell_quote(&name)
    );
    println!("  export MINDS_WITNESS_HOME={}", shell_quote(utf8(&home)?));
    println!();
    println!(
        "allowed_signers — put this line in a file outside the repository that your team \
         distributes out of band (never commit it here), and pass that file to \
         `minds verify --signers`:"
    );
    println!("  {signers}");
    println!();
    println!("Prove the isolation from inside the container:");
    println!("  minds doctor --probe-home {}", shell_quote(utf8(&home)?));
    if cfg!(target_os = "macos") {
        println!();
        println!(
            "Note: on macOS, container recording stays unqualified until the EA-S2 \
             measurements pass (docs/spikes/ea-s2-container-profile.md)."
        );
    }
    Ok(())
}

/// Das Binary für den Dienst: das laufende, kanonisch — aber nie eines, das
/// im beobachteten Repo liegt (etwa `target/release/minds` beim Dogfooding):
/// Das Repo ist in den Container gemountet, der Agent könnte es ersetzen.
#[cfg(unix)]
fn service_binary(root: &Path) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    if exe.starts_with(root) {
        return Err(other(format!(
            "this minds binary lies inside the repository ({}) — the agent can replace it, \
             and the witness service would run it on the host; install minds outside the \
             repository and run enable from there",
            crate::text::sanitize(&exe.display().to_string())
        )));
    }
    // Ein harter Link teilt den Inode mit einem anderen Namen — etwa
    // `target/release/minds` im Repo. Wer dort in die Datei schreibt,
    // ändert auch das Binary des Dienstes.
    // Gefährlich ist das nur, wenn dieser Inode beschreibbar ist — ein
    // root-eigenes Binary mit mehreren Namen (`/nix/store` mit
    // `auto-optimise-store`) schreibt niemand um.
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(&exe)?;
        // SAFETY: geteuid hat keine Vorbedingungen.
        let mine = meta.uid() == unsafe { libc::geteuid() };
        let writable = (mine && meta.mode() & 0o200 != 0) || meta.mode() & 0o022 != 0;
        if meta.nlink() > 1 && writable {
            return Err(other(format!(
                "this minds binary ({}) is writable and has more than one hard link — \
                 another name for it may lie where the agent can write; install a separate \
                 copy",
                crate::text::sanitize(&exe.display().to_string())
            )));
        }
    }
    utf8(&exe)?;
    Ok(exe)
}

/// Ein Hinweis, wenn das Binary an einem Ort liegt, den ein Update oder eine
/// Garbage-Collection entfernt — der Dienst fände es dann nicht mehr.
fn unstable_binary_note(exe: &Path) -> Option<String> {
    let text = exe.to_string_lossy();
    [
        "/Cellar/",
        "/nix/store/",
        "/target/debug/",
        "/target/release/",
    ]
    .iter()
    .any(|marker| text.contains(marker))
    .then(|| {
        format!(
            "Note: the service runs {} — a version-specific path; after an upgrade, \
                 rerun `minds enable --witness container` from the stable install",
            crate::text::sanitize(&text)
        )
    })
}

/// Der Name des Arbeitsbereichs im Container: der letzte Pfadteil der
/// Repo-Wurzel, so wie `${localWorkspaceFolderBasename}` ihn sieht (EA-S2).
/// `=` trennt in der Pfadabbildung Agent- und Host-Pfad und ist deshalb
/// ausgeschlossen.
#[cfg(unix)]
fn workspace_name(root: &Path) -> std::io::Result<String> {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| other("the repository root has no usable directory name"))?;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.chars().any(char::is_control)
    {
        return Err(other(
            "the repository directory name is not a single path component",
        ));
    }
    if name.contains('=') {
        return Err(other(
            "the repository directory name contains '=' — the container path map cannot \
             carry it; rename the directory",
        ));
    }
    Ok(name.to_owned())
}

/// Das Witness-Home für dieses Repo: `MINDS_WITNESS_HOME` oder der
/// XDG-Zustandspfad (00-conventions) — immer absolut.
#[cfg(unix)]
pub(crate) fn witness_home(root: &Path) -> std::io::Result<PathBuf> {
    let home = crate::witness_cmd::resolve_home_for(None, Some(utf8(root)?)).map_err(other)?;
    Ok(if home.is_absolute() {
        home
    } else {
        std::env::current_dir()?.join(home)
    })
}

/// Das Home darf nicht im Repo liegen und das Repo nicht im Home — auch
/// nicht über einen Symlink (EA-S2: Symlinks vor dem Vergleich auflösen).
/// Geprüft, bevor es angelegt wird: Der nächste vorhandene Vorfahr wird
/// kanonisiert. `..` ist von vornherein ausgeschlossen — ein `..` hinter
/// einem noch fehlenden Verzeichnis ließe sich nicht auflösen.
#[cfg(unix)]
fn refuse_home_inside(root: &Path, home: &Path) -> std::io::Result<()> {
    let resolved = resolve_nearest(home)?;
    if resolved.starts_with(root) || root.starts_with(&resolved) {
        return Err(other(
            "the witness home must lie outside the repository (and must not contain it) — \
             set MINDS_WITNESS_HOME elsewhere",
        ));
    }
    Ok(())
}

/// `<repo-id>` = die ersten 16 Hex von `blake3(kanonische Repo-Wurzel)`.
pub(crate) fn repo_id(root: &Path) -> String {
    let digest = blake3::hash(root.as_os_str().as_encoded_bytes()).to_hex();
    digest[..16].to_owned()
}

/// Ein Pfad als UTF-8 ohne Steuerzeichen — er landet in gedruckten
/// Befehlen, die ein Administrator kopiert; ein Escape-Code oder
/// Zeilenumbruch könnte dort einen anderen Befehl vortäuschen.
fn utf8(path: &Path) -> std::io::Result<&str> {
    let text = path
        .to_str()
        .ok_or_else(|| other(format!("{} is not valid UTF-8", path.display())))?;
    if text.chars().any(char::is_control) {
        return Err(other(format!(
            "{} contains control characters",
            crate::text::sanitize(text)
        )));
    }
    Ok(text)
}

fn quoted_display(path: &Path) -> String {
    format!("\"{}\"", crate::text::sanitize(&path.display().to_string()))
}

/// Ein Wort für die Shell, in einfachen Anführungszeichen.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Legt eine Datei in der Arbeitskopie an — oder, wenn dort schon eine
/// andere liegt, einen Vorschlag daneben. Nie durch einen Symlink.
fn place_in_repo(root: &Path, path: &Path, content: &str) -> std::io::Result<Placed> {
    crate::enable::check_agent_path(root, path)?;
    let proposal = proposal_path(path);
    crate::enable::check_agent_path(root, &proposal)?;
    place(path, &proposal, content)
}

fn proposal_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(PROPOSED_SUFFIX);
    path.with_file_name(name)
}

/// Der gemeinsame Kern: gleich → unverändert; fehlt → anlegen (ohne je
/// etwas zu ersetzen, das inzwischen dort entstand); weicht ab → Vorschlag.
fn place(path: &Path, proposal: &Path, content: &str) -> std::io::Result<Placed> {
    match read_regular(path)? {
        Some(existing) if existing == content => return Ok(Placed::Unchanged),
        Some(_) => {}
        None => {
            crate::enable::create_parent(path)?;
            match create_new(path, content) {
                Ok(()) => return Ok(Placed::Created),
                // Zwischen Prüfung und Anlegen entstanden: nicht ersetzen.
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if read_regular(path)?.as_deref() == Some(content) {
                        return Ok(Placed::Unchanged);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }
    // Der Vorschlag gehört uns: Ist er veraltet, wird er ersetzt — aber nie
    // durch einen Symlink hindurch.
    if read_regular(proposal)?.as_deref() != Some(content) {
        crate::enable::write_atomic_no_follow(proposal, content, false)?;
    }
    Ok(Placed::Proposed(proposal.to_path_buf()))
}

/// Liest eine gewöhnliche Datei bis 1 MiB; `None`, wenn sie fehlt.
pub(crate) fn read_regular(path: &Path) -> std::io::Result<Option<String>> {
    use std::io::Read;
    // Am offenen Deskriptor geprüft, nicht vorher am Pfad: Der Agent kann die
    // Datei zwischen Prüfung und Öffnen gegen einen Symlink, ein FIFO oder
    // `/dev/zero` tauschen.
    let file = match open_no_follow(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(other(format!(
                "{} cannot be opened safely ({err}) — move it aside",
                crate::enable::display_path(path)
            )));
        }
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > crate::enable::MAX_CONFIG_BYTES {
        return Err(other(format!(
            "{} is not a regular file minds compares — move it aside",
            crate::enable::display_path(path)
        )));
    }
    let mut bytes = Vec::new();
    file.take(crate::enable::MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > crate::enable::MAX_CONFIG_BYTES {
        return Err(other(format!(
            "{} grew beyond 1 MiB while being read",
            crate::enable::display_path(path)
        )));
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Öffnet ohne einem Symlink am Blatt zu folgen und ohne zu blockieren.
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    options.open(path)
}

/// Legt `path` neu an; scheitert, wenn dort schon etwas liegt (auch ein
/// Symlink: `O_EXCL` folgt keinem).
fn create_new(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let written = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all());
    if written.is_err() {
        let _ = std::fs::remove_file(path);
    }
    written
}

/// Wohin der Nutzerdienst gehört: systemd (Linux) oder launchd (macOS).
#[derive(Debug, Clone)]
struct ServiceTarget {
    path: PathBuf,
    launchd: bool,
    root: PathBuf,
}

/// Der Ort der Dienstdatei — `None` ohne bekannten Dienstverwalter.
///
/// Nie im beobachteten Repo (etwa ein Dotfiles-Repo unter `$HOME`, oder ein
/// `XDG_CONFIG_HOME` darin): Der Agent könnte `ExecStart` umschreiben, und
/// beim nächsten Start liefe sein Code auf dem Host. Ein relativer
/// `XDG_CONFIG_HOME` gilt nicht (XDG verlangt absolute Pfade).
#[cfg(unix)]
fn service_path(root: &Path, id: &str) -> std::io::Result<Option<ServiceTarget>> {
    let user_home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| other("HOME is not set to an absolute path"))?;
    let (path, launchd) = if cfg!(target_os = "macos") {
        (
            user_home
                .join("Library/LaunchAgents")
                .join(format!("dev.minds.witness.{id}.plist")),
            true,
        )
    } else if cfg!(target_os = "linux") {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| user_home.join(".config"));
        (
            config
                .join("systemd/user")
                .join(format!("minds-witness-{id}.service")),
            false,
        )
    } else {
        return Ok(None);
    };
    if resolve_nearest(&path)?.starts_with(root) {
        return Err(other(format!(
            "the service file would lie inside the repository ({}) — the agent could \
             rewrite it; move HOME/XDG_CONFIG_HOME out of the repository",
            crate::text::sanitize(&path.display().to_string())
        )));
    }
    utf8(&path)?;
    Ok(Some(ServiceTarget {
        path,
        launchd,
        root: root.to_path_buf(),
    }))
}

/// Schreibt die Dienstdatei. Gibt Pfad, Ergebnis und die Befehle zum
/// Starten zurück.
#[cfg(unix)]
fn write_service(
    target: &ServiceTarget,
    home: &Path,
    exe: &Path,
    id: &str,
) -> std::io::Result<(PathBuf, Placed, Vec<String>)> {
    let path = target.path.clone();
    if target.launchd {
        let content = LAUNCHD_PLIST
            .replace("{{REPO_ID}}", id)
            .replace("{{EXE}}", &xml_escape(utf8(exe)?)?)
            .replace("{{HOME}}", &xml_escape(utf8(home)?)?);
        let placed = place(&path, &proposal_path(&path), &content)?;
        let start = vec![format!(
            "launchctl bootstrap gui/$(id -u) {}",
            shell_quote(utf8(&path)?)
        )];
        Ok((path, placed, start))
    } else {
        let unit = format!("minds-witness-{id}.service");
        let content = SYSTEMD_UNIT
            .replace("{{REPO_ID}}", id)
            .replace(
                "{{ROOT_COMMENT}}",
                &crate::text::sanitize(&target.root.display().to_string()),
            )
            .replace("{{EXEC}}", &systemd_word(utf8(exe)?)?)
            .replace("{{HOME}}", &systemd_word(utf8(home)?)?);
        let placed = place(&path, &proposal_path(&path), &content)?;
        let start = vec![
            "systemctl --user daemon-reload".to_owned(),
            format!("systemctl --user enable --now {unit}"),
        ];
        Ok((path, placed, start))
    }
}

/// Der kanonische Ort eines (womöglich noch nicht vorhandenen) Pfads: der
/// nächste vorhandene Vorfahr kanonisiert, der Rest angehängt. `..` ist
/// ausgeschlossen — hinter einem fehlenden Verzeichnis ließe es sich nicht
/// auflösen.
#[cfg(unix)]
fn resolve_nearest(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if absolute
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(other(format!(
            "{} must be given without `..` components",
            crate::text::sanitize(&path.display().to_string())
        )));
    }
    let mut existing = absolute.as_path();
    let mut rest = Vec::new();
    while std::fs::symlink_metadata(existing).is_err() {
        rest.push(existing.file_name().unwrap_or_default().to_owned());
        existing = existing
            .parent()
            .ok_or_else(|| other("path has no existing ancestor"))?;
    }
    let mut resolved = existing.canonicalize()?;
    for part in rest.iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

/// Ein Wort für `ExecStart=`: in Anführungszeichen, `%`-Spezifizierer und
/// `$`-Ersetzung entschärft. Steuerzeichen lehnt es ab.
fn systemd_word(text: &str) -> std::io::Result<String> {
    if text.chars().any(char::is_control) {
        return Err(other("path contains control characters"));
    }
    let escaped = text
        .replace('\\', r"\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    Ok(format!("\"{escaped}\""))
}

fn xml_escape(text: &str) -> std::io::Result<String> {
    if text.chars().any(char::is_control) {
        return Err(other("path contains control characters"));
    }
    Ok(text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

/// Das `user`-Profil: druckt die Schritte samt Service-Unit. Ausgeführt
/// wird nichts — jeder Schritt braucht Administratorrechte.
#[cfg(unix)]
fn user(root: &Path) -> std::io::Result<()> {
    let id = repo_id(root);
    let user = current_user()?;
    if user.chars().any(char::is_control) {
        return Err(other("the user name contains control characters"));
    }
    let steps = USER_STEPS
        .replace("{{UNIT}}", &USER_UNIT.replace("{{REPO_ID}}", &id))
        .replace("{{REPO_ID}}", &id)
        .replace("{{USER}}", &shell_quote(&user))
        .replace("{{ROOT}}", &shell_quote(utf8(root)?));
    if !cfg!(target_os = "linux") {
        println!(
            "Note: these steps are for Linux with systemd; minds generates no user-profile \
             setup for this platform.\n"
        );
    }
    print!("{steps}");
    Ok(())
}

/// Der Login-Name des aufrufenden Nutzers.
#[cfg(unix)]
fn current_user() -> std::io::Result<String> {
    // SAFETY: getpwuid liefert NULL oder einen Zeiger auf eine statische
    // Struktur, deren Name sofort kopiert wird; geteuid hat keine
    // Vorbedingungen.
    let name = unsafe {
        let entry = libc::getpwuid(libc::geteuid());
        if entry.is_null() || (*entry).pw_name.is_null() {
            return Err(other("the current user has no name"));
        }
        std::ffi::CStr::from_ptr((*entry).pw_name)
            .to_string_lossy()
            .into_owned()
    };
    Ok(name)
}

/// Das `managed`-Profil: nur ein Vorschlag für den Administrator, auf die
/// Pfade dieses Repos gemünzt. Er bleibt A1, bis EA-S1 anderes ergibt.
#[cfg(unix)]
fn managed(root: &Path) -> std::io::Result<()> {
    let home = witness_home(root)?;
    refuse_home_inside(root, &home)?;
    // Die Hooks der Policy rufen dieses Binary — es darf nicht im Repo
    // liegen, wo der Agent es ersetzen könnte.
    let exe = service_binary(root)?;
    let content = managed_settings(root, &home, &exe)?;
    let path = root.join(MANAGED_PROPOSAL);
    crate::enable::check_agent_path(root, &path)?;
    let placed = match read_regular(&path)? {
        Some(existing) if existing == content => Placed::Unchanged,
        // Die Datei ist selbst ein Vorschlag — sie gehört uns.
        Some(_) => {
            crate::enable::write_atomic_no_follow(&path, &content, false)?;
            Placed::Created
        }
        None => {
            create_new(&path, &content)?;
            Placed::Created
        }
    };
    println!("Witness (managed profile)");
    println!(
        "  {MANAGED_PROPOSAL}  {}",
        match placed {
            Placed::Unchanged => "unchanged",
            _ => "written",
        }
    );
    // Die Datei liegt im Repo, das der Agent schreiben kann: Der Hash ist
    // der Anker, mit dem der Administrator prüft, dass er genau diesen
    // Vorschlag installiert.
    println!("  blake3     {}", blake3::hash(content.as_bytes()).to_hex());
    println!();
    println!(
        "This is a proposal for your administrator, not an installed policy. The managed \
         profile yields A1 (observed), not A2, until EA-S1 says otherwise \
         (docs/spikes/ea-s1-managed-profile.md)."
    );
    println!(
        "The agent can change files in the repository: compare the file's BLAKE3 digest \
         with the one above (`b3sum {MANAGED_PROPOSAL}`) before installing it."
    );
    Ok(())
}

/// Die Managed-Settings aus EA-S1, mit den Pfaden dieses Repos und dieses
/// Witness statt der Spike-Fixtures.
#[cfg(unix)]
fn managed_settings(root: &Path, home: &Path, exe: &Path) -> std::io::Result<String> {
    // Die Pfade landen in Berechtigungsregeln mit Glob-Syntax: Ein `[`, `*`
    // oder `)` im Pfad ergäbe eine Regel, die den Pfad selbst nicht trifft —
    // und `.claude/` oder das Witness-Home stünden ungeschützt da.
    for path in [root, home, exe] {
        if utf8(path)?
            .chars()
            .any(|c| matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '(' | ')' | '\\'))
        {
            return Err(other(format!(
                "{} contains glob or rule metacharacters — the managed settings could not \
                 protect it; use a path without them",
                crate::text::sanitize(&path.display().to_string())
            )));
        }
    }
    let home = utf8(home)?;
    let claude = format!("{}/.claude", utf8(root)?);
    let exe_text = utf8(exe)?;
    let rule = |verb: &str, path: &str| format!("{verb}(/{path}/**)");
    let mut deny = Vec::new();
    for verb in ["Read", "Edit", "Write"] {
        deny.push(rule(verb, home));
    }
    for verb in ["Edit", "Write"] {
        deny.push(rule(verb, &claude));
        deny.push(rule(verb, "/Library/Application Support/ClaudeCode"));
        deny.push(rule(verb, "/etc/claude-code"));
        deny.push(format!("{verb}(/{exe_text})"));
    }
    let mut hooks = serde_json::Map::new();
    for registration in crate::enable::expected_entries(crate::enable::Which::ClaudeCode) {
        let arguments = registration
            .command
            .strip_prefix("minds ")
            .unwrap_or(&registration.command);
        let mut group = serde_json::json!({
            "hooks": [{
                "type": "command",
                "command": format!("{} {arguments}", shell_quote(exe_text)),
            }]
        });
        if registration.matcher {
            group["matcher"] = ".*".into();
        }
        hooks.insert(registration.event.to_owned(), serde_json::json!([group]));
    }
    let settings = serde_json::json!({
        "$comment": "Proposed by `minds enable --witness managed` (EA-10). The managed profile \
                     yields A1 (observed) until EA-S1 says otherwise; see \
                     docs/spikes/ea-s1-managed-profile.md. Review before installing it as \
                     managed-settings.json.",
        "allowManagedHooksOnly": true,
        "allowManagedPermissionRulesOnly": true,
        "disableAllHooks": false,
        "permissions": {
            "disableBypassPermissionsMode": "disable",
            "deny": deny,
        },
        "sandbox": {
            "enabled": true,
            "failIfUnavailable": true,
            "autoAllowBashIfSandboxed": true,
            "allowUnsandboxedCommands": false,
            "excludedCommands": [],
            "enableWeakerNestedSandbox": false,
            "enableWeakerNetworkIsolation": false,
            "allowAppleEvents": false,
            "filesystem": {
                "disabled": false,
                "allowManagedReadPathsOnly": true,
                "denyRead": [home],
                "denyWrite": [
                    home,
                    claude,
                    "/Library/Application Support/ClaudeCode",
                    "/etc/claude-code",
                    exe_text,
                ],
            },
            "network": {
                "allowManagedDomainsOnly": true,
                "allowedDomains": [],
                "allowUnixSockets": [],
                "allowAllUnixSockets": false,
                "allowLocalBinding": false,
            },
        },
        "hooks": hooks,
    });
    let mut text = serde_json::to_string_pretty(&settings).map_err(other)?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_parse_and_reject_unknown_names() {
        assert_eq!(Profile::parse("container").unwrap(), Profile::Container);
        assert_eq!(Profile::parse("user").unwrap(), Profile::User);
        assert_eq!(Profile::parse("managed").unwrap(), Profile::Managed);
        assert!(Profile::parse("root").is_err());
    }

    #[test]
    fn systemd_words_neutralise_specifiers_and_quotes() {
        assert_eq!(
            systemd_word(r#"/opt/a b/100%$x"y"#).unwrap(),
            r#""/opt/a b/100%%$$x\"y""#
        );
        assert!(systemd_word("/a\nb").is_err());
    }

    #[test]
    fn shell_quoting_survives_single_quotes() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn the_container_templates_are_the_ea_s2_files() {
        assert!(DEVCONTAINER.contains("\"dockerComposeFile\": \"compose.yaml\""));
        assert!(COMPOSE.contains("target: /run/minds-witness"));
        assert!(COMPOSE.contains("MINDS_WITNESS_SOCKET: /run/minds-witness/witness.sock"));
        assert!(COMPOSE.contains("${MINDS_WITNESS_HOME:?"));
    }

    /// Keine Konstante wird mit einem Platzhalter gedruckt, den niemand
    /// ersetzt.
    #[cfg(unix)]
    #[test]
    fn every_placeholder_is_replaced() {
        let id = "0123456789abcdef";
        let steps = USER_STEPS
            .replace("{{UNIT}}", &USER_UNIT.replace("{{REPO_ID}}", id))
            .replace("{{REPO_ID}}", id)
            .replace("{{USER}}", "x")
            .replace("{{ROOT}}", "x");
        assert!(!steps.contains("{{"), "{steps}");
        let unit = SYSTEMD_UNIT
            .replace("{{REPO_ID}}", id)
            .replace("{{ROOT_COMMENT}}", "x")
            .replace("{{EXEC}}", "x")
            .replace("{{HOME}}", "x");
        assert!(!unit.contains("{{"), "{unit}");
        let plist = LAUNCHD_PLIST
            .replace("{{REPO_ID}}", id)
            .replace("{{EXE}}", "x")
            .replace("{{HOME}}", "x");
        assert!(!plist.contains("{{"), "{plist}");
    }

    /// Glob- und Regelzeichen im Pfad ergäben eine Regel, die den Pfad nicht
    /// trifft — abgelehnt statt still ungeschützt.
    #[cfg(unix)]
    #[test]
    fn managed_settings_refuse_glob_metacharacters() {
        let err = managed_settings(
            Path::new("/work/proj[1]"),
            Path::new("/state/minds-witness/abc"),
            Path::new("/usr/local/bin/minds"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("metacharacters"), "{err}");
    }

    /// Steuerzeichen in einem Pfad landeten in Befehlen, die ein Mensch
    /// kopiert.
    #[test]
    fn paths_with_control_characters_are_refused() {
        assert!(utf8(Path::new("/work/a\u{1b}[2Jb")).is_err());
        assert!(utf8(Path::new("/work/plain")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn the_home_is_refused_inside_or_around_the_repository_and_with_dot_dot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        assert!(refuse_home_inside(&root, &root.join("w")).is_err());
        assert!(refuse_home_inside(&root, root.parent().unwrap()).is_err());
        let sneaky = root.parent().unwrap().join("missing/../repo/w");
        assert!(refuse_home_inside(&root, &sneaky).is_err());
        assert!(refuse_home_inside(&root, &root.parent().unwrap().join("state/w")).is_ok());
    }

    #[test]
    fn version_specific_binaries_get_a_note() {
        assert!(
            unstable_binary_note(Path::new("/opt/homebrew/Cellar/minds/0.3.0/bin/minds")).is_some()
        );
        assert!(unstable_binary_note(Path::new("/usr/local/bin/minds")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn managed_settings_target_this_repo_and_carry_the_a1_note() {
        let text = managed_settings(
            Path::new("/work/demo"),
            Path::new("/state/minds-witness/abc"),
            Path::new("/usr/local/bin/minds"),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(value["$comment"].as_str().unwrap().contains("A1"));
        let deny = value["permissions"]["deny"].to_string();
        assert!(
            deny.contains("Read(//state/minds-witness/abc/**)"),
            "{deny}"
        );
        assert!(deny.contains("Write(//work/demo/.claude/**)"), "{deny}");
        assert_eq!(
            value["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "'/usr/local/bin/minds' hook --agent claude-code"
        );
        assert_eq!(value["hooks"]["PostToolUse"][0]["matcher"], ".*");
        assert!(!text.contains("minds-ea-s1"), "keine Spike-Fixtures");
    }
}
