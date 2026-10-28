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
    case: Case,
}

/// Wie Muster Groß- und Kleinschreibung vergleichen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Case {
    /// Gefaltet (Standard): eher ein Pfad zu viel ignoriert als einer
    /// gefingerprintet — die sichere Richtung beim Beobachten.
    #[default]
    Fold,
    /// Wie Git auf Linux (`core.ignoreCase=false`): Wer eine Ausnahme
    /// darauf stützt, dass Git einen Pfad ignoriert, darf nicht mehr
    /// ignorieren als Git.
    Sensitive,
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
            case: Case::Fold,
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

    /// Dieselben Regeln mit Vergleich nach `case`.
    #[must_use]
    pub fn with_case(mut self, case: Case) -> Self {
        self.case = case;
        self
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
                match self.case {
                    Case::Fold => gix::glob::pattern::Case::Fold,
                    Case::Sensitive => gix::glob::pattern::Case::Sensitive,
                },
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

/// Wie [`index_paths`], aber nur, wenn der Index **alle** Pfade nennt: Ein
/// geteilter Index (Erweiterung `link`, Einträge in `sharedindex.*`) oder ein
/// Pfad, der kein UTF-8 ist, ergibt `None`. Für Entscheidungen, die darauf
/// bauen, dass ein Pfad **nicht** getrackt ist.
pub fn index_paths_complete(bytes: &[u8]) -> Option<BTreeSet<String>> {
    let (state, _) = gix::index::State::from_bytes(
        bytes,
        filetime::FileTime::zero(),
        gix::hash::Kind::Sha1,
        gix::index::decode::Options::default(),
    )
    .ok()?;
    if state.link().is_some() {
        return None;
    }
    state
        .entries()
        .iter()
        .map(|entry| {
            std::str::from_utf8(entry.path(&state))
                .ok()
                .map(str::to_owned)
        })
        .collect()
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
        assert!(index_paths_complete(b"not an index").is_none());
    }

    /// Wie Git auf Linux: `cache/` ignoriert `Cache/` nicht.
    #[test]
    fn case_sensitive_rules_ignore_no_more_than_git() {
        let rules = || {
            let mut rules = IgnoreRules::new(None, Arc::default());
            rules.add_gitignore("", b"cache/\n");
            rules
        };
        assert!(rules().is_ignored("Cache", Some(true)));
        let strict = rules().with_case(Case::Sensitive);
        assert!(!strict.is_ignored("Cache", Some(true)));
        assert!(strict.is_ignored("cache", Some(true)));
        assert!(strict.is_ignored("cache/x", Some(false)));
    }

    /// Ein geteilter Index nennt nicht alle Pfade; ein Pfad, der kein UTF-8
    /// ist, fiele weg: beides kein Beleg für „nicht getrackt".
    #[test]
    fn complete_index_paths_refuse_split_and_non_utf8_indexes() {
        let fixture = crate::fixture::TempRepo::init();
        fixture.write_file("cache/x.rs", "x\n");
        fixture.git(&["add", "cache/x.rs"]);
        let index = || std::fs::read(fixture.path().join(".git/index")).unwrap();
        assert_eq!(
            index_paths_complete(&index()),
            Some(BTreeSet::from(["cache/x.rs".to_owned()]))
        );
        fixture.git(&["update-index", "--split-index"]);
        assert!(index_paths_complete(&index()).is_none(), "geteilt");
        fixture.git(&["update-index", "--no-split-index"]);
        assert!(index_paths_complete(&index()).is_some());
        let blob = fixture.write_raw_object("blob", b"y\n");
        // Ein Name aus rohen Bytes (kein UTF-8) über `--index-info`.
        let line = [
            b"100644 ".as_slice(),
            blob.trim().as_bytes(),
            b"\tcache/\xff.bin\n",
        ]
        .concat();
        fixture.git_stdin(&["update-index", "--index-info"], &line);
        assert!(index_paths(&index()).is_some());
        assert!(index_paths_complete(&index()).is_none(), "kein UTF-8");
    }
}
