//! Ignore-Regeln ohne Git-Prozess (EA-08).
//!
//! Der Witness muss wissen, welche Pfade `.gitignore` und
//! `.git/info/exclude` ausschließen — in einem Repository, dessen
//! Konfiguration und Index der Agent schreibt. Ein `git`-Aufruf dort ist ein
//! Weg, Befehle des Agenten auf dem Host zu starten (Lazy-Fetch eines
//! Promisor-Remotes über `core.sshCommand`, `core.fsmonitor` …). Deshalb wird
//! hier **im Prozess** ausgewertet, aus Bytes, die der Aufrufer selbst
//! gelesen hat: keine Konfiguration, kein Objektzugriff, kein Netz, keine
//! Exclude-Datei außerhalb des Repos.
//!
//! Semantik wie `git check-ignore` mit Index:
//! - tiefere `.gitignore` gehen flacheren vor, `info/exclude` zuletzt;
//! - ist ein Elternverzeichnis ausgeschlossen, ist alles darunter
//!   ausgeschlossen (keine Rück-Einbeziehung);
//! - eine im Index stehende (getrackte) Datei ist nie ignoriert.
//!
//! Abweichung, bewusst: Groß-/Kleinschreibung wird immer gefaltet — die
//! Regel `core.ignoreCase` steht in der Konfiguration des Agenten, und im
//! Zweifel wird eher ein Pfad zu viel ignoriert als einer gefingerprintet.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use gix::bstr::BStr;

/// Die Ignore-Muster eines Repositorys, aufgebaut aus übergebenen Bytes.
#[derive(Default)]
pub struct IgnoreRules {
    search: gix::ignore::Search,
    tracked: Arc<BTreeSet<String>>,
}

const PARSE: gix::ignore::search::Ignore = gix::ignore::search::Ignore {
    support_precious: false,
};

impl IgnoreRules {
    /// Regeln mit dem Inhalt von `.git/info/exclude` (niedrigste Priorität)
    /// und den getrackten Pfaden.
    pub fn new(info_exclude: Option<&[u8]>, tracked: Arc<BTreeSet<String>>) -> Self {
        let mut rules = Self {
            search: gix::ignore::Search::default(),
            tracked,
        };
        if let Some(bytes) = info_exclude {
            // Muster aus `info/exclude` gelten relativ zur Repo-Wurzel: Die
            // Basis ist leer, nicht `.git/info/`.
            rules.search.add_patterns_buffer(
                bytes,
                "/.git/info/exclude",
                Some(Path::new("/.git/info")),
                PARSE,
            );
        }
        rules
    }

    /// Fügt die `.gitignore` des repo-relativen Verzeichnisses `dir` hinzu
    /// (`""` für die Wurzel). Flachere Verzeichnisse zuerst hinzufügen.
    pub fn add_gitignore(&mut self, dir: &str, bytes: &[u8]) {
        let source = if dir.is_empty() {
            "/.gitignore".to_owned()
        } else {
            format!("/{dir}/.gitignore")
        };
        self.search
            .add_patterns_buffer(bytes, source, Some(Path::new("/")), PARSE);
    }

    /// Ob `path` (repo-relativ, `/`-getrennt) ignoriert ist. `is_dir`:
    /// `None`, wenn unbekannt (gelöscht).
    pub fn is_ignored(&self, path: &str, is_dir: Option<bool>) -> bool {
        if self.tracked.contains(path) {
            return false;
        }
        let mut prefix = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        for part in &parts[..parts.len().saturating_sub(1)] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if self.matches(&prefix, Some(true)) {
                return true;
            }
        }
        self.matches(path, is_dir)
    }

    fn matches(&self, path: &str, is_dir: Option<bool>) -> bool {
        self.search
            .pattern_matching_relative_path(
                BStr::new(path.as_bytes()),
                is_dir,
                gix::glob::pattern::Case::Fold,
            )
            .is_some_and(|m| !m.pattern.is_negative())
    }
}

/// Die Pfade eines Index (SHA-1-Repository) aus seinen Bytes — `None`, wenn
/// sie sich nicht lesen lassen. Kein Objektzugriff.
///
/// Nur SHA-1: In einem SHA-256-Repository ist das `None`, und kein Pfad gilt
/// als getrackt — dann entscheidet allein das Muster, eine getrackte Datei,
/// die ein Muster trifft, bleibt unbeobachtet. Die sichere Richtung: eher
/// ignoriert als gefingerprintet.
pub fn index_paths(bytes: &[u8]) -> Option<BTreeSet<String>> {
    let (state, _) = gix::index::State::from_bytes(
        bytes,
        filetime::FileTime::zero(),
        gix::hash::Kind::Sha1,
        gix::index::decode::Options::default(),
    )
    .ok()?;
    Some(
        state
            .entries()
            .iter()
            .filter_map(|entry| std::str::from_utf8(entry.path(&state)).ok())
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> IgnoreRules {
        let mut rules = IgnoreRules::new(
            Some(b"local.txt\n"),
            Arc::new(BTreeSet::from(["target/kept.rs".to_owned()])),
        );
        rules.add_gitignore("", b"target/\n*.log\n!keep.log\n");
        rules.add_gitignore("sub", b"inner.txt\n/anchored\n");
        rules
    }

    #[test]
    fn ignore_rules_follow_git_semantics() {
        let rules = rules();
        for ignored in [
            "local.txt",
            "target/out.bin",
            "target/deep/x",
            "a.log",
            "sub/a.log",
            "sub/inner.txt",
            "sub/deeper/inner.txt",
            "sub/anchored",
            "TARGET/x",
        ] {
            assert!(rules.is_ignored(ignored, Some(false)), "{ignored}");
        }
        for kept in [
            "src/a.rs",
            "keep.log",
            "inner.txt",
            "anchored",
            "sub/deeper/anchored",
            // Getrackt ist nie ignoriert.
            "target/kept.rs",
        ] {
            assert!(!rules.is_ignored(kept, Some(false)), "{kept}");
        }
        // Ein Verzeichnismuster greift auch für einen gelöschten Pfad
        // darunter.
        assert!(rules.is_ignored("target/gone", None));
    }

    #[test]
    fn unreadable_index_bytes_are_none() {
        assert!(index_paths(b"not an index").is_none());
    }
}
