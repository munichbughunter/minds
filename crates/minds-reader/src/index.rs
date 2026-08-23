//! Der Index: alles, was die Seite braucht, einmal aus Git und Store gezogen.
//!
//! Der Reader ist **zustandslos** (Architektur-Prinzip 6): Er hält keine
//! Datenbank, sondern baut bei jedem Lauf ein Bild aus zwei Quellen — den
//! Sessions im Store und den Trailern in der Historie. Dieses Modul ist dieses
//! Bild.
//!
//! ```text
//!   Store  ──list/get──►  SessionId → Session
//!   Repo   ──revwalk───►  Commit    → [SessionId], Change-Id, Betreff
//! ```
//!
//! # Zwei Richtungen, eine Wahrheit
//!
//! Die verbindliche Verknüpfung ist der **Trailer** (Commit → Session); genau
//! die wird hier gesammelt. Der Store liefert dazu die Nutzlast. Was der Index
//! *nicht* tut, ist raten: Eine Session ohne Trailer taucht in `sessions` auf,
//! aber unter keinem Commit — sie ist erfasst, aber (noch) nicht mit Code
//! verbunden. Das ist ein legitimer Zustand und kein Fehler.
//!
//! Der Store-Index steuert die **vermuteten** Kanten bei (importierte
//! Sessions, Datei-Schnittmenge plus Zeitfenster). Welche Quelle eine Kante
//! belegt, bleibt je Kante erhalten ([`Index::evidence_of`]) — der Reader
//! darf eine Vermutung nie wie einen Beleg zeigen.
//!
//! # Eine kaputte Session bringt nicht die Seite zu Fall
//!
//! Lässt sich eine Session nicht auflösen (Inhalt passt nicht zum Hash, kaputtes
//! JSON, Tombstone nach `minds forget`), wird sie übersprungen und **mit
//! Ursache vermerkt** statt verschwiegen — siehe [`Index::degraded`]. Der
//! Reader ist ein Leser; er darf an einem faulen Eintrag nicht sterben, aber er
//! darf ihn auch nicht unterschlagen. `minds fsck` ist das Werkzeug, um dem
//! nachzugehen; ein Tombstone dagegen ist gewollt und kein Defekt.

use std::collections::{BTreeMap, BTreeSet};

use minds_core::{ChangeId, Evidence, Session, SessionId, Trailer};
use minds_git::{CommitId, Repo};
use minds_metrics::Coverage;
use minds_store::{ContextStore, StoreError};

use crate::error::Result;
use crate::text::sanitize;

/// Warum eine im Store gelistete Session nicht gezeigt werden kann.
///
/// **Vergessen** ist ein gewollter Zustand (DSGVO, `minds forget`), alles
/// andere ein Defekt, dem `minds fsck` nachgeht — dieselbe Trennung wie in
/// [`StoreError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Degradation {
    /// Getilgt per `minds forget`; `reason` ist der hinterlegte Grund.
    Forgotten {
        /// Der beim Vergessen hinterlegte Grund, entschärft.
        reason: String,
    },
    /// Der Inhalt hasht nicht auf die Id, unter der er liegt.
    Corrupt,
    /// Der Inhalt ist kein gültiges Session-JSON.
    Malformed,
    /// Der Inhalt ist nicht als redigiert markiert.
    Unredacted,
    /// Gelistet, aber nicht auflösbar.
    Missing,
    /// Ein anderer Fehler beim Holen dieser einen Session, entschärft.
    Failed {
        /// Die Fehlermeldung, entschärft.
        message: String,
    },
}

