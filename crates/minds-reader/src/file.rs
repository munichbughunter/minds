//! Eine Datei, Zeile für Zeile mit ihrer Session verbunden — der Kern des
//! Magic Moments.
//!
//! ```text
//!   Zeile  ──blame──►  Commit  ──Trailer──►  Session
//! ```
//!
//! [`FileView::join`] ist eine **reine Funktion**: Dateiinhalt rein, Blame rein,
//! [`Index`] rein, fertige Ansicht raus. Kein Git, kein Dateisystem. Das ist
//! Absicht — die interessante Logik (welche Zeile gehört zu welcher Session)
//! lässt sich damit ohne Repository prüfen, und der Aufrufer entscheidet, woher
//! Inhalt und Blame kommen.
//!
//! # Nicht jede Zeile hat eine Session
//!
//! Zeilen aus Commits ohne Trailer — alles, was vor Minds entstand oder von Hand
//! geschrieben wurde — tragen schlicht keine. Das ist der Normalfall und wird
//! nicht als Lücke dargestellt: Der Reader zeigt, was belegt ist, und behauptet
//! nichts über den Rest.

use std::collections::BTreeMap;

use minds_core::SessionId;
use minds_git::{BlameLine, CommitId};

use crate::index::Index;

/// Eine Datei mit ihren Zeilen und deren Zuordnung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileView {
    /// Repo-relativer Pfad.
    pub path: String,
    /// Die Zeilen, 1-basiert nummeriert und in Dateireihenfolge.
    pub lines: Vec<Line>,
}

/// Eine einzelne Zeile samt Herkunft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Zeilennummer, **1-basiert** — wie `git blame` und wie ein Editor zählt.
    pub number: u32,
    /// Der Text der Zeile, ohne Zeilenumbruch.
    pub text: String,
    /// Der Commit, der sie zuletzt geändert hat; `None`, wenn der Blame sie
    /// nicht kennt.
    pub commit: Option<CommitId>,
    /// Die Sessions hinter diesem Commit. Leer heißt „kein erfasster Kontext",
    /// nicht „unbekannt".
    pub sessions: Vec<SessionId>,
}

impl Line {
    /// `true`, wenn hinter dieser Zeile mindestens eine Session steht.
    pub fn is_attributed(&self) -> bool {
        !self.sessions.is_empty()
    }
}

impl FileView {
    /// Verbindet Dateiinhalt, Blame und Index zu einer Ansicht.
    ///
    /// `content` wird an Zeilenumbrüchen zerlegt; `blame` darf lückenhaft oder
    /// unsortiert sein — gesucht wird über die Zeilennummer, nicht über die
    /// Position.
    pub fn join(path: &str, content: &str, blame: &[BlameLine], index: &Index) -> Self {
        let by_line: BTreeMap<u32, CommitId> = blame
            .iter()
            .map(|entry| (entry.line, entry.commit))
            .collect();

        let lines = content
            .lines()
            .enumerate()
            .map(|(offset, text)| {
                // `enumerate` zählt ab 0, Blame und Editor ab 1.
                let number = offset as u32 + 1;
                let commit = by_line.get(&number).copied();
                let sessions = commit
                    .map(|commit| index.sessions_of(commit).to_vec())
                    .unwrap_or_default();
                Line {
                    number,
                    text: text.to_string(),
                    commit,
                    sessions,
                }
            })
            .collect();

        Self {
            path: path.to_string(),
            lines,
        }
    }

