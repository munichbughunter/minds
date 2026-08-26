//! `minds-metrics` — Kennzahlen aus Sessions, deterministisch und ohne I/O.
//!
//! Die eine Definition, zwei Oberflächen: dieselbe [`Metrics`]-Ableitung speist
//! später `minds metrics` (Prometheus/JSON, M.2) **und** die KPI-Kacheln im
//! Reader (Track U). Damit „Throughput" an beiden Orten dasselbe bedeutet, steht
//! die Rechnung genau hier — einmal.
//!
//! # Was hier entsteht
//!
//! - **Roh-Aggregate** (Zähler): Sessions, Token, Tool-Calls (auch nach Effekt),
//!   Redaction-Treffer, distinkte Dateien, Aufschlüsselung je Agent.
//! - **Die vier KPI-Kacheln** wie in entires Overview: [`Metrics::throughput`]
//!   (Token/Session), [`Metrics::iteration`] (Tool-Calls/Session),
//!   [`Metrics::continuity_seconds`] (längste Session) und der
//!   [`Metrics::streak_days`] (aufeinanderfolgende aktive Tage).
//!
//! # Was hier bewusst **nicht** entsteht
//!
//! Kennzahlen, die Git brauchen — die zeilengenaue **Agent-vs-Human-Quote** und
//! die **Kontext-Abdeckung** (Anteil agent-authored Commits mit auflösbarem
//! Trailer) — hängen an Blame bzw. `fsck` und damit an I/O. Sie gehören in die
//! CLI-Schicht (M.2), die ein Repo hat, nicht in diese reine Crate.

mod render;
mod time;
pub use render::{openmetrics, prometheus};
pub use time::{day_number, epoch_seconds};

use std::collections::{BTreeMap, BTreeSet};

use minds_core::{EffectKind, Session};
use serde::Serialize;

/// Repo-abgeleitete Kontext-Abdeckung: wie viele Commits erfassten, auflösbaren
/// Kontext tragen.
///
/// **Von außen berechnet.** Diese Crate rührt kein Git an (siehe Modul-Doku); die
/// CLI walkt die Historie, prüft je Commit die Trailer gegen den Store und reicht
/// die beiden Zahlen hier herein — gehalten und gerendert wird sie hier, damit
/// die Prometheus-Ausgabe an einer Stelle entsteht.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Coverage {
    /// Erreichbare Commits insgesamt.
    pub commits_total: u64,
    /// Davon mit ≥1 auflösbarem `Minds-Session-Id`-Trailer.
    pub commits_with_context: u64,
}

impl Coverage {
    /// Anteil abgedeckter Commits, `0.0` bei leerer Historie.
    pub fn ratio(&self) -> f64 {
        if self.commits_total == 0 {
            0.0
        } else {
            self.commits_with_context as f64 / self.commits_total as f64
        }
    }
}

/// Tool-Calls, aufgeschlüsselt nach ihrem normalisierten Effekt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EffectCounts {
    pub read: u64,
    pub write: u64,
    pub delete: u64,
    pub exec: u64,
    pub other: u64,
}

/// Sessions und Token eines einzelnen Agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentCount {
    pub agent: String,
    pub sessions: u64,
    pub tokens: u64,
}

/// Die aus einer Session-Menge abgeleiteten Kennzahlen.
///
/// Alle Felder sind entweder ganzzahlige Zähler oder abgeleitete Mittel/Maxima.
/// Runden und Formatieren ist Sache der Oberfläche (M.2/Reader), nicht dieser
/// Schicht — hier bleibt es exakt.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Metrics {
    // --- Roh-Aggregate ---
    pub sessions: u64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tool_calls: u64,
    pub effects: EffectCounts,
    pub redaction_secrets: u64,
    pub redaction_pii: u64,
    pub distinct_files: u64,
    /// Je Agent, nach Session-Zahl absteigend (Gleichstand: Name aufsteigend).
    pub by_agent: Vec<AgentCount>,

    // --- KPI-Kacheln (entire-Overview) ---
    /// Throughput: Ø Token je Session. `0.0` bei keiner Session.
    pub throughput: f64,
    /// Iteration: Ø Tool-Calls je Session. `0.0` bei keiner Session.
    pub iteration: f64,
    /// Continuity: längste Session in Sekunden (nur Sessions mit Start **und**
    /// Ende in der Herkunft).
    pub continuity_seconds: u64,
    /// Streak: längster Lauf aufeinanderfolgender Kalendertage mit ≥1 Session.
    pub streak_days: u32,
    /// Der Lauf, der am **jüngsten** aktiven Tag endet — die „aktuelle" Serie.
    pub streak_current_days: u32,
}

