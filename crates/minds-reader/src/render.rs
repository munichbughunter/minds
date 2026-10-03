//! Die Seite bauen — der einzige Teil des Readers, der Dateien schreibt.
//!
//! ```text
//!   Index bauen ──► je Datei: Blob + Blame + join ──► HTML schreiben
//! ```
//!
//! Ergebnis ist ein Verzeichnis mit `index.html` und je einer Seite pro Datei,
//! die erfassten Kontext trägt. Nichts darin verweist nach außen, also lässt es
//! sich per `file://` öffnen, hinter jede Firewall stellen und in ein Air-Gap
//! kopieren.
//!
//! # Was gerendert wird — und was nicht
//!
//! Geschrieben wird eine Seite nur für Dateien mit **mindestens einer
//! zugeordneten Zeile**. Eine Datei, an der nie ein erfasster Agent gearbeitet
//! hat, hätte nichts zu zeigen; sie wegzulassen hält die Ausgabe bei dem, was
//! belegt ist.
//!
//! # Kosten, ehrlich benannt
//!
//! Für die Zuordnung wird **jede Datei im Baum von HEAD geblamed** — ein Blame
//! pro Datei. Das ist der korrekte, einfache Weg und für Repositories üblicher
//! Größe schnell genug; auf sehr großen Bäumen ist es der teuerste Teil des
//! Laufs. Ihn zu verengen (nur Dateien, die von getrailerten Commits berührt
//! wurden) braucht eine Diff-Schnittstelle in `minds-git`, die es noch nicht
//! gibt — deshalb steht hier die einfache Variante und keine Heuristik, die
//! stillschweigend etwas ausließe.
//!
//! Dateien, die kein UTF-8 sind (Bilder, Binaries), werden übersprungen; ein
//! Blame, der scheitert, ebenfalls. Beides wird **gezählt** und im Ergebnis
//! ausgewiesen statt verschwiegen.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use minds_core::{ContentHash, SessionId};
use minds_git::{BlameProvider, CommitId, Repo};
use minds_store::ContextStore;

use crate::artifact::{ArtifactState, CommitArtifact};
use crate::error::{ReaderError, Result};
use crate::file::FileView;
use crate::html::{self, FileLink};
use crate::index::Index;

/// Was ein Lauf hervorgebracht hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// Wohin geschrieben wurde.
    pub out: PathBuf,
    /// Wie viele Dateiseiten entstanden sind.
    pub files: usize,
    /// Wie viele Sessions der Index kennt.
    pub sessions: usize,
    /// Dateien, die nicht betrachtet werden konnten (kein UTF-8, Blame
    /// gescheitert).
    pub skipped: usize,
}

