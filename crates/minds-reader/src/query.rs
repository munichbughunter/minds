//! Die Anfrage-Schicht: einmal laden, dann nur noch fragen.
//!
//! [`Inspection`] ist das Bild, das eine Oberfläche beim Start zieht — Index,
//! Reviews, HEAD — und danach ohne weiteres I/O befragt: Karten, Graph,
//! Herkunftskette. Nur zwei Fragen brauchen danach noch Git, und sie sagen
//! es in der Signatur (`repo` als Parameter): der Blame einer Zeile
//! ([`Inspection::why_line`]) und die Erklärung einer vermuteten Kante
//! ([`Inspection::evidence`]), die den Commit-Diff liest.
//!
//! Die Typen hier sind die, die `recap`/`show`/`why` in der CLI brauchen —
//! CLI und TUI sollen Geschwister über derselben Anfrage sein, keine zwei
//! Lesewege.

use std::collections::BTreeMap;

use minds_core::{EdgeKind, Endpoint, Review, Session, SessionId};
use minds_git::{BlameProvider, CommitId, Head, Repo};
use minds_metrics::epoch_seconds;
use minds_store::{ContextStore, ReviewStore};

use crate::error::Result;
use crate::evidence;
use crate::graph::SessionGraph;
use crate::index::{Degradation, Index};
use crate::model::{
    CardState, EvidenceExplanation, Header, LinkEvidence, Provenance, ReviewNote, ReviewState,
    SessionCard, Verdict, WhyChain, WhyStep,
};
use crate::summary::Summary;
use crate::text::{sanitize, sanitize_path};

/// Ein Review samt der Tatsache, ob eine Signatur vorliegt.
#[derive(Debug, Clone)]
struct Stored {
    review: Review,
    signed: bool,
}

/// Das einmal geladene Lese-Modell.
#[derive(Debug, Clone, Default)]
pub struct Inspection {
    index: Index,
    /// Reviews je Subjekt-Id.
    reviews: BTreeMap<String, Vec<Stored>>,
    branch: Option<String>,
    repo_name: String,
}

impl Inspection {
    /// Lädt Index, Reviews und HEAD. `reviews` darf fehlen — dann ist jedes
    /// Verdict „offen". `repo_name` ist der Anzeigename des Repositories.
    pub fn load(
        repo: &Repo,
        store: &dyn ContextStore,
        reviews: Option<&ReviewStore>,
        repo_name: &str,
    ) -> Result<Self> {
        let index = Index::build(repo, store)?;
        let branch = match repo.head()? {
            Head::Branch { name, .. } | Head::Unborn { name } => Some(sanitize(&name)),
            Head::Detached { .. } => None,
        };
        let mut grouped: BTreeMap<String, Vec<Stored>> = BTreeMap::new();
        if let Some(store) = reviews {
            for review in store.list()? {
                let signed = review
                    .content_hash()
                    .ok()
                    .and_then(|hash| store.signature(&hash).ok().flatten())
                    .is_some();
                grouped
                    .entry(review.subject.id().to_string())
                    .or_default()
                    .push(Stored { review, signed });
            }
        }
        Ok(Self::assemble(index, grouped, branch, repo_name))
    }

    /// Baut das Modell aus einem fertigen Index — für Tests und Aufrufer, die
    /// ihre Daten schon haben. Reviews gelten dabei als unsigniert.
    pub fn from_index(index: Index, reviews: Vec<Review>, repo_name: &str) -> Self {
        let mut grouped: BTreeMap<String, Vec<Stored>> = BTreeMap::new();
        for review in reviews {
            grouped
                .entry(review.subject.id().to_string())
                .or_default()
                .push(Stored {
                    review,
                    signed: false,
                });
        }
        Self::assemble(index, grouped, None, repo_name)
    }

