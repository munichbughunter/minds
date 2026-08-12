//! Geteilte Helfer für die Rückführungs-Kommandos `recall`/`distill`/`brief`:
//! Repo und Store öffnen, Sessions laden, ein Ziel zu Sessions auflösen.
//!
//! `show` und `why` haben ihre eigene, schlankere Auflösung (ein Commit, eine
//! Zeile). Die Rückführung braucht mehr — *alle* Sessions, Sessions zu einer
//! Datei — deshalb steht das hier gebündelt statt in jedem Kommando erneut.
//!
//! Beim Vergessen gilt beim Einsammeln derselbe Vertrag wie bei `show`/`why`
//! (ADR-0007): Eine getilgte Session wird übersprungen, nicht zum Fehler des
//! Laufs. Für Defekte (korrupt, missgebildet, unredigiert) geht dieser
//! Sammel-Pfad darüber hinaus — auch sie fallen nur selbst aus (#83). Die
//! Kommandos nennen die Zahl der Übersprungenen ([`Skipped::note`]); ehrlich
//! lückenhaft schlägt still vollständig.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use minds_core::{Session, SessionId};
use minds_git::{BlameProvider, CommitId, Repo};
use minds_store::{ContextStore, StoreError};

use crate::config;
use crate::render;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Beim Einsammeln übersprungene Sessions — je Ursache getrennt, weil sie
/// Verschiedenes bedeuten: **vergessen** ist ein gewollter Zustand (DSGVO,
/// `minds forget`), **unlesbar** ein Defekt, dem `minds fsck` nachgeht.
///
/// Gesammelt werden **Ids**, keine nackten Zähler: Läuft ein Kommando mehrfach
/// über dieselbe Session — `blame` sieht sie an jedem ihrer Commits —, zählt
/// sie trotzdem nur einmal.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// Getilgt per `minds forget` — die Referenz löst auf einen Tombstone auf.
    forgotten: BTreeSet<SessionId>,
    /// Korrupt, missgebildet oder unredigiert — nur diese eine Session ist
    /// betroffen, nicht der Store.
    unreadable: BTreeSet<SessionId>,
}

impl Skipped {
    /// Nimmt die Ids von `other` in diesen auf — für Läufe, die mehrfach
    /// einsammeln (etwa `blame`, Commit für Commit).
    pub fn merge(&mut self, other: Skipped) {
        self.forgotten.extend(other.forgotten);
        self.unreadable.extend(other.unreadable);
    }

    /// Der Hinweis für den Nutzer, oder `None`, wenn nichts übersprungen wurde.
    /// Vergessen und unlesbar sind getrennt beziffert; nur der Defekt verweist
    /// auf `minds fsck`.
    pub fn note(&self) -> Option<String> {
        fn part(n: usize, adjective: &str) -> Option<String> {
            match n {
                0 => None,
                1 => Some(format!("1 {adjective} Session")),
                n => Some(format!("{n} {adjective} Sessions")),
            }
        }
        let forgotten = part(self.forgotten.len(), "vergessene");
        let unreadable = part(self.unreadable.len(), "unlesbare");
        Some(match (forgotten, unreadable) {
            (None, None) => return None,
            (Some(f), None) => format!("{f} übersprungen"),
            (None, Some(u)) => format!("{u} übersprungen — siehe minds fsck"),
            (Some(f), Some(u)) => format!("{f} und {u} übersprungen — siehe minds fsck"),
        })
    }
}

/// Ein geöffnetes Repo samt konfiguriertem Store.
pub struct Context {
    pub repo: Repo,
    pub root: PathBuf,
    pub store: Box<dyn ContextStore>,
}

impl Context {
    /// Öffnet das Repo um das aktuelle Verzeichnis und den daran konfigurierten
    /// Store.
    pub fn open() -> Fallible<Self> {
        let cwd = std::env::current_dir()?;
        let repo = Repo::discover(&cwd)?;
        let root = repo
            .git_dir()
            .parent()
            .unwrap_or_else(|| repo.git_dir())
            .to_path_buf();
        let store = config::load(&root).open(&root)?;
        Ok(Self { repo, root, store })
    }

    /// Alle redigierten Sessions im Store, in Store-Reihenfolge (sortiert) —
    /// dazu, was übersprungen wurde (#83).
    pub fn all_sessions(&self) -> Fallible<(Vec<Session>, Skipped)> {
        let mut out = Vec::new();
        let mut skipped = Skipped::default();
        for id in self.store.list()? {
            if let Some(session) = self.get_skipping(id, &mut skipped)? {
                out.push(session);
            }
        }
        Ok((out, skipped))
    }

    /// Die Sessions hinter einem Commit **mit ihrer Id** — Trailer (beobachtet)
    /// und Store-Index (vermutet) zusammengeführt, wie bei `show`/`why` — dazu,
    /// was übersprungen wurde (#83).
    pub fn linked_sessions(
        &self,
        commit: CommitId,
    ) -> Fallible<(Vec<(SessionId, Session)>, Skipped)> {
        let trailers = self.repo.session_ids_of(commit)?;
        let index = self.store.index()?;
        let links = render::merge_links(&trailers, index.links_of(&commit.to_string()));
        let mut out = Vec::new();
        let mut skipped = Skipped::default();
        for (id, _) in links {
            if let Some(session) = self.get_skipping(id, &mut skipped)? {
                out.push((id, session));
            }
        }
        Ok((out, skipped))
    }