/// Baut die statische Seite nach `out`.
pub fn render(repo: &Repo, store: &dyn ContextStore, out: &Path) -> Result<Site> {
    let index = Index::build(repo, store)?;
    let head = repo.head()?.commit().ok_or(ReaderError::UnbornHead)?;

    std::fs::create_dir_all(out)
        .map_err(|e| ReaderError::io("creating output directory", out, e))?;

    let mut links: Vec<FileLink> = Vec::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut skipped = 0usize;
    // Pfad → die Datei-Seite, die ihn zeigt. Die Session-Seiten verlinken damit
    // jede geänderte Datei auf ihre zeilenweise Ansicht.
    let mut file_href: BTreeMap<String, String> = BTreeMap::new();
    // Der Abgleich je Commit (EA-03), einmal gerechnet und von Datei- und
    // Session-Seiten geteilt. Rein lesend; ein Fehler ist ein Zustand der
    // Seite, kein Abbruch des Laufs.
    let spellings = crate::artifact::roots_of(repo);
    let roots: Vec<&Path> = spellings.iter().map(PathBuf::as_path).collect();
    // `Rc`: Datei-Seiten fragen je Blame-Commit — bei breiten Commits
    // vielfach; geteilt wird der Zeiger, nicht die Zeilenklassen.
    let mut artifacts: BTreeMap<CommitId, Rc<CommitArtifact>> = BTreeMap::new();
    let mut artifact_of = |commit: CommitId| -> Rc<CommitArtifact> {
        Rc::clone(
            artifacts
                .entry(commit)
                .or_insert_with(|| Rc::new(index.artifact(repo, &roots, commit))),
        )
    };

    // Trägt kein Commit einen Trailer, kann auch keine Zeile zugeordnet sein —
    // dann ist jeder Blame verschwendet. Das ist der Zustand eines Repos, in
    // dem Minds gerade erst eingerichtet wurde, also der häufigste erste Lauf.
    let candidates = if index.attributed_commits() == 0 {
        Vec::new()
    } else {
        repo.list_blobs_at("HEAD")?
    };

    for path in candidates {
        let Some(bytes) = repo.read_blob_at("HEAD", &path)? else {
            continue;
        };
        let head_hash = ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes());
        // Binärdateien haben keine Zeilen, die man anklicken könnte.
        let Ok(content) = String::from_utf8(bytes) else {
            skipped += 1;
            continue;
        };
        let Ok(blame) = repo.blame().blame_file(head, &path) else {
            skipped += 1;
            continue;
        };

        let view = FileView::join(&path, &content, &blame, &index);
        if !view.is_attributed() {
            continue;
        }

        let unexplained = unexplained_at_head(&view, &head_hash, &mut artifact_of);
        let href = unique_slug(&path, &mut used);
        write(
            &out.join(&href),
            &html::file_page(&view, &index, &unexplained),
        )?;
        file_href.insert(path.clone(), href.clone());
        links.push(FileLink {
            attributed: view.attributed_lines(),
            total: view.lines.len(),
            path,
            href,
        });
    }

    // Je Session eine eigene Seite: Absicht plus alle Änderungen der Commits,
    // die sie tragen — auf- und zuklappbar. Die Übersichts-Karten verlinken
    // hierher.
    let mut session_page: BTreeMap<SessionId, String> = BTreeMap::new();
    for (id, session) in index.sessions() {
        let diffs = diffs_for(repo, &index, *id);
        let recons: Vec<CommitArtifact> = index
            .claimed_commits(*id)
            .into_iter()
            .map(|commit| CommitArtifact::clone(&artifact_of(commit)))
            .collect();
        let href = unique_slug(&format!("session-{}", short_hex(*id)), &mut used);
        write(
            &out.join(&href),
            &html::session_page(
                *id,
                session,
                &diffs,
                !index.is_observed(*id),
                &file_href,
                &recons,
            ),
        )?;
        session_page.insert(*id, href);
    }

    write(
        &out.join("index.html"),
        &html::index_page(&index, &links, &session_page),
    )?;

    Ok(Site {
        out: out.to_path_buf(),
        files: links.len(),
        sessions: index.len(),
        skipped,
    })
}

/// Die Zeilen der HEAD-Fassung, die der Abgleich ihres Blame-Commits als
/// unerklärt führt.
///
/// Zeilennummern des Abgleichs gelten im Stand **des Commits**. Ohne die
/// Ursprungszeile aus dem Blame ist die Abbildung nur dann exakt, wenn die
/// Datei in HEAD byte-gleich mit dem Commit-Stand ist (der Inhalts-Hash
/// entscheidet). Sonst wird nichts markiert — lieber keine Markierung als
/// eine an der falschen Zeile; die Session-Seite zeigt die Markierungen im
/// Diff des Commits immer.
fn unexplained_at_head(
    view: &crate::file::FileView,
    head_hash: &ContentHash,
    artifact_of: &mut impl FnMut(CommitId) -> Rc<CommitArtifact>,
) -> BTreeSet<u32> {
    let commits: BTreeSet<CommitId> = view
        .lines
        .iter()
        .filter(|l| l.is_attributed())
        .filter_map(|l| l.commit)
        .collect();
    let mut marked = BTreeSet::new();
    for commit in commits {
        let artifact = artifact_of(commit);
        let ArtifactState::Assessed(assessed) = &artifact.state else {
            continue;
        };
        let Some(file) = assessed
            .recon
            .files
            .iter()
            .find(|f| f.path == view.path && !f.deleted && f.committed == *head_hash)
        else {
            continue;
        };
        let lines = file.unexplained_set();
        marked.extend(
            view.lines
                .iter()
                .filter(|l| l.commit == Some(commit) && lines.contains(&l.number))
                .map(|l| l.number),
        );
    }
    marked
}

