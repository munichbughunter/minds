//! Der Graph einer Session: Absicht → Agent → Züge und Tool-Aufrufe →
//! Änderung → Review — als Baum mit Eltern-Verweisen, ohne Layout.
//!
//! Die Züge einer Session sind bereits ein Baum (`Turn::parent`), keine
//! Liste; Sub-Agents hängen über Kanten daran. Dieses Modul macht daraus
//! **einen** Knotenbaum, den eine Oberfläche zeichnen kann — was die Spur
//! ist und was der Seitenast, entscheidet erst das Layout dort. Hier gibt es
//! nur Eltern, Art, Beschriftung und Details.
//!
//! Die Beschriftungen sind entschärft und gekürzt; rohe Tool-Argumente
//! erreichen keine Oberfläche.

use minds_core::{EdgeKind, EffectKind, Endpoint, Role, Session, SessionId, extract};

use crate::index::Index;
use crate::model::{ReviewState, Verdict};
use crate::text::{sanitize, sanitize_path};

/// Auf so viele Zeichen werden Argumente und Zugtexte im Detail gekürzt —
/// Zeichen, nicht Bytes, damit kein Umlaut zerschnitten wird.
pub const DETAIL_MAX: usize = 120;

/// Was ein Tool in der Welt getan hat, in der Sprache der Oberfläche.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Gelesen.
    Read,
    /// Geschrieben.
    Edit,
    /// Ausgeführt.
    Exec,
    /// Gelöscht.
    Delete,
    /// Unbekannt oder ohne Effekt.
    Other,
}

impl ToolKind {
    fn of(kind: Option<EffectKind>) -> Self {
        match kind {
            Some(EffectKind::Read) => ToolKind::Read,
            Some(EffectKind::Write) => ToolKind::Edit,
            Some(EffectKind::Exec) => ToolKind::Exec,
            Some(EffectKind::Delete) => ToolKind::Delete,
            Some(EffectKind::Other) | None => ToolKind::Other,
        }
    }

    /// Das Wort für die Anzeige.
    pub fn word(&self) -> &'static str {
        match self {
            ToolKind::Read => "READ",
            ToolKind::Edit => "EDIT",
            ToolKind::Exec => "EXEC",
            ToolKind::Delete => "DELETE",
            ToolKind::Other => "TOOL",
        }
    }
}

/// Die Art eines Knotens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// Die Absicht des Menschen.
    Intent,
    /// Der Agent.
    Agent,
    /// Ein Zug.
    Turn(Role),
    /// Ein Tool-Aufruf.
    Tool(ToolKind),
    /// Ein gestarteter Sub-Agent.
    Subagent(SessionId),
    /// Eine Änderung (Change-Id).
    Change(minds_core::ChangeId),
    /// Ein Commit ohne Change-Id.
    Commit(minds_git::CommitId),
    /// Die Bewertung.
    Review(Verdict),
}

/// Ein Knoten des Graphen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNode {
    /// Index in [`SessionGraph::nodes`].
    pub id: usize,
    /// Der Elternknoten; nur die Wurzel hat keinen. Immer kleiner als `id`.
    pub parent: Option<usize>,
    /// Die Art.
    pub kind: NodeKind,
    /// Die Beschriftung, entschärft.
    pub label: String,
    /// Schlüssel/Wert-Details für eine Detailansicht, entschärft.
    pub detail: Vec<(String, String)>,
    /// Zeitpunkt, falls erfasst.
    pub at: Option<String>,
    /// Der Pfad, den ein Tool berührt hat — für Aggregation je Datei.
    pub path: Option<String>,
}

/// Der Graph einer Session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionGraph {
    /// Die Knoten, Eltern vor Kindern.
    pub nodes: Vec<GraphNode>,
}

