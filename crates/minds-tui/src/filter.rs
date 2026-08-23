//! Die Suche über Karten — UI-frei.
//!
//! Leerzeichengetrennte Terme, UND-verknüpft, Groß-/Kleinschreibung egal.
//! Gesucht wird in Überschrift, vollständigem Prompt, Akteur, berührten
//! Dateien, Change-Ids und Commit-Hashes (Präfix genügt). Dieselbe Logik
//! trägt Bildschirm und Pipe, damit `minds inspect retry | grep` genau das
//! liefert, was der Bildschirm zeigt.

use minds_reader::Index;
use minds_reader::model::SessionCard;

/// Zerlegt eine Eingabe in Terme.
pub fn terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(|t| t.to_lowercase()).collect()
}

/// `true`, wenn jede Term-Zeile irgendwo in der Karte vorkommt.
pub fn matches(card: &SessionCard, index: &Index, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let haystack = haystack(card, index);
    terms
        .iter()
        .all(|term| haystack.iter().any(|h| h.contains(term.as_str())))
}

fn haystack(card: &SessionCard, index: &Index) -> Vec<String> {
    let mut out = vec![
        card.summary.headline.to_lowercase(),
        card.summary.actor.to_lowercase(),
        card.id.to_string(),
    ];
    out.extend(card.changes.iter().map(|c| c.to_string().to_lowercase()));
    out.extend(card.commits.iter().map(|c| c.to_string()));
    if let Some(session) = index.session(card.id) {
        out.push(session.intent.request.to_lowercase());
        out.extend(session.produced.files.iter().map(|f| f.to_lowercase()));
        out.extend(session.turns.iter().flat_map(|turn| {
            turn.tool_calls.iter().filter_map(|call| {
                call.effect
                    .as_ref()
                    .and_then(|e| e.path.as_deref())
                    .map(|p| p.to_lowercase())
            })
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{Agent, Intent, Model, Session, SessionId};
    use minds_reader::Inspection;
    use std::collections::BTreeMap;

    fn sid(c: char) -> SessionId {
        format!("b3-{}", c.to_string().repeat(64)).parse().unwrap()
    }

    fn sample() -> Inspection {
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
                request: "Retry-Logik für 429 absichern\nZweite Zeile mit Backoff".into(),
                ..Intent::default()
            },
        );
        s.produced.files.push("src/http/retry.rs".into());
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('a'), s);
        Inspection::from_index(
            Index::from_parts(sessions, BTreeMap::new()),
            Vec::new(),
            "repo",
        )
    }

    #[test]
    fn terms_are_anded_and_case_insensitive() {
        let insp = sample();
        let card = insp.card(sid('a')).unwrap();
        assert!(matches(&card, insp.index(), &terms("RETRY 429")));
        assert!(!matches(&card, insp.index(), &terms("retry timeout")));
        assert!(matches(&card, insp.index(), &terms("")));
    }

    #[test]
    fn paths_the_full_prompt_and_the_actor_are_searchable() {
        let insp = sample();
        let card = insp.card(sid('a')).unwrap();
        assert!(matches(&card, insp.index(), &terms("http/retry.rs")));
        assert!(matches(&card, insp.index(), &terms("backoff")));
        assert!(matches(&card, insp.index(), &terms("opus")));
        assert!(matches(&card, insp.index(), &terms("b3-aaaa")));
    }
}
