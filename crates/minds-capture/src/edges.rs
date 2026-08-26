//! Kanten — die Beziehungen einer Session, jede mit ihrer Herkunft.
//!
//! Das Envelope beantwortet „was ist passiert". [`Edge`] beantwortet „in welcher
//! Ordnung, und wie sicher". Die Leitregel aus [`minds_core`] gilt streng: Eine
//! Kante ohne [`Evidence`] wäre eine Behauptung, die wir nicht decken können.
//! Dieses Modul erzeugt deshalb ausschließlich Kanten, die aus den **eigenen
//! beobachteten Events** einer Session folgen — nie aus einer Vermutung über
//! eine andere.
//!
//! # Drei Sorten, drei Quellen
//!
//! - **Sub-Agent** ([`EdgeKind::Spawned`]/[`EdgeKind::SpawnedBy`],
//!   [`Evidence::Observed`]): Ein Hook-Event hat es gesehen. Der Elternteil
//!   sieht seinen `SubagentStart`/`SubagentEnd`, das Kind sieht seinen eigenen
//!   Start als Sub-Agent. Jede Richtung wird aus der Session gegründet, in deren
//!   Journal sie steht — nie über Kreuz geraten.
//! - **Commit** ([`EdgeKind::Produced`], [`Evidence::Observed`]): Der
//!   post-commit-Hook ruft den Checkpoint mit dem gerade entstandenen Commit
//!   auf. Dass *diese* Session *diesen* Commit erzeugt hat, ist damit beobachtet,
//!   nicht geschlossen.
//!
//! # Was hier bewusst *nicht* steht: die Übergabe-Kante
//!
//! [`EdgeKind::ContinuedFrom`] mit [`Evidence::Content`] („Codex las genau die
//! Bytes, die Claude schrieb") ist eine Aussage über *zwei* Sessions. Ein
//! Checkpoint sieht immer nur eine. Diese Kante entsteht später im Store-Index,
//! der beide kennt — hier wird nur das Beweismittel dafür gelegt: der
//! Inhalts-Hash am [`Effect`](minds_core::Effect), den der Adapter beim
//! Checkpoint bildet.

use minds_core::{Edge, EdgeKind, Endpoint, EvidenceMark, EvidenceSource};
use serde::Deserialize;

use crate::journal::{EventKind, JournalEvent};

/// Die Kante zum Commit, den dieser Checkpoint begleitet.
pub fn commit(commit_id: &str) -> Edge {
    Edge {
        kind: EdgeKind::Produced,
        to: Endpoint::Commit {
            id: commit_id.to_string(),
        },
        evidence: EvidenceMark::of(EvidenceSource::Observed),
    }
}

/// Sub-Agent-Kanten, aus den Events *dieser* Session gegründet.
///
/// `agent` ist der Name der aktuellen Session; er füllt die Lücke, wenn der
/// Payload den Agenten der Gegenseite nicht nennt (der Regelfall bei einem
/// Sub-Agenten desselben Agenten).
///
/// Zwei Richtungen, beide beobachtet:
/// - Nennt ein `SubagentStart`/`SubagentEnd` ein Kind, entsteht [`Spawned`].
/// - Nennt der Sessionstart einen Elternteil, entsteht [`SpawnedBy`].
///
/// [`Spawned`]: EdgeKind::Spawned
/// [`SpawnedBy`]: EdgeKind::SpawnedBy
pub fn subagent(agent: &str, events: &[JournalEvent]) -> Vec<Edge> {
    let mut edges = Vec::new();

    for event in events {
        let Ok(marker) = serde_json::from_str::<SubagentMarker>(event.payload.get()) else {
            continue;
        };

        match event.kind {
            EventKind::SubagentStart | EventKind::SubagentEnd => {
                if let Some(child) = marker.child(agent) {
                    push_unique(
                        &mut edges,
                        Edge {
                            kind: EdgeKind::Spawned,
                            to: child,
                            evidence: EvidenceMark::of(EvidenceSource::Observed),
                        },
                    );
                }
            }
            EventKind::SessionStart => {
                if let Some(parent) = marker.parent(agent) {
                    push_unique(
                        &mut edges,
                        Edge {
                            kind: EdgeKind::SpawnedBy,
                            to: parent,
                            evidence: EvidenceMark::of(EvidenceSource::Observed),
                        },
                    );
                }
            }
            _ => {}
        }
    }

    edges
}

/// Fügt eine Kante nur hinzu, wenn sie nicht schon da ist — mehrere
/// `SubagentEnd` für dasselbe Kind ergeben eine Kante, nicht drei.
fn push_unique(edges: &mut Vec<Edge>, edge: Edge) {
    if !edges.contains(&edge) {
        edges.push(edge);
    }
}