    fn assemble(
        index: Index,
        mut reviews: BTreeMap<String, Vec<Stored>>,
        branch: Option<String>,
        repo_name: &str,
    ) -> Self {
        // Jüngstes zuletzt; bei gleichem Zeitstempel entscheidet der Hash —
        // deterministisch, unabhängig von der Store-Reihenfolge.
        for list in reviews.values_mut() {
            list.sort_by_cached_key(|s| {
                (
                    s.review.at.clone().unwrap_or_default(),
                    s.review
                        .content_hash()
                        .map(|h| h.to_string())
                        .unwrap_or_default(),
                )
            });
        }
        Self {
            index,
            reviews,
            branch,
            repo_name: sanitize(repo_name),
        }
    }

    /// Der Index dahinter.
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Die sessionlosen Block-Seals: zurückgehaltene Sessions, deren einziger
    /// Beleg der Seal ist (ADR-0011, Entscheidung 3).
    pub fn rejected_seals(&self) -> &[(minds_core::ContentHash, minds_core::evidence::Seal)] {
        self.index.rejected_seals()
    }

    /// Der Evidence-Report einer Session — Verdikt, Epochen, Coverage,
    /// Signatur-Lage und Grenzen, fertig gerechnet (Semantik von
    /// `minds verify`). `None` heißt Legacy, nie „Bug". Die TUI rendert
    /// das nur; sie rechnet nichts nach.
    pub fn evidence_report(&self, id: SessionId) -> Option<crate::model::EvidenceReport> {
        self.index.evidence_report(id)
    }

    /// Die Kopfzeile.
    pub fn header(&self) -> Header {
        let mut changes: Vec<&minds_core::ChangeId> = self
            .index
            .sessions()
            .flat_map(|(id, _)| self.index.commits_of(*id))
            .filter_map(|commit| self.index.change_of(commit))
            .collect();
        changes.sort();
        changes.dedup();
        Header {
            repo: self.repo_name.clone(),
            branch: self.branch.clone(),
            sessions: self.index.len(),
            changes: changes.len(),
            degraded: self.index.degraded().len(),
            coverage: self.index.coverage(),
        }
    }

    /// Alle Karten: jüngste zuerst, Karten ohne Zeit danach, degradierte am
    /// Ende — innerhalb einer Gruppe stabil nach Id.
    pub fn cards(&self) -> Vec<SessionCard> {
        let mut cards: Vec<SessionCard> = self
            .index
            .sessions()
            .map(|(id, session)| self.card_of(*id, session))
            .collect();
        cards.sort_by(|a, b| b.epoch.cmp(&a.epoch).then_with(|| a.id.cmp(&b.id)));
        cards.extend(self.index.degraded().iter().map(|d| {
            let (state, word) = match &d.cause {
                Degradation::Forgotten { reason } => (
                    CardState::Forgotten {
                        reason: reason.clone(),
                    },
                    format!("vergessen: {reason}"),
                ),
                cause => (
                    CardState::Unreadable {
                        cause: cause.clone(),
                    },
                    format!("unlesbar ({})", degradation_word(cause)),
                ),
            };
            SessionCard {
                id: d.id,
                summary: Summary {
                    id: d.id,
                    headline: word,
                    actor: "—".into(),
                    files: 0,
                    constraints: 0,
                    discarded: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                },
                started_at: None,
                epoch: None,
                evidence: None,
                provenance: Provenance::Legacy,
                uninterpreted_calls: 0,
                epoch_position: None,
                handovers: 0,
                review: ReviewState::open(),
                changes: Vec::new(),
                commits: Vec::new(),
                subagents: Vec::new(),
                parent: None,
                state,
            }
        }));
        cards
    }

    /// Die Karte einer Session; `None`, wenn sie nicht lesbar im Index ist.
    pub fn card(&self, id: SessionId) -> Option<SessionCard> {
        self.index.session(id).map(|s| self.card_of(id, s))
    }

