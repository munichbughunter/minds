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
            {"name": "witness", "usage": "minds witness init --repo <path> [--path-map <agent>=<host>] [--profile container|user|managed] [--socket-group <group>] [--child-repo <path>] [--policy-rev <rev>] | minds witness run|status|keygen [--home <directory>]", "summary": "Host-side witness: init pins git dir, store (child repo only via --child-repo) and the redaction policy from HEAD or --policy-rev (no includes, no replace refs; commit and blob id printed) in witness.json (idempotent; adds pins to an old home); keygen creates the witness key and prints its allowed_signers line. Never overwrites keys or a different configuration."},
            {"name": "enable", "usage": "minds enable [--agent <name>] [--child-repo <path>] [--child-remote <url>] [--witness container|user|managed]", "summary": "Set the repo up for Minds: hooks + store config. --witness additionally sets up an isolation profile (container: witness home, key, .devcontainer/, user service — not started; user: prints setup steps; managed: writes a settings proposal). Never runs sudo, never overwrites existing files (writes *.minds-proposed instead)."},
            {"name": "doctor", "usage": "minds doctor [--probe-home <dir>]", "summary": "One ok/warn/fail line per check: agent hooks, Git hooks, store, witness socket and ping, witness profile and key. --probe-home (agent side): fail if the witness home can be opened; ok only if it exists but is refused (EACCES); an absent home is warn (no proof). Host code in .git and the mount table are heuristics (warn). Exit 1 on any fail."},
            {"name": "hook", "usage": "minds hook --agent <name> [--event <name>]", "summary": "Agent hook event from stdin into the local journal. Always exits 0."},
            {"name": "checkpoint", "usage": "minds checkpoint [--commit <id>]", "summary": "Interpret the journal, redact, store sessions, append trailers."},
            {"name": "show", "usage": "minds show [<commit>] [--full]", "summary": "Intent and attribution behind a commit."},
            {"name": "why", "usage": "minds why <file>:<line> [--full]", "summary": "The session behind a single line."},
            {"name": "blame", "usage": "minds blame [--lines] <file>", "summary": "Which session is behind which lines of a file, aggregated by session, with context coverage; --lines annotates every source line instead."},
            {"name": "recall", "usage": "minds recall <target>", "summary": "Context brief behind a file, line, or commit, including the full intent request."},
            {"name": "distill", "usage": "minds distill [--path <dir>] [--out <file>]", "summary": "AGENTS.md draft from the repo history."},
            {"name": "brief", "usage": "minds brief [<file>...]", "summary": "Size-bounded context block for the start of a session."},
            {"name": "recap", "usage": "minds recap [--limit <n>] [--all]", "summary": "The most recent sessions at a glance."},
            {"name": "search", "usage": "minds search <query>", "summary": "Search prompts and sessions."},
            {"name": "inspect", "usage": "minds inspect [<search> | <file>:<line>]", "summary": "Terminal UI: sessions, a session's graph, a line's why chain. In a pipe: tab-separated lines."},
            {"name": "agent-help", "usage": "minds agent-help", "summary": "This machine-readable command card."},
            {"name": "metrics", "usage": "minds metrics [--format prometheus|openmetrics|json]", "summary": "Metrics from the store — Prometheus, OpenMetrics, or JSON."},
            {"name": "fsck", "usage": "minds fsck [--require-review] [--require-seal] [--require-assurance <A0|A1|A2|A3>] [--signers <file>]", "summary": "Is every trailer resolvable? Journal gaps? With --require-review: policy gate. --require-assurance: every session of an agent-authored commit must reach the level (exit 2 when not; findings stay exit 1)."},
            {"name": "forget", "usage": "minds forget <session> [--reason <text>]", "summary": "GDPR erasure: the payload becomes a tombstone, the reference stays resolvable."},
            {"name": "reinterpret", "usage": "minds reinterpret <session>", "summary": "Reinterpret stored tool calls with the current adapter — strictly read-only, evidence unchanged."},
            {"name": "sign", "usage": "minds sign <session> [--key <path>] | minds sign --seal <seal-id> [--key <path>]", "summary": "Sign a session's attribution (to stdout) or retroactively sign an evidence seal (into the store)."},
            {"name": "intent", "usage": "minds intent bind --file <path> [--scope <glob,glob>] | minds intent sign [<anchor-id>] [--key <path>] [--witness-home <dir>] | minds intent show [<anchor-id>] | minds intent list", "summary": "Bind a requirement version (file:<path>@<blob>, redacted snapshot, scope) as an intent anchor; sign it under ssh-sig namespace minds-intent (a FIDO sk key needs a touch, so an agent cannot approve) and activate it via the witness control socket (host side) or, without a witness, the local A1 file. Prints which path was taken. Agents should not run sign: the approval belongs to a human."},
            {"name": "seals", "usage": "minds seals [--session <id>] [--limit <n>]", "summary": "List Evidence-Chain seals — id, session, event range, gaps, signature presence; most recent first."},
            {"name": "verify", "usage": "minds verify [<session|rev>] [--signers <file>] [--commit <rev>] [--require-explained <percent>] [--all] [--require-in-scope] [--witness-home <dir>] [--require-assurance <A0|A1|A2|A3>] [--limits] | minds verify <session> --sig <file> | minds verify --evidence <seal-id>", "summary": "Evidence verdict for a session or the sessions linked to a revision (default HEAD); exit: 0 VERIFIED, 1 TAMPERED, 2 VERIFIED, INCOMPLETE, 3 NOT VERIFIABLE, 4 operational failure. Multiple sessions: worst wins (4 > 1 > 3 > 2 > 0). The Coverage line adds artifact coverage (explained/changed lines of the commit) and lists unexplained lines (20 max, --all for every line); --require-explained <0-100> is a gate that fails with exit 2 but never masks 1/3/4. When the bound intent anchor declares scope= globs, the Coverage line adds 'N out of scope' and lists each path outside them with its sources (commit, claim, observation); --require-in-scope fails with exit 2 on any such path or when no scope can be assessed (unbound, scope=-, anchor not in the store or unreadable, session payload unreadable, intent changed mid-session, witness observations incomplete, integrity violated, commit not assessed, no linked session), never masking 1/3/4; combine with --require-assurance A2 (and --witness-home) so the scope and the observations are not the agent's own, and with --require-explained 100, since the session link (trailer) decides whose scope applies. Each block ends with Assurance (A0 claimed … A3 reproduced, computed at read time, with the first reason it is not higher), Intent (not bound / unsigned / signed, sk key / signed, software key / signature not checked / signature invalid for minds-intent — an assurance fact, never TAMPERED), Not proven (the limits at that level; --limits prints them in full) and lists write claims no witness observation confirms as uncorroborated. --require-assurance <A0-A3> gates on the weakest session (exit 2, never masks 1/3/4); --witness-home <dir> checks the witness ledger: a witnessed seal missing from the repository is TAMPERED. Or check a signed attribution."},
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

    #[test]
    fn verify_card_describes_the_optional_revision_and_default() {
        let card = card();
        let verify = card["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "verify")
            .unwrap();
        assert!(
            verify["usage"]
                .as_str()
                .unwrap()
                .contains("[<session|rev>]")
        );
        assert!(verify["summary"].as_str().unwrap().contains("default HEAD"));
        // EA-02: die Artefakt-Flags stehen auf der Karte.
        // EA-12: die Assurance-Flags ebenso, EA-17 das Scope-Gate.
        for flag in [
            "--commit <rev>",
            "--require-explained <percent>",
            "--all",
            "--require-in-scope",
            "--witness-home <dir>",
            "--require-assurance <A0|A1|A2|A3>",
            "--limits",
        ] {
            assert!(verify["usage"].as_str().unwrap().contains(flag), "{flag}");
        }
    }
}
