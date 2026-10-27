//! Konfiguration des Hook-Binarys (JSON in S2, TOML ist S3) mit `validate`.
//! Jeder Fehler beendet das Binary mit Exit 1, auch im Observe-Modus, weil
//! der Modus dann nicht verlässlich bekannt ist (Spec S2 §9).

use std::path::{Path, PathBuf};
use std::time::Duration;

use minds_witness_core::{Mode, Policy, Timestamp};
use serde::Deserialize;

use crate::binding::is_project_id;
use crate::limits::AdapterLimits;

/// Optionale Abweichungen von den Grenzen aus §9 (für Tests).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitOverrides {
    /// `max_ssh_keygen_processes`.
    pub max_ssh_keygen_processes: Option<u32>,
    /// `max_total_commits_per_push`.
    pub max_total_commits_per_push: Option<u32>,
    /// `max_refs_per_push`.
    pub max_refs_per_push: Option<usize>,
    /// `deadline` in Sekunden.
    pub deadline_secs: Option<u64>,
}

/// Testschalter, nur bei `"operation": "test"` erlaubt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestSwitches {
    /// Quarantäne nicht lesen (s2_35).
    #[serde(default)]
    pub skip_quarantine: bool,
    /// In dieser Methode des Objektzugriffs mit dem Text `panic_text`
    /// panicken (`changed_paths`).
    #[serde(default)]
    pub panic_in: Option<String>,
    /// Text des Panics.
    #[serde(default)]
    pub panic_text: Option<String>,
}

/// Die Konfiguration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookConfig {
    /// `"test"` (Testsuite) oder `"pilot"` (Demo und Pilot auf eigenem
    /// Git-Server, Spec S3 §3); „production“ regelt S4.
    pub operation: String,
    /// Art des Hosts in den Quittungen: `"git"`; `"memory"` nur bei `test`.
    #[serde(default = "git_kind")]
    pub server_kind: String,
    /// Das Bare-Repository, für das dieser Hook gilt (absolut). Der Hook
    /// prüft, dass `GIT_DIR` darauf zeigt; `reconcile` arbeitet darauf.
    pub repository: PathBuf,
    /// Zustandsverzeichnis: Übergabe (`pending/`, `confirmed/`, `stale/`) und
    /// `log.jsonl`. Gehört der Hook-Kennung, Modus `0700`.
    pub state_dir: PathBuf,
    /// Nach dieser Zeit gilt ein unbestätigter Datensatz als nicht
    /// ausgeführt (Sekunden).
    #[serde(default = "default_ttl")]
    pub pending_ttl_secs: u64,
    /// Bestätigungen, die ein post-receive zusätzlich nachholt.
    #[serde(default = "default_batch")]
    pub reconcile_batch: usize,
    /// Timeout je `git`-Prozess der Ablage (Sekunden).
    #[serde(default = "default_store_timeout")]
    pub store_timeout_secs: u64,
    /// Projekt-Identität (Spec S2 §4.2).
    pub project: String,
    /// Signer-Datei, absolut, nie aus dem Repository.
    pub signers_file: PathBuf,
    /// Principals, die für dieses Projekt binden dürfen.
    pub allowed_witness_principals: Vec<String>,
    /// Absoluter Pfad zu `ssh-keygen` (root-eigene Kette).
    pub ssh_keygen: PathBuf,
    /// Absoluter Pfad zu `git` (root-eigene Kette).
    pub git: PathBuf,
    /// Getrennter Witness Pflicht; `false` + Enforce ist ungültig.
    #[serde(default = "yes")]
    pub require_separated_witness: bool,
    /// Die Policy des Kerns.
    pub policy: Policy,
    /// Seed des Signierschlüssels der Quittungen (64 Hex), Datei nur für
    /// den Eigentümer lesbar.
    pub seed_file: PathBuf,
    /// Gültigkeitsbeginn des Signierschlüssels.
    pub key_valid_from: String,
    /// Abweichende Grenzen.
    #[serde(default)]
    pub limits: LimitOverrides,
    /// Testschalter.
    #[serde(default)]
    pub test_switches: TestSwitches,
}

fn yes() -> bool {
    true
}

fn git_kind() -> String {
    "git".to_owned()
}

fn default_ttl() -> u64 {
    3600
}

fn default_batch() -> usize {
    16
}

fn default_store_timeout() -> u64 {
    10
}

/// Warum die Konfiguration ungültig ist (feste Texte).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// Datei nicht lesbar oder kein gültiges JSON der erwarteten Form.
    #[error("configuration unreadable")]
    Unreadable,
    /// `operation` ist weder `test` noch `pilot`.
    #[error("operation must be test or pilot")]
    NotTest,
    /// Ein Feld verletzt seine Regel.
    #[error("configuration field {0} invalid")]
    Field(&'static str),
}