impl SessionGraph {
    /// Baut den Graphen einer Session. `review` ist der Stand, den die
    /// Oberfläche am Ende zeigt.
    pub fn of(id: SessionId, session: &Session, index: &Index, review: &ReviewState) -> Self {
        let mut graph = Self::default();

        let intent = graph.push(
            None,
            NodeKind::Intent,
            crate::summary::headline(&session.intent.request, DETAIL_MAX),
            vec![
                ("Prompt".into(), sanitize(&session.intent.request)),
                (
                    "Constraints".into(),
                    sanitize(&session.intent.constraints.join("\n")),
                ),
                (
                    "Verworfen".into(),
                    sanitize(&session.intent.discarded.join("\n")),
                ),
            ],
            session.lineage.as_ref().and_then(|l| l.started_at.clone()),
            None,
        );

        let agent = graph.push(
            Some(intent),
            NodeKind::Agent,
            sanitize(&format!("{} · {}", session.agent.name, session.model.id)),
            vec![
                ("Agent".into(), sanitize(&session.agent.name)),
                ("Version".into(), sanitize(&session.agent.version)),
                ("Anbieter".into(), sanitize(&session.model.provider)),
                ("Modell".into(), sanitize(&session.model.id)),
                (
                    "Token".into(),
                    format!(
                        "{} ein / {} aus",
                        session.usage.input_tokens, session.usage.output_tokens
                    ),
                ),
            ],
            None,
            None,
        );

        // Die Züge: `parent` zeigt auf einen früheren Zug — dann ist der Zug
        // ein Seitenast dort. Fehlt er oder zeigt er nach vorn (Invariante
        // verletzt), hängt der Zug als Geschwister unter dem Agenten —
        // fail-soft, der Graph bleibt zusammenhängend und flach.
        let mut turn_nodes: Vec<usize> = Vec::with_capacity(session.turns.len());
        for (i, turn) in session.turns.iter().enumerate() {
            let parent = turn
                .parent
                .map(|p| p as usize)
                .filter(|p| *p < i)
                .map(|p| turn_nodes[p])
                .unwrap_or(agent);
            let text = turn.text.trim();
            let label = if text.is_empty() {
                role_word(&turn.role).to_string()
            } else {
                format!(
                    "{} · {}",
                    role_word(&turn.role),
                    crate::summary::headline(text, DETAIL_MAX)
                )
            };
            let node = graph.push(
                Some(parent),
                NodeKind::Turn(turn.role.clone()),
                label,
                vec![("Text".into(), truncate(&sanitize(text)))],
                turn.at.clone(),
                None,
            );
            turn_nodes.push(node);
            for call in &turn.tool_calls {
                let kind = ToolKind::of(call.effect.as_ref().map(|e| e.kind));
                let path = call
                    .effect
                    .as_ref()
                    .and_then(|e| e.path.as_deref())
                    .map(sanitize_path);
                let label = match kind {
                    ToolKind::Exec => extract::command_of(&call.arguments)
                        .map(|c| sanitize(&c))
                        .unwrap_or_else(|| sanitize(&call.name)),
                    _ => path.clone().unwrap_or_else(|| sanitize(&call.name)),
                };
                let mut detail = vec![
                    ("Tool".into(), sanitize(&call.name)),
                    ("Argumente".into(), truncate(&sanitize(&call.arguments))),
                ];
                if let Some(effect) = &call.effect {
                    detail.push(("Effekt".into(), kind.word().into()));
                    if let Some(p) = &path {
                        detail.push(("Pfad".into(), p.clone()));
                    }
                    if let Some(hash) = &effect.content {
                        detail.push(("Inhalt".into(), hash.to_string()));
                    }
                }
                graph.push(
                    Some(node),
                    NodeKind::Tool(kind),
                    label,
                    detail,
                    turn.at.clone(),
                    path,
                );
            }
        }

        // Sub-Agents hängen am Agenten — ein Seitenast, kein Zug.
        for edge in &session.edges {
            if edge.kind != EdgeKind::Spawned {
                continue;
            }
            let Endpoint::Session {
                agent: name,
                local_id,
            } = &edge.to
            else {
                continue;
            };
            let Some(child) = index.resolve_endpoint(name, local_id) else {
                continue;
            };
            let label = index
                .session(child)
                .map(|s| crate::summary::headline(&s.intent.request, DETAIL_MAX))
                .unwrap_or_else(|| child.to_string());
            graph.push(
                Some(agent),
                NodeKind::Subagent(child),
                label,
                vec![
                    ("Session".into(), child.to_string()),
                    ("Agent".into(), sanitize(name)),
                    (
                        "Beleg".into(),
                        format!("{:?}", edge.evidence).to_lowercase(),
                    ),
                ],
                None,
                None,
            );
        }

        // Änderungen: je Commit ein Knoten — Change-Id, wo der Commit eine
        // trägt, sonst der Commit selbst — als letzte Kinder des Agenten,
        // verkettet; das Review schließt ab.
        let mut tail = agent;
        let mut seen_changes: Vec<minds_core::ChangeId> = Vec::new();
        for commit in index.commits_of(id) {
            let evidence = index
                .evidence_of(commit, id)
                .map(|e| format!("{e:?}").to_lowercase())
                .unwrap_or_default();
            let subject = index.subject_of(commit).unwrap_or("").to_string();
            let short: String = commit.to_string().chars().take(10).collect();
            let detail = vec![
                ("Commit".into(), commit.to_string()),
                ("Betreff".into(), subject.clone()),
                ("Beleg".into(), evidence),
            ];
            let node = match index.change_of(commit) {
                Some(change) => {
                    if seen_changes.contains(change) {
                        continue;
                    }
                    seen_changes.push(change.clone());
                    graph.push(
                        Some(tail),
                        NodeKind::Change(change.clone()),
                        change.to_string(),
                        detail,
                        None,
                        None,
                    )
                }
                None => graph.push(
                    Some(tail),
                    NodeKind::Commit(commit),
                    if subject.is_empty() {
                        short
                    } else {
                        format!("{short} {subject}")
                    },
                    detail,
                    None,
                    None,
                ),
            };
            tail = node;
        }

        graph.push(
            Some(tail),
            NodeKind::Review(review.verdict),
            review.verdict.word().to_string(),
            review
                .notes
                .iter()
                .map(|note| {
                    (
                        note.reviewer.clone(),
                        format!(
                            "{}{} — {}",
                            note.decision.as_str(),
                            if note.signed { " (signiert)" } else { "" },
                            note.summary
                        ),
                    )
                })
                .collect(),
            review.notes.last().and_then(|n| n.at.clone()),
            None,
        );

        graph
    }

