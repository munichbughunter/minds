//! Der Vertrauensanker von `minds verify --receipt` (Spec S3 §6.3,
//! Ergänzung B): woher der Server-Schlüssel kommt und wie gut er vor einem
//! Coding-Agenten unter derselben Kennung geschützt ist. Der Schlüssel kommt
//! nie aus dem geprüften Repository.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use minds_witness_core::KeyStr;

/// Voreinstellung der Schlüsseldatei.
pub(crate) const DEFAULT_KEY_FILE: &str = "/etc/minds/receipt-keys";
/// Der globale Git-Konfigurationsschlüssel (Pilotbetrieb).
pub(crate) const CONFIG_KEY: &str = "minds.witness.receiptKey";
/// Größte Schlüsseldatei.
const MAX_KEY_FILE: u64 = 64 * 1024;

/// Wie gut der Anker geschützt ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// `--receipt-key`: gewählt, aber nicht vor dem Agenten geschützt.
    Explicit,
    /// Datei mit root-eigener Kette ohne Symlinks.
    Protected,
    /// Datei ohne diese Kette, oder `git config --global`.
    Unprotected,
}

/// Der gewählte Anker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub(crate) class: Class,
    /// Die Quelle, wie sie in der Ausgabe steht.
    pub(crate) source: String,
    pub(crate) keys: BTreeSet<String>,
}

impl Anchor {
    /// Die Zeile `Vertrauensanker: …`.
    pub(crate) fn line(&self) -> String {
        match self.class {
            Class::Explicit => format!("Vertrauensanker: ausdrücklich übergeben ({})", self.source),
            Class::Protected => format!("Vertrauensanker: geschützt ({})", self.source),
            Class::Unprotected => format!(
                "Vertrauensanker: ungeschützt ({}) – nur für den Pilotbetrieb",
                self.source
            ),
        }
    }
}

/// Warum kein Anker bestimmt werden konnte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnchorError {
    /// Mehrere Quellen nennen verschiedene Schlüssel (Exit 1).
    Contradiction,
    /// Eine genannte Quelle ist nicht lesbar oder hat keine gültige Form
    /// (Exit 4). Der Text ist fest, ohne Dateiinhalt.
    Unusable(String),
}

/// Die möglichen Quellen, in der Reihenfolge der Tabelle in §6.3.
pub(crate) struct Sources<'a> {
    /// `--receipt-key <key>`.
    pub(crate) key: Option<&'a str>,
    /// `--receipt-key-file <pfad>`; ohne: [`Sources::default_file`], wenn
    /// sie existiert.
    pub(crate) key_file: Option<&'a Path>,
    pub(crate) default_file: &'a Path,
    /// `git config --global minds.witness.receiptKey`.
    pub(crate) config: Option<String>,
}

fn parse_key(text: &str) -> Option<String> {
    KeyStr::parse(text.trim())
        .ok()
        .map(|k| k.as_str().to_owned())
}

/// Liest eine Schlüsseldatei: je Zeile ein `ed25519:<hex>`, Leerzeilen und
/// `#`-Kommentare erlaubt, mindestens ein Schlüssel.
fn read_key_file(path: &Path) -> Result<BTreeSet<String>, AnchorError> {
    let unusable = |why: &str| AnchorError::Unusable(format!("{why} ({})", path.display()));
    let file = std::fs::File::open(path).map_err(|_| unusable("Schlüsseldatei nicht lesbar"))?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err(unusable("Schlüsseldatei ist keine reguläre Datei"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_KEY_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unusable("Schlüsseldatei nicht lesbar"))?;
    if bytes.len() as u64 > MAX_KEY_FILE {
        return Err(unusable("Schlüsseldatei zu groß"));
    }
    let text =
        String::from_utf8(bytes).map_err(|_| unusable("Schlüsseldatei hat keine gültige Form"))?;
    let mut keys = BTreeSet::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        keys.insert(
            parse_key(line).ok_or_else(|| unusable("Schlüsseldatei hat keine gültige Form"))?,
        );
    }
    if keys.is_empty() {
        return Err(unusable("Schlüsseldatei nennt keinen Schlüssel"));
    }
    Ok(keys)
}

