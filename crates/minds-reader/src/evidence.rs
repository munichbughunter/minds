//! Warum eine Kante Commit ↔ Session im Index steht — erklärt, nicht nur
//! behauptet.
//!
//! Der Store-Index speichert je Kante nur die Evidenz-Klasse, nicht die
//! Gründe. Für eine **vermutete** Kante rechnet dieses Modul die Heuristik
//! aus `minds-capture::match_commits` nach: Datei-Schnittmenge und
//! Zeitfenster. Die Konstanten sind von dort übernommen, nicht importiert —
//! der Reader hängt nicht am Schreibpfad (Crate-Grenze), und wandert die
//! Heuristik weiter, muss die Erklärung hier nachziehen.
//!
//! Alles hier ist rein: Wer die Dateien des Commits und seine Zeit schon
//! hat, braucht kein Repository.

use minds_core::{Evidence, Session};
use minds_git::CommitId;
use minds_metrics::epoch_seconds;

use crate::model::EvidenceExplanation;
use crate::text::sanitize_path;

/// Karenz **vor** dem Session-Start (Sekunden). Quelle:
/// `minds-capture::match_commits::GRACE_BEFORE`.
pub const GRACE_BEFORE: i64 = 300;

/// Karenz **nach** dem Session-Ende (Sekunden). Quelle:
/// `minds-capture::match_commits::GRACE_AFTER`.
pub const GRACE_AFTER: i64 = 3_600;

/// Erklärt eine Kante.
///
/// `commit_files` und `commit_time` sind `None`, wenn sie sich nicht lesen
/// ließen — dann wird die Vermutung ehrlich als nicht rekonstruierbar
/// geführt statt mit leeren Listen als „kein Grund" vorgetäuscht.
pub fn explain(
    evidence: Evidence,
    commit: CommitId,
    session: &Session,
    commit_files: Option<&[String]>,
    commit_time: Option<i64>,
) -> EvidenceExplanation {
    match evidence {
        Evidence::Observed => EvidenceExplanation::Trailer { commit },
        Evidence::Declared => EvidenceExplanation::Declared,
        Evidence::Content => EvidenceExplanation::Content,
        Evidence::Inferred => {
            let Some(files) = commit_files else {
                return EvidenceExplanation::Unknown {
                    reason: "Commit-Diff nicht lesbar".into(),
                };
            };
            let shared_files: Vec<String> = session
                .produced
                .files
                .iter()
                .filter(|path| files.iter().any(|f| f == *path))
                .map(|path| sanitize_path(path))
                .collect();
            let window = session_window(session);
            let seconds_apart = match (commit_time, window) {
                (Some(at), Some((_, end))) => Some(at - end),
                _ => None,
            };
            let in_window = match (commit_time, window) {
                (Some(at), Some((start, end))) => {
                    Some((start - GRACE_BEFORE) <= at && at <= (end + GRACE_AFTER))
                }
                _ => None,
            };
            EvidenceExplanation::Heuristic {
                shared_files,
                seconds_apart,
                in_window,
            }
        }
    }
}

