//! Die Vermutung, welcher Commit zu welcher importierten Session gehört.
//!
//! Eine Hook-erfasste Session weiß ihren Commit (der post-commit-Hook reicht ihn
//! herein). Eine **importierte** Session weiß ihn nicht — das Transkript kennt
//! keinen Git-Hash. Dieses Modul rät ihn, und zwar aus zwei Signalen, die beide
//! vorliegen:
//!
//! - **Dateien.** Was die Session geschrieben hat (`produced.files`), geschnitten
//!   mit dem, was ein Commit geändert hat. Kein gemeinsamer Pfad, keine Kante.
//! - **Zeit.** Der Commit muss im Zeitfenster der Session liegen (plus etwas
//!   Karenz davor und danach) — sonst würde eine Session an *jeden* Commit
//!   geheftet, der ihre Datei je berührt hat, quer durch die Historie.
//!
//! # Warum das eine Vermutung bleibt
//!
//! Beides zusammen ist ein starkes Indiz, aber kein Beweis: Zwei Sessions am
//! selben Nachmittag an derselben Datei sind nicht sicher auseinanderzuhalten.
//! Deshalb trägt die Kante im Store-Index [`Evidence::Inferred`](minds_core::Evidence::Inferred)
//! und nicht `Observed`, und der Reader zeigt sie grau. Ehrlich vermutet schlägt
//! falsch behauptet.
//!
//! # Rein, damit prüfbar
//!
//! Dieses Modul liest kein Git. Es bekommt die Fingerabdrücke von Commits und
//! Sessions herein und gibt Kanten heraus — eine Funktion über einfache Daten.
//! Woher die Commit-Daten kommen (`git log`, `git show`), entscheidet der
//! Aufrufer.

use minds_core::{Session, SessionId};

use crate::clock::epoch_seconds_from_rfc3339;

/// Karenz **vor** dem ersten Transkript-Zeitpunkt (Sekunden). Klein: ein Commit
/// entsteht praktisch nie, bevor die Arbeit begann.
pub const GRACE_BEFORE: i64 = 300;

/// Karenz **nach** dem letzten Transkript-Zeitpunkt (Sekunden). Großzügiger: der
/// Commit kommt oft kurz nach dem letzten Modell-Zug, manchmal von Hand etwas
/// später.
pub const GRACE_AFTER: i64 = 3_600;

/// Der Fingerabdruck eines Commits fürs Matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// Voller Commit-Hash in Hex.
    pub hex: String,
    /// Autor-Zeit in Sekunden seit Epoch (Git liefert sie mit `%at` direkt so).
    pub epoch: i64,
    /// Die vom Commit geänderten Dateien, repo-relativ.
    pub files: Vec<String>,
}

/// Der Fingerabdruck einer Session fürs Matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// Die Session-Id.
    pub id: SessionId,
    /// Die geschriebenen Dateien, repo-relativ (`produced.files`).
    pub files: Vec<String>,
    /// Beginn in Sekunden seit Epoch, falls das Transkript einen Zeitpunkt gab.
    pub start: Option<i64>,
    /// Ende, dito.
    pub end: Option<i64>,
}

impl SessionInfo {
    /// Zieht den Fingerabdruck aus einer Session: `produced.files` und das
    /// Zeitfenster aus `lineage.started_at`/`ended_at`.
    pub fn of(id: SessionId, session: &Session) -> Self {
        let (start, end) = session
            .lineage
            .as_ref()
            .map(|l| {
                (
                    l.started_at.as_deref().and_then(epoch_seconds_from_rfc3339),
                    l.ended_at.as_deref().and_then(epoch_seconds_from_rfc3339),
                )
            })
            .unwrap_or((None, None));

        Self {
            id,
            files: session.produced.files.clone(),
            start,
            end,
        }
    }
}

/// Eine gefundene Zuordnung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Der Commit-Hash in Hex.
    pub commit: String,
    /// Die zugeordnete Session.
    pub session: SessionId,
}

/// Ordnet Sessions Commits zu — jede Kante ein starkes Indiz, keine Gewissheit.
pub fn match_sessions(sessions: &[SessionInfo], commits: &[CommitInfo]) -> Vec<Link> {
    let mut links = Vec::new();
    for session in sessions {
        for commit in commits {
            if fits(session, commit) {
                links.push(Link {
                    commit: commit.hex.clone(),
                    session: session.id,
                });
            }
        }
    }
    links
}