    /// Die Kinder eines Knotens, in Reihenfolge.
    pub fn children(&self, id: usize) -> Vec<usize> {
        self.nodes
            .iter()
            .filter(|n| n.parent == Some(id))
            .map(|n| n.id)
            .collect()
    }

    fn push(
        &mut self,
        parent: Option<usize>,
        kind: NodeKind,
        label: String,
        detail: Vec<(String, String)>,
        at: Option<String>,
        path: Option<String>,
    ) -> usize {
        let id = self.nodes.len();
        let detail = detail.into_iter().filter(|(_, v)| !v.is_empty()).collect();
        self.nodes.push(GraphNode {
            id,
            parent,
            kind,
            label,
            detail,
            at,
            path,
        });
        id
    }
}

fn role_word(role: &Role) -> &'static str {
    match role {
        Role::System => "SYSTEM",
        Role::User => "USER",
        Role::Assistant => "ASSISTANT",
        Role::Tool => "TOOL",
    }
}

/// Kürzt auf [`DETAIL_MAX`] Zeichen.
fn truncate(text: &str) -> String {
    if text.chars().count() <= DETAIL_MAX {
        return text.to_string();
    }
    let cut: String = text.chars().take(DETAIL_MAX).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{Agent, Edge, Effect, Evidence, Intent, Lineage, Model, ToolCall, Turn};
    use std::collections::BTreeMap;

    fn sid(c: char) -> SessionId {
        format!("b3-{}", c.to_string().repeat(64)).parse().unwrap()
    }

    fn session(request: &str) -> Session {
        Session::new(
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
        )
    }

    fn turn(role: Role, text: &str, parent: Option<u32>, calls: Vec<ToolCall>) -> Turn {
        Turn {
            role,
            text: text.into(),
            tool_calls: calls,
            parent,
            at: None,
        }
    }

    fn call(name: &str, args: &str, kind: Option<EffectKind>, path: Option<&str>) -> ToolCall {
        ToolCall {
            name: name.into(),
            arguments: args.into(),
            effect: kind.map(|kind| Effect {
                kind,
                path: path.map(str::to_string),
                content: None,
            }),
        }
    }

    fn kinds(graph: &SessionGraph) -> Vec<(usize, Option<usize>, String)> {
        graph
            .nodes
            .iter()
            .map(|n| (n.id, n.parent, format!("{:?}", n.kind)))
            .collect()
    }

    #[test]
    fn a_linear_session_is_a_chain_from_intent_to_review() {
        let mut s = session("Fix retry");
        s.turns.push(turn(
            Role::Assistant,
            "Ich lese.",
            None,
            vec![
                call(
                    "Read",
                    "{\"file_path\":\"a.rs\"}",
                    Some(EffectKind::Read),
                    Some("a.rs"),
                ),
                call("Edit", "{}", Some(EffectKind::Write), Some("a.rs")),
                call(
                    "Bash",
                    "{\"command\":\"cargo test\"}",
                    Some(EffectKind::Exec),
                    None,
                ),
            ],
        ));
        let index = Index::from_parts(BTreeMap::new(), BTreeMap::new());
        let g = SessionGraph::of(sid('a'), &s, &index, &ReviewState::open());
        let labels: Vec<&str> = g.nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Fix retry",
                "claude-code · opus",
                "ASSISTANT · Ich lese.",
                "a.rs",
                "a.rs",
                "cargo test",
                "offen"
            ]
        );
        let parents: Vec<Option<usize>> = g.nodes.iter().map(|n| n.parent).collect();
        assert_eq!(
            parents,
            vec![None, Some(0), Some(1), Some(2), Some(2), Some(2), Some(1)]
        );
        assert_eq!(g.nodes[3].kind, NodeKind::Tool(ToolKind::Read));
        assert_eq!(g.nodes[4].kind, NodeKind::Tool(ToolKind::Edit));
        assert_eq!(g.nodes[5].kind, NodeKind::Tool(ToolKind::Exec));
        assert_eq!(g.nodes[3].path.as_deref(), Some("a.rs"));
        assert_eq!(g.nodes[5].path, None);
    }

    #[test]
    fn a_turn_parent_becomes_a_branch_and_a_bad_parent_falls_back() {
        let mut s = session("x");
        s.turns.push(turn(Role::User, "eins", None, vec![]));
        s.turns.push(turn(Role::Assistant, "zwei", Some(0), vec![]));
        s.turns.push(turn(Role::Assistant, "drei", Some(0), vec![])); // Seitenast an „eins"
        s.turns.push(turn(Role::Assistant, "vier", Some(9), vec![])); // Vorwärtsverweis: fail-soft
        let index = Index::from_parts(BTreeMap::new(), BTreeMap::new());
        let g = SessionGraph::of(sid('a'), &s, &index, &ReviewState::open());
        let parents: Vec<Option<usize>> = g.nodes.iter().map(|n| n.parent).collect();
        // 0 Intent, 1 Agent, 2 eins(→Agent), 3 zwei(→2), 4 drei(→2), 5 vier(→Agent), 6 Review
        assert_eq!(parents[2], Some(1));
        assert_eq!(parents[3], Some(2));
        assert_eq!(parents[4], Some(2));
        assert_eq!(parents[5], Some(1));
        assert!(
            parents
                .iter()
                .enumerate()
                .all(|(i, p)| p.is_none_or(|p| p < i))
        );
    }

    #[test]
    fn a_spawned_subagent_hangs_off_the_agent_when_resolvable() {
        let mut parent = session("Eltern");
        parent.lineage = Some(Lineage::new("p"));
        parent.edges.push(Edge {
            kind: EdgeKind::Spawned,
            to: Endpoint::Session {
                agent: "claude-code".into(),
                local_id: "c".into(),
            },
            evidence: Evidence::Observed,
        });
        parent.edges.push(Edge {
            kind: EdgeKind::Spawned,
            to: Endpoint::Session {
                agent: "claude-code".into(),
                local_id: "unbekannt".into(),
            },
            evidence: Evidence::Observed,
        });
        let mut child = session("Kind");
        child.lineage = Some(Lineage::new("c"));
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('a'), parent.clone());
        sessions.insert(sid('b'), child);
        let index = Index::from_parts(sessions, BTreeMap::new());
        let g = SessionGraph::of(sid('a'), &parent, &index, &ReviewState::open());
        let subs: Vec<&GraphNode> = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Subagent(_)))
            .collect();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].parent, Some(1));
        assert_eq!(subs[0].label, "Kind");
        assert_eq!(subs[0].kind, NodeKind::Subagent(sid('b')));
    }

    #[test]
    fn commits_become_change_nodes_deduplicated_by_change_id() {
        let s = session("x");
        let c1: minds_git::CommitId = "1".repeat(40).parse().unwrap();
        let c2: minds_git::CommitId = "2".repeat(40).parse().unwrap();
        let c3: minds_git::CommitId = "3".repeat(40).parse().unwrap();
        let change: minds_core::ChangeId = format!("I{}", "c".repeat(40)).parse().unwrap();
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('a'), s.clone());
        let mut commits = BTreeMap::new();
        commits.insert(c1, vec![sid('a')]);
        commits.insert(c2, vec![sid('a')]);
        commits.insert(c3, vec![sid('a')]);
        let mut changes = BTreeMap::new();
        changes.insert(c1, change.clone());
        changes.insert(c2, change.clone());
        let index = Index::from_parts(sessions, commits).with_changes(changes);
        let g = SessionGraph::of(sid('a'), &s, &index, &ReviewState::open());
        let tail: Vec<String> = kinds(&g).into_iter().skip(2).map(|(_, _, k)| k).collect();
        assert_eq!(tail.len(), 3, "{tail:?}");
        assert!(tail[0].starts_with("Change("));
        assert!(tail[1].starts_with("Commit("));
        assert!(tail[2].starts_with("Review("));
        // Kette: Agent → Change → Commit → Review
        assert_eq!(g.nodes[2].parent, Some(1));
        assert_eq!(g.nodes[3].parent, Some(2));
        assert_eq!(g.nodes[4].parent, Some(3));
    }

    #[test]
    fn foreign_text_in_arguments_is_sanitized_and_truncated() {
        let mut s = session("x");
        let long = "a".repeat(300);
        s.turns.push(turn(
            Role::Assistant,
            "",
            None,
            vec![call(
                "Bash",
                &format!("\u{1b}[2K{long}"),
                Some(EffectKind::Other),
                None,
            )],
        ));
        let index = Index::from_parts(BTreeMap::new(), BTreeMap::new());
        let g = SessionGraph::of(sid('a'), &s, &index, &ReviewState::open());
        let tool = &g.nodes[3];
        assert_eq!(tool.label, "Bash");
        let args = &tool
            .detail
            .iter()
            .find(|(k, _)| k == "Argumente")
            .unwrap()
            .1;
        assert!(!args.contains('\u{1b}'));
        assert!(args.ends_with('…'));
        assert!(args.chars().count() <= DETAIL_MAX + 1);
        assert_eq!(g.nodes[2].label, "ASSISTANT");
    }
}
