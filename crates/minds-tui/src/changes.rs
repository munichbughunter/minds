//! Der Zustand des Changes-Tabs und seine Navigation — rein, ohne Terminal
//! prüfbar. Gerechnet wird nichts: Diff, Klassen und Herkunft liefert der
//! Reader fertig ([`minds_reader::changes`]).

use minds_git::{CommitId, DiffKind};
use minds_reader::changes::{ChangeSet, FileDiff};
use minds_reader::reconcile::ReconClass;

/// Wo der Fokus im Changes-Tab liegt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Die Dateiliste.
    Files,
    /// Der Diff der gewählten Datei.
    Diff,
}

/// Der Changes-Tab.
#[derive(Debug, Clone)]
pub struct ChangesState {
    /// Die blätterbaren Commits (HEAD zuerst).
    pub commits: Vec<CommitId>,
    /// Der gezeigte Commit, Index in `commits`.
    pub at: usize,
    /// Seine Änderungen — oder warum sie sich nicht lesen ließen.
    pub set: Result<ChangeSet, String>,
    /// Die gewählte Datei.
    pub file: usize,
    /// Wo der Fokus liegt.
    pub focus: Focus,
    /// Die Cursorzeile im Diff (Index in `rows` der Datei).
    pub row: usize,
    /// Split statt Unified.
    pub split: bool,
    /// Die Spalte „Warum diese Zeile?" ist sichtbar.
    pub why: bool,
}

impl ChangesState {
    /// Ein frischer Zustand für `commits[at]` mit geladenem `set`.
    pub fn new(commits: Vec<CommitId>, at: usize, set: Result<ChangeSet, String>) -> Self {
        let mut state = Self {
            commits,
            at,
            set,
            file: 0,
            focus: Focus::Files,
            row: 0,
            split: false,
            why: true,
        };
        state.file = state.first_interesting_file();
        state
    }

    /// Der gezeigte Commit.
    pub fn commit(&self) -> Option<CommitId> {
        self.commits.get(self.at).copied()
    }

    /// Die Dateien, leer ohne lesbaren Commit.
    pub fn files(&self) -> &[FileDiff] {
        self.set.as_ref().map(|s| s.files.as_slice()).unwrap_or(&[])
    }

    /// Die gewählte Datei.
    pub fn current(&self) -> Option<&FileDiff> {
        self.files().get(self.file)
    }

    /// Beim Öffnen: die erste Datei mit unerklärten Zeilen, sonst die erste.
    fn first_interesting_file(&self) -> usize {
        self.files()
            .iter()
            .position(|f| f.unexplained() > 0)
            .unwrap_or(0)
    }

    /// Wählt die Datei `path` (Identität), wenn es sie gibt.
    /// `false`, wenn der Commit sie nicht (mehr) enthält.
    pub fn select_path(&mut self, path: &str) -> bool {
        match self.files().iter().position(|f| f.identity == path) {
            Some(i) => {
                self.file = i;
                self.row = 0;
                true
            }
            None => false,
        }
    }

    /// Im Split eine Zeile weiter oder zurück — paarweise, wie gezeichnet:
    /// Der Cursor steht auf der neuen Seite eines Paars, sonst auf der alten,
    /// damit Rand und „Warum diese Zeile?" dieselbe Zeile meinen.
    pub fn move_pair(&mut self, by: isize) {
        let Some(file) = self.current() else {
            return;
        };
        let pairs = Self::split_pairs(&file.rows);
        if pairs.is_empty() {
            return;
        }
        let at = pairs
            .iter()
            .position(|(l, r)| *l == Some(self.row) || *r == Some(self.row))
            .unwrap_or(0);
        let next = (at as isize + by).clamp(0, pairs.len() as isize - 1) as usize;
        let (left, right) = pairs[next];
        if let Some(row) = right.or(left) {
            self.row = row;
        }
    }

