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
pub(crate) use daemon::{
    CONTROL_SOCKET, InitRequest, Initialized, UNPINNED, init_config, intent_key_line,
    key_display_with, load, observer_status, peer_uid, ping, witness_keygen,
};

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
            .ok_or("expected: witness init, run, keygen, signers, status or activate")?;
        if command != "init"
            && [
                "--repo",
                "--path-map",
                "--profile",
                "--socket-group",
                "--child-repo",
                "--policy-rev",
                "--intent-key",
                "--replace-intent-key",
            ]
            .iter()
            .any(|flag| parsed.value(flag).is_some())
        {
            return Err(
                "--repo, --path-map, --profile, --socket-group, --child-repo, --policy-rev, \
                 --intent-key and --replace-intent-key require witness init"
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
        if parsed.value("--intent-key").is_some() && parsed.value("--replace-intent-key").is_some()
        {
            return Err("--intent-key and --replace-intent-key exclude each other".into());
        }
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
                    intent_key: parsed
                        .value("--intent-key")
                        .or(parsed.value("--replace-intent-key")),
                    replace_intent_key: parsed.value("--replace-intent-key").is_some(),
                },
            ),
            "run" => {
                daemon::run(&home, parsed.has("--follow"))?;
                Ok(String::new())
            }
            "status" => daemon::status(&home),
            // Die Signer-Zeile eines vorhandenen Schlüssels — für die
            // geschützte Einrichtung, die das Home nur als Witness-Nutzer
            // lesen kann.
            "signers" => existing_signers_line(&home)?
                .ok_or_else(|| "no witness key — run minds witness keygen".into()),
            "activate" => activate(&home),
            _ => Err("expected: witness init, run, keygen, signers, status or activate".into()),
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

/// Höchstens so viel liest `witness activate` von stdin.
#[cfg(unix)]
const MAX_ACTIVATE_REQUEST: u64 = 64 * 1024;

/// `minds witness activate`: aktiviert einen Intent über den Steuer-Socket
/// dieses Homes — als der Witness-Nutzer selbst, denn nur dessen
/// Verbindungen nimmt der Steuer-Socket an. Beim geschützten Witness ruft
/// `minds intent sign` das mit der einen Erhöhung auf (das Passwort des
/// Menschen ist der Beleg). Anker und Signatur kommen als JSON über stdin
/// (`{"anchor": …, "signature": …}`); der Anker `-` hebt die Aktivierung
/// auf. Ausgegeben wird die Statuszeile des Witness.
#[cfg(unix)]
fn activate(home: &Path) -> Fallible<String> {
    use minds_capture::witness_proto::Frame;
    use std::io::Read;

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        anchor: String,
        #[serde(default)]
        signature: Option<String>,
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_ACTIVATE_REQUEST + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ACTIVATE_REQUEST {
        return Err("activation request too large".into());
    }
    let request: Request =
        serde_json::from_slice(&bytes).map_err(|_| "activation request unreadable")?;
    let socket = std::path::absolute(home)?.join(CONTROL_SOCKET);
    let frame = Frame::IntentActivate {
        anchor: request.anchor,
        signature: request.signature,
        request_id: crate::intent_cmd::request_id(),
    };
    // SAFETY: geteuid hat keine Vorbedingungen.
    let euid = unsafe { libc::geteuid() };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    match crate::hook::witness::request(&socket, Some(euid), &frame, deadline) {
        Ok(Frame::Ack { status, .. }) => Ok(crate::text::sanitize(&status)),
        Ok(Frame::Nack { reason, .. }) => {
            Err(format!("the witness refused it: {}", crate::text::sanitize(&reason)).into())
        }
        Ok(_) => Err("witness control socket: unexpected response".into()),
        Err(kind) => Err(format!("witness control socket not reachable ({kind})").into()),
    }
}

/// Der Rechnername (`gethostname`).
#[cfg(unix)]
fn host_name() -> Fallible<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: Puffer und Länge passen zusammen; `gethostname` schreibt
    // höchstens so viele Bytes.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return Err("hostname failed".into());
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    Ok(String::from_utf8(buffer[..end].to_vec())?)
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
    let config = daemon::load(&home).ok();
    let shared_gid = config.as_ref().and_then(|config| config.socket_group());
    // Ein Witness unter eigener Kennung (`user`) bekommt den Principal, unter
    // dem ein Mensch ihn als getrennt einträgt — aber nur, wenn er hier
    // wirklich getrennt läuft (eigene Kennung, privates Home, geschütztes
    // Binary); sonst den gewöhnlichen: Wer die Zeile übernimmt, bürgt.
    let user_profile = config
        .as_ref()
        .is_some_and(|config| config.profile() == minds_core::evidence::WitnessProfile::User);
    let separated = user_profile && daemon::separated_here(&home, false);
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
    if user_profile && !separated {
        eprintln!(
            "minds witness keygen: this witness does not run separated from the repository \
             owner (own user ID, private home, root-owned binary) — printing the ordinary \
             principal; its ranges stay A1"
        );
    }
    // Im Prozess, nicht über ein `hostname` aus dem `PATH`.
    let host = host_name()?;
    let host = host.trim();
    // Keep the principal a literal token in allowed_signers (no patterns or options).
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
    {
        return Err("hostname is not a valid witness principal".into());
    }
    let principal = if separated {
        format!(
            "{}{host}",
            minds_core::evidence::SEPARATED_WITNESS_PRINCIPAL
        )
    } else {
        format!("minds-witness@{host}")
    };
    // Generate in a private, random directory. Publish with hard_link's atomic
    // no-clobber semantics: concurrent keygens and dangling symlinks cannot
    // make ssh-keygen overwrite an existing private or public key.
    let temporary = tempfile::tempdir_in(&key_dir)?;
    let generated = temporary.path().join("witness_ed25519");
    // Dasselbe `ssh-keygen` wie der Dienst (fest, ohne geerbte Umgebung):
    // `keygen` kann als Witness-Nutzer mit dem `PATH` des Entwicklers laufen.
    let program = daemon::witness_keygen().ok_or("no root-owned ssh-keygen found")?;
    let output = Command::new(&program)
        .env_clear()
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
    // Den Principal eines getrennten Witness druckt nur ein Home des
    // Profils `user` — im Home eines Witness unter der Kennung des Agenten
    // könnte der Agent den Kommentar umschreiben, und wer die Zeile übernimmt,
    // bürgte ahnungslos für eine Trennung.
    let separated = principal.starts_with(minds_core::evidence::SEPARATED_WITNESS_PRINCIPAL);
    if separated && !daemon::separated_here(home, true) {
        return Err(format!(
            "witness public key claims the principal of a separated witness ({}…), but this \
             witness does not run separated (user profile, own user ID, private home, \
             root-owned binary) — remove key/witness_ed25519* and rerun",
            minds_core::evidence::SEPARATED_WITNESS_PRINCIPAL
        )
        .into());
    }
    if !(principal.starts_with("minds-witness@") || separated)
        || !principal
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@.-_".contains(&b))
    {
        return Err("witness public key carries no valid principal".into());
    }
    Ok(Some(signers_line(principal, algorithm, material)))
}
