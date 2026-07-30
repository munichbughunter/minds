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

use minds_core::SessionId;
use minds_git::{BlameProvider, Repo};
use minds_store::ContextStore;

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
        .map_err(|e| ReaderError::io("Ausgabeverzeichnis anlegen", out, e))?;

    let mut links: Vec<FileLink> = Vec::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut skipped = 0usize;
    // Pfad → die Datei-Seite, die ihn zeigt. Die Session-Seiten verlinken damit
    // jede geänderte Datei auf ihre zeilenweise Ansicht.
    let mut file_href: BTreeMap<String, String> = BTreeMap::new();

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

        let href = unique_slug(&path, &mut used);
        write(&out.join(&href), &html::file_page(&view, &index))?;
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
        let href = unique_slug(&format!("session-{}", short_hex(*id)), &mut used);
        write(
            &out.join(&href),
            &html::session_page(*id, session, &diffs, !index.is_observed(*id), &file_href),
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
    std::fs::write(path, contents).map_err(|e| ReaderError::io("Seite schreiben", path, e))
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
}