    /// Eine Datei weiter oder zurück.
    pub fn move_file(&mut self, down: bool) {
        let last = self.files().len().saturating_sub(1);
        self.file = if down {
            (self.file + 1).min(last)
        } else {
            self.file.saturating_sub(1)
        };
        self.row = 0;
    }

    fn rows_len(&self) -> usize {
        self.current().map_or(0, |f| f.rows.len())
    }

    /// Eine Zeile (oder eine Seite) weiter oder zurück.
    pub fn move_row(&mut self, by: isize) {
        let last = self.rows_len().saturating_sub(1);
        self.row = (self.row as isize + by).clamp(0, last as isize) as usize;
    }

    /// An den Anfang oder ans Ende des Diffs.
    pub fn row_to(&mut self, end: bool) {
        self.row = if end {
            self.rows_len().saturating_sub(1)
        } else {
            0
        };
    }

    /// Zum nächsten (`forward`) oder vorigen Hunk-Kopf. Bleibt stehen, wenn
    /// es keinen gibt.
    pub fn jump_hunk(&mut self, forward: bool) {
        if let Some(row) = self.find(forward, |r| r.kind == DiffKind::Hunk) {
            self.row = row;
        }
    }

    /// Zur nächsten (`forward`) oder vorigen unerklärten `+`-Zeile — über
    /// Dateigrenzen hinweg, im Kreis. `false`, wenn es keine gibt.
    pub fn jump_unexplained(&mut self, forward: bool) -> bool {
        let unexplained = |r: &minds_reader::changes::DiffRow| {
            r.kind == DiffKind::Added && r.class == Some(ReconClass::Unexplained)
        };
        if let Some(row) = self.find(forward, unexplained) {
            self.row = row;
            return true;
        }
        let count = self.files().len();
        for step in 1..=count {
            let file = if forward {
                (self.file + step) % count
            } else {
                (self.file + count - step % count) % count
            };
            let rows = &self.files()[file].rows;
            let hit = if forward {
                rows.iter().position(unexplained)
            } else {
                rows.iter().rposition(unexplained)
            };
            if let Some(row) = hit {
                self.file = file;
                self.row = row;
                return true;
            }
        }
        false
    }

    /// Die nächste/vorige Zeile in dieser Datei, die `pick` erfüllt — ohne
    /// die Cursorzeile selbst.
    fn find(
        &self,
        forward: bool,
        pick: impl Fn(&minds_reader::changes::DiffRow) -> bool,
    ) -> Option<usize> {
        let rows = &self.current()?.rows;
        if forward {
            rows.iter()
                .enumerate()
                .skip(self.row + 1)
                .find(|(_, r)| pick(r))
                .map(|(i, _)| i)
        } else {
            rows.iter()
                .enumerate()
                .take(self.row)
                .rev()
                .find(|(_, r)| pick(r))
                .map(|(i, _)| i)
        }
    }

    /// Die Paare der Split-Ansicht: je Zeile links (alt) und rechts (neu),
    /// als Indizes in `rows`. Entfernte und hinzugefügte Zeilen eines Blocks
    /// stehen nebeneinander wie bei GitHub; ein Hunk-Kopf belegt beide
    /// Seiten.
    pub fn split_pairs(
        rows: &[minds_reader::changes::DiffRow],
    ) -> Vec<(Option<usize>, Option<usize>)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < rows.len() {
            match rows[i].kind {
                DiffKind::Hunk | DiffKind::Context => {
                    out.push((Some(i), Some(i)));
                    i += 1;
                }
                DiffKind::Removed | DiffKind::Added => {
                    let removed_start = i;
                    while i < rows.len() && rows[i].kind == DiffKind::Removed {
                        i += 1;
                    }
                    let added_start = i;
                    while i < rows.len() && rows[i].kind == DiffKind::Added {
                        i += 1;
                    }
                    let removed: Vec<usize> = (removed_start..added_start).collect();
                    let added: Vec<usize> = (added_start..i).collect();
                    for k in 0..removed.len().max(added.len()) {
                        out.push((removed.get(k).copied(), added.get(k).copied()));
                    }
                }
            }
        }
        out
    }
}
