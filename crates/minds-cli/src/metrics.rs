//! `minds metrics [--format prometheus|openmetrics|json]` — die Kennzahlen aus
//! dem Store, on-demand projiziert.
//!
//! Kein zweiter Zustand, kein Dienst: bei jedem Aufruf werden die Sessions
//! gelesen, [`Metrics`] daraus abgeleitet und im gewünschten Format ausgegeben.
//! Der übliche Weg ist ein Prometheus-Textfile/Pushgateway (M.3), aus dem Grafana
//! liest — Grafana läuft beim Kunden, wir hosten nichts.
//!
//! # Die zwei Kennzahl-Quellen
//!
//! Die meisten Zahlen kommen aus dem **Store** (Sessions). Die
//! **Kontext-Abdeckung** kommt aus **Git**: ein Walk über die erreichbaren
//! Commits, der je Commit prüft, ob ein `Minds-Session-Id`-Trailer in den Store
//! auflöst. Diese I/O-Kennzahl gehört bewusst hierher und nicht in die reine
//! `minds-metrics`-Crate.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use minds_core::SessionId;
use minds_metrics::{Coverage, Metrics};

use crate::context::Context;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds metrics` aus. `format` ist der Wert von `--format` (Default
/// `prometheus`).
pub fn run(format: Option<&str>) -> ExitCode {
    match metrics(format.unwrap_or("prometheus")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds metrics: {err}");
            ExitCode::FAILURE
        }
    }
}

fn metrics(format: &str) -> Fallible<()> {
    let ctx = Context::open()?;
    let sessions = ctx.all_sessions()?;
    let metrics = Metrics::from_sessions(&sessions);
    let coverage = coverage(&ctx)?;
    let repo = repo_name(&ctx.root);

    let out = match format {
        "prometheus" => minds_metrics::prometheus(&metrics, &repo, Some(coverage)),
        "openmetrics" => minds_metrics::openmetrics(&metrics, &repo, Some(coverage)),
        "json" => {
            let doc = serde_json::json!({
                "repo": repo,
                "metrics": metrics,
                "coverage": coverage,
            });
            format!("{}\n", serde_json::to_string_pretty(&doc)?)
        }
        other => {
            return Err(
                format!("unbekanntes Format {other:?} (prometheus|openmetrics|json)").into(),
            );
        }
    };
    print!("{out}");
    Ok(())
}

/// Läuft die Historie ab HEAD ab und zählt, wie viele Commits ≥1 auflösbaren
/// Trailer tragen. `store.exists` wird je Session-Id gecacht — derselbe Trailer
/// steht nach einem Rebase an mehreren Commits.
fn coverage(ctx: &Context) -> Fallible<Coverage> {
    let Some(head) = ctx.repo.head()?.commit() else {
        return Ok(Coverage {
            commits_total: 0,
            commits_with_context: 0,
        });
    };

    let mut cache: BTreeMap<SessionId, bool> = BTreeMap::new();
    let mut commits_total = 0u64;
    let mut commits_with_context = 0u64;

    for commit in ctx.repo.revwalk(head)? {
        let commit = commit?;
        commits_total += 1;

        let mut has_context = false;
        for id in ctx.repo.session_ids_of(commit)? {
            let resolvable = match cache.get(&id) {
                Some(known) => *known,
                None => {
                    let known = ctx.store.exists(id)?;
                    cache.insert(id, known);
                    known
                }
            };
            if resolvable {
                has_context = true;
                break;
            }
        }
        if has_context {
            commits_with_context += 1;
        }
    }

    Ok(Coverage {
        commits_total,
        commits_with_context,
    })
}

/// Der Repo-Name für das `repo`-Label: der letzte Pfadbestandteil der Wurzel.
fn repo_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string())
}
