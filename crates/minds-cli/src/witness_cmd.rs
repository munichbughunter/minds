//! Witness key setup, independent of the agent's signing configuration.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

pub fn run(command: Option<&str>, home: Option<&str>) -> ExitCode {
    let result = (|| {
        if command != Some("keygen") {
            return Err("expected: minds witness keygen [--home <directory>]".into());
        }
        keygen(&resolve_home(home)?)
    })();
    match result {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("minds witness: {err}");
            ExitCode::FAILURE
        }
    }
}

fn resolve_home(explicit: Option<&str>) -> Fallible<PathBuf> {
    if let Some(home) = explicit {
        if home.is_empty() {
            return Err("--home must not be empty".into());
        }
        return Ok(home.into());
    }
    if let Some(home) = std::env::var_os("MINDS_WITNESS_HOME").filter(|s| !s.is_empty()) {
        return Ok(home.into());
    }
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err("outside a worktree: pass --home or set MINDS_WITNESS_HOME".into());
    }
    let root = Path::new(String::from_utf8(output.stdout)?.trim()).canonicalize()?;
    let digest = blake3::hash(root.as_os_str().as_encoded_bytes()).to_hex();
    let state = match std::env::var_os("XDG_STATE_HOME").filter(|s| !s.is_empty()) {
        Some(state) => PathBuf::from(state),
        None => {
            PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(".local/state")
        }
    };
    Ok(state.join("minds-witness").join(&digest[..16]))
}

fn private_directory(path: &Path) -> Fallible<()> {
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
        // SAFETY: geteuid takes no arguments and has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(format!(
                "{} must be owned by this user with mode 0700",
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn keygen(home: &Path) -> Fallible<String> {
    // Strip trailing separators / `.` so symlink_metadata inspects the home
    // itself instead of following a directory symlink through a trailing `/`.
    let home: PathBuf = home.components().collect();
    private_directory(&home)?;
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
    Ok(format!(
        "{principal} namespaces=\"{}\" {algorithm} {material}",
        minds_attest::NS_WITNESS
    ))
}
