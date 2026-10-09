//! Die Historie als Graph: Commits mit Eltern, Autor, Zeit und Betreff — für
//! eine Ansicht, die Zweige und Merges zeichnet ([`Repo::graph`]).
//!
//! Strikt lesend und ohne die Abkürzungen, die ein Agent fälschen könnte:
//! Jeder Commit wird roh gelesen und muss zu seiner Id hashen — ein
//! Ersatzobjekt (`refs/replace`, auch bei verdrehtem `core.useReplaceRefs`)
//! fällt so auf, statt fremden Betreff und fremde Eltern unter echter Id zu
//! zeigen; die commit-graph-Datei wird nicht benutzt. Was sich nicht lesen
//! lässt (ein Ref auf einen Baum, ein fehlendes Elternobjekt), lässt den
//! Graphen nicht scheitern: Er zeigt, was er lesen konnte, und sagt, dass
//! etwas fehlt.
//!
//! Die Reihenfolge ist **topologisch**: Ein Kind steht immer vor seinen
//! Eltern — auch wenn Zeitstempel lügen (Rebase, verstellte Uhr). Innerhalb
//! dieser Regel gehen neuere Commits vor (Committer-Zeit, wie `git log
//! --date-order`).

use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::diff::verify_object;
use crate::oid::CommitId;
use crate::repo::Repo;

/// Höchstens so viele Startpunkte (Tips) liest der Graph.
pub const MAX_TIPS: usize = 256;

/// Höchstens so vielen Eltern eines Commits folgt der Graph (ein Octopus
/// mit mehr bleibt gekürzt).
pub const MAX_PARENTS: usize = 16;

/// So viele Bytes eines Autornamens oder Betreffs werden gelesen.
const FIELD_LIMIT: usize = 1024;

/// So viele Zeichen behält der Betreff, so viele der Autor.
const SUMMARY_CHARS: usize = 300;
const AUTHOR_CHARS: usize = 100;

/// So groß darf ein Commit-Objekt höchstens sein — geprüft am Header,
/// **bevor** es entpackt wird: Ein Commit mit gigabytegroßer Nachricht (als
/// zlib-Bombe nur wenige MB) füllte sonst den Speicher beim Öffnen der
/// Übersicht.
pub const MAX_COMMIT_OBJECT: u64 = 256 * 1024;

/// Die Allokationsgrenze der Lesewege der Übersicht (gix `allocLimit`): Sie
/// gilt auch beim Auflösen von Delta-Ketten, deren Basis der Header nicht
/// verrät.
pub const READ_ALLOC_LIMIT: u64 = 1024 * 1024;

/// So viele Bytes (laut Header) liest ein Graph insgesamt höchstens; darüber
/// endet er gekappt.
pub const GRAPH_BUDGET: u64 = 64 * 1024 * 1024;

/// So viele Delta-Stufen darf ein Objekt im Pack haben (Git nimmt 50):
/// `allocLimit` begrenzt den Speicher, nicht die Arbeit — eine lange Kette
/// winziger Deltas kopierte sonst Gigabytes.
pub const MAX_DELTAS: u32 = 64;

/// So viele Header-Lookups erlaubt ein begrenzter Zugriff insgesamt (Graph,
/// Refs, HEAD) — `find_header` läuft eine Delta-Kette ganz ab, ehe
/// [`MAX_DELTAS`] greifen kann; gix bietet dafür keine Grenze.
pub const MAX_LOOKUPS: usize = 20_000;

/// So lange darf ein begrenzter Zugriff insgesamt lesen.
pub const READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// Der begrenzte Lesezugriff einer Ansicht ([`Repo::bounded_reader`]) —
/// einmal geöffnet, für Graph, Refs und HEAD; mit gemeinsamem Deckel für
/// Lookups und Zeit.
pub struct BoundedRepo {
    gix: gix::Repository,
    lookups: std::cell::Cell<usize>,
    started: std::time::Instant,
    limit: usize,
    deadline: std::time::Duration,
}

