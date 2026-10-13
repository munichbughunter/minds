//! Vorfahren und Pfade in einer Historie, die der beobachtete Agent
//! geschrieben haben kann: jeder Commit und jeder Baum am Header begrenzt,
//! neu gehasht, mit Deckeln für Commits, Eltern und Bytes.
//!
//! Für Aussagen, bei denen ein Fehlen nicht „nein" heißen darf: Was ein
//! Deckel abschneidet oder was fehlt (Shallow Clone), macht das Ergebnis
//! **unentschieden**, nie negativ.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::{GitError, Result};
use crate::oid::{BlobId, CommitId};
use crate::repo::Repo;

/// Höchstgröße eines Commit-Objekts in einer Vorfahren-Suche. Echte Commits
/// (auch signierte, auch mit langer Squash-Nachricht) bleiben weit darunter.
pub const ANCESTRY_COMMIT_MAX: u64 = 1024 * 1024;

/// Höchstzahl der Eltern eines Commits (wie im Graphen der Übersicht).
pub const ANCESTRY_MAX_PARENTS: usize = 16;

/// Was eine Suche verbrauchen darf — geteilt über mehrere Aufrufe einer
/// Auswertung.
#[derive(Debug, Clone)]
pub struct Budget {
    /// Noch zu lesende Commits.
    pub commits: usize,
    /// Noch zu lesende Bytes (Commits und Bäume).
    pub bytes: u64,
    /// Noch erlaubte Pfad-Nachschläge ([`Repo::entry_at`]) — auch aus dem
    /// Cache: Ein Weg über Hunderttausende Commits kostet sonst Zeit, ohne
    /// Bytes zu lesen.
    pub lookups: usize,
}

/// So viele Pfad-Nachschläge erlaubt [`Budget::new`] je gelesenem Commit.
pub const LOOKUPS_PER_COMMIT: usize = 8;

impl Budget {
    /// Ein Budget; Nachschläge: [`LOOKUPS_PER_COMMIT`] je Commit.
    pub fn new(commits: usize, bytes: u64) -> Self {
        Self {
            commits,
            bytes,
            lookups: commits.saturating_mul(LOOKUPS_PER_COMMIT),
        }
    }

    fn charge(&mut self, bytes: u64) -> bool {
        if self.bytes < bytes {
            self.bytes = 0;
            return false;
        }
        self.bytes -= bytes;
        true
    }
}

/// Eine Breitensuche über Vorfahren.
#[derive(Debug, Clone, Default)]
pub struct Walk {
    /// Je besuchtem und expandiertem Commit seine Eltern.
    pub parents: HashMap<CommitId, Vec<CommitId>>,
    /// Alle besuchten Commits (auch nicht expandierte Grenzen).
    pub seen: HashSet<CommitId>,
    /// Ob die Suche vollständig ist (kein Deckel, nichts fehlte).
    pub complete: bool,
}

/// Ob ein Commit auf dem Weg liegt — oder warum es nicht entschieden ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathResult {
    /// `at` ist Vorfahr (oder gleich) von `commit`; die Commits, die
    /// Nachfahren von `at` und Vorfahren von `commit` sind (beide
    /// eingeschlossen), wie `git log --ancestry-path`.
    Path(Vec<CommitId>),
    /// `at` liegt nicht in der Historie von `commit`.
    NotInHistory,
    /// Nicht entschieden (Deckel, fehlende oder beschädigte Objekte).
    Undecided,
}

impl Repo {
    /// Die Eltern eines Commits, geprüft und gedeckelt: höchstens
    /// [`ANCESTRY_COMMIT_MAX`] Bytes, höchstens [`ANCESTRY_MAX_PARENTS`]
    /// Eltern (doppelte zählen einmal), Bytes gegen `budget`.
    pub fn bounded_parents(&self, commit: CommitId, budget: &mut Budget) -> Result<Vec<CommitId>> {
        let limit = ANCESTRY_COMMIT_MAX.min(budget.bytes);
        let data = self.verified_object(commit.to_gix(), gix::objs::Kind::Commit, limit)?;
        if !budget.charge(data.len() as u64) {
            return Err(GitError::TooLarge {
                id: commit.to_string(),
                size: data.len() as u64,
                limit,
            });
        }
        let parsed = gix::objs::CommitRef::from_bytes(&data, self.gix().object_hash())
            .map_err(|err| GitError::read_object(commit, err))?;
        let mut parents: Vec<CommitId> = Vec::new();
        for parent in parsed.parents() {
            let parent = CommitId::from_gix(parent);
            if parents.contains(&parent) {
                continue;
            }
            if parents.len() == ANCESTRY_MAX_PARENTS {
                return Err(GitError::read_object(commit, "too many parents"));
            }
            parents.push(parent);
        }
        Ok(parents)
    }

