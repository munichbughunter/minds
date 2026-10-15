//! Der Overview-Tab: die Historie als Commit-Graph, an jedem Commit seine
//! Sessions — und oben die Sessions, die noch an keinem Commit hängen. Von
//! einem Commit aus geht es in die Sessions, die ihn tragen.

use std::collections::HashMap;

use minds_core::{ContentHash, SessionId};
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
    /// Der Filter aus `/` (B4) — leer: alles.
    pub filter: String,
    /// `a` (C3.3): nur Commits mit Agent-Session (und die Sessions ohne
    /// Commit). Bleibt über Tab-Wechsel und Neuladen.
    pub agent_only: bool,
    /// Das Urteil von `minds verify` je Commit (C3.4) — nur für den
    /// gewählten, gedrosselt; ein Neuladen beginnt leer (nie ein altes
    /// Urteil übernehmen).
    pub checks: HashMap<CommitId, Result<crate::CommitVerify, String>>,
    /// Wann das Urteil kam (Sekunden seit Epoch).
    pub checked_at: HashMap<CommitId, i64>,
    /// Der Commit, dessen Prüfung beim Worker liegt.
    pub asked: Option<CommitId>,
    /// Die Zeilen je Commit (C3.4): unerklärte und hinzugefügte — aus
    /// derselben Rechnung wie der Changes-Tab — oder warum nicht.
    pub lines: HashMap<CommitId, Result<minds_reader::changes::LineClasses, String>>,
    /// Die gelesenen Anker (C3.4), aus derselben Quelle wie der Intent-Tab.
    pub requirements: HashMap<ContentHash, Result<crate::IntentDetail, String>>,
    /// Ob der Anforderungstext aufgeklappt ist (Space).
    pub requirement_open: bool,
    /// Wie weit das Detail gescrollt ist (aufgeklappt, PgUp/PgDn).
    pub detail_scroll: u16,
    /// Ob die Übersicht schmal gezeichnet wird (unter 100 Spalten): dann
    /// ist das Detail eine eigene Ebene hinter Enter.
    pub narrow: bool,
}

impl OverviewState {
    /// Liest die Übersicht.
    pub fn build(inspection: &Inspection, repo: &Repo) -> Self {
        let mut state = Self {
            data: inspection
                .overview(repo)
                .map_err(|err| minds_reader::sanitize(&err.to_string())),
            cursor: 0,
            focus: OverviewFocus::Graph,
            session: 0,
            stale: false,
            conformance: HashMap::new(),
            filter: String::new(),
            agent_only: false,
            checks: HashMap::new(),
            checked_at: HashMap::new(),
            asked: None,
            lines: HashMap::new(),
            requirements: HashMap::new(),
            requirement_open: false,
            detail_scroll: 0,
            narrow: false,
        };
        // Die Startauswahl ist HEAD (C3) — laufende Sessions stehen darüber.
        if let Some(head) = state.head() {
            state.select_commit(head);
        }
        state
    }

    /// Der Commit, auf dem HEAD steht, falls die Übersicht ihn zeigt.
    pub fn head(&self) -> Option<CommitId> {
        self.data.as_ref().ok()?.rows.iter().find_map(|row| {
            row.refs
                .iter()
                .any(|label| {
                    matches!(
                        label,
                        minds_reader::overview::RefLabel::Head(_)
                            | minds_reader::overview::RefLabel::Detached
                    )
                })
                .then_some(row.id)
        })
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
        next.filter = std::mem::take(&mut self.filter);
        next.agent_only = self.agent_only;
        next.narrow = self.narrow;
        next.cursor = next.nearest_shown(next.cursor);
        *self = next;
    }

    /// Space (C3.4): klappt den Anforderungstext auf oder zu — nur, wenn
    /// es ihn gibt; sonst bleibt PgUp/PgDn bei der Liste.
    pub fn toggle_requirement(&mut self) {
        if !self.has_requirement_text() {
            return;
        }
        self.requirement_open = !self.requirement_open;
        self.detail_scroll = 0;
    }

    /// Ob der gewählte Commit einen belegten Anker mit Text hat.
    pub fn has_requirement_text(&self) -> bool {
        self.agent_commit().is_some_and(|row| {
            row.anchors.iter().any(|a| {
                matches!(
                    self.requirements.get(a),
                    Some(Ok(d)) if d.proof.is_ok() && d.snapshot.is_some()
                )
            })
        })
    }

    /// Der gewählte Commit, wenn eine Agent-Session an ihm hängt — nur für
    /// ihn rechnet das Detail Beleg und Zeilen (C3.4).
    pub fn agent_commit(&self) -> Option<&CommitRow> {
        match self.selected()? {
            Selected::Commit(row) if row.has_agent_session() => Some(row),
            _ => None,
        }
    }

    /// Der gewählte Commit, dessen Urteil noch fehlt und nicht unterwegs ist.
    pub fn check_due(&self) -> Option<CommitId> {
        let row = self.agent_commit()?;
        (!self.checks.contains_key(&row.id) && self.asked != Some(row.id)).then_some(row.id)
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

    /// `a` (C3.3): schaltet den Agent-Filter um; die Auswahl springt auf
    /// den nächsten sichtbaren Eintrag.
    pub fn toggle_agent_only(&mut self) {
        self.agent_only = !self.agent_only;
        let cursor = self.nearest_shown(self.cursor);
        if cursor != self.cursor {
            self.cursor = cursor;
            self.session = 0;
        }
    }

    /// Der gezeigte Eintrag an `cursor` oder der nächste darunter, sonst der
    /// letzte darüber.
    pub fn nearest_shown(&self, cursor: usize) -> usize {
        let shown = self.shown();
        shown
            .iter()
            .find(|&&i| i >= cursor)
            .or_else(|| shown.last())
            .copied()
            .unwrap_or(cursor)
    }

    /// Die Zeilen, die die Filter durchlassen: `a` nur Commits mit
    /// Agent-Session (C3.3), dazu `/` über Betreff, Autor, Agent, Refs — bei
    /// einer WIP-Session Agent und Auftrag.
    pub fn shown(&self) -> Vec<usize> {
        let Ok(overview) = self.data.as_ref() else {
            return Vec::new();
        };
        let mut shown = self.matching(overview);
        if self.agent_only {
            shown.retain(|&i| match i.checked_sub(overview.wip.len()) {
                None => true,
                Some(at) => overview.rows[at].has_agent_session(),
            });
        }
        shown
    }

    fn matching(&self, overview: &Overview) -> Vec<usize> {
        crate::filter::shown_by(self.len(), &self.filter, |i| {
            match i.checked_sub(overview.wip.len()) {
                None => {
                    let wip = &overview.wip[i];
                    format!("{} {}", wip.agent, wip.request)
                }
                Some(at) => {
                    let row = &overview.rows[at];
                    let refs: Vec<String> = row.refs.iter().map(|r| format!("{r:?}")).collect();
                    format!(
                        "{} {} {} {} {}",
                        row.subject,
                        row.author,
                        row.agent.clone().unwrap_or_default(),
                        refs.join(" "),
                        row.id
                    )
                }
            }
        })
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
            // Mit `a` ist ein Commit ohne Agent-Session ausgeblendet — nie
            // gewählt (Leerzustand des Filters).
            Some(at) => overview
                .rows
                .get(at)
                .filter(|row| !self.agent_only || row.has_agent_session())
                .map(Selected::Commit),
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