/// Ein Dateiname, der in diesem Lauf noch nicht vergeben ist.
///
/// [`html::slug`] ist nicht injektiv (`a/b` und `a-b` fallen zusammen). Statt
/// eines Hashes, der den Namen unlesbar machte, wird bei Kollision
/// durchnummeriert — deterministisch, weil die Dateien in der sortierten
/// Reihenfolge von `list_blobs_at` verarbeitet werden.
fn unique_slug(path: &str, used: &mut BTreeSet<String>) -> String {
    let base = html::slug(path);
    if used.insert(base.clone()) {
        return base;
    }

    let stem = base.strip_suffix(".html").unwrap_or(&base).to_string();
    for n in 2u32.. {
        let candidate = format!("{stem}-{n}.html");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("u32 reicht für Namenskollisionen")
}

/// Die Diffs aller Commits, die diese Session tragen. Ein Commit, dessen Diff
/// sich nicht ermitteln lässt (etwa weil er nach einem Rebase nicht mehr
/// existiert), wird übersprungen statt den ganzen Lauf zu Fall zu bringen — der
/// Reader ist ein Leser.
fn diffs_for(repo: &Repo, index: &Index, id: SessionId) -> Vec<minds_git::CommitDiff> {
    index
        .commits_of(id)
        .into_iter()
        .filter_map(|commit| repo.diff_commit(commit).ok())
        .collect()
}

/// Die ersten zwölf Hex-Zeichen einer Session-Id — genug, um Dateinamen
/// auseinanderzuhalten, und ohne das `b3-`-Präfix, das jede Id teilt.
fn short_hex(id: SessionId) -> String {
    id.to_string()
        .trim_start_matches("b3-")
        .chars()
        .take(12)
        .collect()
}

fn write(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents).map_err(|e| ReaderError::io("writing page", path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_name_is_used_as_is() {
        let mut used = BTreeSet::new();
        assert_eq!(unique_slug("src/retry.rs", &mut used), "src-retry.rs.html");
    }

    #[test]
    fn a_collision_is_numbered_not_overwritten() {
        // `a/b.rs` und `a-b.rs` ergeben denselben Slug — die zweite Datei darf
        // die erste Seite nicht überschreiben.
        let mut used = BTreeSet::new();
        assert_eq!(unique_slug("a/b.rs", &mut used), "a-b.rs.html");
        assert_eq!(unique_slug("a-b.rs", &mut used), "a-b.rs-2.html");
        assert_eq!(unique_slug("a b.rs", &mut used), "a-b.rs-3.html");
    }

    /// Nur byte-gleiche Fassungen werden markiert — sonst stünde die
    /// Markierung womöglich an der falschen Zeile.
    #[test]
    fn head_lines_are_marked_only_when_head_matches_the_commit() {
        use crate::artifact::Assessed;
        use crate::file::{FileView, Line};
        use crate::reconcile::{FileRecon, LineLevel, LineRecon, ReconClass, Reconciliation};

        let commit: CommitId = "1".repeat(40).parse().unwrap();
        let sid: SessionId = format!("b3-{}", "a".repeat(64)).parse().unwrap();
        let committed = ContentHash::from_bytes([7; 32]);
        let view = FileView {
            path: "a".into(),
            lines: (1..=3)
                .map(|number| Line {
                    number,
                    text: String::new(),
                    commit: Some(commit),
                    sessions: vec![sid],
                })
                .collect(),
        };
        let artifact = CommitArtifact {
            commit,
            subject: None,
            inferred: false,
            claimants: 1,
            state: ArtifactState::Assessed(Assessed {
                recon: Reconciliation {
                    commit,
                    base: None,
                    files: vec![FileRecon {
                        path: "a".into(),
                        class: ReconClass::Unexplained,
                        line_level: LineLevel::Available(vec![
                            LineRecon {
                                line: 1,
                                class: ReconClass::ReportedOnly,
                            },
                            LineRecon {
                                line: 2,
                                class: ReconClass::Unexplained,
                            },
                        ]),
                        committed: committed.clone(),
                        deleted: false,
                        changed_lines: 2,
                        removes: false,
                        last_observed: None,
                    }],
                    explained_lines: 0,
                    total_changed_lines: 2,
                },
                structural: Vec::new(),
            }),
        };
        let artifact = Rc::new(artifact);
        let mut lookup = |_| Rc::clone(&artifact);
        assert_eq!(
            unexplained_at_head(&view, &committed, &mut lookup),
            BTreeSet::from([2])
        );
        let later = ContentHash::from_bytes([8; 32]);
        assert!(unexplained_at_head(&view, &later, &mut lookup).is_empty());
    }
}
