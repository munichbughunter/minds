//! Vom Graphen zur Zeilenliste — UI-frei.
//!
//! Der Graph kennt Eltern und Kinder; eine Zeile kennt Tiefe, Box-Zeichen
//! und Text. Dazwischen liegt die Detailstufe: Welche Knoten überhaupt
//! gezeigt werden und was zu einem zusammenfällt.
//!
//! ```text
//!   ● Fix retry handling
//!   ┗━ ◉ claude-code · opus
//!      ┣━ ◇ READ src/retry.rs
//!      ┣━ ✎ EDIT src/retry.rs
//!      ┣━ ▶ EXEC cargo test
//!      ┗━ ◆ CHANGE I…
//!         ┗━ ⚠ REVIEW offen
//! ```
//!
//! Ausgeblendete Knoten verlieren ihre Kinder nicht: Die hängen sich an den
//! nächsten sichtbaren Vorfahren — so bleibt der Baum zusammenhängend, egal
//! welche Stufe gewählt ist.

use std::collections::BTreeMap;

use minds_metrics::epoch_seconds;
use minds_reader::graph::{GraphNode, NodeKind, SessionGraph};

/// Die Detailstufe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    /// Eine Zeile je Datei, Änderung und Review.
    Summary,
    /// Jeder Tool-Aufruf.
    Normal,
    /// Dazu jeder Zug mit Text und Zeit.
    Verbose,
}

impl Zoom {
    /// Die Stufe zu `1`, `2`, `3`; alles andere bleibt unverändert.
    pub fn from_digit(self, digit: u8) -> Self {
        match digit {
            1 => Zoom::Summary,
            2 => Zoom::Normal,
            3 => Zoom::Verbose,
            _ => self,
        }
    }

    /// Die Ziffer für die Anzeige.
    pub fn digit(self) -> u8 {
        match self {
            Zoom::Summary => 1,
            Zoom::Normal => 2,
            Zoom::Verbose => 3,
        }
    }
}

/// Eine gezeichnete Zeile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Der Knoten dahinter — bei einer Zusammenfassung der erste.
    pub node: usize,
    /// Wie viele Knoten die Zeile zusammenfasst (≥ 1).
    pub count: usize,
    /// Die Art.
    pub kind: NodeKind,
    /// Die Beschriftung.
    pub label: String,
    /// Der Zeitpunkt, falls erfasst.
    pub at: Option<String>,
    /// Die Box-Zeichen vor dem Glyph (Einrückung und Verbinder).
    pub prefix: String,
    /// Die Tiefe im Baum (Wurzel 0).
    pub depth: usize,
}

/// Die Zeilen eines Graphen in Baumform.
pub fn rows(graph: &SessionGraph, zoom: Zoom) -> Vec<Row> {
    let items = visible(graph, zoom);
    let mut out = Vec::with_capacity(items.len());
    // Kinder je (sichtbarem) Elternteil, in Reihenfolge.
    let mut children: BTreeMap<Option<usize>, Vec<usize>> = BTreeMap::new();
    for (i, item) in items.iter().enumerate() {
        children.entry(item.parent).or_default().push(i);
    }
    let roots = children.get(&None).cloned().unwrap_or_default();
    for (n, root) in roots.iter().enumerate() {
        walk(
            &items,
            &children,
            *root,
            "",
            n + 1 == roots.len(),
            true,
            &mut out,
        );
    }
    out
}

/// Die Zeilen eines Graphen als Zeitleiste: flach, nach Zeit sortiert;
/// Zeilen ohne Zeit behalten ihre Reihenfolge am Ende.
pub fn timeline(graph: &SessionGraph, zoom: Zoom) -> Vec<Row> {
    let mut rows: Vec<Row> = rows(graph, zoom)
        .into_iter()
        .map(|mut row| {
            row.prefix.clear();
            row.depth = 0;
            row
        })
        .collect();
    rows.sort_by_key(|row| {
        row.at
            .as_deref()
            .and_then(epoch_seconds)
            .map_or((1, 0), |t| (0, t))
    });
    rows
}

/// Ein sichtbarer Knoten mit seinem sichtbaren Elternteil.
#[derive(Debug, Clone)]
struct Item {
    parent: Option<usize>,
    node: usize,
    count: usize,
    kind: NodeKind,
    label: String,
    at: Option<String>,
}

fn shown(node: &GraphNode, zoom: Zoom) -> bool {
    match (&node.kind, zoom) {
        (NodeKind::Turn(_), Zoom::Verbose) => true,
        (NodeKind::Turn(_), _) => false,
        _ => true,
    }
}

/// Die sichtbaren Knoten, bereits an sichtbare Eltern gehängt und — in der
/// Übersicht — je Datei zusammengefasst.
fn visible(graph: &SessionGraph, zoom: Zoom) -> Vec<Item> {
    // Sichtbarer Vorfahr je Knoten (Index in `items`).
    let mut visible_of: Vec<Option<usize>> = vec![None; graph.nodes.len()];
    let mut items: Vec<Item> = Vec::new();
    // Zusammenfassung: (Eltern-Item, Art, Pfad) → Item-Index.
    let mut merged: BTreeMap<(Option<usize>, String, String), usize> = BTreeMap::new();

    for node in &graph.nodes {
        let parent_item = node.parent.and_then(|p| visible_of[p]);
        if !shown(node, zoom) {
            visible_of[node.id] = parent_item;
            continue;
        }
        if zoom == Zoom::Summary
            && let NodeKind::Tool(kind) = &node.kind
            && let Some(path) = &node.path
        {
            let key = (parent_item, format!("{kind:?}"), path.clone());
            if let Some(&existing) = merged.get(&key) {
                items[existing].count += 1;
                let count = items[existing].count;
                items[existing].label = format!("{path} ×{count}");
                visible_of[node.id] = Some(existing);
                continue;
            }
            merged.insert(key, items.len());
        }
        visible_of[node.id] = Some(items.len());
        items.push(Item {
            parent: parent_item,
            node: node.id,
            count: 1,
            kind: node.kind.clone(),
            label: node.label.clone(),
            at: node.at.clone(),
        });
    }
    items
}