impl Metrics {
    /// Leitet die Kennzahlen aus einer Session-Menge ab.
    pub fn from_sessions(sessions: &[Session]) -> Self {
        let mut tokens_input = 0u64;
        let mut tokens_output = 0u64;
        let mut tool_calls = 0u64;
        let mut effects = EffectCounts::default();
        let mut redaction_secrets = 0u64;
        let mut redaction_pii = 0u64;
        let mut files: BTreeSet<String> = BTreeSet::new();
        let mut per_agent: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        let mut continuity_seconds = 0u64;
        let mut active_days: BTreeSet<i64> = BTreeSet::new();

        for session in sessions {
            let tokens = session.usage.input_tokens + session.usage.output_tokens;
            tokens_input += session.usage.input_tokens;
            tokens_output += session.usage.output_tokens;
            redaction_secrets += u64::from(session.redaction.counts.secrets);
            redaction_pii += u64::from(session.redaction.counts.pii);

            for turn in &session.turns {
                tool_calls += turn.tool_calls.len() as u64;
                for call in &turn.tool_calls {
                    if let Some(effect) = &call.effect {
                        match effect.kind {
                            EffectKind::Read => effects.read += 1,
                            EffectKind::Write => effects.write += 1,
                            EffectKind::Delete => effects.delete += 1,
                            EffectKind::Exec => effects.exec += 1,
                            EffectKind::Other => effects.other += 1,
                        }
                    }
                }
            }

            for file in &session.produced.files {
                files.insert(file.clone());
            }

            let entry = per_agent.entry(session.agent.name.clone()).or_default();
            entry.0 += 1;
            entry.1 += tokens;

            if let Some(lineage) = &session.lineage {
                if let (Some(start), Some(end)) = (&lineage.started_at, &lineage.ended_at) {
                    if let (Some(a), Some(b)) = (epoch_seconds(start), epoch_seconds(end)) {
                        continuity_seconds = continuity_seconds.max((b - a).max(0) as u64);
                    }
                }
                if let Some(start) = &lineage.started_at {
                    if let Some(day) = day_number(start) {
                        active_days.insert(day);
                    }
                }
            }
        }

        let n = sessions.len() as u64;
        let total_tokens = tokens_input + tokens_output;
        let (throughput, iteration) = if n == 0 {
            (0.0, 0.0)
        } else {
            (total_tokens as f64 / n as f64, tool_calls as f64 / n as f64)
        };

        let mut by_agent: Vec<AgentCount> = per_agent
            .into_iter()
            .map(|(agent, (sessions, tokens))| AgentCount {
                agent,
                sessions,
                tokens,
            })
            .collect();
        by_agent.sort_by(|a, b| {
            b.sessions
                .cmp(&a.sessions)
                .then_with(|| a.agent.cmp(&b.agent))
        });

        let (streak_days, streak_current_days) = streaks(&active_days);

        Metrics {
            sessions: n,
            tokens_input,
            tokens_output,
            tool_calls,
            effects,
            redaction_secrets,
            redaction_pii,
            distinct_files: files.len() as u64,
            by_agent,
            throughput,
            iteration,
            continuity_seconds,
            streak_days,
            streak_current_days,
        }
    }
}

