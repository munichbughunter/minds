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

/// Testschalter, nur bei `"operation": "test"` gelesen.
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
    /// Muss `"test"` sein.
    pub operation: String,
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
    /// Ausgabeverzeichnis für Quittungen und `log.jsonl`.
    pub out_dir: PathBuf,
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

/// Warum die Konfiguration ungültig ist (feste Texte).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// Datei nicht lesbar oder kein gültiges JSON der erwarteten Form.
    #[error("configuration unreadable")]
    Unreadable,
    /// `operation` ist nicht `test`.
    #[error("operation must be test")]
    NotTest,
    /// Ein Feld verletzt seine Regel.
    #[error("configuration field {0} invalid")]
    Field(&'static str),
}

impl HookConfig {
    /// Liest und prüft die Konfiguration.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read(path).map_err(|_| ConfigError::Unreadable)?;
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
        Ok(config)
    }

    /// Die Regeln aus Spec S2 §9 und §11. `ssh_keygen` und `git` müssen
    /// kanonisch sein ([`HookConfig::load`] sorgt dafür).
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.operation != "test" {
            return Err(ConfigError::NotTest);
        }
        if !is_project_id(&self.project) {
            return Err(ConfigError::Field("project"));
        }
        for (name, p) in [
            ("signers_file", &self.signers_file),
            ("ssh_keygen", &self.ssh_keygen),
            ("git", &self.git),
            ("seed_file", &self.seed_file),
            ("out_dir", &self.out_dir),
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
        Ok(())
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
    let meta = std::fs::metadata(path).map_err(|_| ConfigError::Field("seed_file"))?;
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
