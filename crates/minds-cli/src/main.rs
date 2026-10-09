//! Das `minds`-Binary.
//!
//! Die Kommandos: [`enable`] (Setup — Hooks + Store-Config), [`hook`] (heißer
//! Pfad), `checkpoint`/`show`/`why`/`fsck` (kalter Pfad, M6).
//!
//! # Warum hier kein clap steht — und der Parser trotzdem strikt ist
//!
//! Der Parser unten ist handgerollt, aber seit #11 nicht mehr dumm: [`SPECS`]
//! sagt für jedes Unterkommando, welche Flags es kennt, und alles andere ist
//! ein Fehler. Vorher war ein unbekanntes Flag Rauschen — `minds fsck
//! --require-reviews` (Tippfehler) lief als nacktes `fsck` durch, Exit 0, und
//! das CI-Gate war lautlos abgeschaltet. Für ein Werkzeug, dessen Nutzer
//! Flags generieren (Agents!), ist still-falsch die schlimmste Fehlerklasse.
//!
//! clap bleibt trotzdem draußen, aus zwei Gründen: Das zentrale
//! Kommando-Gerüst (#22) ist der Moment, an dem sich ein Umbau lohnt — dann
//! wandern USAGE, `--help` je Subkommando und `agent-help` in eine Quelle.
//! Und der heiße Pfad: `minds hook` startet bei jedem Tool-Call des Agenten
//! neu — der Prozess soll stdin lesen, eine Datei schreiben und enden.
//! [`SPECS`] ist so gebaut, dass ein clap-Derive es später ersetzen kann,
//! ohne dass die `run()`-Signaturen sich ändern.
//!
//! # Die Ausnahme bei den Rückgabewerten
//!
//! Alle Unterkommandos melden Fehler über den Rückgabewert, wie es sich
//! gehört — auch Parse-Fehler. `hook` nicht: Es endet **immer** mit 0, selbst
//! bei falschen Argumenten. Der Grund steht in [`hook`] — bei Claude Code
//! bedeutet Exit 2 „blockiere diese Aktion", und ein Rekorder, der wegen eines
//! fehlenden `--agent` die Arbeit des Nutzers stoppt, hat seinen Zweck
//! verfehlt. Ein Parse-Fehler im `hook`-Pfad geht deshalb in
//! `<git-dir>/minds/hook.log`, und der Lauf macht mit dem Verwertbaren weiter.
//!
//! Dasselbe gilt für den **kalten** Pfad: `checkpoint`, `prepare-commit-msg`
//! und `sync` laufen aus Git-Hooks, die ihre Ausgabe wegwerfen. Auch ihre
//! Fehler gehen in dieselbe Datei — siehe [`hooklog`]. Ohne das bräche ein
//! Tippfehler in `.minds/redact.json` die Erfassung dauerhaft und lautlos.

mod agent_help;
mod anchor_cmd;
mod audit;
mod blame;
mod brief_cmd;
mod checkpoint;
mod config;
mod context;
mod distill;
mod doctor;
mod enable;
mod enable_witness;
mod forget_cmd;
mod fsck;
mod gitlab_cmd;
mod hook;
mod hooklog;
mod import_cmd;
#[cfg(feature = "tui")]
mod inspect;
mod intent_cmd;
mod intent_issue;
mod intent_proof;
mod metrics;
mod prepare_commit_msg;
mod recall;
mod recap;
mod reinterpret_cmd;
mod render;
mod render_cmd;
mod replay_cmd;
mod review_cmd;
mod seals_cmd;
mod search;
mod show;
mod sign_cmd;
mod stack;
mod sync;
mod text;
mod verify_cmd;
mod why;
mod witness_cmd;

use std::process::ExitCode;

use minds_core::Decision;
use minds_store::StoreConfig;

const USAGE: &str = "\
minds — durable context for agent sessions, in Git.