    /// Breitensuche über die Vorfahren von `starts` (eingeschlossen); Commits
    /// in `stop` werden besucht, aber nicht expandiert.
    pub fn walk_ancestors(
        &self,
        starts: &[CommitId],
        stop: &HashSet<CommitId>,
        budget: &mut Budget,
    ) -> Walk {
        let mut walk = Walk {
            complete: true,
            ..Walk::default()
        };
        let mut queue: VecDeque<CommitId> = VecDeque::new();
        for start in starts {
            if walk.seen.insert(*start) {
                queue.push_back(*start);
            }
        }
        while let Some(at) = queue.pop_front() {
            if stop.contains(&at) {
                continue;
            }
            if budget.commits == 0 {
                walk.complete = false;
                break;
            }
            budget.commits -= 1;
            if !self.has_commit(at) {
                walk.complete = false; // abgeschnitten (Shallow Clone)
                continue;
            }
            let Ok(parents) = self.bounded_parents(at, budget) else {
                walk.complete = false;
                break;
            };
            for parent in &parents {
                if walk.seen.insert(*parent) {
                    queue.push_back(*parent);
                }
            }
            walk.parents.insert(at, parents);
        }
        walk
    }

    /// Die Historie von `commit` — einmal gelaufen, für beliebig viele
    /// Fragen danach ([`History::place`], [`History::descends`]).
    pub fn history(&self, commit: CommitId, budget: &mut Budget) -> History {
        let walk = self.walk_ancestors(&[commit], &HashSet::new(), budget);
        let mut children: HashMap<CommitId, Vec<CommitId>> = HashMap::new();
        for (child, parents) in &walk.parents {
            for parent in parents {
                children.entry(*parent).or_default().push(*child);
            }
        }
        History { walk, children }
    }
}

/// Die Vorfahren eines Commits samt Kinder-Karte.
#[derive(Debug, Clone, Default)]
pub struct History {
    walk: Walk,
    children: HashMap<CommitId, Vec<CommitId>>,
}

impl History {
    /// Ob die Suche vollständig ist (kein Deckel, nichts fehlte).
    pub fn complete(&self) -> bool {
        self.walk.complete
    }

    /// Die Commits auf dem Weg von `at` zum Ausgangs-Commit (beide
    /// eingeschlossen, wie `git log --ancestry-path`). Nur entschieden,
    /// wenn die Suche vollständig ist: Sonst könnten Commits des Wegs oder
    /// `at` selbst hinter der Grenze liegen.
    pub fn place(&self, at: CommitId) -> PathResult {
        if !self.walk.complete {
            return PathResult::Undecided;
        }
        if !self.walk.seen.contains(&at) {
            return PathResult::NotInHistory;
        }
        PathResult::Path(self.closure(at))
    }

    /// Ob `descendant` ein Nachfahre von (oder gleich) `ancestor` ist —
    /// innerhalb dieser Historie.
    pub fn descends(&self, ancestor: CommitId, descendant: CommitId) -> bool {
        ancestor == descendant || self.closure(ancestor).contains(&descendant)
    }

    /// `at` und alle seine Nachfahren in dieser Historie.
    pub fn descendants(&self, at: CommitId) -> HashSet<CommitId> {
        self.closure(at).into_iter().collect()
    }

    /// Ob `at` in der Historie liegt — `None`, wenn die Suche unvollständig
    /// ist (dann ist nichts entschieden).
    pub fn contains(&self, at: CommitId) -> Option<bool> {
        self.walk.complete.then(|| self.walk.seen.contains(&at))
    }

    /// Alle Commits dieser Historie (bei vollständiger Suche: genau die
    /// Vorfahren des Ausgangs-Commits samt ihm selbst).
    pub fn commits(&self) -> impl Iterator<Item = &CommitId> {
        self.walk.parents.keys()
    }

    /// `at` und alle seine Vorfahren, in Breitensuche-Reihenfolge ab `at`.
    pub fn ancestors(&self, at: CommitId) -> Vec<CommitId> {
        let mut order = vec![at];
        let mut seen: HashSet<CommitId> = HashSet::from([at]);
        let mut queue = VecDeque::from([at]);
        while let Some(node) = queue.pop_front() {
            for parent in self.walk.parents.get(&node).into_iter().flatten() {
                if seen.insert(*parent) {
                    order.push(*parent);
                    queue.push_back(*parent);
                }
            }
        }
        order
    }