impl Degradation {
    /// Ein kurzes Wort für die Anzeige: `vergessen` oder `unlesbar`.
    pub fn word(&self) -> &'static str {
        match self {
            Degradation::Forgotten { .. } => "vergessen",
            _ => "unlesbar",
        }
    }

    /// `true` für den gewollten Zustand — den Tombstone.
    pub fn is_forgotten(&self) -> bool {
        matches!(self, Degradation::Forgotten { .. })
    }

    fn of(err: &StoreError) -> Self {
        match err {
            StoreError::Forgotten { reason, .. } => Degradation::Forgotten {
                reason: sanitize(reason),
            },
            StoreError::Corrupt { .. } => Degradation::Corrupt,
            StoreError::Malformed { .. } => Degradation::Malformed,
            StoreError::Unredacted { .. } => Degradation::Unredacted,
            other => Degradation::Failed {
                message: sanitize(&other.to_string()),
            },
        }
    }
}

/// Eine Session, die im Store gelistet ist, aber nicht gezeigt werden kann.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Degraded {
    /// Die betroffene Session.
    pub id: SessionId,
    /// Warum.
    pub cause: Degradation,
}

/// Das gesammelte Bild aus Store und Historie.
///
/// Zwei Quellen verknüpfen Commits mit Sessions: die **Trailer** (verbindlich)
/// und der **Store-Index** (heuristisch, für importierte Sessions). Der Index
/// führt beide zusammen und merkt sich in [`Index::observed`], welche Sessions
/// über mindestens einen Trailer belegt sind — der Rest ist „vermutet" und wird
/// im Reader als solcher gezeigt.
#[derive(Debug, Default, Clone)]
pub struct Index {
    sessions: BTreeMap<SessionId, Session>,
    commits: BTreeMap<CommitId, Vec<SessionId>>,
    /// Die Gegenrichtung zu `commits`, damit `commits_of` nicht die ganze
    /// Historie durchsucht.
    by_session: BTreeMap<SessionId, Vec<CommitId>>,
    /// Woher jede Kante bekannt ist; bei mehreren Belegen gewinnt der beste.
    evidence: BTreeMap<(CommitId, SessionId), Evidence>,
    changes: BTreeMap<CommitId, ChangeId>,
    subjects: BTreeMap<CommitId, String>,
    /// `(agent, lineage.local_id)` → Id — für die symbolischen Endpunkte der
    /// Kanten (`Endpoint::Session`).
    locals: BTreeMap<(String, String), SessionId>,
    observed: BTreeSet<SessionId>,
    degraded: Vec<Degraded>,
    commits_total: u64,
    covered: u64,
}

impl Index {
    /// Zieht Sessions, Trailer und den Store-Index aus Store und Repository.
    pub fn build(repo: &Repo, store: &dyn ContextStore) -> Result<Self> {
        let mut sessions = BTreeMap::new();
        let mut degraded = Vec::new();

        for id in store.list()? {
            match store.get(id) {
                // Sessions ohne erfasste Absicht tragen nichts bei und werden
                // gar nicht erst aufgenommen — dann verschwinden sie überall:
                // Übersicht, Datei-Panels und die Zeilen-Zuordnung. Das ist der
                // frühere „(kein Prompt erfasst)"-Ballast, an einer Stelle
                // ausgesiebt.
                Ok(Some(session)) if session.intent.request.trim().is_empty() => {}
                Ok(Some(session)) => {
                    sessions.insert(id, session);
                }
                // Im Store gelistet, aber nicht auflösbar — vermerken, nicht
                // sterben.
                Ok(None) => degraded.push(Degraded {
                    id,
                    cause: Degradation::Missing,
                }),
                Err(err) => degraded.push(Degraded {
                    id,
                    cause: Degradation::of(&err),
                }),
            }
        }
        let known: BTreeSet<SessionId> = sessions
            .keys()
            .copied()
            .chain(degraded.iter().map(|d| d.id))
            .collect();

        let mut index = Self {
            sessions,
            degraded,
            ..Self::default()
        };

        // 1. Die Trailer — die verbindliche Richtung. Nur Kanten zu Sessions,
        //    die wir behalten haben (keine ausgesiebten). Die Message wird je
        //    Commit einmal gelesen; Session-Ids, Change-Id und Betreff kommen
        //    aus demselben Text.
        if let Some(head) = repo.head()?.commit() {
            for commit in repo.revwalk(head)? {
                let commit = commit?;
                index.commits_total += 1;
                let message = repo.message_of(commit)?;
                let mut covered = false;
                for id in Trailer::session_ids(&message) {
                    covered |= known.contains(&id);
                    if index.sessions.contains_key(&id) {
                        index.link(commit, id, Evidence::Observed);
                    }
                }
                if let Some(change) = Trailer::change_id(&message) {
                    index.changes.insert(commit, change);
                }
                if let Some(subject) = message.lines().map(str::trim).find(|l| !l.is_empty()) {
                    index.subjects.insert(commit, sanitize(subject));
                }
                // Für die Abdeckung zählt die Verknüpfung, nicht die
                // Lesbarkeit: Ein Tombstone ist eine erfasste, bewusst getilgte
                // Session — derselbe Vertrag wie bei `minds metrics`.
                index.covered += u64::from(covered);
            }
        }

        // 2. Der Store-Index — die vermuteten Kanten (z. B. importiert). Was
        //    schon über einen Trailer da ist, behält seinen besseren Beleg.
        for (hex, links) in store.index()?.iter() {
            let Ok(commit) = hex.parse::<CommitId>() else {
                continue;
            };
            for link in links {
                if index.sessions.contains_key(&link.session) {
                    index.link(commit, link.session, link.evidence);
                }
            }
        }

        index.finish();
        Ok(index)
    }

