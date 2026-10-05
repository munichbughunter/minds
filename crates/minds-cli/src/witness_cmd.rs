//! Witness key setup, independent of the agent's signing configuration.

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(unix)]
use std::process::{Command, Stdio};

#[cfg(unix)]
type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

#[cfg(unix)]
mod daemon;

#[cfg(unix)]
pub(crate) use daemon::{InitRequest, Initialized, UNPINNED, init_config, load, ping};

/// Das versteckte Unterkommando, unter dem der Witness seinen Checkpoint als
/// eigenen Prozess startet (EA-10). Nicht in USAGE — niemand ruft es von Hand.
#[cfg(unix)]
pub(crate) const WORKER_COMMAND: &str = "__checkpoint";

pub fn run(parsed: &crate::Parsed) -> ExitCode {
    #[cfg(not(unix))]
    {
        let _ = parsed;
        eprintln!("minds witness: not supported on this platform");
        ExitCode::from(4)
    }
    #[cfg(unix)]
    run_unix(parsed)
}

#[cfg(unix)]
fn run_unix(parsed: &crate::Parsed) -> ExitCode {
    let result = (|| {
        let command = parsed
            .positional(0)
            .ok_or("expected: witness init, run, keygen or status")?;
        if command != "init"
            && [
                "--repo",
                "--path-map",
                "--profile",
                "--socket-group",
                "--child-repo",
                "--policy-rev",
            ]
            .iter()
            .any(|flag| parsed.value(flag).is_some())
        {
            return Err(
                "--repo, --path-map, --profile, --socket-group, --child-repo and --policy-rev require witness init"
                    .into(),
            );
        }
        if command != "run" && parsed.has("--follow") {
            return Err("--follow requires witness run".into());
        }
        if command == WORKER_COMMAND {
            // Der Worker läuft nur mit dem Home, das ihm sein Witness nennt —
            // nie mit einem aus Umgebung oder Repo erratenen.
            let home = parsed.value("--home").ok_or("worker requires --home")?;
            return daemon::worker(Path::new(home)).map(|()| String::new());
        }
        let home = resolve_home_for(parsed.value("--home"), parsed.value("--repo"))?;
        match command {
            "keygen" => keygen(&home),
            "init" => daemon::init(
                &home,
                &InitRequest {
                    repo: parsed.value("--repo").ok_or("init requires --repo")?,
                    mapping: parsed.value("--path-map"),
                    profile: parsed.value("--profile"),
                    socket_group: parsed.value("--socket-group"),
                    child_repo: parsed.value("--child-repo").map(Path::new),
                    policy_rev: parsed.value("--policy-rev"),
                },
            ),
            "run" => {
                daemon::run(&home, parsed.has("--follow"))?;
                Ok(String::new())
            }
            "status" => daemon::status(&home),
            _ => Err("expected: witness init, run, keygen or status".into()),
        }
    })();
    match result {
        Ok(line) => {
            if !line.is_empty() {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!(
                "minds witness: {}",
                crate::hooklog::diagnostic(&err.to_string())
            );
            ExitCode::from(4)
        }
    }
}

#[cfg(unix)]
pub(crate) fn resolve_home_for(explicit: Option<&str>, repo: Option<&str>) -> Fallible<PathBuf> {
    if let Some(home) = explicit {
        if home.is_empty() {
            return Err("--home must not be empty".into());
        }
        return Ok(home.into());
    }
    if let Some(home) = std::env::var_os("MINDS_WITNESS_HOME").filter(|s| !s.is_empty()) {
        return Ok(home.into());
    }
    let start = Path::new(repo.unwrap_or(".")).canonicalize()?;
    let root =
        repo_root_of(&start).ok_or("outside a worktree: pass --home or set MINDS_WITNESS_HOME")?;
    home_for_root(&root)
}

/// Die Repo-Wurzel über dem Pfad `start`: das erste Verzeichnis mit einem
/// `.git`. Bewusst ohne `git rev-parse`: Das folgte `core.worktree` aus der
/// `.git/config`, die der Agent schreiben kann — und ein umgelenkter Pfad
/// ergäbe ein anderes Witness-Home (EA-10).
#[cfg(unix)]
pub(crate) fn repo_root_of(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| std::fs::symlink_metadata(dir.join(".git")).is_ok())
        .map(Path::to_path_buf)
}

