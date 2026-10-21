//! Die Vertrauensdatei und der Freigabe-Schlüssel des Nutzers — was `minds
//! enable` einmal je Nutzer anlegt, damit `verify`, `inspect` und `intent
//! sign` ohne Zutun des Menschen zusammenfinden.
//!
//! - **`~/.config/minds/allowed_signers`** (`$XDG_CONFIG_HOME/minds/…`): die
//!   Signer-Zeilen des Witness (`namespaces="minds-witness"`) und des
//!   eigenen Intent-Schlüssels (`namespaces="minds-intent"`). `enable` hängt
//!   jede Zeile genau einmal an (idempotent) und zeigt mit `git config
//!   --global minds.signers` auf die Datei. `minds verify` und die TUI
//!   nehmen sie als Vorgabe ([`resolve`]); `--signers` geht vor.
//! - **`~/.config/minds/intent_ed25519`**: ein Software-Schlüssel ohne
//!   Passphrase, mit dem `minds intent sign` ohne `--key` signiert
//!   (`git config --global minds.intentKey`). Ein Sicherheitsschlüssel
//!   (FIDO) bleibt Opt-in: Steht in `minds.intentKey` schon ein anderer
//!   Schlüssel, legt `enable` keinen an.
//!
//! Die Datei liegt im Home des Nutzers — ein Agent unter derselben Kennung
//! kann sie schreiben. Für einen Auditor gilt deshalb weiter: eigene Datei
//! außerhalb jeder Agenten-Reichweite, `--signers`. Was `enable` hier tut,
//! erspart dem Entwickler das Kopieren der Zeile, nicht dem Auditor sein
//! Urteil.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Der globale Konfigurationsschlüssel für die Vertrauensdatei.
pub(crate) const SIGNERS_KEY: &str = "minds.signers";
/// Der Konfigurationsschlüssel für den eigenen Freigabe-Schlüssel.
pub(crate) const INTENT_KEY_KEY: &str = "minds.intentKey";

/// Dateiname der Vertrauensdatei unter dem Konfigurationsverzeichnis.
const SIGNERS_FILE: &str = "allowed_signers";
/// Dateiname des Freigabe-Schlüssels unter dem Konfigurationsverzeichnis.
const INTENT_KEY_FILE: &str = "intent_ed25519";

/// Höchstgröße der Vertrauensdatei, die gelesen wird (wie `witness_trust`).
const MAX_SIGNERS_BYTES: u64 = 1024 * 1024;

/// `$XDG_CONFIG_HOME/minds`, sonst `~/.config/minds`.
pub(crate) fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        let xdg = PathBuf::from(xdg);
        if xdg.is_absolute() {
            return Some(xdg.join("minds"));
        }
    }
    let home = std::env::var_os("HOME").filter(|s| !s.is_empty())?;
    Some(PathBuf::from(home).join(".config").join("minds"))
}

/// Die Vorgabe für die Vertrauensdatei (`~/.config/minds/allowed_signers`).
pub(crate) fn signers_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(SIGNERS_FILE))
}

/// Der eigene Freigabe-Schlüssel (`~/.config/minds/intent_ed25519`).
pub(crate) fn intent_key_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(INTENT_KEY_FILE))
}

/// Woher die Signer-Datei stammt — für die Ausgabe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignersOrigin {
    /// `--signers`.
    Flag,
    /// `git config --global minds.signers`.
    Config,
    /// Die Vorgabe `~/.config/minds/allowed_signers`.
    Default,
    /// Die frühere Vorgabe `~/.ssh/allowed_signers`.
    Legacy,
}

impl SignersOrigin {
    fn word(self) -> &'static str {
        match self {
            Self::Flag => "--signers",
            Self::Config => SIGNERS_KEY,
            Self::Default => "default",
            Self::Legacy => "~/.ssh/allowed_signers",
        }
    }
}

/// Die aufgelöste Signer-Datei.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Signers {
    pub(crate) path: PathBuf,
    pub(crate) origin: SignersOrigin,
}