    fn card_of(&self, id: SessionId, session: &Session) -> SessionCard {
        let mut summary = Summary::of(id, session);
        summary.headline = sanitize(&summary.headline);
        summary.actor = sanitize(&summary.actor);
        let started_at = session.lineage.as_ref().and_then(|l| l.started_at.clone());
        let epoch = started_at
            .as_deref()
            .and_then(epoch_seconds)
            .or_else(|| evidence::session_window(session).map(|(start, _)| start));
        let mut subagents = Vec::new();
        let mut parent = None;
        for edge in &session.edges {
            let Endpoint::Session { agent, local_id } = &edge.to else {
                continue;
            };
            let Some(other) = self.index.resolve_endpoint(agent, local_id) else {
                continue;
            };
            match edge.kind {
                EdgeKind::Spawned => subagents.push(other),
                EdgeKind::SpawnedBy => parent = Some(other),
                EdgeKind::ContinuedFrom | EdgeKind::Produced => {}
            }
        }
        SessionCard {
            id,
            summary,
            started_at: started_at.map(|s| sanitize(&s)),
            epoch,
            evidence: self.index.evidence_for_session(id),
            provenance: match self.index.evidence_state(id) {
                Some(state) => Provenance::Chained(state),
                None => Provenance::Legacy,
            },
            uninterpreted_calls: session
                .turns
                .iter()
                .flat_map(|t| &t.tool_calls)
                .filter(|c| {
                    c.capture
                        .as_ref()
                        .is_some_and(|cap| cap.status == minds_core::CaptureStatus::Uninterpreted)
                })
                .count(),
            epoch_position: self.index.epoch_position(id),
            handovers: self.index.content_links_of(id).len(),
            review: self.review_state(id),
            changes: self.index.changes_of(id),
            commits: self.index.commits_of(id),
            subagents,
            parent,
            state: CardState::Ok,
        }
    }

    /// Der Review-Stand einer Session: Reviews an der Session selbst und an
    /// jeder Change-Id ihrer Commits, zusammengeführt.
    pub fn review_state(&self, id: SessionId) -> ReviewState {
        let mut subjects = vec![id.to_string()];
        subjects.extend(self.index.changes_of(id).iter().map(|c| c.to_string()));
        self.review_state_of(&subjects)
    }

    /// Der Review-Stand eines Commits: Reviews an seiner Change-Id und an
    /// jeder seiner Sessions.
    pub fn review_state_of_commit(&self, commit: CommitId) -> ReviewState {
        let mut subjects: Vec<String> = self
            .index
            .change_of(commit)
            .map(|c| vec![c.to_string()])
            .unwrap_or_default();
        subjects.extend(self.index.sessions_of(commit).iter().map(|s| s.to_string()));
        self.review_state_of(&subjects)
    }

    fn review_state_of(&self, subjects: &[String]) -> ReviewState {
        let mut stored: Vec<&Stored> = subjects
            .iter()
            .filter_map(|s| self.reviews.get(s))
            .flatten()
            .collect();
        stored.sort_by_cached_key(|s| {
            (
                s.review.at.clone().unwrap_or_default(),
                s.review
                    .content_hash()
                    .map(|h| h.to_string())
                    .unwrap_or_default(),
            )
        });
        stored.dedup_by(|a, b| a.review.content_hash().ok() == b.review.content_hash().ok());
        let notes: Vec<ReviewNote> = stored
            .iter()
            .map(|s| ReviewNote {
                subject: s.review.subject.clone(),
                decision: s.review.decision,
                reviewer: sanitize(&s.review.reviewer),
                summary: sanitize(&s.review.summary),
                at: s.review.at.as_deref().map(sanitize),
                signed: s.signed,
            })
            .collect();
        let verdict = notes
            .last()
            .map(|n| Verdict::of(n.decision))
            .unwrap_or(Verdict::Open);
        ReviewState { verdict, notes }
    }

    /// Der Graph einer Session.
    pub fn graph(&self, id: SessionId) -> Option<SessionGraph> {
        let session = self.index.session(id)?;
        Some(SessionGraph::of(
            id,
            session,
            &self.index,
            &self.review_state(id),
        ))
    }