    /// Baut einen Index aus fertigen Teilen — für Tests und für Aufrufer, die
    /// ihre Daten schon haben. Alle Verknüpfungen gelten dabei als beobachtet.
    ///
    /// Auch hier fliegen absichtslose Sessions und Kanten auf sie raus, damit
    /// der Test denselben Vertrag prüft wie [`Index::build`].
    pub fn from_parts(
        sessions: BTreeMap<SessionId, Session>,
        commits: BTreeMap<CommitId, Vec<SessionId>>,
    ) -> Self {
        let sessions: BTreeMap<SessionId, Session> = sessions
            .into_iter()
            .filter(|(_, s)| !s.intent.request.trim().is_empty())
            .collect();
        let mut index = Self {
            sessions,
            ..Self::default()
        };
        for (commit, ids) in commits {
            let mut any = false;
            for id in ids {
                if index.sessions.contains_key(&id) {
                    index.link(commit, id, Evidence::Observed);
                    any = true;
                }
            }
            if any {
                index.commits_total += 1;
                index.covered += 1;
            }
        }
        index.finish();
        index
    }

    /// Ergänzt Change-Ids je Commit — für Tests, die den Strang
    /// Session → Commit → Change prüfen.
    pub fn with_changes(mut self, changes: BTreeMap<CommitId, ChangeId>) -> Self {
        self.changes.extend(changes);
        self
    }

    /// Ergänzt degradierte Einträge — für Tests, die den Leerlauf einer
    /// kaputten oder vergessenen Session prüfen.
    pub fn with_degraded(mut self, degraded: Vec<Degraded>) -> Self {
        self.degraded.extend(degraded);
        self
    }

    /// Trägt eine Kante ein; ein besserer Beleg ersetzt einen schwächeren,
    /// ein schwächerer ändert nichts.
    fn link(&mut self, commit: CommitId, id: SessionId, evidence: Evidence) {
        let slot = self.evidence.entry((commit, id)).or_insert(evidence);
        if evidence > *slot {
            *slot = evidence;
        }
        if evidence == Evidence::Observed {
            self.observed.insert(id);
        }
        push_unique(self.commits.entry(commit).or_default(), id);
        push_unique(self.by_session.entry(id).or_default(), commit);
    }

    /// Leitet die abgeleiteten Tabellen ab, sobald alle Kanten stehen.
    fn finish(&mut self) {
        self.locals = self
            .sessions
            .iter()
            .filter_map(|(id, session)| {
                let lineage = session.lineage.as_ref()?;
                Some(((session.agent.name.clone(), lineage.local_id.clone()), *id))
            })
            .collect();
    }