impl Signers {
    /// `~/.config/minds/allowed_signers (minds.signers)` — Pfad und Quelle,
    /// entschärft.
    pub(crate) fn describe(&self) -> String {
        format!(
            "{} ({})",
            crate::text::sanitize(&tilde(&self.path)),
            self.origin.word()
        )
    }
}

/// Die vertrauenswürdige Signer-Datei: `--signers`, sonst `minds.signers`
/// aus der **globalen** Git-Konfiguration (nie der des Repos, die der Agent
/// schreibt), sonst `~/.config/minds/allowed_signers`, sonst die frühere
/// Vorgabe `~/.ssh/allowed_signers` — die Vorgaben nur, wenn dort eine
/// reguläre Datei liegt; `--signers` gilt immer.
pub(crate) fn resolve(explicit: Option<&str>) -> Option<Signers> {
    if let Some(explicit) = explicit {
        return Some(Signers {
            path: PathBuf::from(explicit),
            origin: SignersOrigin::Flag,
        });
    }
    if let Some(configured) = global_config(SIGNERS_KEY) {
        let path = PathBuf::from(&configured);
        if path.is_absolute() && path.is_file() {
            return Some(Signers {
                path,
                origin: SignersOrigin::Config,
            });
        }
    }
    if let Some(path) = signers_path().filter(|path| path.is_file()) {
        return Some(Signers {
            path,
            origin: SignersOrigin::Default,
        });
    }
    let home = std::env::var_os("HOME").filter(|s| !s.is_empty())?;
    let legacy = Path::new(&home).join(".ssh").join(SIGNERS_FILE);
    legacy.is_file().then_some(Signers {
        path: legacy,
        origin: SignersOrigin::Legacy,
    })
}

/// Ein Wert aus der globalen Git-Konfiguration — nie aus der des Repos.
pub(crate) fn global_config(key: &str) -> Option<String> {
    let output = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["config", "--global", "--get", key])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !value.is_empty()).then_some(value)
}

/// Setzt einen Wert in der globalen Git-Konfiguration.
fn set_global_config(key: &str, value: &str) -> std::io::Result<()> {
    let output = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["config", "--global", key, value])
        .stdin(std::process::Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "`git config --global {key}` failed: {}",
            crate::text::sanitize(String::from_utf8_lossy(&output.stderr).trim())
        )))
    }
}

/// `~/…` statt des vollen Home-Pfads — nur für die Anzeige.
pub(crate) fn tilde(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").filter(|s| !s.is_empty()) {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// Was [`ensure_signer_line`] vorfand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recorded {
    Added,
    Present,
}

/// Hängt `line` an die Vertrauensdatei an, wenn sie dort noch nicht steht —
/// Zeile für Zeile verglichen, ohne Rand. Datei und Verzeichnis entstehen
/// mit 0600/0700; ein Symlink oder etwas anderes als eine reguläre Datei
/// wird nicht beschrieben.
pub(crate) fn ensure_signer_line(line: &str) -> std::io::Result<Recorded> {
    let line = line.trim();
    if line.is_empty() || line.chars().any(char::is_control) {
        return Err(std::io::Error::other(
            "the signers line is not a plain line",
        ));
    }
    let path = signers_path().ok_or_else(|| std::io::Error::other("HOME is not set"))?;
    ensure_signer_line_at(&path, line)
}

/// [`ensure_signer_line`] für genau diese Datei.
fn ensure_signer_line_at(path: &Path, line: &str) -> std::io::Result<Recorded> {
    let line = line.trim();
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the signers file has no directory"))?;
    private_dir(dir)?;
    let existing = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {
            if meta.len() > MAX_SIGNERS_BYTES {
                return Err(std::io::Error::other(format!(
                    "{} is larger than 1 MiB",
                    crate::text::sanitize(&tilde(path))
                )));
            }
            let mut text = String::new();
            std::fs::File::open(path)?
                .take(MAX_SIGNERS_BYTES)
                .read_to_string(&mut text)?;
            Some(text)
        }
        Ok(_) => {
            return Err(std::io::Error::other(format!(
                "{} is not a regular file",
                crate::text::sanitize(&tilde(path))
            )));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };
    if existing
        .as_deref()
        .is_some_and(|text| text.lines().any(|known| known.trim() == line))
    {
        return Ok(Recorded::Present);
    }
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let mut text = String::new();
    if existing.is_some_and(|text| !text.is_empty() && !text.ends_with('\n')) {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(Recorded::Added)
}

/// Legt `dir` mit 0700 an (rekursiv), ohne einem Symlink zu folgen.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(std::io::Error::other(format!(
            "{} is not a directory",
            crate::text::sanitize(&tilde(dir))
        )));
    }
    Ok(())
}