    /// Die Herkunftskette einer Session — ohne Zeile und Commit-Schritt,
    /// beginnend bei der Session.
    pub fn why_session(&self, id: SessionId) -> Option<WhyChain> {
        let session = self.index.session(id)?;
        let card = self.card_of(id, session);
        let links = self
            .index
            .commits_of(id)
            .into_iter()
            .filter_map(|commit| self.link_unexplained(commit, id))
            .collect();
        let mut steps = vec![WhyStep::Sessions { cards: vec![card] }];
        steps.extend(self.inner_steps(session));
        steps.push(WhyStep::Evidence { links });
        steps.push(WhyStep::Review {
            state: self.review_state(id),
        });
        Some(WhyChain { steps })
    }

    /// Die Herkunftskette eines Commits.
    pub fn why_commit(&self, commit: CommitId) -> WhyChain {
        let mut steps = vec![
            WhyStep::Commit {
                id: Some(commit),
                subject: self.index.subject_of(commit).map(str::to_string),
            },
            WhyStep::Change {
                id: self.index.change_of(commit).cloned(),
            },
        ];
        let ids: Vec<SessionId> = self.index.sessions_of(commit).to_vec();
        let cards: Vec<SessionCard> = ids.iter().filter_map(|id| self.card(*id)).collect();
        steps.push(WhyStep::Sessions {
            cards: cards.clone(),
        });
        if let Some(first) = ids.first().and_then(|id| self.index.session(*id)) {
            steps.extend(self.inner_steps(first));
        }
        steps.push(WhyStep::Evidence {
            links: ids
                .iter()
                .filter_map(|id| self.link_unexplained(commit, *id))
                .collect(),
        });
        steps.push(WhyStep::Review {
            state: self.review_state_of_commit(commit),
        });
        WhyChain { steps }
    }

    /// Die Herkunftskette einer Zeile: Blame, dann wie [`Inspection::why_commit`].
    /// Kennt Blame die Zeile nicht, beginnt die Kette mit `Commit { id: None }`
    /// und endet dort — kein Fehler.
    pub fn why_line(&self, repo: &Repo, path: &str, line: u32) -> Result<WhyChain> {
        let line_step = WhyStep::Line {
            path: sanitize_path(path),
            line,
        };
        let Some(head) = repo.head()?.commit() else {
            return Ok(WhyChain {
                steps: vec![
                    line_step,
                    WhyStep::Commit {
                        id: None,
                        subject: None,
                    },
                ],
            });
        };
        let Some(commit) = repo.blame().blame_line(head, path, line)? else {
            return Ok(WhyChain {
                steps: vec![
                    line_step,
                    WhyStep::Commit {
                        id: None,
                        subject: None,
                    },
                ],
            });
        };
        let mut chain = self.why_commit(commit);
        chain.steps.insert(0, line_step);
        Ok(chain)
    }

    /// Erklärt eine Kante — liest dafür Diff und Zeit des Commits. Ein
    /// unlesbarer Diff macht die Erklärung zu `Unknown`, nicht den Aufruf
    /// zum Fehler.
    pub fn evidence(&self, repo: &Repo, commit: CommitId, id: SessionId) -> Option<LinkEvidence> {
        let evidence = self.index.evidence_of(commit, id)?;
        let session = self.index.session(id)?;
        let files: Option<Vec<String>> = repo
            .diff_commit(commit)
            .ok()
            .map(|diff| diff.files.into_iter().map(|f| f.path).collect());
        let time = repo.commit_time(commit).ok();
        Some(LinkEvidence {
            commit,
            session: id,
            evidence,
            why: evidence::explain(evidence, commit, session, files.as_deref(), time),
        })
    }

    /// Erklärt jede Kante einer Liste — Vermutungen werden nachgerechnet, der
    /// Rest bleibt, wie er ist. Scheitert die Nachrechnung, bleibt die Kante
    /// mit ihrer bisherigen Erklärung stehen.
    pub fn explain_links(&self, repo: &Repo, links: &[LinkEvidence]) -> Vec<LinkEvidence> {
        links
            .iter()
            .map(|l| {
                self.evidence(repo, l.commit, l.session)
                    .unwrap_or_else(|| l.clone())
            })
            .collect()
    }