/// Ein Commit im Graphen. Autor und Betreff sind **roh** (aus dem Objekt),
/// nicht entschärft — die Anzeige entschärft an ihrer Senke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphCommit {
    /// Der Commit.
    pub id: CommitId,
    /// Die Eltern, erster zuerst, höchstens [`MAX_PARENTS`]. Eltern
    /// außerhalb der Grenze stehen trotzdem hier — der Graph zeichnet sie
    /// als offene Kante.
    pub parents: Vec<CommitId>,
    /// Der Name des Autors.
    pub author: String,
    /// Die Autor-Zeit in Sekunden seit Unix-Epoch.
    pub time: i64,
    /// Die erste Zeile der Nachricht.
    pub summary: String,
}

/// Der gelesene Graph. `GraphCommit::time` ist die Autor-Zeit (Anzeige);
/// geordnet wird nach der Committer-Zeit, wie `git log --date-order`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Graph {
    /// Die Commits, Kinder vor Eltern.
    pub commits: Vec<GraphCommit>,
    /// Ob es jenseits der Grenze weitere Commits gibt.
    pub truncated: bool,
    /// Die Tips, denen der Graph nicht folgte (kein lesbarer Commit, zu
    /// groß, ersetzt, über [`MAX_TIPS`]).
    pub skipped: Vec<CommitId>,
    /// Ob ein Elter fehlte, kein Commit war, zu groß war oder nicht zu
    /// seiner Id hashte (ersetzt; auch die Grenze eines Shallow Clones).
    pub incomplete: bool,
    /// Ob [`GRAPH_BUDGET`] die Kappung auslöste (nicht die Commit-Zahl).
    pub budget_hit: bool,
}

/// Ein gelesener Commit samt Committer-Zeit (für die Reihenfolge).
struct Read {
    commit: GraphCommit,
    order: i64,
}

impl Repo {
    /// Der begrenzte Lesezugriff für Ansichten — einmal öffnen, mehrfach
    /// lesen.
    pub fn bounded_reader(&self) -> crate::error::Result<BoundedRepo> {
        self.bounded_reader_with(MAX_LOOKUPS, READ_DEADLINE)
    }

    /// [`Repo::bounded_reader`] mit ausdrücklichen Deckeln (Tests).
    pub fn bounded_reader_with(
        &self,
        lookups: usize,
        deadline: std::time::Duration,
    ) -> crate::error::Result<BoundedRepo> {
        Ok(BoundedRepo {
            gix: self.bounded(READ_ALLOC_LIMIT)?,
            lookups: std::cell::Cell::new(0),
            started: std::time::Instant::now(),
            limit: lookups,
            deadline,
        })
    }

    /// [`BoundedRepo::graph`] über einen eigens geöffneten Zugriff.
    pub fn graph(&self, tips: &[CommitId], limit: usize) -> Graph {
        match self.bounded_reader() {
            Ok(reader) => reader.graph(tips, limit),
            Err(_) => Graph {
                incomplete: true,
                ..Graph::default()
            },
        }
    }
}

impl BoundedRepo {
    /// Der begrenzte gix-Zugriff (für Refs und HEAD in diesem Crate).
    pub(crate) fn gix(&self) -> &gix::Repository {
        &self.gix
    }

    /// Ob noch ein Header-Lookup erlaubt ist — und zählt ihn.
    pub(crate) fn permit(&self) -> bool {
        let used = self.lookups.get();
        if used >= self.limit || self.started.elapsed() >= self.deadline {
            return false;
        }
        self.lookups.set(used + 1);
        true
    }

    /// Ob ein Deckel (Lookups oder Zeit) erreicht ist.
    pub fn exhausted(&self) -> bool {
        self.lookups.get() >= self.limit || self.started.elapsed() >= self.deadline
    }

