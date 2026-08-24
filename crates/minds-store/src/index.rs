//! Der Commit-Index: die Verknüpfung Commit → Session, die *nicht* im Commit
//! steht.
//!
//! Der verbindliche Verweis Commit → Session ist der Trailer in der
//! Commit-Message; er überlebt Rebase und ist [`Observed`](minds_core::Evidence::Observed).
//! Aber ein Trailer lässt sich nur an **HEAD** sicher nachrüsten (siehe
//! `minds-cli::checkpoint`). Für alles Ältere — vor allem für **importierte**
//! Sessions (ADR-0004) — bräuchte man ein History-Rewrite, das jeden Klon
//! bricht. Das ist ausgeschlossen.
//!
//! Dieser Index ist der Ausweg: eine Zuordnung als **Daten** neben den Sessions
//! (`index.json`), die keine History anfasst. `minds show`/`why`/`render`/`fsck`
//! lesen Trailer **und** Index; der Reader zeigt die eine Sorte als beobachtet,
//! die andere als vermutet. Und weil der Index in `refs/minds/context` liegt,
//! reist er beim Push des Refs mit — der Kontext erreicht so ein geteiltes Repo.
//!
//! # Warum jede Kante ihre Herkunft trägt
//!
//! Dieselbe Leitregel wie bei den [`Edge`](minds_core)-Kanten: Ein
//! ungekennzeichneter Pfeil wäre eine Behauptung, die wir nicht decken. Eine
//! importierte Zuordnung ist heuristisch (die geschriebenen Dateien der Session,
//! geschnitten mit denen eines Commits im Zeitfenster) — also
//! [`Heuristic`](minds_core::EvidenceSource::Heuristic). Ein Reader darf „vermutet" von
//! „beobachtet" unterscheiden; ohne das Feld müsste er beides gleich behandeln
//! und wäre im Zweifel unehrlich.

use std::collections::BTreeMap;

use minds_core::{EvidenceMark, SessionId};
use serde::{Deserialize, Serialize};

/// Eine Zuordnung Commit → Sessions, jede Kante mit ihrer Herkunft.
///
/// Serialisiert als schlichtes Objekt `{ "<commit-hex>": [ … ] }`, damit ein
/// Fremdleser (Auditor, ein Skript) es ohne Kenntnis des Wrappers versteht.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommitIndex {
    commits: BTreeMap<String, Vec<IndexLink>>,
}

/// Eine einzelne Kante: welche Session, und woher wir das wissen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexLink {
    /// Die verknüpfte Session.
    pub session: SessionId,
    /// Woher die Verknüpfung stammt — und ob sie geprüft wurde. Für Importe
    /// `(Heuristic, Unknown)`. Liest tolerant auch die Legacy-Stringform aus
    /// Bestands-Indizes (siehe [`EvidenceMark`]).
    pub evidence: EvidenceMark,
}

impl CommitIndex {
    /// Ein leerer Index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Trägt eine Kante ein — idempotent: dieselbe Session am selben Commit gibt
    /// es nur einmal. Kommt sie mit stärkerem Beleg erneut, gewinnt der
    /// stärkere ([`EvidenceMark::merge`], ADR-0011: erst die Quelle, dann der
    /// Status).
    pub fn link(
        &mut self,
        commit_hex: impl Into<String>,
        session: SessionId,
        evidence: EvidenceMark,
    ) {
        let links = self.commits.entry(commit_hex.into()).or_default();
        if let Some(existing) = links.iter_mut().find(|l| l.session == session) {
            existing.evidence = existing.evidence.merge(evidence);
        } else {
            links.push(IndexLink { session, evidence });
        }
    }

    /// Die Kanten eines Commits — leer, wenn keiner eingetragen ist.
    pub fn links_of(&self, commit_hex: &str) -> &[IndexLink] {
        self.commits
            .get(commit_hex)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Wie viele Commits eine Kante tragen.
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    /// `true`, wenn keine Kante eingetragen ist.
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    /// Alle Commits mit ihren Kanten.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &[IndexLink])> {
        self.commits.iter().map(|(c, links)| (c, links.as_slice()))
    }
}

#[cfg(test)]
mod tests {
    use minds_core::{EvidenceSource, EvidenceStatus};

    use super::*;

    fn observed() -> EvidenceMark {
        EvidenceMark::of(EvidenceSource::Observed)
    }

    fn inferred() -> EvidenceMark {
        EvidenceMark::of(EvidenceSource::Heuristic)
    }

    fn sid(hex: char) -> SessionId {
        format!("b3-{}", hex.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    #[test]
    fn a_link_is_recorded_and_found() {
        let mut index = CommitIndex::new();
        index.link("deadbeef", sid('a'), inferred());
        assert_eq!(index.links_of("deadbeef").len(), 1);
        assert_eq!(index.links_of("deadbeef")[0].session, sid('a'));
        assert_eq!(index.links_of("deadbeef")[0].evidence, inferred());
    }

    #[test]
    fn an_unknown_commit_has_no_links() {
        assert!(CommitIndex::new().links_of("cafe").is_empty());
    }

    #[test]
    fn the_same_session_is_not_duplicated() {
        let mut index = CommitIndex::new();
        index.link("c", sid('a'), inferred());
        index.link("c", sid('a'), inferred());
        assert_eq!(index.links_of("c").len(), 1);
    }

    #[test]
    fn a_stronger_evidence_wins() {
        let mut index = CommitIndex::new();
        index.link("c", sid('a'), inferred());
        index.link("c", sid('a'), observed());
        assert_eq!(index.links_of("c")[0].evidence, observed());
        // Und nicht zurück:
        index.link("c", sid('a'), inferred());
        assert_eq!(index.links_of("c")[0].evidence, observed());
    }

    #[test]
    fn several_sessions_at_one_commit() {
        let mut index = CommitIndex::new();
        index.link("c", sid('a'), inferred());
        index.link("c", sid('b'), inferred());
        assert_eq!(index.links_of("c").len(), 2);
    }

    #[test]
    fn serializes_as_a_plain_commit_map() {
        let mut index = CommitIndex::new();
        index.link("deadbeef", sid('a'), inferred());
        let json = serde_json::to_string(&index).unwrap();
        assert!(
            json.starts_with(r#"{"deadbeef":[{"session":"b3-"#),
            "{json}"
        );
        // Ab Schema 2 die Objektform — golden, weil Fremdleser sie parsen.
        assert!(
            json.contains(r#""evidence":{"source":"heuristic","status":"unknown"}"#),
            "{json}"
        );

        let back: CommitIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(back, index);
    }

    #[test]
    fn a_legacy_index_with_string_evidence_still_reads() {
        // So sehen Bestands-Indizes (Schema 1) aus: `tolerant lesen` heißt,
        // dass der Legacy-String weiter verstanden wird — auf Status Unknown.
        let json = format!(
            r#"{{"deadbeef":[{{"session":"b3-{}","evidence":"observed"}}]}}"#,
            "a".repeat(64)
        );
        let index: CommitIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(index.links_of("deadbeef")[0].evidence, observed());
        assert_eq!(
            index.links_of("deadbeef")[0].evidence.status,
            EvidenceStatus::Unknown
        );
    }
}
