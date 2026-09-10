//! `minds agent-help` — die eigene Kommando-Karte, maschinenlesbar.
//!
//! Gedacht für den Agenten, nicht für den Menschen (`--help` bleibt für den
//! Menschen). Ein Agent, der `minds` fahren soll, parst diese JSON-Karte und
//! weiß, welche Kommandos es gibt und wie sie heißen — ohne die Prosa-Hilfe zu
//! interpretieren. Billig, aber in Agent-Workflows hebelstark: die CLI
//! beschreibt sich selbst.
//!
//! Die Karte ist handgeschrieben (USAGE-Prosa gehört nicht generiert), aber
//! nicht handgepflegt-driftend: Ein Test vergleicht ihre Namen mit
//! [`crate::public_commands`] — wer ein Kommando ergänzt, ohne es hier
//! einzutragen, wird rot. Vor #11 fehlten acht Kommandos, und die Karte nannte
//! sich trotzdem vollständig.
//!
//! Die `usage`-Zeilen sind bewusst **kuratierte Kurzformen**, keine
//! vollständigen Flag-Listen — vollständig ist die USAGE in `main.rs`. Der
//! Test bewacht deshalb nur die Namensmenge, nicht die Flags.

use std::process::ExitCode;

/// Die Kommando-Karte als JSON-Wert — getrennt von [`run`], damit der Test sie
/// lesen kann, statt stdout zu parsen.
fn card() -> serde_json::Value {
    serde_json::json!({
        "tool": "minds",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Durable context for agent sessions, in Git.",
        "commands": [
            {"name": "enable", "usage": "minds enable [--agent <name>] [--child-repo <path>] [--child-remote <url>]", "summary": "Set the repo up for Minds: hooks + store config."},
            {"name": "hook", "usage": "minds hook --agent <name> [--event <name>]", "summary": "Agent hook event from stdin into the local journal. Always exits 0."},
            {"name": "checkpoint", "usage": "minds checkpoint [--commit <id>]", "summary": "Interpret the journal, redact, store sessions, append trailers."},
            {"name": "show", "usage": "minds show [<commit>] [--full]", "summary": "Intent and attribution behind a commit."},
            {"name": "why", "usage": "minds why <file>:<line> [--full]", "summary": "The session behind a single line."},
            {"name": "blame", "usage": "minds blame <file>", "summary": "Attribution per line, aggregated by session, with context coverage."},
            {"name": "recall", "usage": "minds recall <target>", "summary": "Context brief behind a file, line, or commit — agent-friendly."},
            {"name": "distill", "usage": "minds distill [--path <dir>] [--out <file>]", "summary": "AGENTS.md draft from the repo history."},
            {"name": "brief", "usage": "minds brief [<file>...]", "summary": "Size-bounded context block for the start of a session."},
            {"name": "recap", "usage": "minds recap [--limit <n>] [--all]", "summary": "The most recent sessions at a glance."},
            {"name": "search", "usage": "minds search <query>", "summary": "Search prompts and sessions."},
            {"name": "inspect", "usage": "minds inspect [<search> | <file>:<line>]", "summary": "Terminal UI: sessions, a session's graph, a line's why chain. In a pipe: tab-separated lines."},
            {"name": "agent-help", "usage": "minds agent-help", "summary": "This machine-readable command card."},
            {"name": "metrics", "usage": "minds metrics [--format prometheus|openmetrics|json]", "summary": "Metrics from the store — Prometheus, OpenMetrics, or JSON."},
            {"name": "fsck", "usage": "minds fsck [--require-review] [--require-seal]", "summary": "Is every trailer resolvable? Journal gaps? With --require-review: policy gate."},
            {"name": "forget", "usage": "minds forget <session> [--reason <text>]", "summary": "GDPR erasure: the payload becomes a tombstone, the reference stays resolvable."},
            {"name": "reinterpret", "usage": "minds reinterpret <session>", "summary": "Reinterpret stored tool calls with the current adapter — strictly read-only, evidence unchanged."},
            {"name": "sign", "usage": "minds sign <session> [--key <path>] | minds sign --seal <seal-id> [--key <path>]", "summary": "Sign a session's attribution (to stdout) or retroactively sign an evidence seal (into the store)."},
            {"name": "verify", "usage": "minds verify <session> [--signers <file>] | minds verify <session> --sig <file> | minds verify --evidence <seal-id>", "summary": "Evidence verdict (exit: 0 VERIFIED, 1 TAMPERED, 2 VERIFIED, INCOMPLETE, 3 NOT VERIFIABLE) or check a signed attribution."},
            {"name": "review", "usage": "minds review <change-id|session-id> --approve|--reject|--needs-work [--summary <text>] [--sign]", "summary": "Create a verdict as a Git object; --sign turns it into proof."},
            {"name": "reviews", "usage": "minds reviews <subject> [--signers <file>]", "summary": "Verdicts and thread for a change; --signers checks the signatures."},
            {"name": "comment", "usage": "minds comment <subject> [--on <file:line|turn:<n>>] \"<text>\"", "summary": "Attach a remark to the review thread — append-only, mergeable without conflicts."},
            {"name": "stack", "usage": "minds stack [--base <ref>]", "summary": "Dependent changes and their review state; survives rebase and force-push."},
            {"name": "gitlab", "usage": "minds gitlab mirror <subject> --mr <nr> | minds gitlab webhook [--write]", "summary": "Mirror verdicts as an MR note, or interpret a webhook comment as a verdict."},
            {"name": "audit", "usage": "minds audit --export [--out <file>] [--base <ref>] [--mode redacted|proof]", "summary": "Provenance chain as a portable bundle, verifiable without this tool."},
            {"name": "sync", "usage": "minds sync [--remote <name>]", "summary": "Context and reviews to the remote — all refs in one connection, never with --force, except to transfer a GDPR erasure (tombstone ref)."},
            {"name": "render", "usage": "minds render [--out <directory>]", "summary": "Static HTML page over the context."}
        ]
    })
}

/// Führt `minds agent-help` aus — schreibt die Kommando-Karte als JSON.
pub fn run() -> ExitCode {
    // `to_string_pretty` über einen festen literalen Wert kann nicht fehlschlagen.
    println!(
        "{}",
        serde_json::to_string_pretty(&card()).expect("statische JSON-Karte serialisiert immer")
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Die Drift-Bremse aus #11: Die Karte nannte sich vollständig, und acht
    /// Kommandos fehlten. Maßstab ist die Parser-Tabelle — **die** Quelle, die
    /// beim Anlegen eines Kommandos ohnehin gepflegt werden muss.
    #[test]
    fn the_card_lists_exactly_the_public_commands() {
        let card = card();
        let listed: BTreeSet<&str> = card["commands"]
            .as_array()
            .expect("commands ist ein Array")
            .iter()
            .map(|entry| {
                entry["name"]
                    .as_str()
                    .expect("jedes Kommando hat einen Namen")
            })
            .collect();
        let public: BTreeSet<&str> = crate::public_commands().collect();

        assert_eq!(
            listed, public,
            "agent-help und die Parser-Tabelle (SPECS) driften auseinander"
        );
    }

    /// Jeder Eintrag braucht die drei Felder, die ein Agent parst.
    #[test]
    fn every_entry_carries_name_usage_and_summary() {
        let card = card();
        for entry in card["commands"].as_array().unwrap() {
            for field in ["name", "usage", "summary"] {
                assert!(
                    entry[field].as_str().is_some_and(|s| !s.is_empty()),
                    "{field} fehlt oder leer: {entry}"
                );
            }
        }
    }
}