    /// Die von `tips` aus erreichbaren Commits, höchstens `limit`, Kinder vor
    /// Eltern, sonst neuere zuerst.
    pub fn graph(&self, tips: &[CommitId], limit: usize) -> Graph {
        let mut graph = Graph::default();
        let gix = &self.gix;
        let spent = std::cell::Cell::new(0u64);
        // Belastet wird, sobald ein Objekt zugelassen ist — vor dem Entpacken,
        // auch wenn es danach scheitert (falscher Hash, kaputt).
        let read = |id: CommitId| -> Option<Read> {
            if spent.get() > GRAPH_BUDGET || !self.permit() {
                return None;
            }
            read_graph_commit(gix, id, &spent)
        };
        let mut skipped: HashSet<CommitId> = HashSet::new();
        let mut queued: HashSet<CommitId> = HashSet::new();
        let mut heap: BinaryHeap<(i64, CommitId)> = BinaryHeap::new();
        let mut pending: HashMap<CommitId, GraphCommit> = HashMap::new();
        for tip in tips {
            if queued.contains(tip) || skipped.contains(tip) {
                continue;
            }
            if queued.len() >= MAX_TIPS {
                skipped.insert(*tip);
                continue;
            }
            queued.insert(*tip);
            match read(*tip) {
                Some(read) => {
                    heap.push((read.order, *tip));
                    pending.insert(*tip, read.commit);
                }
                None => {
                    skipped.insert(*tip);
                }
            }
        }
        while let Some((_, id)) = heap.pop() {
            if graph.commits.len() == limit || spent.get() > GRAPH_BUDGET || self.exhausted() {
                graph.truncated = true;
                graph.budget_hit = spent.get() > GRAPH_BUDGET || self.exhausted();
                break;
            }
            let Some(commit) = pending.remove(&id) else {
                continue;
            };
            for parent in &commit.parents {
                if !queued.insert(*parent) {
                    continue;
                }
                match read(*parent) {
                    Some(read) => {
                        heap.push((read.order, *parent));
                        pending.insert(*parent, read.commit);
                    }
                    None => graph.incomplete = true,
                }
            }
            graph.commits.push(commit);
        }
        graph.commits = topological(std::mem::take(&mut graph.commits));
        if self.exhausted() || spent.get() > GRAPH_BUDGET {
            graph.truncated = true;
            graph.budget_hit = true;
        }
        let mut seen = HashSet::new();
        graph.skipped = tips
            .iter()
            .filter(|t| skipped.contains(t) && seen.insert(**t))
            .copied()
            .collect();
        graph
    }
}

/// Liest einen Commit über den begrenzten Zugriff, prüft Größe (Header),
/// Hash und Art — `None`, wenn das nicht geht (fehlt, zu groß, kein Commit,
/// ersetzt).
fn read_graph_commit(
    gix: &gix::Repository,
    id: CommitId,
    spent: &std::cell::Cell<u64>,
) -> Option<Read> {
    let header = gix.find_header(id.to_gix()).ok()?;
    if !admissible(
        &header,
        gix::objs::Kind::Commit,
        MAX_COMMIT_OBJECT,
        MAX_DELTAS,
    ) {
        return None;
    }
    spent.set(spent.get().saturating_add(header.size()));
    let object = gix.find_object(id.to_gix()).ok()?;
    if object.kind != gix::objs::Kind::Commit {
        return None;
    }
    let data = object.detach().data;
    verify_object(id.to_gix(), gix::objs::Kind::Commit, &data).ok()?;
    let decoded = gix::objs::CommitRef::from_bytes(&data, id.to_gix().kind()).ok()?;
    let parents = decoded
        .parents()
        .take(MAX_PARENTS)
        .map(CommitId::from_gix)
        .collect();
    let (author, time) = match decoded.author() {
        Ok(signature) => (
            clip(signature.name, AUTHOR_CHARS),
            signature.time().map(|t| t.seconds).unwrap_or(0),
        ),
        Err(_) => (String::new(), 0),
    };
    let order = decoded
        .committer()
        .ok()
        .and_then(|c| c.time().ok())
        .map_or(time, |t| t.seconds);
    let line = decoded
        .message
        .split(|b| *b == b'\n')
        .next()
        .unwrap_or_default();
    Some(Read {
        commit: GraphCommit {
            id,
            parents,
            author,
            time,
            summary: clip(line, SUMMARY_CHARS),
        },
        order,
    })
}