/// Das Zeitfenster der Session in Epoch-Sekunden: vom Start (Herkunft, sonst
/// erster Zug) bis zum Ende (Herkunft, sonst letzter Zug). `None`, wenn
/// keine Zeit lesbar ist.
pub fn session_window(session: &Session) -> Option<(i64, i64)> {
    let lineage = session.lineage.as_ref();
    let turns: Vec<i64> = session
        .turns
        .iter()
        .filter_map(|turn| turn.at.as_deref())
        .filter_map(epoch_seconds)
        .collect();
    let start = lineage
        .and_then(|l| l.started_at.as_deref())
        .and_then(epoch_seconds)
        .or_else(|| turns.iter().copied().min())?;
    let end = lineage
        .and_then(|l| l.ended_at.as_deref())
        .and_then(epoch_seconds)
        .or_else(|| turns.iter().copied().max())
        .unwrap_or(start);
    Some((start, end.max(start)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{Agent, Intent, Lineage, Model, Produced};

    fn commit() -> CommitId {
        "1".repeat(40).parse().unwrap()
    }

    fn session(files: &[&str], start: Option<&str>, end: Option<&str>) -> Session {
        let mut session = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "opus".into(),
            },
            Intent {
                request: "x".into(),
                ..Intent::default()
            },
        );
        session.produced = Produced {
            commit_hint: None,
            files: files.iter().map(|f| f.to_string()).collect(),
        };
        if start.is_some() || end.is_some() {
            let mut lineage = Lineage::new("local");
            lineage.started_at = start.map(str::to_string);
            lineage.ended_at = end.map(str::to_string);
            session.lineage = Some(lineage);
        }
        session
    }

    #[test]
    fn an_observed_edge_points_at_its_trailer_commit() {
        let s = session(&[], None, None);
        assert_eq!(
            explain(Evidence::Observed, commit(), &s, None, None),
            EvidenceExplanation::Trailer { commit: commit() }
        );
        assert_eq!(
            explain(Evidence::Declared, commit(), &s, None, None),
            EvidenceExplanation::Declared
        );
        assert_eq!(
            explain(Evidence::Content, commit(), &s, None, None),
            EvidenceExplanation::Content
        );
    }

    #[test]
    fn an_inferred_edge_without_a_diff_is_honestly_unknown() {
        let s = session(&["a.rs"], None, None);
        assert!(matches!(
            explain(Evidence::Inferred, commit(), &s, None, None),
            EvidenceExplanation::Unknown { .. }
        ));
    }

    #[test]
    fn the_heuristic_is_recomputed_from_files_and_window() {
        let s = session(
            &["src/a.rs", "src/b.rs", "c\u{1b}[2K.rs"],
            Some("2026-07-25T09:00:00Z"),
            Some("2026-07-25T10:00:00Z"),
        );
        let files = vec!["src/a.rs".to_string(), "c\u{1b}[2K.rs".to_string()];
        let end = epoch_seconds("2026-07-25T10:00:00Z").unwrap();
        let got = explain(
            Evidence::Inferred,
            commit(),
            &s,
            Some(&files),
            Some(end + 412),
        );
        assert_eq!(
            got,
            EvidenceExplanation::Heuristic {
                shared_files: vec!["src/a.rs".into(), "c\\u{1b}[2K.rs".into()],
                seconds_apart: Some(412),
                in_window: Some(true),
            }
        );
    }

    #[test]
    fn the_window_edges_match_the_capture_heuristic() {
        let s = session(
            &["a"],
            Some("2026-07-25T09:00:00Z"),
            Some("2026-07-25T10:00:00Z"),
        );
        let (start, end) = session_window(&s).unwrap();
        let files = vec!["a".to_string()];
        let at = |t: i64| match explain(Evidence::Inferred, commit(), &s, Some(&files), Some(t)) {
            EvidenceExplanation::Heuristic { in_window, .. } => in_window,
            other => panic!("{other:?}"),
        };
        assert_eq!(at(end + GRACE_AFTER), Some(true));
        assert_eq!(at(end + GRACE_AFTER + 1), Some(false));
        assert_eq!(at(start - GRACE_BEFORE), Some(true));
        assert_eq!(at(start - GRACE_BEFORE - 1), Some(false));
    }

    #[test]
    fn missing_times_leave_the_window_open() {
        let s = session(&["a"], None, None);
        assert_eq!(session_window(&s), None);
        let files = vec!["a".to_string()];
        assert_eq!(
            explain(Evidence::Inferred, commit(), &s, Some(&files), Some(5)),
            EvidenceExplanation::Heuristic {
                shared_files: vec!["a".into()],
                seconds_apart: None,
                in_window: None,
            }
        );
    }

    #[test]
    fn the_window_falls_back_to_turn_times() {
        let mut s = session(&[], None, None);
        s.turns.push(minds_core::Turn {
            role: minds_core::Role::User,
            text: String::new(),
            tool_calls: Vec::new(),
            parent: None,
            at: Some("2026-07-25T09:00:00Z".into()),
        });
        s.turns.push(minds_core::Turn {
            role: minds_core::Role::Assistant,
            text: String::new(),
            tool_calls: Vec::new(),
            parent: None,
            at: Some("2026-07-25T09:30:00Z".into()),
        });
        let (start, end) = session_window(&s).unwrap();
        assert_eq!(end - start, 1_800);
    }
}