    /// Eine Kante mit der Erklärung, die ohne Git möglich ist: Trailer,
    /// Declared und Content sind aus dem Index klar; eine Vermutung bleibt
    /// bis [`Inspection::evidence`] als „noch nicht nachgerechnet" stehen.
    fn link_unexplained(&self, commit: CommitId, id: SessionId) -> Option<LinkEvidence> {
        let evidence = self.index.evidence_of(commit, id)?;
        let session = self.index.session(id)?;
        let why = match evidence.source {
            minds_core::EvidenceSource::Heuristic => EvidenceExplanation::Unknown {
                reason: "noch nicht nachgerechnet".into(),
            },
            _ => evidence::explain(evidence, commit, session, None, None),
        };
        Some(LinkEvidence {
            commit,
            session: id,
            evidence,
            why,
        })
    }

    fn inner_steps(&self, session: &Session) -> Vec<WhyStep> {
        vec![
            WhyStep::Agent {
                name: sanitize(&session.agent.name),
                version: sanitize(&session.agent.version),
                model: sanitize(&session.model.id),
            },
            WhyStep::Intent {
                request: sanitize(&session.intent.request),
                constraints: session
                    .intent
                    .constraints
                    .iter()
                    .map(|c| sanitize(c))
                    .collect(),
                discarded: session
                    .intent
                    .discarded
                    .iter()
                    .map(|c| sanitize(c))
                    .collect(),
            },
        ]
    }
}

fn degradation_word(cause: &Degradation) -> &'static str {
    match cause {
        Degradation::Forgotten { .. } => "vergessen",
        Degradation::Corrupt => "Hash passt nicht",
        Degradation::Malformed => "kein gültiges JSON",
        Degradation::Unredacted => "nicht redigiert",
        Degradation::Missing => "nicht auflösbar",
        Degradation::Failed { .. } => "Lesefehler",
    }
}

