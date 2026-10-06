//! Rate-Limit je Client statt global (EA-10).
//!
//! Bis EA-10 galt ein Mindestabstand zwischen **allen** Checkpoint-Läufen:
//! Ein Agent, der einmal je Sekunde anfragt, ließ die Anfrage aus dem
//! post-commit-Hook des Menschen `rate limited` sehen. Den Absender verraten
//! die Socket-Credentials nicht — Agent und Mensch sind im `container`- wie
//! im `user`-Profil derselbe Nutzer, und jedes `minds checkpoint` ist ein
//! neuer Prozess. Was eine Anfrage tatsächlich unterscheidet, ist ihr
//! **Commit**: Der Client eines Checkpoints ist der Commit, den er
//! versiegeln lassen will.
//!
//! - Je Commit höchstens ein Lauf je Intervall, gemessen vom Ende des
//!   letzten Laufs für ihn — gezählt wird jeder Lauf, auch ein
//!   gescheiterter.
//! - Der Commit, an den ein Lauf die Trailer nachgerüstet hat, gilt als
//!   **derselbe** Client: Sonst ließe sich die Grenze umgehen, indem man nach
//!   jedem Lauf den frisch nachgerüsteten HEAD anfragt.
//! - Eine Anfrage für einen anderen Commit — der frische Commit des
//!   Menschen — wird von fremden **erfolgreichen** Läufen nicht aufgehalten.
//! - Ein **gescheiterter** Lauf sperrt dagegen alle Clients für das
//!   Intervall. Der Client-Schlüssel ist frei wählbar: Wer für erfundene
//!   Commits anfragt, bekäme sonst je einen Lauf ohne jede Grenze — jeder
//!   ein Worker-Prozess und eine Epochen-Grenze im Journal, auch wenn er an
//!   der HEAD-Prüfung scheitert, bevor etwas versiegelt wird. So bleibt es
//!   bei höchstens einem gescheiterten Lauf je Intervall, wie vor EA-10.
//!
//! Gemerkt werden die letzten [`MAX_CLIENTS`] Clients; was herausfällt, darf
//! wieder sofort. Den Socket kann die Agent-Seite ohnehin entfernen;
//! Verfügbarkeit gegen den Agenten verspricht das Profil nicht (EA-S2).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// So viele Clients merkt sich der Witness.
const MAX_CLIENTS: usize = 64;

/// So viele Commits (angefragt plus nachgerüstete) je Client.
const MAX_ALIASES: usize = 8;

struct Client {
    commits: VecDeque<String>,
    ended: Instant,
}

#[derive(Default)]
pub(super) struct Limits {
    clients: VecDeque<Client>,
    /// Ende des letzten gescheiterten Laufs, gleich für welchen Client.
    failed: Option<Instant>,
}

impl Limits {
    fn position(&self, commit: &str) -> Option<usize> {
        let commit = commit.to_ascii_lowercase();
        self.clients
            .iter()
            .position(|client| client.commits.contains(&commit))
    }

    /// Ob für `commit` (oder einen seiner Nachfolger) ein Lauf vor weniger als
    /// `interval` endete — oder irgendein Lauf vor so kurzer Zeit scheiterte.
    pub(super) fn blocked(&self, commit: &str, interval: Duration) -> bool {
        self.failed.is_some_and(|at| at.elapsed() < interval)
            || self
                .position(commit)
                .is_some_and(|at| self.clients[at].ended.elapsed() < interval)
    }

    /// Hält einen gescheiterten Lauf für `commit` fest.
    pub(super) fn record_failure(&mut self, commit: &str) {
        self.record(commit, None);
        self.failed = Some(Instant::now());
    }

    /// Hält das Ende eines Laufs für `commit` fest, samt des Commits, an den
    /// er die Trailer nachgerüstet hat.
    pub(super) fn record(&mut self, commit: &str, retrofitted: Option<&str>) {
        let mut client = match self.position(commit) {
            Some(at) => self.clients.remove(at).expect("Position aus der Liste"),
            None => Client {
                commits: VecDeque::from([commit.to_ascii_lowercase()]),
                ended: Instant::now(),
            },
        };
        client.ended = Instant::now();
        if let Some(next) = retrofitted.map(str::to_ascii_lowercase) {
            if !client.commits.contains(&next) {
                client.commits.push_back(next);
            }
            // Der älteste Commit fällt zuerst heraus — der jüngste Nachfolger
            // ist der, den eine Flut als Nächstes anfragt.
            while client.commits.len() > MAX_ALIASES {
                client.commits.pop_front();
            }
        }
        self.clients.push_back(client);
        while self.clients.len() > MAX_CLIENTS {
            self.clients.pop_front();
        }
    }

    #[cfg(test)]
    pub(super) fn backdate(&mut self, by: Duration) {
        for client in &mut self.clients {
            client.ended -= by;
        }
        if let Some(failed) = &mut self.failed {
            *failed -= by;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn a_run_for_one_commit_does_not_block_another() {
        let mut limits = Limits::default();
        limits.record("aaaa", None);
        assert!(limits.blocked("aaaa", SECOND));
        assert!(limits.blocked("AAAA", SECOND), "hex is case-insensitive");
        assert!(!limits.blocked("bbbb", SECOND));
        limits.backdate(SECOND);
        assert!(!limits.blocked("aaaa", SECOND));
    }

    /// Erfundene Commits umgehen die Grenze nicht: Ein gescheiterter Lauf
    /// sperrt jeden Client für das Intervall.
    #[test]
    fn a_failed_run_blocks_every_client() {
        let mut limits = Limits::default();
        limits.record_failure("made-up-1");
        for commit in ["made-up-2", "made-up-3", "human-head"] {
            assert!(limits.blocked(commit, SECOND), "{commit}");
        }
        limits.backdate(SECOND);
        assert!(!limits.blocked("human-head", SECOND));
    }

    #[test]
    fn the_retrofitted_commit_is_the_same_client() {
        let mut limits = Limits::default();
        limits.record("aaaa", Some("bbbb"));
        assert!(limits.blocked("bbbb", SECOND));
        limits.backdate(SECOND);
        limits.record("bbbb", Some("cccc"));
        assert!(limits.blocked("aaaa", SECOND));
        assert!(limits.blocked("cccc", SECOND));
    }

    #[test]
    fn memory_is_bounded() {
        let mut limits = Limits::default();
        limits.record("first", None);
        for i in 0..MAX_CLIENTS {
            limits.record(&format!("c{i}"), None);
        }
        assert_eq!(limits.clients.len(), MAX_CLIENTS);
        assert!(!limits.blocked("first", SECOND));
        let mut chain = "x0".to_owned();
        for i in 1..=MAX_ALIASES * 2 {
            let next = format!("x{i}");
            limits.record(&chain, Some(&next));
            chain = next;
        }
        let client = &limits.clients[limits.position(&chain).unwrap()];
        assert!(client.commits.len() <= MAX_ALIASES);
    }
}