    /// `true`, wenn diese Session über mindestens einen Trailer belegt ist (im
    /// Gegensatz zu nur heuristisch über den Store-Index verknüpft).
    pub fn is_observed(&self, id: SessionId) -> bool {
        self.observed.contains(&id)
    }

    /// Woher die Kante Commit → Session bekannt ist; `None`, wenn es sie
    /// nicht gibt.
    pub fn evidence_of(&self, commit: CommitId, id: SessionId) -> Option<Evidence> {
        self.evidence.get(&(commit, id)).copied()
    }

    /// Der beste Beleg, mit dem diese Session an irgendeinem Commit hängt —
    /// `None`, wenn sie mit keinem Code verbunden ist.
    pub fn evidence_for_session(&self, id: SessionId) -> Option<Evidence> {
        self.by_session
            .get(&id)?
            .iter()
            .filter_map(|commit| self.evidence_of(*commit, id))
            .max()
    }

    /// Die Change-Id aus dem Trailer dieses Commits.
    pub fn change_of(&self, commit: CommitId) -> Option<&ChangeId> {
        self.changes.get(&commit)
    }

    /// Die Change-Ids aller Commits, die diese Session tragen — dedupliziert,
    /// in Commit-Reihenfolge.
    pub fn changes_of(&self, id: SessionId) -> Vec<ChangeId> {
        let mut out: Vec<ChangeId> = Vec::new();
        for commit in self.commits_of(id) {
            if let Some(change) = self.changes.get(&commit)
                && !out.contains(change)
            {
                out.push(change.clone());
            }
        }
        out
    }