/// Die Felder, mit denen ein Hook-Event eine Sub-Agent-Beziehung benennen kann.
///
/// Bewusst tolerant: Der Agent der Gegenseite ist optional (ein Sub-Agent
/// desselben Agenten nennt ihn nicht), und fehlt die Kennung ganz, entsteht
/// keine Kante — geraten wird nicht.
#[derive(Debug, Default, Deserialize)]
struct SubagentMarker {
    subagent_session_id: Option<String>,
    subagent_agent: Option<String>,
    parent_session_id: Option<String>,
    parent_agent: Option<String>,
}

impl SubagentMarker {
    /// Der Endpunkt des Kindes, falls benannt. `default_agent` springt ein, wenn
    /// der Payload keinen eigenen nennt.
    fn child(&self, default_agent: &str) -> Option<Endpoint> {
        self.subagent_session_id
            .clone()
            .map(|local_id| Endpoint::Session {
                agent: self
                    .subagent_agent
                    .clone()
                    .unwrap_or_else(|| default_agent.to_string()),
                local_id,
            })
    }

    /// Der Endpunkt des Elternteils, falls benannt.
    fn parent(&self, default_agent: &str) -> Option<Endpoint> {
        self.parent_session_id
            .clone()
            .map(|local_id| Endpoint::Session {
                agent: self
                    .parent_agent
                    .clone()
                    .unwrap_or_else(|| default_agent.to_string()),
                local_id,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::value::RawValue;

    fn ev(kind: EventKind, payload: &str) -> JournalEvent {
        JournalEvent {
            seq: 0,
            at: "t".into(),
            at_nanos: 0,
            kind,
            raw_kind: format!("{kind:?}"),
            cwd: None,
            transcript_path: None,
            payload: RawValue::from_string(payload.to_string()).unwrap(),
            payload_hash: None,
            event_hash: None,
        }
    }

    #[test]
    fn a_commit_edge_is_produced_and_observed() {
        let e = commit("deadbeef");
        assert_eq!(e.kind, EdgeKind::Produced);
        assert_eq!(e.evidence, EvidenceMark::of(EvidenceSource::Observed));
        assert_eq!(
            e.to,
            Endpoint::Commit {
                id: "deadbeef".into()
            }
        );
    }

    #[test]
    fn a_named_child_becomes_a_spawned_edge() {
        let events = vec![ev(
            EventKind::SubagentEnd,
            r#"{"subagent_session_id":"child-1","subagent_agent":"codex"}"#,
        )];
        let edges = subagent("claude-code", &events);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::Spawned);
        assert_eq!(
            edges[0].evidence,
            EvidenceMark::of(EvidenceSource::Observed)
        );
        assert_eq!(
            edges[0].to,
            Endpoint::Session {
                agent: "codex".into(),
                local_id: "child-1".into()
            }
        );
    }

    #[test]
    fn a_child_of_the_same_agent_inherits_the_agent_name() {
        let events = vec![ev(
            EventKind::SubagentStart,
            r#"{"subagent_session_id":"sc-1"}"#,
        )];
        let edges = subagent("claude-code", &events);
        assert_eq!(
            edges[0].to,
            Endpoint::Session {
                agent: "claude-code".into(),
                local_id: "sc-1".into()
            }
        );
    }

    #[test]
    fn a_declared_parent_becomes_a_spawned_by_edge() {
        let events = vec![ev(
            EventKind::SessionStart,
            r#"{"source":"subagent","parent_session_id":"root-1","parent_agent":"claude-code"}"#,
        )];
        let edges = subagent("codex", &events);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::SpawnedBy);
        assert_eq!(
            edges[0].to,
            Endpoint::Session {
                agent: "claude-code".into(),
                local_id: "root-1".into()
            }
        );
    }

    #[test]
    fn nothing_is_invented_without_an_identity() {
        // Ein SubagentEnd ohne Kind-Kennung darf keine Kante erfinden.
        let events = vec![ev(EventKind::SubagentEnd, r#"{"stop_hook_active":true}"#)];
        assert!(subagent("claude-code", &events).is_empty());
    }

    #[test]
    fn repeated_markers_yield_one_edge() {
        let payload = r#"{"subagent_session_id":"c","subagent_agent":"codex"}"#;
        let events = vec![
            ev(EventKind::SubagentStart, payload),
            ev(EventKind::SubagentEnd, payload),
        ];
        assert_eq!(subagent("claude-code", &events).len(), 1);
    }
}