    /// Die Generation eines Commits: 1 + die größte seiner Eltern (Wurzeln:
    /// 1). Ein Nachfahre hat immer eine größere als sein Vorfahr.
    pub fn generations(&self) -> HashMap<CommitId, usize> {
        let mut generation: HashMap<CommitId, usize> = HashMap::new();
        for start in self.walk.parents.keys() {
            if generation.contains_key(start) {
                continue;
            }
            let mut stack = vec![(*start, false)];
            while let Some((node, ready)) = stack.pop() {
                if generation.contains_key(&node) {
                    continue;
                }
                let parents = self
                    .walk
                    .parents
                    .get(&node)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if ready {
                    let value = parents
                        .iter()
                        .filter_map(|p| generation.get(p))
                        .max()
                        .copied()
                        .unwrap_or(0)
                        + 1;
                    generation.insert(node, value);
                } else {
                    stack.push((node, true));
                    for parent in parents {
                        if !generation.contains_key(parent) {
                            stack.push((*parent, false));
                        }
                    }
                }
            }
        }
        generation
    }

    /// `at` und alle seine Nachfahren in dieser Historie.
    fn closure(&self, at: CommitId) -> Vec<CommitId> {
        let mut path = vec![at];
        let mut seen: HashSet<CommitId> = HashSet::from([at]);
        let mut queue = VecDeque::from([at]);
        while let Some(node) = queue.pop_front() {
            for child in self.children.get(&node).into_iter().flatten() {
                if seen.insert(*child) {
                    path.push(*child);
                    queue.push_back(*child);
                }
            }
        }
        path
    }
}

/// Die geprüften Bäume einer Auswertung, nach Id — ein Baum wird einmal
/// gehasht, egal in wie vielen Commits er vorkommt.
#[derive(Debug, Default)]
pub struct TreeCache {
    trees: HashMap<gix::ObjectId, HashMap<Vec<u8>, (Kind, gix::ObjectId)>>,
    /// Der Wurzelbaum je schon geprüftem Commit.
    roots: HashMap<CommitId, gix::ObjectId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tree,
    Blob,
    Other,
}

/// Was unter einem Pfad eines Commits liegt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathEntry {
    /// Nichts.
    Absent,
    /// Eine reguläre Datei.
    Blob(BlobId),
    /// Ein Symlink, Submodul oder Verzeichnis — kein lesbarer Inhalt.
    Other,
}