/// Was [`ensure_intent_key`] ergab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IntentKey {
    /// Neu erzeugt, `minds.intentKey` gesetzt, Zeile in der Vertrauensdatei.
    Created(PathBuf),
    /// Lag schon da; `minds.intentKey` und Vertrauensdatei geprüft.
    Present(PathBuf),
    /// `minds.intentKey` nennt einen anderen Schlüssel (etwa einen
    /// Sicherheitsschlüssel) — der bleibt, nichts wird angelegt.
    Foreign(String),
}

/// Der eigene Freigabe-Schlüssel: einmal je Nutzer unter
/// `~/.config/minds/intent_ed25519` (ed25519, ohne Passphrase), mit
/// `minds.intentKey` darauf und seiner `minds-intent`-Zeile in der
/// Vertrauensdatei. `identity` ist der Principal der Zeile (und der
/// Kommentar des Schlüssels).
pub(crate) fn ensure_intent_key(identity: &str) -> std::io::Result<IntentKey> {
    let path = intent_key_path().ok_or_else(|| std::io::Error::other("HOME is not set"))?;
    let configured = global_config(INTENT_KEY_KEY);
    if let Some(configured) = &configured {
        if Path::new(configured) != path {
            return Ok(IntentKey::Foreign(configured.clone()));
        }
    }
    let public = path.with_extension("pub");
    let regular = |file: &Path| match std::fs::symlink_metadata(file) {
        Ok(meta) if meta.is_file() => Ok(true),
        Ok(_) => Err(std::io::Error::other(format!(
            "{} is not a regular file",
            crate::text::sanitize(&tilde(file))
        ))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    };
    let created = match (regular(&path)?, regular(&public)?) {
        (true, true) => false,
        (false, false) => {
            generate(&path, identity)?;
            true
        }
        _ => {
            return Err(std::io::Error::other(format!(
                "the intent key pair {} is incomplete — remove it and rerun minds enable",
                crate::text::sanitize(&tilde(&path))
            )));
        }
    };
    let line = crate::intent_cmd::read_public_key(&path.to_string_lossy())
        .ok_or_else(|| std::io::Error::other("the intent public key is unreadable"))?;
    let signers = intent_signer_line(identity, &line)?;
    ensure_signer_line(&signers)?;
    if configured.is_none() {
        set_global_config(INTENT_KEY_KEY, &path.to_string_lossy())?;
    }
    Ok(if created {
        IntentKey::Created(path)
    } else {
        IntentKey::Present(path)
    })
}

/// `<identity> namespaces="minds-intent" <typ> <base64>` aus einer
/// `.pub`-Zeile — ohne deren Kommentar.
fn intent_signer_line(identity: &str, public: &str) -> std::io::Result<String> {
    let mut words = public.split_whitespace();
    let (Some(kind), Some(blob)) = (words.next(), words.next()) else {
        return Err(std::io::Error::other("the intent public key is malformed"));
    };
    Ok(format!(
        "{identity} namespaces=\"{}\" {kind} {blob}",
        minds_attest::NS_INTENT
    ))
}

/// Erzeugt das Schlüsselpaar: in einem privaten Zufallsverzeichnis daneben,
/// dann per `hard_link` (nie überschreibend) an seinen Platz — wie
/// `witness keygen`.
fn generate(path: &Path, identity: &str) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the intent key has no directory"))?;
    private_dir(dir)?;
    let temporary = tempfile::tempdir_in(dir)?;
    let generated = temporary.path().join(INTENT_KEY_FILE);
    let program = keygen_program();
    let comment = format!("{identity} (minds intent)");
    let output = Command::new(&program)
        .args(["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f"])
        .arg(&generated)
        .stdin(std::process::Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "ssh-keygen failed: {}",
            crate::text::sanitize(String::from_utf8_lossy(&output.stderr).trim())
        )));
    }
    let generated_public = generated.with_extension("pub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in [&generated, &generated_public] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    std::fs::hard_link(&generated, path)?;
    std::fs::hard_link(&generated_public, path.with_extension("pub"))?;
    Ok(())
}