/// Ob ein Objekt laut Header gelesen werden darf: die erwartete Art, nicht
/// größer als `max_size`, nicht mehr als `max_deltas` Delta-Stufen.
pub(crate) fn admissible(
    header: &gix::odb::find::Header,
    kind: gix::objs::Kind,
    max_size: u64,
    max_deltas: u32,
) -> bool {
    header.kind() == kind
        && header.size() <= max_size
        && header.num_deltas().unwrap_or(0) <= max_deltas
}

/// Höchstens [`FIELD_LIMIT`] Bytes, dann tolerant dekodiert (ein
/// angeschnittenes Zeichen wird zu U+FFFD), dann höchstens `chars` Zeichen.
fn clip(bytes: &[u8], chars: usize) -> String {
    let cut = &bytes[..bytes.len().min(FIELD_LIMIT)];
    String::from_utf8_lossy(cut)
        .chars()
        .take(chars)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Bringt `commits` in eine Ordnung, in der jedes Kind vor seinen Eltern
/// steht; sonst bleibt die gegebene Reihenfolge. Kahn über die Kanten
/// innerhalb der Menge — je Runde der erste freie Commit, quadratisch, für
/// die Grenzen einer Ansicht (Hunderte) genug.
fn topological(commits: Vec<GraphCommit>) -> Vec<GraphCommit> {
    let present: HashSet<CommitId> = commits.iter().map(|c| c.id).collect();
    let mut waiting: HashMap<CommitId, usize> = HashMap::new();
    for commit in &commits {
        for parent in &commit.parents {
            if present.contains(parent) {
                *waiting.entry(*parent).or_default() += 1;
            }
        }
    }
    let mut out = Vec::with_capacity(commits.len());
    let mut rest = commits;
    while !rest.is_empty() {
        let at = rest
            .iter()
            .position(|c| waiting.get(&c.id).copied().unwrap_or(0) == 0)
            // Ein Kreis kann es in Git nicht geben; falls doch, nicht hängen.
            .unwrap_or(0);
        let commit = rest.remove(at);
        for parent in &commit.parents {
            if let Some(count) = waiting.get_mut(parent) {
                *count = count.saturating_sub(1);
            }
        }
        out.push(commit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::TempRepo;

    #[test]
    fn a_linear_history_comes_newest_first_with_parents() {
        let fixture = TempRepo::init();
        let a = fixture.commit("a: first");
        let b = fixture.commit("b: second\n\nbody");
        let repo = Repo::open(fixture.path()).unwrap();
        let graph = repo.graph(&[b], 10);
        assert_eq!(
            graph.commits.iter().map(|c| c.id).collect::<Vec<_>>(),
            [b, a]
        );
        assert_eq!(graph.commits[0].parents, [a]);
        assert!(graph.commits[1].parents.is_empty());
        assert_eq!(graph.commits[0].summary, "b: second");
        assert!(!graph.commits[0].author.is_empty());
        assert!(!graph.truncated && !graph.incomplete);
    }

    #[test]
    fn a_merge_keeps_children_before_parents_and_the_limit_holds() {
        let fixture = TempRepo::init();
        let base = fixture.commit("base");
        fixture.git(&["switch", "-q", "-c", "side"]);
        let side = fixture.commit("side");
        fixture.git(&["switch", "-q", "-"]);
        let main = fixture.commit("main");
        fixture.git(&["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        let merge = fixture.rev_parse("HEAD");
        let repo = Repo::open(fixture.path()).unwrap();
        let graph = repo.graph(&[merge, side], 10).commits;
        let at = |id: CommitId| graph.iter().position(|c| c.id == id).unwrap();
        assert_eq!(at(merge), 0);
        assert!(at(main) < at(base));
        assert!(at(side) < at(base));
        assert_eq!(graph[0].parents, [main, side]);
        let cut = repo.graph(&[merge], 2);
        assert_eq!(cut.commits.len(), 2);
        assert!(cut.truncated);
        assert!(repo.graph(&[], 10).commits.is_empty());
    }

    /// Ein unbekannter Tip legt den Graphen nicht lahm: übersprungen, gezählt.
    #[test]
    fn an_unreadable_tip_is_skipped_not_fatal() {
        let fixture = TempRepo::init();
        let a = fixture.commit("a");
        let repo = Repo::open(fixture.path()).unwrap();
        let missing: CommitId = "1234567890123456789012345678901234567890".parse().unwrap();
        let graph = repo.graph(&[missing, a, missing], 10);
        assert_eq!(graph.skipped, [missing], "deduplicated");
        assert_eq!(graph.commits.len(), 1);
    }

    /// Ein übergroßes Commit-Objekt (etwa eine zlib-Bombe) wird nie
    /// entpackt: übersprungen und gemeldet.
    #[test]
    fn an_oversized_commit_is_never_inflated() {
        let fixture = TempRepo::init();
        let base = fixture.commit("base");
        let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
        let message = "x".repeat(MAX_COMMIT_OBJECT as usize + 1);
        let raw = format!(
            "tree {}\nparent {base}\nauthor A <a@x> 0 +0000\ncommitter A <a@x> 0 +0000\n\n{message}\n",
            tree.trim()
        );
        let big: CommitId = fixture
            .write_raw_object("commit", raw.as_bytes())
            .trim()
            .parse()
            .unwrap();
        let child_raw = format!(
            "tree {}\nparent {big}\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nchild\n",
            tree.trim()
        );
        let child: CommitId = fixture
            .write_raw_object("commit", child_raw.as_bytes())
            .trim()
            .parse()
            .unwrap();
        let repo = Repo::open(fixture.path()).unwrap();
        let graph = repo.graph(&[child], 10);
        assert_eq!(graph.commits.len(), 1);
        assert!(graph.incomplete);
        assert_eq!(repo.graph(&[big], 10).skipped, [big]);
    }

    /// Ein ersetzter Commit (`git replace`) kommt nie mit fremdem Betreff
    /// unter echter Id — auch nicht bei verdrehter Replace-Konfiguration.
    #[test]
    fn a_replaced_commit_never_passes_as_the_original() {
        let fixture = TempRepo::init();
        let base = fixture.commit("base");
        let real = fixture.commit("real subject");
        let tip = fixture.commit("tip");
        fixture.git(&["switch", "-q", "-c", "fake", &base.to_string()]);
        let fake = fixture.commit("FAKE SUBJECT");
        fixture.git(&["switch", "-q", "-"]);
        fixture.git(&["replace", &real.to_string(), &fake.to_string()]);
        for setting in ["true", "false"] {
            fixture.git(&["config", "core.useReplaceRefs", setting]);
            let repo = Repo::open(fixture.path()).unwrap();
            let graph = repo.graph(&[tip], 10);
            assert!(
                graph.commits.iter().all(|c| c.summary != "FAKE SUBJECT"),
                "{setting}: {graph:?}"
            );
            // Der begrenzte Zugriff lässt Ersatzobjekte aus: Der echte
            // Commit steht da, die Historie bleibt ganz — bei beiden
            // Einstellungen (gix 0.85 liest den Schlüssel verkehrt).
            let shown = graph.commits.iter().find(|c| c.id == real);
            assert_eq!(
                shown.map(|c| c.summary.as_str()),
                Some("real subject"),
                "{setting}: {graph:?}"
            );
            assert!(!graph.incomplete, "{setting}");
            assert!(graph.commits.iter().any(|c| c.id == base), "{setting}");
        }
    }

    /// Eine Delta-Kette über der Grenze wird nicht aufgelöst (Arbeit, nicht
    /// nur Speicher) — geprüft an einem echten Pack-Objekt mit einer
    /// Grenze unter seiner Tiefe.
    #[test]
    fn objects_deeper_than_the_delta_limit_are_refused() {
        let fixture = TempRepo::init();
        let body = "line of a long commit message that repeats\n".repeat(200);
        let mut commits = vec![fixture.commit(&format!("c0\n\n{body}"))];
        for n in 1..40 {
            commits.push(fixture.commit(&format!("c{n}\n\n{body}{n}\n")));
        }
        fixture.git(&["repack", "-adfq", "--depth=200", "--window=250"]);
        let repo = Repo::open(fixture.path()).unwrap();
        let reader = repo.bounded_reader().unwrap();
        let (header, depth) = commits
            .iter()
            .filter_map(|c| reader.gix().find_header(c.to_gix()).ok())
            .filter_map(|h| h.num_deltas().map(|d| (h, d)))
            .max_by_key(|(_, d)| *d)
            .expect("repack built deltas");
        assert!(depth > 0);
        let kind = gix::objs::Kind::Commit;
        assert!(admissible(&header, kind, MAX_COMMIT_OBJECT, depth));
        assert!(!admissible(&header, kind, MAX_COMMIT_OBJECT, depth - 1));
        assert!(!admissible(
            &header,
            gix::objs::Kind::Tag,
            MAX_COMMIT_OBJECT,
            depth
        ));
        // Mit der echten Grenze liest der Graph die ganze Kette.
        assert_eq!(repo.graph(&[commits[39]], 100).commits.len(), 40);
    }

    /// Der gemeinsame Deckel für Header-Lookups (lange Delta-Ketten kosten
    /// Arbeit schon im Header): erreicht → gekappt und gesagt, nie hängen.
    #[test]
    fn the_lookup_cap_truncates_instead_of_hanging() {
        let fixture = TempRepo::init();
        let mut last = fixture.commit("c0");
        for n in 1..10 {
            last = fixture.commit(&format!("c{n}"));
        }
        let repo = Repo::open(fixture.path()).unwrap();
        let reader = repo
            .bounded_reader_with(3, std::time::Duration::from_secs(60))
            .unwrap();
        let graph = reader.graph(&[last], 100);
        assert!(graph.commits.len() <= 3, "{graph:?}");
        assert!(graph.truncated && graph.budget_hit);
        let expired = repo
            .bounded_reader_with(1000, std::time::Duration::ZERO)
            .unwrap();
        assert!(expired.graph(&[last], 100).commits.is_empty());
    }

    /// Die topologische Ordnung hält, auch wenn die Eingabe Eltern vor
    /// Kindern bringt (lügende Zeitstempel).
    #[test]
    fn children_come_before_parents_whatever_the_input_order() {
        let id = |n: u8| -> CommitId { format!("{n:040x}").parse().unwrap() };
        let commit = |n: u8, parents: &[u8]| GraphCommit {
            id: id(n),
            parents: parents.iter().map(|p| id(*p)).collect(),
            author: String::new(),
            time: 0,
            summary: String::new(),
        };
        // Raute 4 → (2, 3) → 1, verkehrt herum; dazu ein Elter außerhalb.
        let out = topological(vec![
            commit(1, &[]),
            commit(2, &[1]),
            commit(3, &[1, 9]),
            commit(4, &[2, 3]),
        ]);
        let at = |n: u8| out.iter().position(|c| c.id == id(n)).unwrap();
        assert!(at(4) < at(2) && at(4) < at(3));
        assert!(at(2) < at(1) && at(3) < at(1));
    }
}
