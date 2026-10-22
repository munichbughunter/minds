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
            {"name": "witness", "usage": "minds witness init --repo <path> [--path-map <agent>=<host>] [--profile container|user|managed|local] [--socket-group <group>] [--child-repo <path>] [--policy-rev <rev>] [--intent-key <pub>|--replace-intent-key <pub>] | minds witness run|status|keygen|signers|activate [--home <directory>] | minds witness serve [--base <dir>] | minds witness register|unregister --repo <root>", "summary": "Host-side witness (serve: one service per host running a witness per registered repository; register/unregister over its socket — unregister from the witness user only): init pins git dir, store (child repo only via --child-repo) and the redaction policy from HEAD or --policy-rev (no includes, no replace refs; commit and blob id printed) in witness.json (idempotent; adds pins to an old home); keygen creates the witness key and prints its allowed_signers line; signers prints the line of an existing key; --intent-key pins the human's dedicated intent security key (minds.intentKey; sk only; never replaces a pinned one, --replace-intent-key does) whose fresh touch may activate an intent on the agent socket; activate (witness user only) activates an intent from JSON on stdin over the control socket. Never overwrites keys or a different configuration."},
            {"name": "enable", "usage": "minds enable [--agent <name>] [--child-repo <path>] [--child-remote <url>] [--witness container|user|managed]", "summary": "Set the repo up for Minds: hooks + store config. Without --agent: the agents that look installed (none found: all). Without --witness it always sets up a witness: on a terminal it asks once whether to set it up protected (one sudo: own user, root-owned binary, system service, deny-only managed settings — A2-capable), otherwise a local one (same user ID, user service) with a warning that it runs unprotected (A1); MINDS_WITNESS=protected|local|print|off chooses without asking. Agents must not stop or touch it, and must never answer the prompt or run the sudo themselves. --witness additionally sets up an isolation profile (container: witness home, key, .devcontainer/, user service — not started; user: prints setup steps; managed: writes a settings proposal). Elevates only for the protected setup, after the human agreed; never overwrites existing files (writes *.minds-proposed instead)."},
            {"name": "disable", "usage": "minds disable [--all]", "summary": "Take back what enable set up in this repository: the agent hook registrations (foreign entries stay), the minds blocks of the Git hooks, the local witness (user service, home) and the protected witness of this repository (one sudo: system service, witness home, its managed-settings rules); --all removes every protected witness of the machine with the managed settings and the root-owned binary. The store, the trusted signers file and the intent key stay. Agents must not run it."},
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
            {"name": "inspect", "usage": "minds inspect [<search> | <file>:<line>]", "summary": "Terminal UI: sessions, a session's story, a commit's changes with why-this-line, evidence. In a pipe: tab-separated lines."},
            {"name": "agent-help", "usage": "minds agent-help", "summary": "This machine-readable command card."},
            {"name": "metrics", "usage": "minds metrics [--format prometheus|openmetrics|json]", "summary": "Metrics from the store — Prometheus, OpenMetrics, or JSON."},
            {"name": "fsck", "usage": "minds fsck [--require-review] [--require-seal] [--require-assurance <A0|A1|A2|A3>] [--signers <file>]", "summary": "Is every trailer resolvable? Journal gaps? With --require-review: policy gate. --require-assurance: every session of an agent-authored commit must reach the level (exit 2 when not; findings stay exit 1)."},
            {"name": "forget", "usage": "minds forget <session> [--reason <text>]", "summary": "GDPR erasure: the payload becomes a tombstone, the reference stays resolvable."},
            {"name": "reinterpret", "usage": "minds reinterpret <session>", "summary": "Reinterpret stored tool calls with the current adapter — strictly read-only, evidence unchanged."},
            {"name": "sign", "usage": "minds sign <session> [--key <path>] | minds sign --seal <seal-id> [--key <path>]", "summary": "Sign a session's attribution (to stdout) or retroactively sign an evidence seal (into the store)."},
            {"name": "intent", "usage": "minds intent bind --file <path> [--scope <glob,glob>] | minds intent bind --issue <group/project#iid> [--scope <glob,glob>] [--gitlab-url <url>] [--allow-confidential] | minds intent sign [<anchor-id>] [--key <path>] [--witness-home <dir>] | minds intent show [<anchor-id>] | minds intent list", "summary": "Bind a requirement version (file:<path>@<blob>, or a GitLab issue as issue:<project>#<iid>@<updated_at> with the redacted canonical JSON of title and description; instance from --gitlab-url, MINDS_GITLAB_URL or CI_SERVER_URL, never .git/config; token only from MINDS_GITLAB_TOKEN; a confidential issue is refused without --allow-confidential) with its redacted snapshot and scope as an intent anchor; sign it under ssh-sig namespace minds-intent (a FIDO sk key needs a touch, so an agent cannot approve) and activate it via the witness control socket (host side), at the protected witness with a second touch of the pinned security key over a witness nonce (or, with a software key, the human's password), or, without a witness, the local A1 file. Prints which path was taken. Agents should not run sign: the approval belongs to a human."},
            {"name": "spec", "usage": "minds spec sign [--key <path>] [--signers <file>] | minds spec show [<rev>] [--signers <file>]", "summary": "The spec (.minds/spec.json, minds-spec-v1): rules no-dependency (crates), no-import (modules), json-schema (data contracts) and graph (expected minds sessions/intents). sign signs the version in HEAD with schema hashes and the signing commit under ssh-sig namespace minds-spec (a FIDO sk key needs a touch; root-owned ssh-keygen only) and stores it in refs/minds/specs/<id>; show prints id, rules and signature state. Agents must not run sign or edit the spec to pass: an unsigned change is DRIFT."},
            {"name": "seals", "usage": "minds seals [--session <id>] [--limit <n>]", "summary": "List Evidence-Chain seals — id, session, event range, gaps, signature presence; most recent first."},
            {"name": "replay", "usage": "minds replay [--commit <rev>] [--unsigned]", "summary": "Unix only (refused elsewhere). CI only meaningfully: on a checkout of the commit (default HEAD, must equal HEAD), re-runs the decisive test/bench commands of its linked sessions (last occurrence of each distinct class+argv+cwd) without a shell, with a cleared environment (PATH absolute entries, HOME, CARGO_*/RUSTUP_* without credential names, plus .minds/replay.json env), in the recorded repo-relative cwd confined to directories the commit tracks (never the git dir), and only if .minds/replay.json in the commit allowlists runner, subcommand and every flag; everything else is reported as skipped and never executed. Tests: failed==0 status and passed count must match (ignored differences reported); benches: within the policy tolerance; exit code only if recorded. Stores a canonical replay record under refs/minds/anchors/replay/<id>, signed under minds-anchor with MINDS_ANCHOR_KEY_FILE, or unsigned with --unsigned (shown, never counted for A3). Signing is refused in merge/pull request pipelines (the replayed code runs as the same user and could read the key): sign only on protected branches. Records count in verify only for the exact commit they ran on. Exit: 0 all reproduced or skipped, 2 claim not reproduced, 4 operational failure."},
            {"name": "anchor", "usage": "minds anchor [--mirror]", "summary": "GitLab CI only, push pipelines of the protected default branch (GITLAB_CI, CI_PROJECT_PATH, CI_PIPELINE_ID, CI_COMMIT_SHA = HEAD, CI_COMMIT_REF_PROTECTED=true, CI_PIPELINE_SOURCE=push, CI_COMMIT_BRANCH = CI_DEFAULT_BRANCH, MINDS_ANCHOR_KEY_FILE as private key file; merge request, trigger, API and tag pipelines are refused; anything missing: exit 4, nothing written; never a developer key or ssh-agent). Countersigns the seals of the sessions named by the pushed commits (CI_COMMIT_BEFORE_SHA..HEAD; GIT_DEPTH: 0): minds-anchor-v1 text (seal, project, pipeline, CI clock) signed under minds-anchor, stored under refs/minds/anchors/first-sight/<hex>; an existing ref is never overwritten (first sight wins; one not validly made with this key is reported, exit 4), so reruns write nothing new. Job order: fetch refs/minds/*, minds anchor, git push --atomic refs/minds/*, minds anchor --mirror. --mirror uses its own bot token MINDS_ANCHOR_GITLAB_TOKEN (refused if equal to MINDS_GITLAB_TOKEN; instance only CI_SERVER_URL) and posts the push's countersignatures not yet in an own note to the merge requests merged into the default branch. verify shows anchored: pipeline #N, <at> per seal (valid signature from a principal restricted to minds-anchor only); verify --online reports a signed MR note whose anchor ref or seal is missing as an integrity finding when the note predates the CI job or a valid local anchor is at least as new, else exit 4; edited anchor notes, unvalidated entries and failed checks are exit 4. Never prints the key path or a token. Exit: 0 or 4."},
            {"name": "verify", "usage": "minds verify [<session|rev>] [--signers <file>] [--commit <rev>] [--require-explained <percent>] [--allow-heuristic] [--all] [--require-in-scope] [--witness-home <dir>] [--require-assurance <A0|A1|A2|A3>] [--limits] [--online [--gitlab-url <url>]] [--conformance] | minds verify <session> --sig <file> | minds verify --evidence <seal-id>", "summary": "Evidence verdict for a session or the sessions linked to a revision (default HEAD); exit: 0 VERIFIED, 1 TAMPERED, 2 VERIFIED, INCOMPLETE, 3 NOT VERIFIABLE, 4 operational failure. Multiple sessions: worst wins (4 > 1 > 3 > 2 > 0). The Coverage line adds artifact coverage (explained/changed lines of the commit) and lists unexplained lines (20 max, --all for every line); --require-explained <0-100> is a gate that fails with exit 2 but never masks 1/3/4. Observed lines count as explained, including what a shell command wrote while a witness watched (the observation falls in the window of the Bash call and is attributed to it); lines that only a shell command mentions (heuristic) count for the gate only with --allow-heuristic and never change a class or the assurance. When the bound intent anchor declares scope= globs, the Coverage line adds 'N out of scope' and lists each path outside them with its sources (commit, claim, observation); --require-in-scope fails with exit 2 on any such path or when no scope can be assessed (unbound, scope=-, anchor not in the store or unreadable, session payload unreadable, intent changed mid-session, witness observations incomplete, integrity violated, commit not assessed, no linked session), never masking 1/3/4; combine with --require-assurance A2 (and --witness-home) so the scope and the observations are not the agent's own, and with --require-explained 100, since the session link (trailer) decides whose scope applies. Each block ends with Assurance (A0 claimed … A3 reproduced, computed at read time, with the first reason it is not higher), Intent (not bound / unsigned / signed, sk key / signed, software key / signature not checked / signature invalid for minds-intent — an assurance fact, never TAMPERED), Not proven (the limits at that level; --limits prints them in full) and lists write claims no witness observation confirms as uncorroborated. --require-assurance <A0-A3> gates on the weakest session (exit 2, never masks 1/3/4); --witness-home <dir> checks the witness ledger: a witnessed seal missing from the repository is TAMPERED. An intent bound to a GitLab issue adds 'Issue version': not checked (offline) without --online; with --online (MINDS_GITLAB_TOKEN) current, changed since binding (bound version confirmed in / not found in / not in the readable description history, no description history available, description history query failed) or version check unavailable (reason) — never changes assurance, verdict or exit code. With a spec (.minds/spec.json, signed by a human with minds spec sign) a Conformance block lists per rule expected, observed and PASS/DRIFT/UNKNOWN. A signed spec counts only if fresh: its signing commit lies in the history and the spec and every pinned schema are unchanged on every commit of the ancestry path since (all parents); a rollback, removal, merge or changed schema without a new signature is DRIFT and never replaces the signed rules; a shallow clone is UNKNOWN. Shown without changing the exit code; --conformance (CI) fails with exit 2 on any DRIFT or UNKNOWN or without a spec, never masking 1/3/4. Or check a signed attribution."},
            {"name": "decide", "usage": "minds decide [--supersedes <id>] [--human] [--template] < decision.json", "summary": "Record a decision as a Git object: topic, chosen path, rejected alternatives and the assumptions behind them, each edge weighted load-bearing or marginal, assumptions optionally with a deterministic probe (pattern in file/glob, tracked path, unchanged blob). Omit chosen and set open:true to hand the choice to a human with its options; --supersedes links a re-evaluation to the earlier record; --template prints the JSON form. Free text is redacted, a probe or path the policy would change is refused. Prints decision <id> first: that tool-call output links this session to the record. Run it whenever you choose between approaches or hand a choice back."},
            {"name": "decisions", "usage": "minds decisions [<path>|<id>] [--check]", "summary": "Decisions and their standing today, probes evaluated against the index: open · re-evaluate (a load-bearing assumption fell, reconsider the alternative it rejected) · re-check (changed since) · unchecked · in force. A path filters by scope; an id shows one decision with every assumption, probe result and its supersedes history. --check exits 2 on re-evaluate, re-check, unchecked or a record that could not be read (CI) — a gate against unnoticed drift, not against a writer of the store: anyone with minds decide --supersedes can retire a decision."},
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
            "--allow-heuristic",
            "--all",
            "--require-in-scope",
            "--witness-home <dir>",
            "--require-assurance <A0|A1|A2|A3>",
            "--limits",
            "--online",
            "--gitlab-url <url>",
        ] {
            assert!(verify["usage"].as_str().unwrap().contains(flag), "{flag}");
        }
    }
}