/// `true`, wenn die Session `path` verändert oder berührt hat — über die
/// erzeugten Dateien oder einen Effekt mit diesem Pfad.
pub fn touches(session: &Session, path: &str) -> bool {
    session.produced.files.iter().any(|f| f == path)
        || session.turns.iter().any(|turn| {
            turn.tool_calls.iter().any(|call| {
                call.effect
                    .as_ref()
                    .and_then(|effect| effect.path.as_deref())
                    .is_some_and(|p| p == path)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Degraded;
    use minds_core::{Agent, Decision, Intent, Lineage, Model, Subject};

    fn sid(c: char) -> SessionId {
        format!("b3-{}", c.to_string().repeat(64)).parse().unwrap()
    }

    fn commit(c: char) -> CommitId {
        c.to_string().repeat(40).parse().unwrap()
    }

    fn session(request: &str, started: Option<&str>) -> Session {
        let mut s = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "opus".into(),
            },
            Intent {
                request: request.into(),
                ..Intent::default()
            },
        );
        if let Some(at) = started {
            let mut l = Lineage::new("l");
            l.started_at = Some(at.into());
            s.lineage = Some(l);
        }
        s
    }

    fn review(subject: Subject, decision: Decision, at: &str) -> Review {
        Review::new(subject, decision, "reviewer", "weil", Some(at.to_string()))
    }

    fn sample() -> Inspection {
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('a'), session("alt", Some("2026-07-01T09:00:00Z")));
        sessions.insert(sid('b'), session("neu", Some("2026-07-02T09:00:00Z")));
        sessions.insert(sid('c'), session("ohne Zeit", None));
        let mut commits = BTreeMap::new();
        commits.insert(commit('1'), vec![sid('a')]);
        let change: minds_core::ChangeId = format!("I{}", "c".repeat(40)).parse().unwrap();
        let mut changes = BTreeMap::new();
        changes.insert(commit('1'), change.clone());
        let index = Index::from_parts(sessions, commits)
            .with_changes(changes)
            .with_degraded(vec![Degraded {
                id: sid('d'),
                cause: Degradation::Forgotten {
                    reason: "DSGVO".into(),
                },
            }]);
        Inspection::from_index(
            index,
            vec![
                review(
                    Subject::Change(change.to_string()),
                    Decision::NeedsWork,
                    "2026-07-03T00:00:00Z",
                ),
                review(
                    Subject::Change(change.to_string()),
                    Decision::Approve,
                    "2026-07-04T00:00:00Z",
                ),
                review(
                    Subject::Session(sid('b').to_string()),
                    Decision::Reject,
                    "2026-07-05T00:00:00Z",
                ),
            ],
            "payment\u{1b}[2K",
        )
    }

    #[test]
    fn cards_come_newest_first_then_timeless_then_degraded() {
        let cards = sample().cards();
        let ids: Vec<SessionId> = cards.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![sid('b'), sid('a'), sid('c'), sid('d')]);
        assert!(cards[3].is_degraded());
        assert_eq!(cards[3].summary.headline, "vergessen: DSGVO");
        assert_eq!(cards[3].epoch, None);
    }

    #[test]
    fn the_latest_review_wins_and_all_notes_stay() {
        let insp = sample();
        let a = insp.review_state(sid('a'));
        assert_eq!(a.verdict, Verdict::Approved);
        assert_eq!(a.notes.len(), 2);
        assert_eq!(a.notes[0].decision, Decision::NeedsWork);
        let b = insp.review_state(sid('b'));
        assert_eq!(b.verdict, Verdict::Rejected);
        assert_eq!(insp.review_state(sid('c')).verdict, Verdict::Open);
        assert_eq!(
            insp.review_state_of_commit(commit('1')).verdict,
            Verdict::Approved
        );
    }

    #[test]
    fn the_header_counts_changes_once_and_sanitizes_the_name() {
        let header = sample().header();
        assert_eq!(header.repo, "payment\\u{1b}[2K");
        assert_eq!(header.sessions, 3);
        assert_eq!(header.changes, 1);
        assert_eq!(header.degraded, 1);
        assert_eq!(header.coverage.commits_total, 1);
    }

    #[test]
    fn a_card_carries_evidence_changes_and_commits() {
        let insp = sample();
        let a = insp.card(sid('a')).unwrap();
        assert_eq!(
            a.evidence,
            Some(minds_core::EvidenceMark::of(
                minds_core::EvidenceSource::Observed
            ))
        );
        assert_eq!(a.commits, vec![commit('1')]);
        assert_eq!(a.changes.len(), 1);
        let c = insp.card(sid('c')).unwrap();
        assert_eq!(c.evidence, None);
        assert!(insp.card(sid('d')).is_none());
    }

    #[test]
    fn why_commit_walks_from_commit_to_review() {
        let chain = sample().why_commit(commit('1'));
        let kinds: Vec<&str> = chain
            .steps
            .iter()
            .map(|s| match s {
                WhyStep::Line { .. } => "line",
                WhyStep::Commit { .. } => "commit",
                WhyStep::Change { .. } => "change",
                WhyStep::Sessions { .. } => "sessions",
                WhyStep::Agent { .. } => "agent",
                WhyStep::Intent { .. } => "intent",
                WhyStep::Evidence { .. } => "evidence",
                WhyStep::Review { .. } => "review",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "commit", "change", "sessions", "agent", "intent", "evidence", "review"
            ]
        );
        match &chain.steps[5] {
            WhyStep::Evidence { links } => {
                assert_eq!(links.len(), 1);
                assert_eq!(
                    links[0].why,
                    EvidenceExplanation::Trailer {
                        commit: commit('1')
                    }
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn why_commit_without_context_has_an_empty_session_step_not_an_error() {
        let chain = sample().why_commit(commit('9'));
        assert!(matches!(&chain.steps[1], WhyStep::Change { id: None }));
        assert!(matches!(&chain.steps[2], WhyStep::Sessions { cards } if cards.is_empty()));
        // Ohne Session kein Agent/Intent — direkt zu Evidenz und Review.
        assert_eq!(chain.steps.len(), 5);
    }

    #[test]
    fn touches_looks_at_produced_files_and_effect_paths() {
        let mut s = session("x", None);
        s.produced.files.push("a.rs".into());
        assert!(touches(&s, "a.rs"));
        assert!(!touches(&s, "b.rs"));
    }
}
