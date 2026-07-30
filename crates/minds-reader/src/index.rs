//! Der Index: alles, was die Seite braucht, einmal aus Git und Store gezogen.
//!
//! Der Reader ist **zustandslos** (Architektur-Prinzip 6): Er hält keine
//! Datenbank, sondern baut bei jedem Lauf ein Bild aus zwei Quellen — den
//! Sessions im Store und den Trailern in der Historie. Dieses Modul ist dieses
//! Bild.
//!
//! ```text
//!   Store  ──list/get──►  SessionId → Session
//!   Repo   ──revwalk───►  Commit    → [SessionId]
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
//! # Eine kaputte Session bringt nicht die Seite zu Fall
//!
//! Lässt sich eine Session nicht auflösen (Inhalt passt nicht zum Hash, kaputtes
//! JSON), wird sie übersprungen und **gezählt** statt verschwiegen — siehe
//! [`Index::unreadable`]. Der Reader ist ein Leser; er darf an einem faulen
//! Eintrag nicht sterben, aber er darf ihn auch nicht unterschlagen. `minds
//! fsck` ist das Werkzeug, um dem nachzugehen.

use std::collections::{BTreeMap, BTreeSet};

use minds_core::{Session, SessionId};
use minds_git::{CommitId, Repo};
use minds_store::ContextStore;

use crate::error::Result;

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
    observed: BTreeSet<SessionId>,
    unreadable: usize,
}

impl Index {
    /// Zieht Sessions, Trailer und den Store-Index aus Store und Repository.
    pub fn build(repo: &Repo, store: &dyn ContextStore) -> Result<Self> {
        let mut sessions = BTreeMap::new();
        let mut unreadable = 0usize;

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
                // Im Store gelistet, aber nicht auflösbar — zählen, nicht sterben.
                Ok(None) | Err(_) => unreadable += 1,
            }
        }

        let mut commits: BTreeMap<CommitId, Vec<SessionId>> = BTreeMap::new();
        let mut observed: BTreeSet<SessionId> = BTreeSet::new();

        // 1. Die Trailer — die verbindliche Richtung. Nur Kanten zu Sessions,
        //    die wir behalten haben (keine ausgesiebten).
        if let Some(head) = repo.head()?.commit() {
            for commit in repo.revwalk(head)? {
                let commit = commit?;
                for id in repo.session_ids_of(commit)? {
                    if sessions.contains_key(&id) {
                        observed.insert(id);
                        push_unique(commits.entry(commit).or_default(), id);
                    }
                }
            }
        }

        // 2. Der Store-Index — die vermuteten Kanten (z. B. importiert). Was
        //    schon über einen Trailer da ist, wird nicht doppelt geführt.
        for (hex, links) in store.index()?.iter() {
            let Ok(commit) = hex.parse::<CommitId>() else {
                continue;
            };
            for link in links {
                if sessions.contains_key(&link.session) {
                    push_unique(commits.entry(commit).or_default(), link.session);
                }
            }
        }

        Ok(Self {
            sessions,
            commits,
            observed,
            unreadable,
        })
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
        let commits: BTreeMap<CommitId, Vec<SessionId>> = commits
            .into_iter()
            .map(|(commit, ids)| {
                (
                    commit,
                    ids.into_iter()
                        .filter(|id| sessions.contains_key(id))
                        .collect::<Vec<_>>(),
                )
            })
            .filter(|(_, ids)| !ids.is_empty())
            .collect();
        let observed = commits.values().flatten().copied().collect();
        Self {
            sessions,
            commits,
            observed,
            unreadable: 0,
        }
    }

    /// `true`, wenn diese Session über mindestens einen Trailer belegt ist (im
    /// Gegensatz zu nur heuristisch über den Store-Index verknüpft).
    pub fn is_observed(&self, id: SessionId) -> bool {
        self.observed.contains(&id)
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
        self.commits
            .iter()
            .filter(|(_, ids)| ids.contains(&id))
            .map(|(commit, _)| *commit)
            .collect()
    }

    /// Wie viele Sessions insgesamt bekannt sind.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// `true`, wenn keine Session bekannt ist — der Empty-State der Seite.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Wie viele im Store gelistete Sessions sich nicht auflösen ließen.
    pub fn unreadable(&self) -> usize {
        self.unreadable
    }

    /// Wie viele Commits mindestens eine Session tragen.
    pub fn attributed_commits(&self) -> usize {
        self.commits.len()
    }
}

/// Fügt `id` an, wenn sie nicht schon in `ids` steht.
fn push_unique(ids: &mut Vec<SessionId>, id: SessionId) {
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
}