/// Aus der aufsteigend sortierten Menge aktiver Tage: (längster Lauf, Lauf am
/// jüngsten Tag). Beide 0 bei leerer Menge.
fn streaks(days: &BTreeSet<i64>) -> (u32, u32) {
    let mut longest = 0u32;
    let mut current = 0u32;
    let mut prev: Option<i64> = None;
    for &day in days {
        current = match prev {
            Some(p) if day == p + 1 => current + 1,
            _ => 1,
        };
        longest = longest.max(current);
        prev = Some(day);
    }
    // Da `days` aufsteigend läuft, hält `current` am Ende den Lauf, der am
    // größten (jüngsten) Tag endet — genau die „aktuelle" Serie.
    (longest, current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{
        Agent, Effect, EffectKind, Intent, Lineage, Model, Produced, Redaction, RedactionCounts,
        Role, ToolCall, Turn, Usage,
    };

    fn session(agent: &str) -> Session {
        Session::new(
            Agent {
                name: agent.into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "m".into(),
            },
            Intent::default(),
        )
    }

    fn effect_call(kind: EffectKind) -> ToolCall {
        ToolCall {
            capture: None,
            name: "T".into(),
            arguments: "{}".into(),
            effect: Some(Effect {
                kind,
                path: None,
                content: None,
            }),
        }
    }

    fn assistant(calls: Vec<ToolCall>) -> Turn {
        Turn {
            role: Role::Assistant,
            text: String::new(),
            tool_calls: calls,
            parent: None,
            at: None,
        }
    }

    #[test]
    fn empty_yields_zeroes() {
        let m = Metrics::from_sessions(&[]);
        assert_eq!(m.sessions, 0);
        assert_eq!(m.throughput, 0.0);
        assert_eq!(m.iteration, 0.0);
        assert_eq!(m.streak_days, 0);
        assert!(m.by_agent.is_empty());
    }

    #[test]
    fn aggregates_tokens_calls_effects_and_derives_tiles() {
        let mut a = session("claude-code");
        a.usage = Usage {
            input_tokens: 100,
            output_tokens: 300,
        };
        a.turns.push(assistant(vec![
            effect_call(EffectKind::Exec),
            effect_call(EffectKind::Write),
        ]));

        let mut b = session("claude-code");
        b.usage = Usage {
            input_tokens: 200,
            output_tokens: 0,
        };
        b.turns.push(assistant(vec![effect_call(EffectKind::Read)]));

        let m = Metrics::from_sessions(&[a, b]);
        assert_eq!(m.sessions, 2);
        assert_eq!(m.tokens_input, 300);
        assert_eq!(m.tokens_output, 300);
        assert_eq!(m.tool_calls, 3);
        assert_eq!(m.effects.exec, 1);
        assert_eq!(m.effects.write, 1);
        assert_eq!(m.effects.read, 1);
        // Throughput = 600 Token / 2 Sessions, Iteration = 3 Calls / 2.
        assert_eq!(m.throughput, 300.0);
        assert_eq!(m.iteration, 1.5);
    }

    #[test]
    fn distinct_files_are_counted_once_across_sessions() {
        let mut a = session("x");
        a.produced = Produced {
            commit_hint: None,
            files: vec!["a.rs".into(), "b.rs".into()],
        };
        let mut b = session("x");
        b.produced = Produced {
            commit_hint: None,
            files: vec!["b.rs".into(), "c.rs".into()],
        };
        assert_eq!(Metrics::from_sessions(&[a, b]).distinct_files, 3);
    }

    #[test]
    fn redaction_hits_sum_up() {
        let mut a = session("x");
        a.redaction = Redaction {
            applied: true,
            counts: RedactionCounts { secrets: 2, pii: 1 },
        };
        let m = Metrics::from_sessions(&[a]);
        assert_eq!(m.redaction_secrets, 2);
        assert_eq!(m.redaction_pii, 1);
    }

    #[test]
    fn by_agent_ranks_by_session_count() {
        let m = Metrics::from_sessions(&[
            session("codex"),
            session("claude-code"),
            session("claude-code"),
        ]);
        assert_eq!(m.by_agent.len(), 2);
        assert_eq!(m.by_agent[0].agent, "claude-code");
        assert_eq!(m.by_agent[0].sessions, 2);
        assert_eq!(m.by_agent[1].agent, "codex");
    }

    #[test]
    fn continuity_is_the_longest_session() {
        let mut short = session("x");
        short.lineage = Some(Lineage {
            local_id: "s".into(),
            started_at: Some("2026-07-25T09:00:00Z".into()),
            ended_at: Some("2026-07-25T09:10:00Z".into()),
            cwd: None,
        });
        let mut long = session("x");
        long.lineage = Some(Lineage {
            local_id: "l".into(),
            started_at: Some("2026-07-25T09:00:00Z".into()),
            ended_at: Some("2026-07-25T11:00:00Z".into()),
            cwd: None,
        });
        assert_eq!(
            Metrics::from_sessions(&[short, long]).continuity_seconds,
            7_200
        );
    }

    #[test]
    fn streak_finds_longest_and_current_runs() {
        // Aktive Tage: 24, 25, 26 (Lauf 3), Lücke, dann 28 (isoliert).
        let day = |d: &str| {
            let mut s = session("x");
            s.lineage = Some(Lineage {
                local_id: d.into(),
                started_at: Some(format!("2026-07-{d}T09:00:00Z")),
                ended_at: None,
                cwd: None,
            });
            s
        };
        let m = Metrics::from_sessions(&[day("24"), day("25"), day("26"), day("28")]);
        assert_eq!(m.streak_days, 3);
        // Jüngster aktiver Tag (28) steht allein → aktuelle Serie 1.
        assert_eq!(m.streak_current_days, 1);
    }

    #[test]
    fn order_of_input_does_not_change_the_result() {
        let mut a = session("a");
        a.usage = Usage {
            input_tokens: 10,
            output_tokens: 0,
        };
        let mut b = session("b");
        b.usage = Usage {
            input_tokens: 20,
            output_tokens: 0,
        };
        assert_eq!(
            Metrics::from_sessions(&[a.clone(), b.clone()]),
            Metrics::from_sessions(&[b, a])
        );
    }
}