/// Ob `given` geschützt ist: der angegebene Pfad (absolut gemacht) ist sein
/// eigener kanonischer Pfad — kein Symlink irgendwo in der Kette —, eine
/// reguläre Datei, und Datei wie jedes Elternverzeichnis gehören root und
/// sind für niemanden sonst beschreibbar ([`minds_attest::root_owned_chain`]).
#[cfg(unix)]
pub(crate) fn protected(given: &Path) -> bool {
    let absolute = if given.is_absolute() {
        given.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(here) => here.join(given),
            Err(_) => return false,
        }
    };
    let Ok(real) = std::fs::canonicalize(&absolute) else {
        return false;
    };
    real == absolute
        && std::fs::symlink_metadata(&real).is_ok_and(|m| m.file_type().is_file())
        && minds_attest::root_owned_chain(&real)
}

/// Ohne Unix gibt es diese Prüfung nicht: nie geschützt.
#[cfg(not(unix))]
pub(crate) fn protected(_: &Path) -> bool {
    false
}

/// Bestimmt den Anker: Die erste vorhandene Quelle der Tabelle gilt;
/// nennen mehrere Quellen verschiedene Schlüsselmengen, ist das ein
/// Widerspruch. `Ok(None)`: keine Quelle.
pub(crate) fn resolve(sources: &Sources<'_>) -> Result<Option<Anchor>, AnchorError> {
    let mut found: Vec<Anchor> = Vec::new();
    if let Some(key) = sources.key {
        let key = parse_key(key)
            .ok_or_else(|| AnchorError::Unusable("--receipt-key hat keine gültige Form".into()))?;
        found.push(Anchor {
            class: Class::Explicit,
            source: "--receipt-key".into(),
            keys: BTreeSet::from([key]),
        });
    }
    let file: Option<PathBuf> = match sources.key_file {
        Some(path) => Some(path.to_path_buf()),
        None => std::fs::symlink_metadata(sources.default_file)
            .is_ok()
            .then(|| sources.default_file.to_path_buf()),
    };
    if let Some(path) = file {
        let keys = read_key_file(&path)?;
        found.push(Anchor {
            class: if protected(&path) {
                Class::Protected
            } else {
                Class::Unprotected
            },
            source: path.display().to_string(),
            keys,
        });
    }
    if let Some(value) = &sources.config {
        let key = parse_key(value).ok_or_else(|| {
            AnchorError::Unusable(format!("git config {CONFIG_KEY} hat keine gültige Form"))
        })?;
        found.push(Anchor {
            class: Class::Unprotected,
            source: "git config".into(),
            keys: BTreeSet::from([key]),
        });
    }
    let Some(first) = found.first() else {
        return Ok(None);
    };
    if found.iter().any(|other| other.keys != first.keys) {
        return Err(AnchorError::Contradiction);
    }
    Ok(Some(first.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> String {
        format!("ed25519:{}", format!("{n:02x}").repeat(32))
    }

    fn sources<'a>(
        key: Option<&'a str>,
        file: Option<&'a Path>,
        config: Option<String>,
    ) -> Sources<'a> {
        Sources {
            key,
            key_file: file,
            default_file: Path::new("/nonexistent/minds/receipt-keys"),
            config,
        }
    }

    #[test]
    fn s3_28_receipt_anchor_sources_basis() {
        let dir = tempfile::tempdir().unwrap();
        // Nur im Repository: kein Anker.
        assert_eq!(resolve(&sources(None, None, None)), Ok(None));

        let k1 = key(1);
        let explicit = resolve(&sources(Some(&k1), None, None)).unwrap().unwrap();
        assert_eq!(explicit.class, Class::Explicit);
        assert_eq!(
            explicit.line(),
            "Vertrauensanker: ausdrücklich übergeben (--receipt-key)"
        );

        // Benutzereigene Datei: ungeschützt, mit Pilot-Hinweis.
        let file = dir.path().join("receipt-keys");
        std::fs::write(&file, format!("# Server\n\n{k1}\n")).unwrap();
        let own = resolve(&sources(None, Some(&file), None)).unwrap().unwrap();
        assert_eq!(own.class, Class::Unprotected);
        assert!(
            own.line().ends_with("– nur für den Pilotbetrieb"),
            "{}",
            own.line()
        );
        assert_eq!(own.keys, BTreeSet::from([k1.clone()]));

        let config = resolve(&sources(None, None, Some(k1.clone())))
            .unwrap()
            .unwrap();
        assert_eq!(
            config.line(),
            "Vertrauensanker: ungeschützt (git config) – nur für den Pilotbetrieb"
        );

        // Erste Quelle gewinnt, wenn alle dasselbe sagen.
        let all = resolve(&sources(Some(&k1), Some(&file), Some(k1.clone())))
            .unwrap()
            .unwrap();
        assert_eq!(all.class, Class::Explicit);
        // Widerspruch: Exit 1, in jeder Kombination.
        let k2 = key(2);
        for case in [
            sources(Some(&k2), Some(&file), None),
            sources(None, Some(&file), Some(k2.clone())),
            sources(Some(&k1), None, Some(k2.clone())),
        ] {
            assert_eq!(resolve(&case), Err(AnchorError::Contradiction));
        }
        // Die Voreinstellung zählt, wenn es sie gibt.
        let s = Sources {
            key: Some(&k2),
            key_file: None,
            default_file: &file,
            config: None,
        };
        assert_eq!(resolve(&s), Err(AnchorError::Contradiction));

        // Ungültige Formen: Betriebsfehler, nie ein stiller Verzicht.
        for bad in [
            "",
            "# nur Kommentar\n",
            "ed25519:zz\n",
            "ssh-ed25519 AAAA\n",
        ] {
            std::fs::write(&file, bad).unwrap();
            assert!(
                matches!(
                    resolve(&sources(None, Some(&file), None)),
                    Err(AnchorError::Unusable(_))
                ),
                "{bad:?}"
            );
        }
        assert!(matches!(
            resolve(&sources(Some("ed25519:00"), None, None)),
            Err(AnchorError::Unusable(_))
        ));
        assert!(matches!(
            resolve(&sources(None, Some(&dir.path().join("fehlt")), None)),
            Err(AnchorError::Unusable(_))
        ));
        assert!(matches!(
            resolve(&sources(None, Some(dir.path()), None)),
            Err(AnchorError::Unusable(_))
        ));
    }

    /// Was ohne root geht: fremder (eigener) Eigentümer, Symlink auf eine
    /// root-eigene Datei, Symlink-Verzeichnis in der Kette. Der Teil mit
    /// root-eigenem Baum steht im Isolations-Job (`tests/witness_isolation.rs`).
    #[cfg(unix)]
    #[test]
    fn s3_28_protection_without_root() {
        let dir = tempfile::tempdir().unwrap();
        let own = dir.path().join("receipt-keys");
        std::fs::write(&own, "x").unwrap();
        assert!(!protected(&own), "eigene Datei");
        // Eine root-eigene Datei des Systems, falls die Umgebung eine hat.
        let system = ["/usr/bin/env", "/bin/sh", "/etc/hostname"]
            .into_iter()
            .map(Path::new)
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .find(|p| p.is_file() && minds_attest::root_owned_chain(p));
        if let Some(real) = system {
            assert!(protected(&real), "{}", real.display());
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(!protected(&link), "Symlink auf eine geschützte Datei");
            let linked_dir = dir.path().join("dir");
            std::os::unix::fs::symlink(real.parent().unwrap(), &linked_dir).unwrap();
            assert!(
                !protected(&linked_dir.join(real.file_name().unwrap())),
                "Symlink-Verzeichnis in der Kette"
            );
            // Ein Verzeichnis ist keine Datei.
            assert!(!protected(real.parent().unwrap()));
        }
    }
}