/// Das `ssh-keygen` für den eigenen Schlüssel: das root-eigene, das auch
/// der Witness nimmt, sonst das erste im `PATH`.
fn keygen_program() -> PathBuf {
    #[cfg(unix)]
    if let Some(program) = crate::witness_cmd::witness_keygen() {
        return program;
    }
    PathBuf::from("ssh-keygen")
}

/// Der Principal für die eigene `minds-intent`-Zeile: `user.email`, wenn es
/// ein schlichtes Wort ist (ohne Leerzeichen, Anführungszeichen oder
/// Muster), sonst `<nutzer>@<rechner>`.
pub(crate) fn identity(root: &Path) -> String {
    let plain = |text: &str| {
        !text.is_empty()
            && text.len() <= 256
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"@.+-_".contains(&b))
    };
    if let Some(email) = git_config(root, "user.email").filter(|email| plain(email)) {
        return email;
    }
    let user = std::env::var("USER")
        .ok()
        .filter(|user| plain(user))
        .unwrap_or_else(|| "developer".to_owned());
    let host = host_name()
        .filter(|host| plain(host))
        .unwrap_or_else(|| "localhost".to_owned());
    format!("{user}@{host}")
}

fn git_config(root: &Path, key: &str) -> Option<String> {
    let output = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .arg("-C")
        .arg(root)
        .args(["config", "--get", key])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !value.is_empty()).then_some(value)
}

#[cfg(unix)]
fn host_name() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: Puffer und Länge passen zusammen; `gethostname` schreibt
    // höchstens so viele Bytes.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return None;
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    let host = String::from_utf8(buffer[..end].to_vec()).ok()?;
    // Nur der Rechnername, nicht die Domäne.
    Some(host.split('.').next().unwrap_or(&host).to_owned())
}

#[cfg(not(unix))]
fn host_name() -> Option<String> {
    None
}

/// Legt den Freigabe-Schlüssel an (oder findet ihn) und sagt, was gilt —
/// für `minds enable`; ein Fehler ist eine Warnung, nie ein gescheitertes
/// `enable`.
pub(crate) fn report_intent_key(root: &Path) {
    match ensure_intent_key(&identity(root)) {
        Ok(IntentKey::Created(path)) => eprintln!(
            "minds: intent key {} created (software key, no passphrase) — {INTENT_KEY_KEY} set, \
             its minds-intent line recorded in {}",
            crate::text::sanitize(&tilde(&path)),
            signers_display()
        ),
        Ok(IntentKey::Present(_)) => {}
        Ok(IntentKey::Foreign(_)) => {}
        Err(err) => crate::text::warn(&format!(
            "intent key not set up: {} — pass --key to `minds intent sign`",
            crate::text::sanitize(&err.to_string())
        )),
    }
}

/// Trägt die Signer-Zeile des Witness in die Vertrauensdatei ein und
/// zeigt mit `minds.signers` darauf — für `minds enable`; ein Fehler ist
/// eine Warnung.
pub(crate) fn report_signers(line: Option<&str>) {
    let Some(line) = line else {
        return;
    };
    let path = match signers_path() {
        Some(path) => path,
        None => {
            crate::text::warn("HOME is not set — the witness signers line was not recorded");
            return;
        }
    };
    match ensure_signer_line(line) {
        Ok(Recorded::Added) => eprintln!(
            "minds: witness signers line recorded in {}",
            crate::text::sanitize(&tilde(&path))
        ),
        Ok(Recorded::Present) => {}
        Err(err) => {
            crate::text::warn(&format!(
                "the witness signers line was not recorded in {}: {} — add it yourself",
                crate::text::sanitize(&tilde(&path)),
                crate::text::sanitize(&err.to_string())
            ));
            return;
        }
    }
    point_config_at(&path);
}