fn walk(
    items: &[Item],
    children: &BTreeMap<Option<usize>, Vec<usize>>,
    item: usize,
    indent: &str,
    last: bool,
    root: bool,
    out: &mut Vec<Row>,
) {
    let it = &items[item];
    let connector = if root {
        ""
    } else if last {
        "┗━ "
    } else {
        "┣━ "
    };
    out.push(Row {
        node: it.node,
        count: it.count,
        kind: it.kind.clone(),
        label: it.label.clone(),
        at: it.at.clone(),
        prefix: format!("{indent}{connector}"),
        depth: if root {
            0
        } else {
            indent.chars().count() / 3 + 1
        },
    });
    let kids = children.get(&Some(item)).cloned().unwrap_or_default();
    let child_indent = if root {
        String::new()
    } else if last {
        format!("{indent}   ")
    } else {
        format!("{indent}┃  ")
    };
    for (n, kid) in kids.iter().enumerate() {
        walk(
            items,
            children,
            *kid,
            &child_indent,
            n + 1 == kids.len(),
            false,
            out,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{
        Agent, Effect, EffectKind, Intent, Model, Role, Session, SessionId, ToolCall, Turn,
    };
    use minds_reader::Index;
    use minds_reader::model::ReviewState;
    use std::collections::BTreeMap;

    fn sid() -> SessionId {
        format!("b3-{}", "a".repeat(64)).parse().unwrap()
    }

    fn call(kind: EffectKind, path: &str) -> ToolCall {
        ToolCall {
            capture: None,
            name: "T".into(),
            arguments: format!("{{\"command\":\"{path}\"}}"),
            effect: Some(Effect {
                kind,
                path: Some(path.into()),
                content: None,
            }),
        }
    }

    fn session() -> Session {
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
                request: "Fix".into(),
                ..Intent::default()
            },
        );
        s.turns.push(Turn {
            role: Role::Assistant,
            text: "lese".into(),
            tool_calls: vec![
                call(EffectKind::Read, "a.rs"),
                call(EffectKind::Write, "a.rs"),
                call(EffectKind::Write, "a.rs"),
            ],
            parent: None,
            at: Some("2026-07-25T09:00:00Z".into()),
        });
        s.turns.push(Turn {
            role: Role::Assistant,
            text: "teste".into(),
            tool_calls: vec![call(EffectKind::Exec, "cargo")],
            parent: None,
            at: Some("2026-07-25T08:00:00Z".into()),
        });
        s
    }

    fn graph() -> SessionGraph {
        let index = Index::from_parts(BTreeMap::new(), BTreeMap::new());
        SessionGraph::of(sid(), &session(), &index, &ReviewState::open())
    }

    fn lines(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| format!("{}{}", r.prefix, r.label))
            .collect()
    }

    #[test]
    fn normal_zoom_hides_turns_and_hangs_tools_off_the_agent() {
        let rows = rows(&graph(), Zoom::Normal);
        assert_eq!(
            lines(&rows),
            vec![
                "Fix",
                "┗━ claude-code · opus",
                "   ┣━ a.rs",
                "   ┣━ a.rs",
                "   ┣━ a.rs",
                "   ┣━ cargo",
                "   ┗━ offen",
            ]
        );
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].depth, 2);
    }

    #[test]
    fn summary_zoom_merges_tools_per_file_and_kind() {
        let rows = rows(&graph(), Zoom::Summary);
        assert_eq!(
            lines(&rows),
            vec![
                "Fix",
                "┗━ claude-code · opus",
                "   ┣━ a.rs",
                "   ┣━ a.rs ×2",
                "   ┣━ cargo",
                "   ┗━ offen",
            ]
        );
        assert_eq!(rows[3].count, 2);
    }

    #[test]
    fn verbose_zoom_shows_turns_as_branches() {
        let rows = rows(&graph(), Zoom::Verbose);
        assert_eq!(
            lines(&rows),
            vec![
                "Fix",
                "┗━ claude-code · opus",
                "   ┣━ ASSISTANT · lese",
                "   ┃  ┣━ a.rs",
                "   ┃  ┣━ a.rs",
                "   ┃  ┗━ a.rs",
                "   ┣━ ASSISTANT · teste",
                "   ┃  ┗━ cargo",
                "   ┗━ offen",
            ]
        );
    }

    #[test]
    fn the_timeline_is_flat_and_sorted_by_time_with_timeless_last() {
        let rows = timeline(&graph(), Zoom::Verbose);
        assert!(rows.iter().all(|r| r.prefix.is_empty() && r.depth == 0));
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        // „teste" (08:00) vor „lese" (09:00); Zeitlose (Agent, Review) hinten.
        let teste = labels.iter().position(|l| l.contains("teste")).unwrap();
        let lese = labels.iter().position(|l| l.contains("lese")).unwrap();
        assert!(teste < lese);
        assert_eq!(labels.last(), Some(&"offen"));
    }

    #[test]
    fn zoom_digits_map_and_unknown_digits_keep_the_level() {
        assert_eq!(Zoom::Normal.from_digit(1), Zoom::Summary);
        assert_eq!(Zoom::Normal.from_digit(3), Zoom::Verbose);
        assert_eq!(Zoom::Verbose.from_digit(9), Zoom::Verbose);
        assert_eq!(Zoom::Summary.digit(), 1);
    }
}