Usage:
  minds enable [--agent <name>] [--child-repo <path>] [--child-remote <url>] [-v] [--ref <name>] [--recall]
        Sets the repo up for Minds: registers the hooks with the agent
        and in the repo and writes the store config to .git/config.
        Runs quietly; -v/--verbose shows every single step.
        Without --agent: all known agents. Idempotent, leaves foreign
        entries alone. Agents: claude-code, codex, cursor, gemini,
        opencode, all.
        --child-repo puts the context into a separate repo instead of
        in-repo; it is created (bare) or cloned from --child-remote.
        --recall (Claude Code): SessionStart hook that puts the context
        brief of previous sessions in front of the new session. Opt-in
        (costs tokens).
        --global-hooks: confirms a hooks directory outside the repo
        (e.g. a globally set core.hooksPath) — hooks there apply to all
        repositories. Without the flag, enable asks or aborts.
        --witness <profile>: also sets up a witnessed configuration.
        container: witness home + key, .devcontainer/ templates, a user
        service (systemd/launchd, not started). user: prints the setup
        steps. managed: writes managed-settings.minds-proposed.json.
        Never runs privileged commands, never overwrites existing files
        (a *.minds-proposed file is written next to them instead).

  minds hook --agent <name> [--event <name>]
        Accepts an agent hook event on stdin and stores it in the local
        journal. Always exits 0.

  minds checkpoint [--commit <id>]
        Interprets the journal, redacts (policy optionally from
        .minds/redact.json: allow/deny_secrets/deny_pii/secret_keys …),
        stores the sessions and appends the Minds-Session-Id trailer to
        HEAD. Called by the post-commit hook.

  minds witness init --repo <host-path> [--home <directory>] [--path-map <agent>=<host>]
                     [--profile container|user|managed] [--socket-group <group>]
                     [--child-repo <path>] [--policy-rev <rev>]
        init pins git dir, store and redaction policy (.minds/redact.json at
        HEAD or --policy-rev, read without config includes or replace refs)
        in witness.json and prints them with commit and blob id; a
        child-repo store only with --child-repo. Idempotent; never
        overwrites a different configuration.
  minds witness run [--home <directory>] [--follow]
  minds witness status [--home <directory>]
  minds witness keygen [--home <directory>]
        Creates a private Ed25519 witness key without overwriting an existing
        key. Prints its namespace-restricted allowed_signers line to stdout.
        Home: --home, MINDS_WITNESS_HOME, or the repository's XDG state path.

  minds show [<commit>] [--full]
        Shows intent and attribution of the session(s) behind a commit
        (default HEAD). Compact; --full shows prompt, all files and edges.

  minds why <file>:<line> [--full]
        Shows the session behind a single line (blame → trailer).

  minds blame [--lines] <file>
        Overview of which session sits behind which lines of a file,
        aggregated by session, with context coverage in percent.
        --lines prints one annotated line per source line instead of
        the session summary — the git-blame-shaped view.

  minds recall <target>
        Condenses the session(s) behind a file, a line (<file>:<line>) or
        a commit into a concise context brief.
        Includes the full intent request, wrapped to the terminal width.
        Deterministic, 0 tokens — the agent sibling of why.

  minds distill [--path <directory>] [--out <file>]
        Condenses the repo's history (or a path's) into an AGENTS.md
        draft: commands, hot files, dead ends, corrections.
        Without --out, to stdout.

  minds brief [<file>...]
        Size-bounded context block for the start of an agent session.
        Without paths, the whole repo.

  minds recap [--limit <n>] [--all]
        The most recent sessions at a glance (default 10; --all shows all).

  minds search <query>
        Searches intent, transcript and files of the captured sessions.

  minds inspect [<search> | <file>:<line>]
        How a change came to be, in the terminal: session list, a
        session's graph (intent → agent → effects → change → review) and
        a line's why chain. Tabs (Tab, F1–F4): Sessions, Verify — the
        verdict of minds verify at a glance (verdict, assurance, intent,
        integrity, coverage, artifact bar, scope) — and Changes —
        the diff of a commit as in git diff (unified, s for split) with
        each added line's evidence class and, beside it, why the line
        exists (session, step, what the agent said) — and Intent — the
        intent anchors (source, scope, proof, signature as minds verify
        checks it, snapshot) and the sessions naming them. Read-only. Live:
        reloads by itself when HEAD or a ref under refs/minds/ changes;
        r reloads at once.
        If stdout is not a console, the lines come tab-separated (for
        grep/fzf).

  minds agent-help
        Machine-readable command card (JSON) — for agents, not humans.

  minds metrics [--format prometheus|openmetrics|json]
        Metrics from the store (throughput, iteration, continuity, streak,
        redaction, context coverage). Default Prometheus, for Grafana.

  minds fsck [--require-review] [--require-seal]
             [--require-assurance <A0|A1|A2|A3>] [--signers <file>]
        Checks that every trailer is resolvable and reports journal gaps.
        Exit code 1 on orphaned trailers. --require-review: demands an
        approve for every agent-authored change (policy gate for CI).
        --require-assurance <level> demands that every session of an
        agent-authored commit reaches the level (exit 2 when not; never
        masks exit 1).

  minds doctor [--probe-home <directory>]
        Checks the setup, one line per check (ok / warn / fail): agent
        hooks, Git hooks, store config and the witness wiring.
        --probe-home (agent side): fails if the agent can open the witness
        home. Exit code 1 if any check fails.

  minds forget <session> [--reason <text>]
        GDPR erasure: replaces a session's payload with a tombstone.
        The reference stays resolvable, the content vanishes from the store.

  minds reinterpret <session>
        Reinterprets the preserved tool calls of a stored session with the
        current adapter — strictly read-only, the evidence stays
        unchanged.
  minds sign <session> [--key <path>]
  minds sign --seal <seal-id> [--key <path>]
        Signs a session's attribution (ssh-sig) to stdout.
        Key from --key or git config user.signingkey.

  minds intent bind --file <path> [--scope <glob,glob>]
        Binds a requirement version as an intent anchor
        (refs/minds/intents): source file:<path>@<git blob>, the redacted
        snapshot and the expected scope. Warns when the file's version is
        not in HEAD. Refused: a file the redaction policy would change,
        credential files (.env, .pgpass, …) and .gitignored files.
  minds intent bind --issue <group/project#iid> [--scope <glob,glob>]
                    [--gitlab-url <url>] [--allow-confidential]
        Binds a GitLab issue in its current version: source
        issue:<project>#<iid>@<updated_at>, snapshot = the redacted
        canonical JSON of title and description. Instance from
        --gitlab-url, MINDS_GITLAB_URL or CI_SERVER_URL (never
        .git/config; https only, http only for 127.0.0.1/[::1]); token only
        from MINDS_GITLAB_TOKEN. A confidential issue is refused unless
        --allow-confidential is given (refs/minds/ is synced).
  minds intent sign [<anchor-id>] [--key <path>] [--witness-home <dir>]
        Signs the anchor (default: last bound, only a file version in
        HEAD) under ssh-sig namespace minds-intent and activates it:
        through the witness control socket (host side; --witness-home,
        MINDS_WITNESS_HOME or the repository's XDG state path) or,
        without a witness, the local file
        <git-dir>/minds/intent/active (A1, unchained). Prints which.
        A FIDO key (sk-…) asks for a touch; key from --key or git config
        user.signingkey.
  minds intent show [<anchor-id>]
        Anchor, source, scope, signature and the redacted snapshot.
  minds intent list
        All stored intent anchors.

  minds seals [--session <id>] [--limit <n>]
        Lists Evidence-Chain seals — id, linked session (if any), event
        range, gap/signature status, timestamp. Most recent first.
        Without --session, every seal in the store; --limit caps how
        many print (applied after sorting).

  minds verify [<session|rev>] [--signers <file>] [--identity <id>]
               [--commit <rev>] [--require-explained <percent>] [--all]
               [--require-in-scope] [--witness-home <dir>]
               [--require-assurance <A0|A1|A2|A3>] [--limits]
               [--online [--gitlab-url <url>]]
        The evidence verdict: integrity × coverage over the session's seals.
        Defaults to HEAD; revisions use session trailers, then the store index.
        Multiple sessions print separate blocks; the worst verdict wins.
        The Coverage line adds the artifact coverage of the commit (the
        revision, --commit, or the session's trailer commit) and lists
        unexplained lines (at most 20 without --all). --require-explained
        fails with exit 2 below the given percentage (never masks 1/3/4).
        When the bound intent anchor declares a scope, the Coverage line
        counts paths outside it (commit files, write claims, file
        observations) and lists them as out of scope; --require-in-scope
        fails with exit 2 on any such path or when no scope can be
        assessed (never masks 1/3/4); combine with --require-assurance A2
        (and --witness-home) so the scope and the observations are not the
        agent's own, and with --require-explained 100: the session link
        (trailer) decides whose scope applies, and only the explained gate
        shows that this session produced the commit.
        Each block states the Assurance level (who observed, A0–A3), the
        Intent (minds intent: not bound / unsigned / signed under
        minds-intent, checked against --signers; never TAMPERED) and
        what is Not proven at that level (--limits: in full), and lists
        write claims no witness observation confirms (uncorroborated).
        --require-assurance gates on the weakest session (exit 2, never
        masks 1/3/4). --witness-home checks the witness ledger: a witnessed
        seal missing from the repository is TAMPERED.
        An intent bound to a GitLab issue adds an Issue version line:
        without --online \"not checked (offline)\"; with --online (token
        in MINDS_GITLAB_TOKEN, instance as for intent bind --issue)
        current, changed since binding (confirmed in, not found in or no
        description history) or version check unavailable. Never changes
        the assurance, the verdict or the exit code.
        A seal countersigned by minds anchor shows \"anchored: pipeline
        #N, <at>\" (counted for A3 only with a valid minds-anchor signature
        from --signers or ~/.ssh/allowed_signers). With --online (project
        from MINDS_GITLAB_PROJECT or CI_PROJECT_PATH) the notes of the
        merge requests containing the commit are compared: a signed note
        whose anchor ref or seal is missing is TAMPERED (\"anchor ref
        missing, MR note present\") when the note is older than the CI job
        (CI_JOB_STARTED_AT) or a valid local anchor is at least as new;
        otherwise, an edited anchor note, unvalidated entries or a failed
        check are exit 4. In GitLab CI only CI_SERVER_URL/CI_PROJECT_PATH
        count, the token must be read_api (never api), and a check that
        cannot run is exit 4. Fetch refs/minds/* explicitly first; next to
        the anchor job use needs: [minds-anchor].
        Exit codes: 0 VERIFIED, 1 TAMPERED, 2 \"VERIFIED, INCOMPLETE\",
        3 NOT VERIFIABLE, 4 operational failure (priority: 4 > 1 > 3 > 2 > 0).
  minds verify <session> --sig <file> [--signers <file>] [--identity <id>]
        Checks a signed attribution. Exit code is non-zero when invalid.
  minds verify --evidence <seal-id>
        The verdict of a single seal — even without a session
        (redaction block).

  minds replay [--commit <rev>] [--unsigned]
        Unix only. In CI, on a checkout of the commit (default HEAD): re-runs the
        decisive test and benchmark commands of its sessions — without a
        shell, with a cleared environment, only what .minds/replay.json in
        the commit allowlists — and compares them with what the sessions
        reported. Stores a replay record under refs/minds/anchors/replay/,
        signed (namespace minds-anchor) with the key in
        MINDS_ANCHOR_KEY_FILE; --unsigned stores it unsigned (never counts
        for A3). The replayed code runs as the same user and could read
        the key: signing is refused in merge/pull request pipelines — sign
        only on protected branches. The key must only reach those: a
        GitLab protected variable, or a GitHub environment secret with
        deployment rules for protected refs (never a repository secret).
        One key and principal per project; ephemeral, project-dedicated
        runners; pushes to the signing branch only by merge. Exit
        codes: 0 reproduced or skipped, 2 claim not reproduced,
        4 operational failure.

  minds anchor [--mirror]
        GitLab CI only, in push pipelines of the protected default branch
        (CI_COMMIT_REF_PROTECTED, CI_PIPELINE_SOURCE=push, CI_COMMIT_BRANCH
        = CI_DEFAULT_BRANCH, HEAD equals CI_COMMIT_SHA; never in merge
        request, trigger, API or tag pipelines). Countersigns the seals of
        the sessions the pushed commits name (CI_COMMIT_BEFORE_SHA..HEAD;
        needs GIT_DEPTH: 0): \"existed no later than pipeline #N\"
        (CI_PROJECT_PATH, CI_PIPELINE_ID, the CI clock), signed (namespace
        minds-anchor) with the private key file in MINDS_ANCHOR_KEY_FILE —
        never a developer key or ssh-agent — under
        refs/minds/anchors/first-sight/. An existing anchor is never
        overwritten (first sight wins); one not validly made with this key
        is reported and ends the run with exit 4. One key per project,
        scoped to the anchor job on protected runners. Fetch refs/minds/*
        before, push it after (git push --atomic); then --mirror posts the
        push's countersignatures made with this key (only those not yet in
        an own note) to the merge requests merged into the default branch,
        with its own bot token MINDS_ANCHOR_GITLAB_TOKEN (never the
        MINDS_GITLAB_TOKEN of verify; instance only CI_SERVER_URL). Missing
        CI variables: exit 4, nothing written. Run the job in a
        resource_group (oldest first), not interruptible, and push even
        when anchor exits 4. Never prints the key path or a token.
        Exit codes: 0 anchored, mirrored or nothing new,
        4 operational failure.

  minds review <subject> --approve|--reject|--needs-work [--summary <text>]
                          [--sign] [--key <path>]
        Creates a review verdict as a Git object (refs/minds/reviews).
        <subject> is a change id (I…) or session id (b3…).
        --sign signs it (ssh-sig) — a claim becomes proof.
        Key from --key or git config user.signingkey.

  minds reviews <subject> [--signers <file>] [--identity <id>]
        Shows verdicts and thread for a change id or session id.
        With --signers, the signatures are checked instead of just listed.

  minds comment <subject> [--on <file:line|turn:<n>>] \"<text>\"
        Attaches a remark to the review thread. The thread is an
        append-only log of content-addressed entries — two reviewers
        offline yield no conflict but a union.

  minds sync [--remote <name>] [--detach] [-v]
        Sends context and reviews to the remote — all due refs in one
        connection, never with --force; the only exception is the transfer
        of a GDPR erasure (tombstone ref). Called by the pre-push hook;
        without new refs the call costs no connection.
        --detach (the hook) hands the transport to a background process
        so the user's push does not wait for it.

  minds stack [--base <ref>]
        Shows the dependent changes above the base and their respective
        review state. Because the verdict hangs off the change id, it
        survives rebase and force-push.

  minds gitlab mirror <subject> --mr <nr> [--url <base>] [--project <id>]
                      [--token-env <var>] [--approve]
        Mirrors a change's verdicts as an MR note to GitLab — one-way
        and idempotent. The repo stays the source. Token only from the
        environment (default MINDS_GITLAB_TOKEN), never as an argument.

  minds gitlab webhook [--write] [--secret-env <var>]
        Reads a GitLab webhook payload from stdin and interprets an MR
        comment (/minds approve|reject|needs-work) as a verdict.
        Without --write, only shows what would be created. Opt-in, not a
        service. If MINDS_GITLAB_WEBHOOK_SECRET (or whatever --secret-env
        names) holds a secret, the X-Gitlab-Token header is required: the
        receiver passes it through in MINDS_GITLAB_WEBHOOK_TOKEN, compared
        in constant time; without a match the payload is discarded.

  minds audit --export [--out <file>] [--base <ref>] [--mode redacted|proof]
             [--signers <file>]
        Bundles the provenance chain (change → session → attribution →
        verdict) as a portable JSON file. Carries the canonical payloads
        and signatures — verifiable without this tool. States the bundle's
        assurance level (its weakest session) and exactly what it proves
        and does not prove at that level. --signers is the trusted
        allowed_signers for witness signatures (default
        ~/.ssh/allowed_signers); without it, no level above A1. Without
        --out, to stdout.

  minds render [--out <directory>]
        Builds a static HTML page over the context (default ./site):
        click a line → see the prompt behind it. Stateless.

  minds --version
  minds --help
";

/// Was ein Unterkommando an Argumenten kennt — die eine Quelle für den Parser.
struct Spec {
    /// Der Name des Unterkommandos.
    name: &'static str,
    /// Flags, auf die ein Wert folgt (`--name wert`).
    value_flags: &'static [&'static str],
    /// Flags ohne Wert.
    bool_flags: &'static [&'static str],
    /// Akzeptiert, aber in keiner Fehlermeldung genannt — interne Flags, die
    /// bewusst nicht in USAGE stehen.
    hidden_flags: &'static [&'static str],
    /// Wie viele positionale Argumente das Kommando höchstens nimmt. Auch
    /// Überzählige sind ein Fehler, keine Deko: `minds fsck require-review`
    /// (Bindestriche vergessen) lief sonst als nacktes `fsck` durch — dieselbe
    /// stille Abschaltung wie beim Flag-Tippfehler, nur ohne Bindestriche.
    positionals: usize,
}

const fn spec(
    name: &'static str,
    value_flags: &'static [&'static str],
    bool_flags: &'static [&'static str],
    positionals: usize,
) -> Spec {
    Spec {
        name,
        value_flags,
        bool_flags,
        hidden_flags: &[],
        positionals,
    }
}

/// Die Kommando-Tabelle — der Parser prüft strikt dagegen (#11).
///
/// Ein Flag, das hier nicht steht, ist ein **Fehler**, kein Rauschen: `minds
/// fsck --require-reviews` (Tippfehler) lief vorher als nacktes `fsck` durch
/// und lieferte Exit 0 — das CI-Gate war lautlos abgeschaltet. Und ein
/// Wert-Flag, dem versehentlich ein weiteres Flag folgt (`--summary --sign`),
/// fraß dieses als Wert — das Review entstand unsigniert, mit Erfolgsmeldung.
///
/// Ein Test hält [`agent_help`] mit dieser Tabelle im Gleichschritt; wer hier
/// ein Kommando ergänzt, bekommt den Zwang zur Karte geschenkt.
const SPECS: &[Spec] = &[
    Spec {
        name: "enable",
        value_flags: &[
            "--agent",
            "--child-repo",
            "--child-remote",
            "--ref",
            "--witness",
        ],
        bool_flags: &["-v", "--verbose", "--recall", "--global-hooks"],
        hidden_flags: &[enable::BACKGROUND_IMPORT_FLAG],
        positionals: 0,
    },
    spec("hook", &["--agent", "--event"], &[], 0),
    spec("checkpoint", &["--commit"], &[], 0),
    spec(
        "witness",
        &[
            "--home",
            "--repo",
            "--path-map",
            "--profile",
            "--socket-group",
            "--child-repo",
            "--policy-rev",
        ],
        &["--follow"],
        1,
    ),
    spec("show", &[], &["--full"], 1),
    spec("why", &[], &["--full"], 1),
    spec("blame", &[], &["--lines"], 1),
    spec("recall", &[], &[], 1),
    spec("distill", &["--path", "--out"], &[], 0),
    spec("brief", &[], &["--hook"], usize::MAX),
    spec("recap", &["--limit"], &["--all"], 0),
    spec("inspect", &[], &[], 1),
    spec("search", &[], &[], 1),
    spec("agent-help", &[], &[], 0),
    spec("metrics", &["--format"], &[], 0),
    spec(
        "fsck",
        &["--require-assurance", "--signers"],
        &["--require-review", "--require-seal"],
        0,
    ),
    spec("doctor", &["--probe-home"], &[], 0),
    spec("forget", &["--reason"], &[], 1),
    spec("reinterpret", &[], &[], 1),
    spec("sign", &["--key", "--seal"], &[], 1),
    spec(
        "intent",
        &[
            "--file",
            "--issue",
            "--scope",
            "--gitlab-url",
            "--key",
            "--witness-home",
        ],
        &["--allow-confidential"],
        2,
    ),
    spec("seals", &["--session", "--limit"], &[], 0),
    spec("replay", &["--commit"], &["--unsigned"], 0),
    spec("anchor", &[], &["--mirror"], 0),
    spec(
        "verify",
        &[
            "--sig",
            "--signers",
            "--identity",
            "--evidence",
            "--commit",
            "--require-explained",
            "--witness-home",
            "--require-assurance",
            "--gitlab-url",
        ],
        &["--all", "--limits", "--require-in-scope", "--online"],
        1,
    ),
    spec(
        "review",
        &["--summary", "--key"],
        &["--approve", "--reject", "--needs-work", "--sign"],
        1,
    ),
    spec("reviews", &["--signers", "--identity"], &[], 1),
    spec("comment", &["--on"], &[], 2),
    spec("sync", &["--remote"], &["-v", "--verbose", "--detach"], 0),
    spec("stack", &["--base"], &[], 0),
    spec(
        "gitlab",
        &["--mr", "--url", "--project", "--token-env", "--secret-env"],
        &["--approve", "--write"],
        2,
    ),
    spec(
        "audit",
        &["--out", "--base", "--mode", "--signers"],
        &["--export"],
        0,
    ),
    spec("render", &["--out"], &[], 0),
    spec("prepare-commit-msg", &[], &[], 1),
];

/// Kommandos, die in USAGE fehlen, weil sie kein Nutzer aufruft.
#[cfg(test)]
const INTERNAL: &[&str] = &["prepare-commit-msg"];

/// Die Namen der öffentlichen Kommandos — der Maßstab, gegen den
/// [`agent_help`] getestet wird. Nur im Test gebraucht: Zur Laufzeit fragt
/// niemand die Tabelle nach Öffentlichkeit, nur der Drift-Test tut es.
#[cfg(test)]
pub(crate) fn public_commands() -> impl Iterator<Item = &'static str> {
    SPECS
        .iter()
        .map(|spec| spec.name)
        .filter(|name| !INTERNAL.contains(name))
}

/// Das Ergebnis eines strikten Parse-Laufs.
#[derive(Debug)]
struct Parsed {
    values: Vec<(&'static str, String)>,
    bools: Vec<&'static str>,
    positionals: Vec<String>,
}

impl Parsed {
    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(flag, _)| *flag == name)
            .map(|(_, value)| value.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.bools.contains(&name)
    }

    fn positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }
}

/// Parst die Argumente **nach** dem Unterkommando strikt gegen dessen [`Spec`].
///
/// Die Regeln, jede gegen eine reale Fehlklasse (#11):
///
/// - Ein `-`-Argument, das die Tabelle nicht kennt, ist ein Fehler — nicht
///   Rauschen. Sonst schaltet ein Tippfehler das CI-Gate ab, bei Exit 0.
/// - Auf ein Wert-Flag darf kein weiteres Flag folgen. Sonst wird aus
///   `--summary --sign` eine Zusammenfassung namens „--sign", und das Review
///   entsteht unsigniert.
/// - Ein Wert-Flag darf nicht zweimal stehen — sonst gewönne still das erste,
///   und der Aufrufer glaubte, das zweite gelte.
/// - Überzählige Positionale sind derselbe Fehler ohne Bindestriche:
///   `minds fsck require-review` lief sonst als nacktes `fsck` durch, und
///   `minds forget a b` vergäße nur `a` — mit Erfolgsmeldung.
/// - Positionale und Flags sind reihenfolgeunabhängig: `verify --sig s.sig
///   b3-…` findet das Subjekt hinter dem Flag-Wert, nicht die Datei.
/// - `--` beendet die Flag-Deutung: Danach ist alles positional — der einzige
///   Weg, ein Argument auszusprechen, das mit `-` beginnt
///   (`minds comment I… -- "-1 zu diesem Ansatz"`).
fn parse(spec: &Spec, args: &[String]) -> Result<Parsed, String> {
    let mut parsed = Parsed {
        values: Vec::new(),
        bools: Vec::new(),
        positionals: Vec::new(),
    };

    let mut literal = false;
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if !literal {
            if arg == "--" {
                literal = true;
                i += 1;
                continue;
            }
            if let Some(&name) = spec.value_flags.iter().find(|&&flag| flag == arg) {
                if parsed.value(name).is_some() {
                    return Err(format!("das Flag {name} steht zweimal"));
                }
                match args.get(i + 1) {
                    Some(value) if !value.starts_with('-') => {
                        parsed.values.push((name, value.clone()));
                        i += 2;
                    }
                    Some(value) => {
                        return Err(format!(
                            "das Flag {name} braucht einen Wert — darauf folgt „{}“",
                            text::sanitize(value)
                        ));
                    }
                    None => return Err(format!("das Flag {name} braucht einen Wert")),
                }
                continue;
            }
            if let Some(&name) = spec
                .bool_flags
                .iter()
                .chain(spec.hidden_flags)
                .find(|&&flag| flag == arg)
            {
                parsed.bools.push(name);
                i += 1;
                continue;
            }
            if arg.starts_with('-') {
                return Err(unknown_flag(spec, arg));
            }
        }

        if parsed.positionals.len() >= spec.positionals {
            return Err(unexpected_positional(spec, arg));
        }
        parsed.positionals.push(arg.clone());
        i += 1;
    }

    Ok(parsed)
}

/// Die Meldung zu einem überzähligen positionalen Argument.
fn unexpected_positional(spec: &Spec, arg: &str) -> String {
    let arg = text::sanitize(arg);
    match spec.positionals {
        0 => format!(
            "unexpected argument \"{arg}\" — `minds {}` takes no positional arguments",
            spec.name
        ),
        1 => format!(
            "unexpected argument \"{arg}\" — `minds {}` takes at most one positional argument",
            spec.name
        ),
        n => format!(
            "unexpected argument \"{arg}\" — `minds {}` takes at most {n} positional arguments",
            spec.name
        ),
    }
}

/// Die Meldung zu einem unbekannten Flag — nennt, was das Kommando kennt,
/// damit der Tippfehler ohne Blick in die Doku auffindbar ist. Versteckte
/// Flags bleiben versteckt.
fn unknown_flag(spec: &Spec, arg: &str) -> String {
    let known: Vec<&str> = spec
        .value_flags
        .iter()
        .chain(spec.bool_flags)
        .copied()
        .collect();
    let arg = text::sanitize(arg);
    if known.is_empty() {
        format!("unknown flag {arg} — `minds {}` knows no flags", spec.name)
    } else {
        format!(
            "unknown flag {arg}\nknown for `minds {}`: {}",
            spec.name,
            known.join(", ")
        )
    }
}

fn main() -> ExitCode {
    // `args()` **panickt** bei einem Argument, das kein UTF-8 ist — in der
    // allerersten Zeile, vor jeder eigenen Vorkehrung. Für `minds hook` wäre
    // das der schlimmste Ort: Backtrace auf stderr und Exit 101, und die
    // Agent-Registrierung ruft ihn ohne `2>/dev/null` auf. `args_os` plus
    // verlustbehaftete Wandlung kann nicht scheitern; ein solches Argument
    // wird dann eben ein unbekanntes Flag und bekommt die übliche Meldung.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let Some(command) = args.first().map(String::as_str) else {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    };

    match command {
        "--version" | "-V" => {
            println!("minds {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        "--help" | "-h" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    let Some(spec) = SPECS.iter().find(|spec| spec.name == command) else {
        eprintln!("unbekanntes Unterkommando: {}\n", text::sanitize(command));
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };

    // Sobald feststeht, dass dieser Prozess ein Hook-Pfad ist, gelten die
    // Hook-Regeln — **ab hier**, nicht erst in `guarded`. Die Zusage aus #54
    // lautet „`minds hook` schreibt kein Byte auf stderr", und ein Panic im
    // Parser ginge sonst mit Exit 101 und vollem Backtrace an den Agenten:
    // Die Claude-Registrierung ruft `minds hook` **ohne** `2>/dev/null` auf,
    // anders als die drei Git-Hookbodies.
    //
    // `brief --hook` gehört dazu: Sein stdout *ist* der injizierte Kontext.
    // Und der Hintergrund-Import aus `enable`: Er läuft ohne Terminal, also
    // käme ein Panic oder Parse-Fehler dort nirgends an (#69).
    let hook_path = match spec.name {
        "hook" => Some(hooklog::Source::Hook),
        "checkpoint" => Some(hooklog::Source::Checkpoint),
        "prepare-commit-msg" => Some(hooklog::Source::PrepareCommitMsg),
        "sync" => Some(hooklog::Source::Sync),
        "brief" if args.iter().any(|a| a == "--hook") => Some(hooklog::Source::Brief),
        "enable" if args.iter().any(|a| a == enable::BACKGROUND_IMPORT_FLAG) => {
            Some(hooklog::Source::Import)
        }
        _ => None,
    };
    if let Some(source) = hook_path {
        hooklog::silence_panics_for(source);
    }

    let rest = &args[1..];

    // `minds fsck --help` soll die Hilfe zeigen, nicht „unbekanntes Flag" —
    // aber nur als **erstes** Argument. Ein `--help` irgendwo dahinter wäre
    // eine Hintertür: `minds fsck --require-reviews --help` endete sonst mit
    // Exit 0, und der Tippfehler bliebe unsichtbar — genau die Klasse, die
    // dieser Parser schließt. Und `hook` bleibt ganz draußen: Es darf kein
    // Byte auf stdout schreiben (stdout ist beim Agenten Steuerkanal).
    if spec.name != "hook" && rest.first().is_some_and(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let parsed = match parse(spec, rest) {
        Ok(parsed) => parsed,
        Err(message) => {
            if spec.name == "hook" {
                // Die Rekorder-Regel: `hook` endet immer mit 0, auch hier. Der
                // Fehler geht ins Log, und der Lauf macht mit dem weiter, was
                // sich aus den Argumenten noch lesen lässt — ein fremdes Flag
                // in einer Agent-Registrierung darf keine Session kosten.
                hooklog::log(hooklog::Source::Hook, &message);
                return hook::run(
                    flag(rest, "--agent").as_deref(),
                    flag(rest, "--event").as_deref(),
                );
            }
            // Die übrigen Hook-Pfade laufen aus Skripten und Konfigurationen,
            // die stderr wegwerfen — ihr Parse-Fehler gehört zusätzlich ins
            // Log, sonst stünde #10 eine Etage höher wieder offen: Ein
            // Hook-Body, der gegen dieses Binary driftet, bräche die Erfassung
            // dauerhaft und lautlos. `brief --hook` gehört dazu (#68); es fiel
            // bisher als einziges durch.
            //
            // Dieselbe Zuordnung wie oben, statt sie ein zweites Mal zu
            // schreiben. `hook` ist ausgenommen: Sein Notpfad steht darüber
            // und endet mit 0.
            if let Some(source) = hook_path.filter(|s| *s != hooklog::Source::Hook) {
                hooklog::log(source, &message);
            }
            eprintln!("minds {}: {message}", spec.name);
            return ExitCode::FAILURE;
        }
    };

    run(spec.name, &parsed)
}

/// Führt das geparste Kommando aus.
fn run(command: &str, parsed: &Parsed) -> ExitCode {
    match command {
        "enable" => {
            // Verstecktes internes Flag: `minds enable` startet den Backfill als
            // losgelösten Hintergrundprozess, der sich selbst hiermit aufruft.
            // Kein öffentliches `minds import` — der Nutzer sieht davon nichts
            // (steht bewusst nicht in USAGE).
            if parsed.has(enable::BACKGROUND_IMPORT_FLAG) {
                import_cmd::run()
            } else {
                let store = store_config_from(parsed);
                enable::run(
                    parsed.value("--agent"),
                    &store,
                    parsed.value("--child-remote"),
                    parsed.has("-v") || parsed.has("--verbose"),
                    parsed.has("--recall"),
                    parsed.has("--global-hooks"),
                    parsed.value("--witness"),
                )
            }
        }

        "hook" => hook::run(parsed.value("--agent"), parsed.value("--event")),

        "checkpoint" => checkpoint::run(parsed.value("--commit")),

        "show" => show::run(parsed.positional(0), parsed.has("--full")),

        "why" => why::run(parsed.positional(0), parsed.has("--full")),

        "blame" => blame::run(parsed.positional(0), parsed.has("--lines")),

        "recall" => recall::run(parsed.positional(0)),

        "distill" => distill::run(parsed.value("--path"), parsed.value("--out")),

        "brief" => brief_cmd::run(&parsed.positionals, parsed.has("--hook")),

        "recap" => recap::run(parsed.value("--limit"), parsed.has("--all")),

        "search" => search::run(parsed.positional(0)),

        #[cfg(feature = "tui")]
        "inspect" => inspect::run(parsed.positional(0)),
        // Ohne Feature bleibt das Kommando in SPECS (agent-help und USAGE
        // bleiben eine Quelle), sagt aber ehrlich, warum es nichts tut.
        #[cfg(not(feature = "tui"))]
        "inspect" => {
            eprintln!("minds inspect: dieses Binary wurde ohne das Feature `tui` gebaut");
            ExitCode::FAILURE
        }

        "agent-help" => agent_help::run(),

        "metrics" => metrics::run(parsed.value("--format")),

        "review" => {
            let decision = if parsed.has("--approve") {
                Some(Decision::Approve)
            } else if parsed.has("--reject") {
                Some(Decision::Reject)
            } else if parsed.has("--needs-work") {
                Some(Decision::NeedsWork)
            } else {
                None
            };
            review_cmd::run_review(
                parsed.positional(0),
                decision,
                parsed.value("--summary"),
                parsed.has("--sign"),
                parsed.value("--key"),
            )
        }

        "audit" => audit::run(
            parsed.has("--export"),
            parsed.value("--out"),
            parsed.value("--base"),
            parsed.value("--mode"),
            parsed.value("--signers"),
        ),

        "witness" => witness_cmd::run(parsed),

        "gitlab" => gitlab_cmd::run(
            parsed.positional(0),
            gitlab_cmd::Options {
                subject: parsed.positional(1),
                merge_request: parsed.value("--mr"),
                url: parsed.value("--url"),
                project: parsed.value("--project"),
                token_env: parsed.value("--token-env"),
                secret_env: parsed.value("--secret-env"),
                approve: parsed.has("--approve"),
                write: parsed.has("--write"),
            },
        ),

        "stack" => stack::run(parsed.value("--base")),

        "comment" => review_cmd::run_comment(
            parsed.positional(0),
            parsed.value("--on"),
            parsed.positional(1),
        ),

        "reviews" => review_cmd::run_reviews(
            parsed.positional(0),
            parsed.value("--signers"),
            parsed.value("--identity"),
        ),

        "fsck" => fsck::run(fsck::Options {
            require_review: parsed.has("--require-review"),
            require_seal: parsed.has("--require-seal"),
            require_assurance: parsed.value("--require-assurance"),
            signers: parsed.value("--signers"),
        }),

        "doctor" => doctor::run(parsed.value("--probe-home")),

        "forget" => forget_cmd::run(parsed.positional(0), parsed.value("--reason")),

        "reinterpret" => reinterpret_cmd::run(parsed.positional(0)),

        "sign" => sign_cmd::run(
            parsed.positional(0),
            parsed.value("--key"),
            parsed.value("--seal"),
        ),

        "intent" => intent_cmd::run(parsed),

        "seals" => seals_cmd::run(parsed.value("--session"), parsed.value("--limit")),

        "replay" => replay_cmd::run(parsed.value("--commit"), parsed.has("--unsigned")),

        "anchor" => anchor_cmd::run(parsed.has("--mirror")),

        "verify" => verify_cmd::run(
            parsed.positional(0),
            parsed.value("--sig"),
            parsed.value("--signers"),
            parsed.value("--identity"),
            parsed.value("--evidence"),
            verify_cmd::ArtifactOptions {
                commit: parsed.value("--commit"),
                require_explained: parsed.value("--require-explained"),
                all: parsed.has("--all"),
                require_in_scope: parsed.has("--require-in-scope"),
            },
            verify_cmd::AssuranceOptions {
                witness_home: parsed.value("--witness-home"),
                require_assurance: parsed.value("--require-assurance"),
                limits: parsed.has("--limits"),
                online: parsed.has("--online"),
                gitlab_url: parsed.value("--gitlab-url"),
            },
        ),

        "render" => render_cmd::run(parsed.value("--out")),

        "sync" => sync::run(
            parsed.value("--remote"),
            parsed.has("-v") || parsed.has("--verbose"),
            parsed.has("--detach"),
        ),

        // Interner Git-Hook: sorgt für eine stabile Change-Id (nicht in USAGE).
        "prepare-commit-msg" => prepare_commit_msg::run(parsed.positional(0)),

        // `main` dispatcht nur Namen aus SPECS — jeder davon hat hier einen Arm.
        other => unreachable!("Kommando in SPECS, aber ohne Arm: {other}"),
    }
}

/// Liest `--name wert` lax aus rohen Argumenten — nur noch für den
/// `hook`-Notpfad, wenn der strikte Parser ablehnt: Der Rekorder holt sich,
/// was lesbar ist, statt eine Session zu verlieren.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Baut die [`StoreConfig`] aus `--child-repo`/`--ref`. Ohne Flags: In-Repo mit
/// Default-Ref — die Einstellung, für die niemand etwas tun muss.
fn store_config_from(parsed: &Parsed) -> StoreConfig {
    let base = match parsed.value("--child-repo") {
        Some(path) => StoreConfig::child_repo(path),
        None => StoreConfig::in_repo(),
    };
    match parsed.value("--ref") {
        Some(reference) => base.with_ref(reference),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_named(name: &str) -> &'static Spec {
        SPECS
            .iter()
            .find(|spec| spec.name == name)
            .expect("Kommando steht in SPECS")
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    /// Die Regression, die #11 eröffnet hat: Ein Tippfehler im Gate-Flag lief
    /// als nacktes `fsck` durch — Exit 0, CI-Gate lautlos abgeschaltet.
    #[test]
    fn a_flag_typo_is_an_error_that_names_the_alternatives() {
        let err = parse(spec_named("fsck"), &args(&["--require-reviews"])).unwrap_err();
        assert!(err.contains("unknown flag"), "{err}");
        // Die Meldung nennt das richtige Flag — der Tippfehler ist ohne Blick
        // in die Doku auffindbar.
        assert!(err.contains("--require-review"), "{err}");
    }

    /// `minds review I… --summary --sign` legte das Review mit der
    /// Zusammenfassung „--sign" an — unsigniert, mit Erfolgsmeldung.
    #[test]
    fn a_value_flag_never_eats_the_following_flag() {
        let err = parse(
            spec_named("review"),
            &args(&["I0123", "--summary", "--sign"]),
        )
        .unwrap_err();
        assert!(err.contains("--summary"), "{err}");
        assert!(err.contains("braucht einen Wert"), "{err}");
    }

    /// Ein Wert-Flag am Zeilenende ist derselbe Fehler, nur ohne Nachfolger.
    #[test]
    fn a_value_flag_at_the_end_is_an_error() {
        let err = parse(spec_named("checkpoint"), &args(&["--commit"])).unwrap_err();
        assert!(err.contains("braucht einen Wert"), "{err}");
    }

    /// Flags und Positionale dürfen in jeder Reihenfolge stehen — Agents
    /// generieren beide Varianten.
    #[test]
    fn flags_and_positionals_are_order_independent() {
        for order in [
            &["I0123", "--summary", "gut", "--sign"][..],
            &["--summary", "gut", "I0123", "--sign"][..],
            &["--sign", "--summary", "gut", "I0123"][..],
        ] {
            let parsed = parse(spec_named("review"), &args(order)).unwrap();
            assert_eq!(parsed.positional(0), Some("I0123"), "{order:?}");
            assert_eq!(parsed.value("--summary"), Some("gut"), "{order:?}");
            assert!(parsed.has("--sign"), "{order:?}");
        }
    }

    /// `minds verify --sig s.sig b3-…`: Das Subjekt ist die Session, nicht die
    /// Signatur-Datei — der Flag-Wert zählt nicht als Positional.
    #[test]
    fn the_verify_subject_is_not_the_signature_file() {
        let parsed = parse(spec_named("verify"), &args(&["--sig", "s.sig", "b3-abc"])).unwrap();
        assert_eq!(parsed.positional(0), Some("b3-abc"));
        assert_eq!(parsed.value("--sig"), Some("s.sig"));
    }

    #[test]
    fn verify_accepts_zero_or_one_positional() {
        assert!(parse(spec_named("verify"), &args(&[])).is_ok());
        assert!(parse(spec_named("verify"), &args(&["HEAD~1"])).is_ok());
        assert!(parse(spec_named("verify"), &args(&["HEAD", "HEAD~1"])).is_err());
    }

    /// `minds gitlab mirror --mr 5 I…`: Das Subjekt ist die Change-Id, nicht
    /// die MR-Nummer.
    #[test]
    fn the_gitlab_subject_is_not_the_mr_number() {
        let parsed = parse(
            spec_named("gitlab"),
            &args(&["mirror", "--mr", "5", "I0123"]),
        )
        .unwrap();
        assert_eq!(parsed.positional(0), Some("mirror"));
        assert_eq!(parsed.positional(1), Some("I0123"));
        assert_eq!(parsed.value("--mr"), Some("5"));
    }

    /// Das interne Backfill-Flag funktioniert, bleibt aber unerwähnt — es
    /// steht bewusst nicht in USAGE, also auch nicht in Fehlermeldungen.
    #[test]
    fn hidden_flags_are_accepted_but_not_advertised() {
        let spec = spec_named("enable");
        let parsed = parse(spec, &args(&[enable::BACKGROUND_IMPORT_FLAG])).unwrap();
        assert!(parsed.has(enable::BACKGROUND_IMPORT_FLAG));

        let err = unknown_flag(spec, "--tippfehler");
        assert!(
            !err.contains("background"),
            "internes Flag in der Meldung: {err}"
        );
    }

    /// `minds blame --lines <datei>`: Das Flag ist in der Tabelle, der
    /// Tippfehler daneben nicht — sonst liefe `--linez` als nacktes `blame`
    /// durch und lieferte still die falsche Ansicht, mit Exit 0.
    #[test]
    fn blame_knows_lines_and_rejects_the_typo_next_to_it() {
        let parsed = parse(spec_named("blame"), &args(&["--lines", "src/retry.rs"])).unwrap();
        assert!(parsed.has("--lines"));
        assert_eq!(parsed.positional(0), Some("src/retry.rs"));

        let err = parse(spec_named("blame"), &args(&["--linez", "src/retry.rs"])).unwrap_err();
        assert!(err.contains("unknown flag"), "{err}");
        assert!(err.contains("--lines"), "{err}");
    }

    /// Zwei Kommandos mit demselben Namen wären ein stiller Dispatch-Fehler.
    #[test]
    fn spec_names_are_unique() {
        let mut names: Vec<&str> = SPECS.iter().map(|spec| spec.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SPECS.len());
    }

    /// Der Tippfehler ohne Bindestriche: `minds fsck require-review` lief als
    /// nacktes `fsck` durch — dieselbe stille Gate-Abschaltung, nur positional.
    #[test]
    fn an_excess_positional_is_an_error_not_decoration() {
        let err = parse(spec_named("fsck"), &args(&["require-review"])).unwrap_err();
        assert!(err.contains("unexpected argument"), "{err}");

        // Und bei begrenzter Stelligkeit zählt die Grenze: `forget a b` vergäße
        // sonst nur `a` — mit Erfolgsmeldung.
        let err = parse(spec_named("forget"), &args(&["b3-a", "b3-b"])).unwrap_err();
        assert!(err.contains("unexpected argument"), "{err}");
    }

    /// Ein doppelt gesetztes Wert-Flag ist eine Entscheidung, die der Parser
    /// nicht still treffen darf — vorher gewann lautlos das erste.
    #[test]
    fn a_duplicate_value_flag_is_an_error() {
        let err = parse(
            spec_named("review"),
            &args(&["I0123", "--summary", "a", "--summary", "b"]),
        )
        .unwrap_err();
        assert!(err.contains("zweimal"), "{err}");
    }

    /// `--` beendet die Flag-Deutung — der einzige Weg, ein Argument
    /// auszusprechen, das mit `-` beginnt.
    #[test]
    fn a_double_dash_makes_the_rest_positional() {
        let parsed = parse(
            spec_named("comment"),
            &args(&["I0123", "--", "-1 zu diesem Ansatz"]),
        )
        .unwrap();
        assert_eq!(parsed.positional(0), Some("I0123"));
        assert_eq!(parsed.positional(1), Some("-1 zu diesem Ansatz"));
    }

    /// Ein Flag-Wert mit Steuerzeichen darf die Fehlermeldung nicht fälschen —
    /// sie landet via hook.log auch in Dateien, die andere lesen.
    #[test]
    fn error_messages_sanitize_foreign_text() {
        let err = parse(
            spec_named("review"),
            &args(&["--summary", "--x\u{1b}[31mrot"]),
        )
        .unwrap_err();
        assert!(
            !err.contains('\u{1b}'),
            "rohes Escape in der Meldung: {err}"
        );
    }
}