impl Repo {
    /// Was unter `path` in `commit` liegt — Commit und Bäume geprüft (Header,
    /// Hash, Sortierung, eindeutige Namen), Bytes gegen `budget`, Bäume aus
    /// `cache`. Liest keinen Blob.
    pub fn entry_at(
        &self,
        commit: CommitId,
        path: &str,
        cache: &mut TreeCache,
        budget: &mut Budget,
    ) -> Result<PathEntry> {
        if budget.lookups == 0 {
            return Err(GitError::TooLarge {
                id: commit.to_string(),
                size: 0,
                limit: 0,
            });
        }
        budget.lookups -= 1;
        let hash_kind = self.gix().object_hash();
        let mut tree = match cache.roots.get(&commit) {
            Some(tree) => *tree,
            None => {
                let limit = ANCESTRY_COMMIT_MAX.min(budget.bytes);
                let data = self.verified_object(commit.to_gix(), gix::objs::Kind::Commit, limit)?;
                if !budget.charge(data.len() as u64) {
                    return Err(GitError::TooLarge {
                        id: commit.to_string(),
                        size: data.len() as u64,
                        limit,
                    });
                }
                let tree = gix::objs::CommitRef::from_bytes(&data, hash_kind)
                    .map_err(|err| GitError::read_object(commit, err))?
                    .tree();
                cache.roots.insert(commit, tree);
                tree
            }
        };
        let parts: Vec<&str> = path.split('/').collect();
        for (index, part) in parts.iter().enumerate() {
            if let std::collections::hash_map::Entry::Vacant(slot) = cache.trees.entry(tree) {
                let limit = crate::objects::MAX_VERIFIED_OBJECT.min(budget.bytes);
                let data = self.verified_object(tree, gix::objs::Kind::Tree, limit)?;
                if !budget.charge(data.len() as u64) {
                    return Err(GitError::TooLarge {
                        id: tree.to_string(),
                        size: data.len() as u64,
                        limit,
                    });
                }
                let parsed = gix::objs::TreeRef::from_bytes(&data, hash_kind)
                    .map_err(|err| GitError::read_object(tree, err))?;
                if !crate::objects::strictly_sorted(&parsed) {
                    return Err(GitError::read_object(
                        tree,
                        "tree is not sorted or has duplicate entries",
                    ));
                }
                let entries = parsed
                    .entries
                    .iter()
                    .map(|entry| {
                        let kind = if entry.mode.is_tree() {
                            Kind::Tree
                        } else if entry.mode.is_blob() {
                            Kind::Blob
                        } else {
                            Kind::Other
                        };
                        (entry.filename.to_vec(), (kind, entry.oid.to_owned()))
                    })
                    .collect();
                slot.insert(entries);
            }
            let entries = &cache.trees[&tree];
            let Some((kind, id)) = entries.get(part.as_bytes()) else {
                return Ok(PathEntry::Absent);
            };
            let last = index + 1 == parts.len();
            match (kind, last) {
                (Kind::Tree, false) => tree = *id,
                (Kind::Blob, true) => return Ok(PathEntry::Blob(BlobId::from_gix(*id))),
                (_, true) => return Ok(PathEntry::Other),
                (_, false) => return Ok(PathEntry::Other),
            }
        }
        Ok(PathEntry::Absent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::TempRepo;

    fn budget() -> Budget {
        Budget::new(10_000, 64 << 20)
    }

    /// c1 ── c2 ── m (Merge) ── c4
    ///   └── f1 ──┘
    #[test]
    fn history_places_commits_across_merges() {
        let fixture = TempRepo::init();
        let c1 = fixture.commit("c1");
        let c2 = fixture.commit("c2");
        fixture.git(&["checkout", "--quiet", "-b", "feature", &c1.to_string()]);
        let f1 = fixture.commit("f1");
        fixture.git(&["checkout", "--quiet", "main"]);
        fixture.git(&["merge", "--quiet", "--no-ff", "-m", "m", "feature"]);
        let m = fixture.rev_parse("HEAD");
        let c4 = fixture.commit("c4");
        let repo = Repo::open(fixture.path()).unwrap();
        let history = repo.history(c4, &mut budget());
        assert!(history.complete());
        let PathResult::Path(mut path) = history.place(c2) else {
            panic!("c2 liegt in der Historie");
        };
        path.sort();
        let mut expected = vec![c2, m, c4];
        expected.sort();
        assert_eq!(path, expected);
        // Der Weg von c1 aus enthält beide Zweige.
        let PathResult::Path(path) = history.place(c1) else {
            panic!();
        };
        assert!(path.contains(&f1) && path.contains(&c2));
        assert!(history.descends(c1, f1));
        assert!(!history.descends(f1, c2));
        assert!(!history.descends(c2, f1));
        // f1 ist kein Vorfahr von c2.
        let history_c2 = repo.history(c2, &mut budget());
        assert_eq!(history_c2.place(f1), PathResult::NotInHistory);
        // Ein zu kleines Budget entscheidet nichts.
        let history = repo.history(c4, &mut Budget::new(2, 64 << 20));
        assert_eq!(history.place(c1), PathResult::Undecided);
    }

    /// Ein Commit mit tausend gleichen `parent`-Zeilen: einmal gezählt.
    #[test]
    fn duplicate_parents_count_once() {
        let fixture = TempRepo::init();
        let c1 = fixture.commit("c1");
        let tree = fixture.hash("HEAD^{tree}");
        let mut text = format!("tree {tree}\n");
        for _ in 0..1000 {
            text.push_str(&format!("parent {c1}\n"));
        }
        text.push_str("author A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nm\n");
        let commit: CommitId = fixture
            .write_raw_object("commit", text.as_bytes())
            .parse()
            .unwrap();
        let repo = Repo::open(fixture.path()).unwrap();
        assert_eq!(
            repo.bounded_parents(commit, &mut budget()).unwrap(),
            vec![c1]
        );
    }

    #[test]
    fn entry_at_reads_paths_without_blobs() {
        let fixture = TempRepo::init();
        fixture.write_file(".minds/spec.json", "{}");
        let commit = fixture.commit("spec");
        let repo = Repo::open(fixture.path()).unwrap();
        let mut cache = TreeCache::default();
        let mut budget = budget();
        let PathEntry::Blob(blob) = repo
            .entry_at(commit, ".minds/spec.json", &mut cache, &mut budget)
            .unwrap()
        else {
            panic!();
        };
        assert_eq!(blob.to_string(), fixture.hash("HEAD:.minds/spec.json"));
        assert_eq!(
            repo.entry_at(commit, ".minds/nope", &mut cache, &mut budget)
                .unwrap(),
            PathEntry::Absent
        );
        assert_eq!(
            repo.entry_at(commit, ".minds", &mut cache, &mut budget)
                .unwrap(),
            PathEntry::Other
        );
    }
}