    /// Wie viele Zeilen mindestens eine Session tragen.
    pub fn attributed_lines(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.is_attributed())
            .count()
    }

    /// `true`, wenn irgendeine Zeile eine Session trägt — nur solche Dateien
    /// sind für den Reader interessant.
    pub fn is_attributed(&self) -> bool {
        self.lines.iter().any(Line::is_attributed)
    }

    /// Die Sessions, die diese Datei berühren, in der Reihenfolge ihres ersten
    /// Auftretens und ohne Wiederholung.
    pub fn sessions(&self) -> Vec<SessionId> {
        let mut seen = Vec::new();
        for id in self.lines.iter().flat_map(|line| &line.sessions) {
            if !seen.contains(id) {
                seen.push(*id);
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{Agent, Intent, Model, Session};

    fn sid(hex: char) -> SessionId {
        format!("b3-{}", hex.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    fn cid(hex: char) -> CommitId {
        hex.to_string().repeat(40).parse().unwrap()
    }

    fn session() -> Session {
        Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            // Nicht-leere Absicht: der Reader-Index siebt absichtslose Sessions
            // aus, ein Testfixture soll nicht daran hängenbleiben.
            Intent {
                request: "eine Absicht".into(),
                ..Intent::default()
            },
        )
    }

    /// Commit '1' trägt Session 'a', Commit '2' trägt 'a' und 'b'.
    fn index() -> Index {
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('a'), session());
        sessions.insert(sid('b'), session());
        let mut commits = BTreeMap::new();
        commits.insert(cid('1'), vec![sid('a')]);
        commits.insert(cid('2'), vec![sid('a'), sid('b')]);
        Index::from_parts(sessions, commits)
    }

    fn blame(pairs: &[(u32, char)]) -> Vec<BlameLine> {
        pairs
            .iter()
            .map(|(line, hex)| BlameLine {
                line: *line,
                commit: cid(*hex),
            })
            .collect()
    }

    #[test]
    fn every_line_gets_its_number_and_text() {
        let view = FileView::join("a.rs", "eins\nzwei\ndrei\n", &[], &index());
        assert_eq!(view.lines.len(), 3);
        assert_eq!(view.lines[0].number, 1);
        assert_eq!(view.lines[0].text, "eins");
        assert_eq!(view.lines[2].number, 3);
        assert_eq!(view.lines[2].text, "drei");
    }

    #[test]
    fn a_line_carries_the_sessions_of_its_commit() {
        let view = FileView::join(
            "a.rs",
            "eins\nzwei\n",
            &blame(&[(1, '1'), (2, '2')]),
            &index(),
        );
        assert_eq!(view.lines[0].sessions, vec![sid('a')]);
        assert_eq!(view.lines[1].sessions, vec![sid('a'), sid('b')]);
        assert!(view.lines[0].is_attributed());
    }

    #[test]
    fn a_line_from_a_commit_without_a_trailer_has_no_session() {
        // Der Normalfall fuer alles, was vor Minds entstand.
        let view = FileView::join("a.rs", "alt\n", &blame(&[(1, '9')]), &index());
        assert_eq!(view.lines[0].commit, Some(cid('9')));
        assert!(view.lines[0].sessions.is_empty());
        assert!(!view.lines[0].is_attributed());
        assert!(!view.is_attributed());
    }

    #[test]
    fn a_gap_in_the_blame_is_not_a_panic() {
        // Blame kennt nur Zeile 2 — die anderen bleiben ohne Commit.
        let view = FileView::join("a.rs", "eins\nzwei\ndrei\n", &blame(&[(2, '1')]), &index());
        assert!(view.lines[0].commit.is_none());
        assert_eq!(view.lines[1].commit, Some(cid('1')));
        assert!(view.lines[2].commit.is_none());
        assert_eq!(view.attributed_lines(), 1);
    }

    #[test]
    fn unsorted_blame_is_matched_by_number_not_position() {
        let view = FileView::join(
            "a.rs",
            "eins\nzwei\n",
            &blame(&[(2, '2'), (1, '1')]),
            &index(),
        );
        assert_eq!(view.lines[0].commit, Some(cid('1')));
        assert_eq!(view.lines[1].commit, Some(cid('2')));
    }

    #[test]
    fn sessions_of_a_file_are_distinct_and_in_first_appearance_order() {
        let view = FileView::join(
            "a.rs",
            "eins\nzwei\ndrei\n",
            &blame(&[(1, '1'), (2, '2'), (3, '1')]),
            &index(),
        );
        // '1' bringt a; '2' bringt a (schon da) und b.
        assert_eq!(view.sessions(), vec![sid('a'), sid('b')]);
    }

    #[test]
    fn an_empty_file_yields_no_lines() {
        let view = FileView::join("leer.rs", "", &[], &index());
        assert!(view.lines.is_empty());
        assert!(!view.is_attributed());
        assert_eq!(view.attributed_lines(), 0);
    }
}
