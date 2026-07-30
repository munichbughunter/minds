//! Die Store-Config, wo sie hingehört: in `.git/config`.
//!
//! [`minds_store::StoreConfig`] ist ein Wert ohne Dateiformat — bewusst, denn wo
//! er liegt, entscheidet die CLI. Der naheliegende Ort ist `.git/config`
//! (`minds.backend`, `minds.contextRef`, `minds.childPath`): keine neue Datei,
//! pro Klon gültig, von Git verwaltet, überlebt jeden Pull. `minds enable`
//! schreibt ihn, die übrigen Kommandos lesen ihn.
//!
//! # Warum über `git config` und nicht über einen INI-Parser
//!
//! `.git/config` ist INI mit Feinheiten (Sub-Sections, `include`, Groß-/Klein-
//! schreibung). Es selbst zu parsen hieße, einen Teil von Git nachzubauen und
//! dabei falsch zu liegen. Stattdessen rufen wir `git config` auf — dieselbe
//! pragmatische Linie wie [`minds_git::ShellBlame`]: Git ist ohnehin die eine
//! harte Abhängigkeit, also darf die CLI es benutzen, wo die Bibliothek (noch)
//! keine Schnittstelle bietet.
//!
//! # Fehlt die Config, ist das kein Fehler
//!
//! [`load`] fällt auf den Default zurück (In-Repo, `refs/minds/context`). Ein
//! Repo, in dem `minds enable` nie lief, ist damit trotzdem lesbar — nur eben
//! mit den Standardwerten. Fail-open beim Lesen, streng erst beim Schreiben.

use std::path::Path;
use std::process::Command;

use minds_git::DEFAULT_CONTEXT_REF;
use minds_redact::RedactionConfig;
use minds_store::{Backend, StoreConfig};

/// Schlüssel in `.git/config`.
const KEY_BACKEND: &str = "minds.backend";
const KEY_REF: &str = "minds.contextRef";
const KEY_CHILD_PATH: &str = "minds.childPath";

/// Optionale Redaction-Policy, relativ zur Repo-Wurzel. JSON, damit keine neue
/// Format-Abhängigkeit nötig ist (das Envelope-Crate ist ohnehin serde-basiert).
const REDACT_CONFIG: &str = ".minds/redact.json";

/// Wert von [`KEY_BACKEND`] für die beiden Backends.
const BACKEND_IN_REPO: &str = "in-repo";
const BACKEND_CHILD: &str = "child-repo";

/// Schreibt die Config nach `.git/config` unter `repo_root`.
///
/// Idempotent von Natur aus: `git config --local <key> <value>` setzt oder
/// ersetzt, es dupliziert nicht.
pub fn write(repo_root: &Path, config: &StoreConfig) -> std::io::Result<()> {
    match config.backend() {
        Backend::InRepo => {
            set(repo_root, KEY_BACKEND, BACKEND_IN_REPO)?;
        }
        Backend::ChildRepo { path } => {
            set(repo_root, KEY_BACKEND, BACKEND_CHILD)?;
            set(repo_root, KEY_CHILD_PATH, &path.to_string_lossy())?;
        }
    }
    set(repo_root, KEY_REF, config.reference())?;
    Ok(())
}

/// Liest die Config aus `.git/config`. Fehlt sie ganz oder teilweise, greifen
/// die Defaults (In-Repo, [`DEFAULT_CONTEXT_REF`]).
pub fn load(repo_root: &Path) -> StoreConfig {
    let reference = get(repo_root, KEY_REF).unwrap_or_else(|| DEFAULT_CONTEXT_REF.to_string());

    match get(repo_root, KEY_BACKEND).as_deref() {
        Some(BACKEND_CHILD) => {
            let path = get(repo_root, KEY_CHILD_PATH).unwrap_or_default();
            StoreConfig::child_repo(path).with_ref(reference)
        }
        _ => StoreConfig::in_repo().with_ref(reference),
    }
}