/// Passt diese Session zu diesem Commit?
fn fits(session: &SessionInfo, commit: &CommitInfo) -> bool {
    shares_a_file(&session.files, &commit.files) && in_window(session, commit.epoch)
}

/// Teilen sich beide mindestens eine Datei?
fn shares_a_file(a: &[String], b: &[String]) -> bool {
    a.iter().any(|f| b.contains(f))
}

/// Liegt `epoch` im (großzügigen) Zeitfenster der Session?
///
/// Ohne Zeitangaben gibt es kein Fenster — dann **kein** Match, statt eine
/// Datei quer durch die Historie zu heften.
fn in_window(session: &SessionInfo, epoch: i64) -> bool {
    let (Some(start), Some(end)) = (session.start, session.end) else {
        return false;
    };
    (start - GRACE_BEFORE) <= epoch && epoch <= (end + GRACE_AFTER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(hex: char) -> SessionId {
        format!("b3-{}", hex.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    fn session(hex: char, files: &[&str], start: i64, end: i64) -> SessionInfo {
        SessionInfo {
            id: sid(hex),
            files: files.iter().map(|s| s.to_string()).collect(),
            start: Some(start),
            end: Some(end),
        }
    }

    fn commit(hex: &str, epoch: i64, files: &[&str]) -> CommitInfo {
        CommitInfo {
            hex: hex.to_string(),
            epoch,
            files: files.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_shared_file_in_the_window_links() {
        let s = session('a', &["src/retry.rs"], 1000, 2000);
        let c = commit("dead", 2100, &["src/retry.rs", "README.md"]);
        let links = match_sessions(&[s], &[c]);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].commit, "dead");
        assert_eq!(links[0].session, sid('a'));
    }

    #[test]
    fn no_shared_file_no_link() {
        let s = session('a', &["src/retry.rs"], 1000, 2000);
        let c = commit("dead", 2100, &["docs/other.md"]);
        assert!(match_sessions(&[s], &[c]).is_empty());
    }

    #[test]
    fn a_commit_outside_the_window_does_not_link() {
        let s = session('a', &["src/retry.rs"], 1000, 2000);
        // Weit nach Ende + Karenz.
        let late = commit("late", 2000 + GRACE_AFTER + 1, &["src/retry.rs"]);
        // Weit vor Beginn - Karenz.
        let early = commit("early", 1000 - GRACE_BEFORE - 1, &["src/retry.rs"]);
        assert!(match_sessions(std::slice::from_ref(&s), &[late]).is_empty());
        assert!(match_sessions(&[s], &[early]).is_empty());
    }

    #[test]
    fn a_commit_just_after_the_end_still_links_within_grace() {
        let s = session('a', &["src/retry.rs"], 1000, 2000);
        let c = commit("ok", 2000 + GRACE_AFTER - 1, &["src/retry.rs"]);
        assert_eq!(match_sessions(&[s], &[c]).len(), 1);
    }

    #[test]
    fn a_session_without_times_matches_nothing() {
        // Sonst würde ihre Datei an jeden Commit der Historie geheftet.
        let s = SessionInfo {
            id: sid('a'),
            files: vec!["src/retry.rs".into()],
            start: None,
            end: None,
        };
        let c = commit("dead", 2100, &["src/retry.rs"]);
        assert!(match_sessions(&[s], &[c]).is_empty());
    }

    #[test]
    fn a_session_can_link_to_several_commits_it_produced() {
        let s = session('a', &["src/retry.rs"], 1000, 5000);
        let c1 = commit("c1", 2000, &["src/retry.rs"]);
        let c2 = commit("c2", 4000, &["src/retry.rs", "x.rs"]);
        let links = match_sessions(&[s], &[c1, c2]);
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn session_info_reads_files_and_window_from_a_session() {
        use minds_core::{Agent, Intent, Lineage, Model, Produced};
        let mut s = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent::default(),
        );
        s.produced = Produced {
            commit_hint: None,
            files: vec!["src/retry.rs".into()],
        };
        s.lineage = Some(Lineage {
            local_id: "s".into(),
            started_at: Some("1970-01-01T00:00:00Z".into()),
            ended_at: Some("1970-01-01T01:00:00Z".into()),
            cwd: None,
        });

        let info = SessionInfo::of(sid('a'), &s);
        assert_eq!(info.files, vec!["src/retry.rs"]);
        assert_eq!(info.start, Some(0));
        assert_eq!(info.end, Some(3600));
    }
}