    /// Der Betreff (erste Zeile der Message) eines Commits, entschärft.
    pub fn subject_of(&self, commit: CommitId) -> Option<&str> {
        self.subjects
            .get(&commit)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    /// Löst den symbolischen Endpunkt einer Kante (`agent` + `local_id`) zu
    /// einer Session-Id auf, sofern diese Session im Index ist.
    pub fn resolve_endpoint(&self, agent: &str, local_id: &str) -> Option<SessionId> {
        self.locals
            .get(&(agent.to_string(), local_id.to_string()))
            .copied()
    }

    /// Die Session zu einer Id.
    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    /// Alle Sessions, nach Id sortiert (die Ordnung von [`SessionId`]).
    pub fn sessions(&self) -> impl Iterator<Item = (&SessionId, &Session)> {
        self.sessions.iter()
    }

    /// Die Sessions, deren Trailer an diesem Commit stehen.
    pub fn sessions_of(&self, commit: CommitId) -> &[SessionId] {
        self.commits.get(&commit).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Die Commits, die diese Session tragen — die Gegenrichtung zu
    /// [`Index::sessions_of`]. Reihenfolge ist die von [`CommitId`] (Hash),
    /// also deterministisch, aber nicht chronologisch.
    pub fn commits_of(&self, id: SessionId) -> Vec<CommitId> {
        let mut commits = self.by_session.get(&id).cloned().unwrap_or_default();
        commits.sort();
        commits
    }

    /// Wie viele Sessions insgesamt bekannt sind.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// `true`, wenn keine Session bekannt ist — der Empty-State der Seite.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Wie viele im Store gelistete Sessions sich nicht zeigen ließen —
    /// Tombstones eingeschlossen. Die Ursachen stehen in [`Index::degraded`].
    pub fn unreadable(&self) -> usize {
        self.degraded.len()
    }

    /// Die gelisteten, aber nicht zeigbaren Sessions, je mit Ursache.
    pub fn degraded(&self) -> &[Degraded] {
        &self.degraded
    }

    /// Wie viele Commits mindestens eine Session tragen.
    pub fn attributed_commits(&self) -> usize {
        self.commits.len()
    }

    /// Die Abdeckung der Historie: Wie viele Commits ab HEAD eine über
    /// Trailer verknüpfte, im Store bekannte Session tragen.
    pub fn coverage(&self) -> Coverage {
        Coverage {
            commits_total: self.commits_total,
            commits_with_context: self.covered,
        }
    }
}

/// Fügt `id` an, wenn sie nicht schon in `ids` steht.
fn push_unique<T: PartialEq>(ids: &mut Vec<T>, id: T) {
    if !ids.contains(&id) {
        ids.push(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{Agent, Intent, Model};

    fn id(hex: char) -> SessionId {
        format!("b3-{}", hex.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    fn commit(hex: char) -> CommitId {
        hex.to_string().repeat(40).parse().unwrap()
    }

    fn session(request: &str) -> Session {
        Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.0.0".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent {
                request: request.into(),
                ..Intent::default()
            },
        )
    }

    fn sample() -> Index {
        let mut sessions = BTreeMap::new();
        sessions.insert(id('a'), session("erste Absicht"));
        sessions.insert(id('b'), session("zweite Absicht"));

        let mut commits = BTreeMap::new();
        commits.insert(commit('1'), vec![id('a')]);
        commits.insert(commit('2'), vec![id('a'), id('b')]);

        Index::from_parts(sessions, commits)
    }

    #[test]
    fn resolves_a_session_by_id() {
        let index = sample();
        assert_eq!(
            index.session(id('a')).unwrap().intent.request,
            "erste Absicht"
        );
        assert!(index.session(id('f')).is_none());
    }

    #[test]
    fn a_commit_can_carry_several_sessions() {
        let index = sample();
        assert_eq!(index.sessions_of(commit('2')), &[id('a'), id('b')]);
        assert_eq!(index.sessions_of(commit('1')), &[id('a')]);
    }

    #[test]
    fn commits_of_is_the_reverse_direction() {
        let index = sample();
        // 'a' steckt in Commit '1' und '2', 'b' nur in '2'.
        assert_eq!(index.commits_of(id('a')), vec![commit('1'), commit('2')]);
        assert_eq!(index.commits_of(id('b')), vec![commit('2')]);
        assert!(index.commits_of(id('f')).is_empty());
    }

    #[test]
    fn a_commit_without_a_trailer_yields_nothing_not_a_panic() {
        assert!(sample().sessions_of(commit('9')).is_empty());
    }

    #[test]
    fn an_empty_index_is_the_empty_state() {
        let empty = Index::default();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.attributed_commits(), 0);
        assert_eq!(empty.unreadable(), 0);
    }

    #[test]
    fn sessions_come_out_in_id_order() {
        let index = sample();
        let ids: Vec<_> = index.sessions().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![id('a'), id('b')]);
    }

    #[test]
    fn every_edge_from_parts_is_observed_with_its_evidence() {
        let index = sample();
        assert_eq!(
            index.evidence_of(commit('1'), id('a')),
            Some(Evidence::Observed)
        );
        assert_eq!(index.evidence_of(commit('1'), id('b')), None);
        assert_eq!(
            index.evidence_for_session(id('a')),
            Some(Evidence::Observed)
        );
        // Ohne Kante: mit keinem Code verbunden, nicht „vermutet".
        let mut lonely = BTreeMap::new();
        lonely.insert(id('c'), session("dritte Absicht"));
        let index = Index::from_parts(lonely, BTreeMap::new());
        assert_eq!(index.evidence_for_session(id('c')), None);
    }

    #[test]
    fn a_better_proof_replaces_a_weaker_one_never_the_reverse() {
        let mut index = Index::default();
        index.sessions.insert(id('a'), session("x"));
        index.link(commit('1'), id('a'), Evidence::Inferred);
        assert_eq!(
            index.evidence_of(commit('1'), id('a')),
            Some(Evidence::Inferred)
        );
        assert!(!index.is_observed(id('a')));
        index.link(commit('1'), id('a'), Evidence::Observed);
        assert_eq!(
            index.evidence_of(commit('1'), id('a')),
            Some(Evidence::Observed)
        );
        assert!(index.is_observed(id('a')));
        index.link(commit('1'), id('a'), Evidence::Declared);
        assert_eq!(
            index.evidence_of(commit('1'), id('a')),
            Some(Evidence::Observed)
        );
        // Die Kante steht nur einmal in beiden Richtungen.
        assert_eq!(index.sessions_of(commit('1')).len(), 1);
        assert_eq!(index.commits_of(id('a')).len(), 1);
    }

    #[test]
    fn changes_follow_the_commits_and_are_deduplicated() {
        let change: ChangeId = format!("I{}", "c".repeat(40)).parse().unwrap();
        let mut changes = BTreeMap::new();
        changes.insert(commit('1'), change.clone());
        changes.insert(commit('2'), change.clone());
        let index = sample().with_changes(changes);
        assert_eq!(index.change_of(commit('1')), Some(&change));
        assert_eq!(index.change_of(commit('9')), None);
        // 'a' hängt an '1' und '2' — dieselbe Change-Id, einmal genannt.
        assert_eq!(index.changes_of(id('a')), vec![change.clone()]);
        assert_eq!(index.changes_of(id('b')), vec![change]);
        assert!(index.changes_of(id('f')).is_empty());
    }

    #[test]
    fn degraded_entries_keep_their_cause_and_count_as_unreadable() {
        let index = sample().with_degraded(vec![
            Degraded {
                id: id('d'),
                cause: Degradation::Forgotten {
                    reason: "DSGVO".into(),
                },
            },
            Degraded {
                id: id('e'),
                cause: Degradation::Corrupt,
            },
        ]);
        assert_eq!(index.unreadable(), 2);
        assert_eq!(index.degraded()[0].cause.word(), "vergessen");
        assert!(index.degraded()[0].cause.is_forgotten());
        assert_eq!(index.degraded()[1].cause.word(), "unlesbar");
        // Degradierte sind keine Sessions.
        assert_eq!(index.len(), 2);
        assert!(index.session(id('d')).is_none());
    }

    #[test]
    fn a_forgotten_store_error_carries_its_reason_sanitized() {
        let err = StoreError::Forgotten {
            id: id('a'),
            reason: "Kunde\u{1b}[2Kweg".into(),
        };
        let cause = Degradation::of(&err);
        assert_eq!(
            cause,
            Degradation::Forgotten {
                reason: "Kunde\\u{1b}[2Kweg".into()
            }
        );
        assert_eq!(
            Degradation::of(&StoreError::Unredacted { id: id('a') }),
            Degradation::Unredacted
        );
    }

    #[test]
    fn coverage_counts_linked_commits_over_all_commits() {
        let index = sample();
        let coverage = index.coverage();
        assert_eq!(coverage.commits_total, 2);
        assert_eq!(coverage.commits_with_context, 2);
        assert_eq!(Index::default().coverage().commits_total, 0);
    }

    #[test]
    fn a_symbolic_endpoint_resolves_over_agent_and_local_id() {
        let mut with_lineage = session("mit Herkunft");
        with_lineage.lineage = Some(minds_core::Lineage::new("sess-42"));
        let mut sessions = BTreeMap::new();
        sessions.insert(id('a'), with_lineage);
        sessions.insert(id('b'), session("ohne Herkunft"));
        let index = Index::from_parts(sessions, BTreeMap::new());
        assert_eq!(
            index.resolve_endpoint("claude-code", "sess-42"),
            Some(id('a'))
        );
        assert_eq!(index.resolve_endpoint("codex", "sess-42"), None);
        assert_eq!(index.resolve_endpoint("claude-code", "sess-43"), None);
    }

    #[test]
    fn an_unknown_commit_has_no_subject() {
        assert_eq!(sample().subject_of(commit('1')), None);
    }
}