/// Lädt die Redaction-Policy aus `.minds/redact.json`.
///
/// Fehlt die Datei, gilt der **strenge Default** (alle Detektoren an) — wer
/// nichts konfiguriert, bekommt volle Redaction. Ist die Datei vorhanden, aber
/// fehlerhaft (Tippfehler, unbekanntes Feld), **bricht** das den Aufrufer ab
/// (fail-closed): Eine getippte Policy darf nicht stillschweigend auf den Default
/// zurückfallen und damit die vom Team ergänzten Deny-Regeln verschlucken. Genau
/// dafür trägt [`RedactionConfig`] `deny_unknown_fields`.
pub fn load_redaction(repo_root: &Path) -> Result<RedactionConfig, Box<dyn std::error::Error>> {
    let path = repo_root.join(REDACT_CONFIG);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(serde_json::from_str(&text)
            .map_err(|err| format!("{}: ungültige Redaction-Policy: {err}", path.display()))?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(RedactionConfig::default()),
        Err(err) => Err(format!("{}: nicht lesbar: {err}", path.display()).into()),
    }
}

fn set(repo_root: &Path, key: &str, value: &str) -> std::io::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--local", key, value])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "`git config --local {key}` schlug fehl"
        )))
    }
}

fn get(repo_root: &Path, key: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--local", "--get", key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein frisch initialisiertes Repo — die Config-Kommandos brauchen ein
    /// echtes `.git`. Gibt `None` zurück, wenn kein `git` im Pfad ist, damit der
    /// Test dort nicht falsch-rot wird.
    fn repo() -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().unwrap();
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["init", "-q"])
            .status()
            .ok()?
            .success();
        ok.then_some(dir)
    }

    #[test]
    fn default_when_nothing_is_written() {
        let Some(dir) = repo() else { return };
        let cfg = load(dir.path());
        assert_eq!(cfg.backend(), &Backend::InRepo);
        assert_eq!(cfg.reference(), DEFAULT_CONTEXT_REF);
    }

    #[test]
    fn in_repo_roundtrips() {
        let Some(dir) = repo() else { return };
        write(
            dir.path(),
            &StoreConfig::in_repo().with_ref("refs/minds/ctx"),
        )
        .unwrap();
        let cfg = load(dir.path());
        assert_eq!(cfg.backend(), &Backend::InRepo);
        assert_eq!(cfg.reference(), "refs/minds/ctx");
    }

    #[test]
    fn child_repo_roundtrips() {
        let Some(dir) = repo() else { return };
        write(dir.path(), &StoreConfig::child_repo("../minds-kontext")).unwrap();
        let cfg = load(dir.path());
        assert_eq!(
            cfg.backend(),
            &Backend::ChildRepo {
                path: "../minds-kontext".into()
            }
        );
        assert_eq!(cfg.reference(), DEFAULT_CONTEXT_REF);
    }

    // --- Redaction-Policy -----------------------------------------------------

    #[test]
    fn redaction_defaults_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_redaction(dir.path()).unwrap(),
            RedactionConfig::default()
        );
    }

    #[test]
    fn redaction_reads_a_policy_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".minds")).unwrap();
        std::fs::write(
            dir.path().join(REDACT_CONFIG),
            r#"{"deny_pii": ["Nordlicht"]}"#,
        )
        .unwrap();

        let cfg = load_redaction(dir.path()).unwrap();
        assert_eq!(cfg.deny_pii, vec!["Nordlicht".to_string()]);
        // Nicht genannte Felder behalten den strengen Default.
        assert!(cfg.known_tokens);
    }

    #[test]
    fn redaction_rejects_a_malformed_policy() {
        // Fail-closed: ein Tippfehler (`deny_pi`) darf nicht still auf den Default
        // zurückfallen, sondern muss den Checkpoint abbrechen.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".minds")).unwrap();
        std::fs::write(dir.path().join(REDACT_CONFIG), r#"{"deny_pi": ["x"]}"#).unwrap();

        assert!(load_redaction(dir.path()).is_err());
    }

    #[test]
    fn write_is_idempotent() {
        let Some(dir) = repo() else { return };
        let cfg = StoreConfig::child_repo("../ctx").with_ref("refs/minds/context");
        write(dir.path(), &cfg).unwrap();
        write(dir.path(), &cfg).unwrap();
        // Kein doppelter Eintrag: `git config --get` ohne `--get-all` faende
        // sonst „mehrere Werte" und schluege fehl.
        assert_eq!(load(dir.path()).reference(), "refs/minds/context");
    }
}