/// `minds.signers` auf die Vertrauensdatei setzen, wenn es noch nicht
/// darauf zeigt — ein anderer Wert (eine eigene Datei) bleibt.
fn point_config_at(path: &Path) {
    match global_config(SIGNERS_KEY) {
        Some(configured) if Path::new(&configured) == path => {}
        Some(configured) => eprintln!(
            "minds: {SIGNERS_KEY} points at {} — the recorded line is in {}",
            crate::text::sanitize(&tilde(Path::new(&configured))),
            crate::text::sanitize(&tilde(path))
        ),
        None => match set_global_config(SIGNERS_KEY, &path.to_string_lossy()) {
            Ok(()) => eprintln!(
                "minds: {SIGNERS_KEY} set to {} — minds verify and minds inspect check \
                 signatures against it",
                crate::text::sanitize(&tilde(path))
            ),
            Err(err) => crate::text::warn(&format!(
                "{SIGNERS_KEY} not set: {}",
                crate::text::sanitize(&err.to_string())
            )),
        },
    }
}

fn signers_display() -> String {
    signers_path()
        .map(|path| crate::text::sanitize(&tilde(&path)))
        .unwrap_or_else(|| SIGNERS_FILE.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signer_line_is_recorded_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg/minds/allowed_signers");
        let line = "minds-witness-user@host namespaces=\"minds-witness\" ssh-ed25519 AAAA";
        assert_eq!(ensure_signer_line_at(&path, line).unwrap(), Recorded::Added);
        assert_eq!(
            ensure_signer_line_at(&path, &format!("  {line} \n")).unwrap(),
            Recorded::Present
        );
        let other = "me@host namespaces=\"minds-intent\" ssh-ed25519 BBBB";
        assert_eq!(
            ensure_signer_line_at(&path, other).unwrap(),
            Recorded::Added
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{line}\n{other}\n")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
    }

    #[test]
    fn a_file_without_trailing_newline_gets_one_before_the_next_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("allowed_signers");
        std::fs::write(&path, "a@b namespaces=\"minds-witness\" ssh-ed25519 AAAA").unwrap();
        ensure_signer_line_at(&path, "c@d namespaces=\"minds-intent\" ssh-ed25519 BBBB").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "a@b namespaces=\"minds-witness\" ssh-ed25519 AAAA\nc@d namespaces=\"minds-intent\" ssh-ed25519 BBBB\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_or_directory_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "").unwrap();
        let link = dir.path().join("allowed_signers");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            ensure_signer_line_at(&link, "x@y namespaces=\"minds-intent\" ssh-ed25519 A").is_err()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
        let directory = dir.path().join("dir");
        std::fs::create_dir(&directory).unwrap();
        assert!(
            ensure_signer_line_at(&directory, "x@y namespaces=\"minds-intent\" ssh-ed25519 A")
                .is_err()
        );
    }

    #[test]
    fn control_characters_are_refused() {
        assert!(ensure_signer_line("a@b\x1b[31m ssh-ed25519 A").is_err());
        assert!(ensure_signer_line("   ").is_err());
    }

    #[test]
    fn the_intent_line_drops_the_comment() {
        assert_eq!(
            intent_signer_line("me@host", "ssh-ed25519 AAAA me@host (minds intent)\n").unwrap(),
            "me@host namespaces=\"minds-intent\" ssh-ed25519 AAAA"
        );
        assert!(intent_signer_line("me@host", "ssh-ed25519").is_err());
    }

    #[test]
    fn the_description_names_path_and_origin() {
        let signers = Signers {
            path: PathBuf::from("/x/allowed_signers"),
            origin: SignersOrigin::Config,
        };
        assert_eq!(signers.describe(), "/x/allowed_signers (minds.signers)");
        let flag = Signers {
            path: PathBuf::from("/explicit\x1b"),
            origin: SignersOrigin::Flag,
        };
        assert_eq!(flag.describe(), "/explicit\\u{1b} (--signers)");
    }
}