    /// Wie [`linked_sessions`](Self::linked_sessions), aber ohne die Ids.
    pub fn sessions_of_commit(&self, commit: CommitId) -> Fallible<(Vec<Session>, Skipped)> {
        let (linked, skipped) = self.linked_sessions(commit)?;
        Ok((
            linked.into_iter().map(|(_, session)| session).collect(),
            skipped,
        ))
    }

    /// Alle Sessions, die `path` geändert haben.
    pub fn sessions_touching(&self, path: &str) -> Fallible<(Vec<Session>, Skipped)> {
        let (sessions, skipped) = self.all_sessions()?;
        Ok((
            sessions
                .into_iter()
                .filter(|session| touches(session, path))
                .collect(),
            skipped,
        ))
    }

    /// Liest eine Session tolerant: Vergessene und unlesbare werden vermerkt
    /// und als `Ok(None)` übersprungen — nur sie selbst fällt aus, nicht der
    /// Lauf (ADR-0007, #83). Alles andere (Backend, Konfiguration) betrifft
    /// den Store als Ganzes und bleibt ein harter Fehler.
    fn get_skipping(&self, id: SessionId, skipped: &mut Skipped) -> Fallible<Option<Session>> {
        match self.store.get(id) {
            Ok(session) => Ok(session),
            Err(StoreError::Forgotten { .. }) => {
                skipped.forgotten.insert(id);
                Ok(None)
            }
            Err(
                StoreError::Corrupt { .. }
                | StoreError::Malformed { .. }
                | StoreError::Unredacted { .. },
            ) => {
                skipped.unreadable.insert(id);
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Löst eine Git-Revision (`HEAD`, Hash, Tag) zu einem Commit auf.
    pub fn resolve_rev(&self, rev: &str) -> Option<CommitId> {
        resolve(&self.root, rev)
    }

    /// Der Commit hinter `datei:zeile` via Blame — `None`, wenn HEAD leer ist
    /// oder die Zeile nicht auflösbar.
    pub fn blame_commit(&self, path: &str, line: u32) -> Fallible<Option<CommitId>> {
        let Some(head) = self.repo.head()?.commit() else {
            return Ok(None);
        };
        Ok(self.repo.blame().blame_line(head, path, line)?)
    }
}

/// `true`, wenn die Session `path` verändert hat — über die erzeugten Dateien
/// oder einen Effekt mit diesem Pfad.
pub fn touches(session: &Session, path: &str) -> bool {
    session.produced.files.iter().any(|f| f == path)
        || session.turns.iter().any(|turn| {
            turn.tool_calls.iter().any(|call| {
                call.effect
                    .as_ref()
                    .and_then(|effect| effect.path.as_deref())
                    .is_some_and(|p| p == path)
            })
        })
}

/// Ein grob sortierbarer Zeit-Schlüssel: die ersten 19 Zeichen von
/// `lineage.started_at` (`YYYY-MM-DDTHH:MM:SS`) — fixe Breite, lexikografisch
/// vergleichbar. Best-effort: fehlt die Herkunft, leer. Bewusst *keine* echte
/// Datums-Dependency (siehe `minds-core::lineage`); für „die jüngsten Sessions"
/// reicht der Präfix-Vergleich, weil Agent-Zeitstempel praktisch immer UTC-`Z`
/// mit gleichem Aufbau sind.
pub fn time_key(session: &Session) -> &str {
    session
        .lineage
        .as_ref()
        .and_then(|lineage| lineage.started_at.as_deref())
        .map(|ts| ts.get(..19).unwrap_or(ts))
        .unwrap_or("")
}

/// `git rev-parse` — versteht jede Schreibweise, die selbst zu implementieren
/// müßig wäre (siehe `show`).
fn resolve(root: &Path, rev: &str) -> Option<CommitId> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::Skipped;
    use minds_core::SessionId;

    fn sid(hex: char) -> SessionId {
        format!("b3-{}", hex.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    fn skipped(forgotten: &[char], unreadable: &[char]) -> Skipped {
        Skipped {
            forgotten: forgotten.iter().map(|c| sid(*c)).collect(),
            unreadable: unreadable.iter().map(|c| sid(*c)).collect(),
        }
    }

    #[test]
    fn nothing_skipped_yields_no_note() {
        assert_eq!(Skipped::default().note(), None);
    }

    #[test]
    fn forgotten_sessions_are_named_without_an_fsck_hint() {
        assert_eq!(
            skipped(&['a'], &[]).note().as_deref(),
            Some("1 vergessene Session übersprungen")
        );
    }

    #[test]
    fn unreadable_sessions_point_to_fsck() {
        assert_eq!(
            skipped(&[], &['a', 'b']).note().as_deref(),
            Some("2 unlesbare Sessions übersprungen — siehe minds fsck")
        );
    }

    #[test]
    fn both_causes_are_counted_separately() {
        assert_eq!(
            skipped(&['a', 'b'], &['c']).note().as_deref(),
            Some("2 vergessene Sessions und 1 unlesbare Session übersprungen — siehe minds fsck")
        );
    }

    #[test]
    fn merge_counts_the_same_session_only_once() {
        // `blame` sieht dieselbe vergessene Session an jedem ihrer Commits —
        // gezählt wird sie trotzdem einmal.
        let mut skipped_ids = skipped(&['a'], &[]);
        skipped_ids.merge(skipped(&['a', 'b'], &['c']));
        assert_eq!(skipped_ids, skipped(&['a', 'b'], &['c']));
    }
}