/// Das Standard-Home für eine schon kanonische Repo-Wurzel:
/// `$XDG_STATE_HOME/minds-witness/<repo-id>` (00-conventions).
#[cfg(unix)]
pub(crate) fn home_for_root(root: &Path) -> Fallible<PathBuf> {
    let digest = blake3::hash(root.as_os_str().as_encoded_bytes()).to_hex();
    let state = match std::env::var_os("XDG_STATE_HOME").filter(|s| !s.is_empty()) {
        Some(state) => PathBuf::from(state),
        None => {
            PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(".local/state")
        }
    };
    Ok(state.join("minds-witness").join(&digest[..16]))
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Fallible<()> {
    private_directory_shared(path, None)
}

/// Wie [`private_directory`]; mit `shared_gid` darf das Verzeichnis dieser
/// Gruppe zusätzlich das Durchqueren erlauben (0710) — das Home im
/// `user`-Profil, wenn seine Gruppe die Socket-Gruppe ist.
#[cfg(unix)]
fn private_directory_shared(path: &Path, shared_gid: Option<u32>) -> Fallible<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!("{} must be a directory, not a symlink", path.display()).into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let traverse = match shared_gid {
            Some(gid) if metadata.gid() == gid => 0o010,
            _ => 0,
        };
        // SAFETY: geteuid takes no arguments and has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 & !traverse != 0
        {
            return Err(format!(
                "{} must be owned by this user with mode 0700",
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn keygen(home: &Path) -> Fallible<String> {
    // Strip trailing separators / `.` so symlink_metadata inspects the home
    // itself instead of following a directory symlink through a trailing `/`.
    let home: PathBuf = home.components().collect();
    // Im `user`-Profil steht das Home der Socket-Gruppe zum Durchqueren
    // offen (0710) — das sagt die schon eingerichtete Konfiguration.
    let shared_gid = daemon::load(&home)
        .ok()
        .and_then(|config| config.socket_group());
    private_directory_shared(&home, shared_gid)?;
    let key_dir = home.join("key");
    private_directory(&key_dir)?;
    let key = key_dir.join("witness_ed25519");
    let public = key_dir.join("witness_ed25519.pub");
    for path in [&key, &public] {
        match std::fs::symlink_metadata(path) {
            Ok(_) => return Err(format!("refusing to overwrite {}", path.display()).into()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    let host = Command::new("hostname").stdin(Stdio::null()).output()?;
    if !host.status.success() {
        return Err("hostname failed".into());
    }
    let host = String::from_utf8(host.stdout)?;
    let host = host.trim();
    // Keep the principal a literal token in allowed_signers (no patterns or options).
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
    {
        return Err("hostname is not a valid witness principal".into());
    }
    let principal = format!("minds-witness@{host}");
    // Generate in a private, random directory. Publish with hard_link's atomic
    // no-clobber semantics: concurrent keygens and dangling symlinks cannot
    // make ssh-keygen overwrite an existing private or public key.
    let temporary = tempfile::tempdir_in(&key_dir)?;
    let generated = temporary.path().join("witness_ed25519");
    let output = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", &principal, "-f"])
        .arg(&generated)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let generated_public = generated.with_extension("pub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&generated, &generated_public] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    let pubkey = std::fs::read_to_string(&generated_public)?;
    let mut fields = pubkey.split_whitespace();
    let algorithm = fields.next().ok_or("missing public key algorithm")?;
    let material = fields.next().ok_or("missing public key material")?;
    std::fs::hard_link(&generated, &key)?;
    std::fs::hard_link(&generated_public, &public)?;
    Ok(signers_line(&principal, algorithm, material))
}

#[cfg(unix)]
fn signers_line(principal: &str, algorithm: &str, material: &str) -> String {
    format!(
        "{principal} namespaces=\"{}\" {algorithm} {material}",
        minds_attest::NS_WITNESS
    )
}

/// Die `allowed_signers`-Zeile eines schon vorhandenen Witness-Schlüssels —
/// für ein erneutes `minds enable --witness`, das `keygen` überspringt. Der
/// Principal steht als Kommentar im öffentlichen Schlüssel, so wie `keygen`
/// ihn geschrieben hat.
#[cfg(unix)]
pub(crate) fn existing_signers_line(home: &Path) -> Fallible<Option<String>> {
    let public = home.join("key/witness_ed25519.pub");
    let private = home.join("key/witness_ed25519");
    let regular = |path: &Path| match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(true),
        Ok(_) => Err(format!("{} is not a regular file", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.to_string()),
    };
    match (regular(&private)?, regular(&public)?) {
        (false, false) => return Ok(None),
        (true, true) => {}
        // Ein halbes Schlüsselpaar ist weder „vorhanden" noch neu zu
        // erzeugen: keygen überschreibt nie, und der Witness startet nicht.
        _ => {
            return Err(
                "witness key pair is incomplete — remove key/witness_ed25519* and rerun".into(),
            );
        }
    }
    let mut text = String::new();
    std::io::Read::read_to_string(
        &mut std::io::Read::take(std::fs::File::open(&public)?, 16 * 1024),
        &mut text,
    )?;
    let mut fields = text.split_whitespace();
    let (Some(algorithm), Some(material), Some(principal)) =
        (fields.next(), fields.next(), fields.next())
    else {
        return Err("witness public key is malformed".into());
    };
    let token = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=-@.".contains(&b))
    };
    if !token(algorithm) || !token(material) {
        return Err("witness public key is malformed".into());
    }
    if !principal.starts_with("minds-witness@")
        || !principal
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@.-_".contains(&b))
    {
        return Err("witness public key carries no valid principal".into());
    }
    Ok(Some(signers_line(principal, algorithm, material)))
}
