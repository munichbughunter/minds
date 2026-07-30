//! Prometheus-/OpenMetrics-Textausgabe der Kennzahlen — rein, ohne I/O.
//!
//! Format: die Prometheus-Text-Exposition (`# HELP`/`# TYPE`, dann Samples).
//! `openmetrics` hängt zusätzlich den `# EOF`-Abschluss an; beide Ausgaben sind
//! von jedem Prometheus-Scraper lesbar. (Die strengere OpenMetrics-Regel, den
//! `_total`-Suffix vom Familiennamen zu trennen, ist eine spätere Feinheit — für
//! einen Scrape spielt sie keine Rolle.)
//!
//! Kardinalität bleibt niedrig: gelabelt wird nach `repo`, `agent`, `effect`,
//! `kind` — **nie** nach Session-Id (siehe Plan-v0.2, Track M).

use std::fmt::Write as _;

use crate::{Coverage, Metrics};

/// Prometheus-Textausgabe.
pub fn prometheus(metrics: &Metrics, repo: &str, coverage: Option<Coverage>) -> String {
    render(metrics, repo, coverage, false)
}

/// Wie [`prometheus`], zusätzlich mit `# EOF`-Abschluss (OpenMetrics).
pub fn openmetrics(metrics: &Metrics, repo: &str, coverage: Option<Coverage>) -> String {
    render(metrics, repo, coverage, true)
}

fn render(m: &Metrics, repo: &str, coverage: Option<Coverage>, eof: bool) -> String {
    let repo = escape(repo);
    let mut s = String::new();

    head(
        &mut s,
        "minds_sessions_total",
        "counter",
        "Erfasste Sessions.",
    );
    int(&mut s, "minds_sessions_total", &lbl(&repo, &[]), m.sessions);

    head(
        &mut s,
        "minds_tokens_total",
        "counter",
        "Token nach Richtung.",
    );
    int(
        &mut s,
        "minds_tokens_total",
        &lbl(&repo, &[("kind", "input")]),
        m.tokens_input,
    );
    int(
        &mut s,
        "minds_tokens_total",
        &lbl(&repo, &[("kind", "output")]),
        m.tokens_output,
    );

    head(
        &mut s,
        "minds_tool_calls_total",
        "counter",
        "Tool-Calls insgesamt.",
    );
    int(
        &mut s,
        "minds_tool_calls_total",
        &lbl(&repo, &[]),
        m.tool_calls,
    );

    head(
        &mut s,
        "minds_tool_effects_total",
        "counter",
        "Tool-Calls mit Effekt, nach Art.",
    );
    for (effect, value) in [
        ("read", m.effects.read),
        ("write", m.effects.write),
        ("delete", m.effects.delete),
        ("exec", m.effects.exec),
        ("other", m.effects.other),
    ] {
        int(
            &mut s,
            "minds_tool_effects_total",
            &lbl(&repo, &[("effect", effect)]),
            value,
        );
    }

    head(
        &mut s,
        "minds_redaction_hits_total",
        "counter",
        "Redaction-Treffer nach Kategorie.",
    );
    int(
        &mut s,
        "minds_redaction_hits_total",
        &lbl(&repo, &[("kind", "secret")]),
        m.redaction_secrets,
    );
    int(
        &mut s,
        "minds_redaction_hits_total",
        &lbl(&repo, &[("kind", "pii")]),
        m.redaction_pii,
    );

    head(
        &mut s,
        "minds_distinct_files",
        "gauge",
        "Distinkte berührte Dateien.",
    );
    int(
        &mut s,
        "minds_distinct_files",
        &lbl(&repo, &[]),
        m.distinct_files,
    );

    head(
        &mut s,
        "minds_sessions_by_agent",
        "gauge",
        "Sessions je Agent.",
    );
    for agent in &m.by_agent {
        let e = escape(&agent.agent);
        int(
            &mut s,
            "minds_sessions_by_agent",
            &lbl(&repo, &[("agent", &e)]),
            agent.sessions,
        );
    }
    head(&mut s, "minds_tokens_by_agent", "gauge", "Token je Agent.");
    for agent in &m.by_agent {
        let e = escape(&agent.agent);
        int(
            &mut s,
            "minds_tokens_by_agent",
            &lbl(&repo, &[("agent", &e)]),
            agent.tokens,
        );
    }

    head(
        &mut s,
        "minds_throughput_tokens_per_session",
        "gauge",
        "Ø Token je Session.",
    );
    float(
        &mut s,
        "minds_throughput_tokens_per_session",
        &lbl(&repo, &[]),
        m.throughput,
    );
    head(
        &mut s,
        "minds_iteration_calls_per_session",
        "gauge",
        "Ø Tool-Calls je Session.",
    );
    float(
        &mut s,
        "minds_iteration_calls_per_session",
        &lbl(&repo, &[]),
        m.iteration,
    );
    head(
        &mut s,
        "minds_continuity_seconds",
        "gauge",
        "Längste Session in Sekunden.",
    );
    int(
        &mut s,
        "minds_continuity_seconds",
        &lbl(&repo, &[]),
        m.continuity_seconds,
    );
    head(
        &mut s,
        "minds_streak_days",
        "gauge",
        "Längster Lauf aktiver Tage.",
    );
    int(
        &mut s,
        "minds_streak_days",
        &lbl(&repo, &[]),
        u64::from(m.streak_days),
    );
    head(
        &mut s,
        "minds_streak_current_days",
        "gauge",
        "Aktueller Lauf aktiver Tage.",
    );
    int(
        &mut s,
        "minds_streak_current_days",
        &lbl(&repo, &[]),
        u64::from(m.streak_current_days),
    );

    if let Some(cov) = coverage {
        head(
            &mut s,
            "minds_commits_total",
            "gauge",
            "Erreichbare Commits.",
        );
        int(
            &mut s,
            "minds_commits_total",
            &lbl(&repo, &[]),
            cov.commits_total,
        );
        head(
            &mut s,
            "minds_commits_with_context",
            "gauge",
            "Commits mit auflösbarem Kontext.",
        );
        int(
            &mut s,
            "minds_commits_with_context",
            &lbl(&repo, &[]),
            cov.commits_with_context,
        );
        head(
            &mut s,
            "minds_context_coverage_ratio",
            "gauge",
            "Anteil abgedeckter Commits.",
        );
        float(
            &mut s,
            "minds_context_coverage_ratio",
            &lbl(&repo, &[]),
            cov.ratio(),
        );
    }

    if eof {
        s.push_str("# EOF\n");
    }
    s
}