impl HookConfig {
    /// Liest und prüft die Konfiguration.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        // Gelesen und geprüft wird dieselbe geöffnete Datei (kein Wechsel
        // zwischen Prüfung und Lesen).
        let mut file = std::fs::File::open(path).map_err(|_| ConfigError::Unreadable)?;
        let opened = file.metadata().map_err(|_| ConfigError::Unreadable)?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes).map_err(|_| ConfigError::Unreadable)?;
        let mut config: Self =
            serde_json::from_slice(&bytes).map_err(|_| ConfigError::Unreadable)?;
        // Ausgeführt wird genau der geprüfte Pfad: kanonisch (ohne
        // Symlinks), danach die root-eigene Kette in `validate`.
        if config.ssh_keygen.is_absolute() {
            config.ssh_keygen = std::fs::canonicalize(&config.ssh_keygen)
                .map_err(|_| ConfigError::Field("ssh_keygen"))?;
        }
        if config.git.is_absolute() {
            config.git =
                std::fs::canonicalize(&config.git).map_err(|_| ConfigError::Field("git"))?;
        }
        config.validate()?;
        if config.is_pilot() {
            let cfg = ConfigError::Field("configuration file");
            check_private(path, false).map_err(|()| cfg.clone())?;
            check_opened(path, &opened).map_err(|()| cfg.clone())?;
            check_chain(path).map_err(|()| cfg)?;
        }
        Ok(config)
    }

    /// Die Regeln aus Spec S2 §9 und §11. `ssh_keygen` und `git` müssen
    /// kanonisch sein ([`HookConfig::load`] sorgt dafür).
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !matches!(self.operation.as_str(), "test" | "pilot") {
            return Err(ConfigError::NotTest);
        }
        match (self.server_kind.as_str(), self.is_pilot()) {
            ("git", _) | ("memory", false) => {}
            _ => return Err(ConfigError::Field("server_kind")),
        }
        if self.is_pilot() && self.test_switches != TestSwitches::default() {
            return Err(ConfigError::Field("test_switches"));
        }
        // Im Pilotbetrieb mindestens 60 s: Ein Datensatz darf nicht verfallen,
        // bevor post-receive ihn bestätigen konnte.
        let min_ttl = if self.is_pilot() { 60 } else { 1 };
        if self.pending_ttl_secs < min_ttl || self.store_timeout_secs == 0 {
            return Err(ConfigError::Field("timeouts"));
        }
        if !is_project_id(&self.project) {
            return Err(ConfigError::Field("project"));
        }
        for (name, p) in [
            ("signers_file", &self.signers_file),
            ("ssh_keygen", &self.ssh_keygen),
            ("git", &self.git),
            ("seed_file", &self.seed_file),
            ("repository", &self.repository),
            ("state_dir", &self.state_dir),
        ] {
            if !p.is_absolute() {
                return Err(ConfigError::Field(name));
            }
        }
        if self.allowed_witness_principals.is_empty() {
            return Err(ConfigError::Field("allowed_witness_principals"));
        }
        if !self.require_separated_witness && self.policy.mode == Mode::Enforce {
            return Err(ConfigError::Field("require_separated_witness"));
        }
        if self.policy.validate().is_err() {
            return Err(ConfigError::Field("policy"));
        }
        if Timestamp::parse(&self.key_valid_from).is_err() {
            return Err(ConfigError::Field("key_valid_from"));
        }
        #[cfg(unix)]
        for (name, p) in [("ssh_keygen", &self.ssh_keygen), ("git", &self.git)] {
            let real = std::fs::canonicalize(p).map_err(|_| ConfigError::Field(name))?;
            if real != *p || !minds_attest::root_owned_chain(&real) {
                return Err(ConfigError::Field(name));
            }
        }
        check_seed_file(&self.seed_file)?;
        if self.is_pilot() {
            check_private(&self.state_dir, true).map_err(|()| ConfigError::Field("state_dir"))?;
            check_chain(&self.state_dir).map_err(|()| ConfigError::Field("state_dir"))?;
            check_chain(&self.seed_file).map_err(|()| ConfigError::Field("seed_file"))?;
            check_trusted_file(&self.signers_file)
                .map_err(|()| ConfigError::Field("signers_file"))?;
        } else if std::fs::create_dir_all(&self.state_dir).is_err() {
            return Err(ConfigError::Field("state_dir"));
        }
        Ok(())
    }

    /// Ob die Betriebsart `pilot` ist.
    pub fn is_pilot(&self) -> bool {
        self.operation == "pilot"
    }

    /// Die Art des Hosts für die Quittungen.
    pub fn server_kind(&self) -> minds_witness_core::ServerKind {
        if self.server_kind == "memory" {
            minds_witness_core::ServerKind::Memory
        } else {
            minds_witness_core::ServerKind::Git
        }
    }

    /// Die Grenzen mit den Abweichungen.
    pub fn limits(&self) -> AdapterLimits {
        let mut l = AdapterLimits::default();
        let o = &self.limits;
        if let Some(v) = o.max_ssh_keygen_processes {
            l.max_ssh_keygen_processes = v;
        }
        if let Some(v) = o.max_total_commits_per_push {
            l.max_total_commits_per_push = v;
        }
        if let Some(v) = o.max_refs_per_push {
            l.max_refs_per_push = v;
        }
        if let Some(v) = o.deadline_secs {
            l.deadline = Duration::from_secs(v);
        }
        l
    }

    /// Der Seed aus der Seed-Datei.
    pub fn seed(&self) -> Result<[u8; 32], ConfigError> {
        let text = std::fs::read_to_string(&self.seed_file)
            .map_err(|_| ConfigError::Field("seed_file"))?;
        let mut out = [0u8; 32];
        hex::decode_to_slice(text.trim(), &mut out).map_err(|_| ConfigError::Field("seed_file"))?;
        Ok(out)
    }
}

