//! Der Overview-Tab: die Historie als Commit-Graph, an jedem Commit seine
//! Sessions — und oben die Sessions, die noch an keinem Commit hängen. Von
//! einem Commit aus geht es in die Sessions, die ihn tragen.

use std::collections::HashMap;

use minds_core::SessionId;
use minds_git::{CommitId, Repo};
use minds_reader::Inspection;
use minds_reader::conformance::Outcome;
use minds_reader::overview::{CommitRow, Overview, WipSession};

/// So viele Sessions eines Commits listet das Detail höchstens — weiter
/// wählt auch der Cursor nicht.
pub const MAX_SESSION_LINES: usize = 20;

/// Wo der Fokus liegt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewFocus {
    /// Im Graphen (links).
    Graph,
    /// In den Sessions des gewählten Commits (rechts).
    Sessions,
}

/// Was unter dem Cursor steht.
#[derive(Debug, Clone, Copy)]
pub enum Selected<'a> {
    /// Eine Session ohne Commit.
    Wip(&'a WipSession),
    /// Ein Commit.
    Commit(&'a CommitRow),
}

/// Die Conformance eines Commits als Pille: das schlechteste Ergebnis und
/// wie viele Regeln abweichen bzw. offen sind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConformanceMark {
    /// DRIFT vor UNKNOWN vor PASS.
    pub outcome: Outcome,
    /// Regeln mit DRIFT.
    pub drift: usize,
    /// Regeln mit UNKNOWN.
    pub unknown: usize,
}

impl ConformanceMark {
    /// Aus einem Bericht.
    pub fn of(report: &minds_reader::conformance::Report) -> Self {
        let (_, drift, unknown) = report.counts();
        Self {
            outcome: report.outcome(),
            drift,
            unknown,
        }
    }
}

/// Der Zustand des Tabs.
#[derive(Debug, Clone)]
pub struct OverviewState {
    /// Die Übersicht — `Err` mit Grund, wenn die Historie nicht lesbar war.
    pub data: Result<Overview, String>,
    /// Die Cursorzeile über WIP-Sessions und Commits.
    pub cursor: usize,
    /// Wo der Fokus liegt.
    pub focus: OverviewFocus,
    /// Die gewählte Session des Commits (Fokus rechts).
    pub session: usize,
    /// Neu geladen, während der Tab verdeckt war — beim Zeigen neu lesen.
    pub stale: bool,
    /// Die schon gerechnete Conformance je Commit (`None`: kein Soll).
    /// Gerechnet wird im Leerlauf, für den gewählten Commit und HEAD; ein
    /// Neuladen beginnt leer (eine neue Signatur kann alles ändern).
    pub conformance: HashMap<CommitId, Option<ConformanceMark>>,
}

impl OverviewState {
    /// Liest die Übersicht.
    pub fn build(inspection: &Inspection, repo: &Repo) -> Self {
        Self {
            data: inspection
                .overview(repo)
                .map_err(|err| minds_reader::sanitize(&err.to_string())),
            cursor: 0,
            focus: OverviewFocus::Graph,
            session: 0,
            stale: false,
            conformance: HashMap::new(),
        }
    }

    /// Der nächste Commit, dessen Conformance noch fehlt: der gewählte,
    /// dann HEAD.
    pub fn conformance_due(&self) -> Option<CommitId> {
        let overview = self.data.as_ref().ok()?;
        let selected = match self.selected() {
            Some(Selected::Commit(row)) => Some(row.id),
            _ => None,
        };
        let head = overview
            .rows
            .iter()
            .find(|row| {
                row.refs.iter().any(|label| {
                    matches!(
                        label,
                        minds_reader::overview::RefLabel::Head(_)
                            | minds_reader::overview::RefLabel::Detached
                    )
                })
            })
            .map(|row| row.id);
        [selected, head]
            .into_iter()
            .flatten()
            .find(|commit| !self.conformance.contains_key(commit))
    }

    /// Neu gelesen — dieselbe Zeile (Commit oder Session), wenn es sie noch
    /// gibt.
    pub fn rebuild(&mut self, inspection: &Inspection, repo: &Repo) {
        let keep = self.key();
        let mut next = Self::build(inspection, repo);
        next.cursor = match keep {
            Some(key) => next
                .position(&key)
                .unwrap_or_else(|| self.cursor.min(next.len().saturating_sub(1))),
            None => 0,
        };
        if next.key() == keep {
            next.focus = self.focus;
            next.session = self.session;
        }
        next.session = next.session.min(next.sessions().len().saturating_sub(1));
        *self = next;
    }

    /// Wählt die Zeile des Commits, wenn es sie gibt (B3: der aktive Commit).
    pub fn select_commit(&mut self, commit: CommitId) -> bool {
        match self.position(&Key::Commit(commit)) {
            Some(at) => {
                if at != self.cursor {
                    self.cursor = at;
                    self.session = 0;
                }
                true
            }
            None => false,
        }
    }

    /// Wie viele Zeilen der Graph hat (WIP-Sessions und Commits).
    pub fn len(&self) -> usize {
        self.data
            .as_ref()
            .map(|o| o.wip.len() + o.rows.len())
            .unwrap_or(0)
    }

    /// Was unter dem Cursor steht.
    pub fn selected(&self) -> Option<Selected<'_>> {
        let overview = self.data.as_ref().ok()?;
        match self.cursor.checked_sub(overview.wip.len()) {
            None => overview.wip.get(self.cursor).map(Selected::Wip),
            Some(at) => overview.rows.get(at).map(Selected::Commit),
        }
    }

    /// Die Sessions der gewählten Zeile — beim Commit seine, bei einer
    /// WIP-Session sie selbst.
    pub fn sessions(&self) -> Vec<SessionId> {
        match self.selected() {
            Some(Selected::Commit(row)) => row.sessions.clone(),
            Some(Selected::Wip(wip)) => vec![wip.id],
            None => Vec::new(),
        }
    }

    /// Die Session unter dem Cursor rechts.
    pub fn chosen_session(&self) -> Option<SessionId> {
        self.sessions().get(self.session).copied()
    }

    fn key(&self) -> Option<Key> {
        match self.selected()? {
            Selected::Wip(wip) => Some(Key::Wip(wip.id)),
            Selected::Commit(row) => Some(Key::Commit(row.id)),
        }
    }

    fn position(&self, key: &Key) -> Option<usize> {
        let overview = self.data.as_ref().ok()?;
        match key {
            Key::Wip(id) => overview.wip.iter().position(|w| w.id == *id),
            Key::Commit(id) => overview
                .rows
                .iter()
                .position(|r| r.id == *id)
                .map(|at| at + overview.wip.len()),
        }
    }
}

/// Woran eine Zeile über ein Neuladen erkannt wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Wip(SessionId),
    Commit(CommitId),
}

/// Die Initialen eines Namens für das Badge: zwei Buchstaben, groß —
/// `claude-code` → `CC`, `Patrick Döring` → `PD`, `codex` → `CO`.
pub fn initials(name: &str) -> String {
    let parts: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .collect();
    let letters: String = match parts.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect(),
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
    };
    letters.to_uppercase()
}