fn head(s: &mut String, name: &str, typ: &str, help: &str) {
    let _ = writeln!(s, "# HELP {name} {help}");
    let _ = writeln!(s, "# TYPE {name} {typ}");
}

fn int(s: &mut String, name: &str, labels: &str, value: u64) {
    let _ = writeln!(s, "{name}{{{labels}}} {value}");
}

fn float(s: &mut String, name: &str, labels: &str, value: f64) {
    let _ = writeln!(s, "{name}{{{labels}}} {value}");
}

/// Baut die Labelmenge `repo="…"[,k="v"…]`.
fn lbl(repo: &str, extra: &[(&str, &str)]) -> String {
    let mut out = format!("repo=\"{repo}\"");
    for (key, value) in extra {
        let _ = write!(out, ",{key}=\"{value}\"");
    }
    out
}

/// Prometheus-Label-Escaping: Backslash, Anführungszeichen, Zeilenumbruch.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{Agent, Intent, Model, Session, Usage};

    fn metrics() -> Metrics {
        let mut a = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "m".into(),
            },
            Intent::default(),
        );
        a.usage = Usage {
            input_tokens: 100,
            output_tokens: 200,
        };
        Metrics::from_sessions(std::slice::from_ref(&a))
    }

    #[test]
    fn prometheus_has_help_type_and_samples() {
        let out = prometheus(&metrics(), "minds", None);
        assert!(out.contains("# TYPE minds_sessions_total counter"));
        assert!(out.contains("minds_sessions_total{repo=\"minds\"} 1"));
        assert!(out.contains("minds_tokens_total{repo=\"minds\",kind=\"input\"} 100"));
        assert!(out.contains("minds_throughput_tokens_per_session{repo=\"minds\"} 300"));
        assert!(out.contains("minds_sessions_by_agent{repo=\"minds\",agent=\"claude-code\"} 1"));
        // Ohne Coverage kein Coverage-Block.
        assert!(!out.contains("minds_context_coverage_ratio"));
    }

    #[test]
    fn coverage_is_rendered_when_given() {
        let cov = Coverage {
            commits_total: 4,
            commits_with_context: 3,
        };
        let out = prometheus(&metrics(), "minds", Some(cov));
        assert!(out.contains("minds_commits_total{repo=\"minds\"} 4"));
        assert!(out.contains("minds_context_coverage_ratio{repo=\"minds\"} 0.75"));
    }

    #[test]
    fn openmetrics_adds_the_eof_marker() {
        let out = openmetrics(&metrics(), "minds", None);
        assert!(out.ends_with("# EOF\n"));
        assert!(!prometheus(&metrics(), "minds", None).contains("# EOF"));
    }

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}