/// Die Seed-Datei gehört der eigenen Kennung und ist für niemanden sonst
/// lesbar.
#[cfg(unix)]
fn check_seed_file(path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).map_err(|_| ConfigError::Field("seed_file"))?;
    // Die eigene Kennung ohne `unsafe`: eine frische Temp-Datei gehört ihr.
    let probe = tempfile::NamedTempFile::new().map_err(|_| ConfigError::Field("seed_file"))?;
    let own = probe
        .as_file()
        .metadata()
        .map_err(|_| ConfigError::Field("seed_file"))?
        .uid();
    if meta.uid() != own || meta.mode() & 0o077 != 0 || !meta.is_file() {
        return Err(ConfigError::Field("seed_file"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_seed_file(_: &Path) -> Result<(), ConfigError> {
    Ok(())
}

/// `pilot`: Datei bzw. Verzeichnis gehört der eigenen Kennung und ist für
/// niemanden sonst beschreibbar (Datei) bzw. zugänglich (Verzeichnis,
/// Modus `0700`); kein Symlink.
#[cfg(unix)]
fn check_private(path: &Path, dir: bool) -> Result<(), ()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).map_err(|_| ())?;
    let probe = tempfile::NamedTempFile::new().map_err(|_| ())?;
    let own = probe.as_file().metadata().map_err(|_| ())?.uid();
    // Datei: nur für die eigene Kennung (`0600`); sie benennt Schlüssel,
    // Seed und Zustandsverzeichnis.
    let mask = 0o077;
    let kind_ok = if dir { meta.is_dir() } else { meta.is_file() };
    if !kind_ok || meta.uid() != own || meta.mode() & mask != 0 {
        return Err(());
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private(_: &Path, _: bool) -> Result<(), ()> {
    Ok(())
}

#[cfg(unix)]
fn own_uid() -> Result<u32, ()> {
    use std::os::unix::fs::MetadataExt;
    let probe = tempfile::NamedTempFile::new().map_err(|_| ())?;
    Ok(probe.as_file().metadata().map_err(|_| ())?.uid())
}

/// Die geöffnete Datei ist die geprüfte (gleiches Gerät und gleicher Inode).
#[cfg(unix)]
fn check_opened(path: &Path, opened: &std::fs::Metadata) -> Result<(), ()> {
    use std::os::unix::fs::MetadataExt;
    let now = std::fs::symlink_metadata(path).map_err(|_| ())?;
    if now.dev() != opened.dev() || now.ino() != opened.ino() {
        return Err(());
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_opened(_: &Path, _: &std::fs::Metadata) -> Result<(), ()> {
    Ok(())
}

/// `pilot`: Kein Verzeichnis über `path` (im angegebenen wie im kanonischen
/// Pfad) ist für andere austauschbar: Eigentümer die eigene Kennung oder
/// root, nicht für Gruppe oder andere beschreibbar, außer mit Sticky-Bit.
#[cfg(unix)]
fn check_chain(path: &Path) -> Result<(), ()> {
    use std::os::unix::fs::MetadataExt;
    let own = own_uid()?;
    let parent = path.parent().ok_or(())?;
    let real = std::fs::canonicalize(parent).map_err(|_| ())?;
    for dir in parent.ancestors().chain(real.ancestors()) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let meta = std::fs::metadata(dir).map_err(|_| ())?;
        let writable = meta.mode() & 0o022 != 0;
        let sticky = meta.mode() & 0o1000 != 0;
        if !meta.is_dir() || (meta.uid() != own && meta.uid() != 0) || (writable && !sticky) {
            return Err(());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_chain(_: &Path) -> Result<(), ()> {
    Ok(())
}

/// `pilot`: die Trust-Datei ist eine reguläre Datei (kein Symlink) der
/// eigenen Kennung oder von root, für niemanden sonst beschreibbar, in
/// einer sicheren Verzeichniskette.
#[cfg(unix)]
fn check_trusted_file(path: &Path) -> Result<(), ()> {
    use std::os::unix::fs::MetadataExt;
    let own = own_uid()?;
    let meta = std::fs::symlink_metadata(path).map_err(|_| ())?;
    if !meta.is_file() || (meta.uid() != own && meta.uid() != 0) || meta.mode() & 0o022 != 0 {
        return Err(());
    }
    check_chain(path)
}

#[cfg(not(unix))]
fn check_trusted_file(_: &Path) -> Result<(), ()> {
    Ok(())
}
