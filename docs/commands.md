# Command Reference

`minds` captures the context of AI-agent coding sessions in Git — intent, attribution, redaction, evidence chain, and reviews — and makes it queryable from the terminal. `minds --help` prints the same command list in the terminal.

## Setup

### minds enable

```
minds enable [--agent <name>] [--child-repo <path>] [--child-remote <url>] [-v] [--ref <name>] [--recall] [--global-hooks]
```

Prepares the repository for Minds: registers the hooks with the agent and the repo, and writes the store config to `.git/config`. Runs silently; `-v`/`--verbose` shows each step. Without `--agent` it configures all known agents (`claude-code`, `codex`, `cursor`, `gemini`, `opencode`, `all`); the command is idempotent and leaves third-party hooks untouched. `--child-repo` places the context in a separate repository instead of in-repo (created bare, or cloned from `--child-remote`); `--recall` (Claude Code) adds an opt-in SessionStart hook that prepends the context brief from previous sessions to a new session (costs tokens); `--global-hooks` confirms a hooks directory outside the repo (e.g. a globally set `core.hooksPath`) — without the flag, enable asks or aborts.

```
minds enable --agent claude-code -v
```

## Daily use

### minds show

```
minds show [<commit>] [--full]
```

Shows the intent and attribution of the session(s) behind a commit (default `HEAD`). The output is compact; `--full` adds the prompt, all files, and edges.

```
minds show HEAD~2 --full
```

### minds why

```
minds why <file>:<line> [--full]
```

Shows the session behind a single line, resolved via `git blame` and the session trailer.

```
minds why src/lib.rs:42
```

### minds blame

```
minds blame <file>
```

Shows which session is behind which lines of a file, aggregated per session, with context coverage as a percentage.

```
minds blame crates/minds-core/src/evidence.rs
```

### minds recap

```
minds recap [--limit <n>] [--all]
```

Lists the most recent sessions at a glance. Shows 10 by default; `--limit` changes the count and `--all` shows everything.

```
minds recap --limit 25
```

### minds search

```
minds search <query>
```

Searches the intent, transcript, and files of the captured sessions.

```
minds search "retry backoff"
```

### minds inspect

```
minds inspect [<query> | <file>:<line>]
```

Shows how a change came to be, in the terminal: a session list, the graph of a session (intent → agent → effects → change → review), and the why-chain of a line. Strictly read-only. When stdout is not a terminal, lines are emitted tab-separated for `grep`/`fzf`.

```
minds inspect src/main.rs:10
```

### minds recall

```
minds recall <target>
```

Condenses the session(s) behind a file, a line (`<file>:<line>`), or a commit into a short context brief. Deterministic and costs 0 tokens — the agent-facing sibling of `why`.

```
minds recall src/lib.rs:42
```

## Agent context

### minds brief

```
minds brief [<file>...]
```

Emits a size-capped context block for the start of an agent session. Without paths it covers the whole repository.

```
minds brief src/lib.rs src/main.rs
```

### minds distill

```
minds distill [--path <directory>] [--out <file>]
```

Condenses the history of the repository (or of a path) into an `AGENTS.md` draft: commands, hot files, dead ends, corrections. Without `--out` it writes to stdout.

```
minds distill --path crates/minds-cli --out AGENTS.md
```

### minds agent-help

```
minds agent-help
```

Prints a machine-readable command map as JSON — meant for agents, not humans.

```
minds agent-help | jq .
```

## Reviews

### minds review

```
minds review <subject> --approve|--reject|--needs-work [--summary <text>] [--sign] [--key <path>]
```

Records a review verdict as a Git object under `refs/minds/reviews`. `<subject>` is a change id (`I…`) or a session id (`b3…`). `--sign` signs the verdict (ssh-sig), turning a claim into evidence; the key comes from `--key` or `git config user.signingkey`.

```
minds review I4f2a9c --approve --summary "LGTM" --sign
```

### minds reviews

```
minds reviews <subject> [--signers <file>] [--identity <id>]
```

Shows the verdicts and the comment thread for a change id or session id. With `--signers`, signatures are verified instead of merely reported.

```
minds reviews I4f2a9c --signers .minds/allowed_signers
```

### minds comment

```
minds comment <subject> [--on <file:line|turn:<n>>] "<text>"
```

Appends a comment to the review thread. The thread is an append-only log of content-addressed entries — two reviewers working offline produce a union, not a conflict.

```
minds comment I4f2a9c --on src/lib.rs:42 "Prefer a bounded channel here."
```

### minds stack

```
minds stack [--base <ref>]
```

Shows the dependent changes above the base and the review state of each. Because the verdict is attached to the change id, it survives rebase and force-push.

```
minds stack --base origin/main
```

## Evidence & compliance

### minds verify

```
minds verify <session> [--signers <file>] [--identity <id>]
minds verify <session> --sig <file> [--signers <file>] [--identity <id>]
minds verify --evidence <seal-id>
```

Renders the evidence verdict: integrity × coverage across the seals of a session. Exit codes: 0 VERIFIED, 1 TAMPERED, 2 INCOMPLETE, 3 NOT VERIFIABLE. With `--sig` it checks a signed attribution and exits non-zero when the signature is invalid. `--evidence` yields the verdict for a single seal, even without a session (redaction block).

```
minds verify b3a1f0e --signers .minds/allowed_signers
```

### minds sign

```
minds sign <session> [--key <path>]
minds sign --seal <seal-id> [--key <path>]
```

Signs the attribution of a session (ssh-sig) and writes the signature to stdout. The key comes from `--key` or `git config user.signingkey`; `--seal` signs a single seal instead of a session.

```
minds sign b3a1f0e --key ~/.ssh/id_ed25519 > attribution.sig
```

### minds fsck

```
minds fsck [--require-review]
```

Checks that every trailer resolves and reports journal gaps. Exits non-zero when orphaned trailers exist. `--require-review` also requires an approval for every agent-authored change — a policy gate for CI.

```
minds fsck --require-review
```

### minds forget

```
minds forget <session> [--reason <text>]
```

GDPR deletion: replaces the payload of a session with a tombstone. The reference stays resolvable while the content disappears from the store.

```
minds forget b3a1f0e --reason "customer data in prompt"
```

### minds reinterpret

```
minds reinterpret <session>
```

Re-interprets the preserved tool calls of a stored session with the current adapter state. Strictly read-only — the evidence remains unchanged.

```
minds reinterpret b3a1f0e
```

### minds audit

```
minds audit --export [--out <file>] [--base <ref>] [--mode redacted|proof]
```

Bundles the provenance chain (change → session → attribution → verdict) into a portable JSON file. It contains the canonical payloads and signatures and is verifiable without this tool. Without `--out` it writes to stdout.

```
minds audit --export --base origin/main --mode proof --out audit.json
```

## Sync & integration

### minds sync

```
minds sync [--remote <name>] [--detach] [-v]
```

Pushes context and reviews to the remote — all pending refs in one connection, never with `--force`; the only exception is transmitting a GDPR deletion (tombstone ref). Invoked by the pre-push hook; with no new refs the call opens no connection. `--detach` (used by the hook) hands the transport to a background process so the user's push does not wait on it.

```
minds sync --remote origin -v
```

### minds gitlab mirror

```
minds gitlab mirror <subject> --mr <nr> [--url <base>] [--project <id>] [--token-env <var>] [--approve]
```

Mirrors the verdicts of a change to GitLab as an MR note — one-way and idempotent; the repository remains the source of truth. The token is read only from the environment (default `MINDS_GITLAB_TOKEN`), never passed as an argument.

```
minds gitlab mirror I4f2a9c --mr 137 --project 42
```

### minds gitlab webhook

```
minds gitlab webhook [--write] [--secret-env <var>]
```

Reads a GitLab webhook payload from stdin and interprets an MR comment (`/minds approve|reject|needs-work`) as a verdict. Without `--write` it only shows what would be created; the feature is opt-in and runs no service. If `MINDS_GITLAB_WEBHOOK_SECRET` (or the variable named by `--secret-env`) holds a secret, the `X-Gitlab-Token` header is required: the receiver passes it through in `MINDS_GITLAB_WEBHOOK_TOKEN`, the comparison is timing-safe, and on mismatch the payload is discarded.

```
minds gitlab webhook --write < payload.json
```

### minds metrics

```
minds metrics [--format prometheus|openmetrics|json]
```

Exports metrics from the store: throughput, iteration, continuity, streak, redaction, and context coverage. The default format is Prometheus, ready for Grafana.

```
minds metrics --format json
```

### minds render

```
minds render [--out <directory>]
```

Builds a static HTML site from the context (default `./site`): click a line to see the prompt behind it. Stateless.

```
minds render --out public
```

## Plumbing — called by hooks, rarely by hand

### minds hook

```
minds hook --agent <name> [--event <name>]
```

Accepts an agent hook event on stdin and stores it in the local journal. Always exits 0.

```
minds hook --agent claude-code --event PostToolUse < event.json
```

### minds checkpoint

```
minds checkpoint [--commit <id>]
```

Interprets the journal, redacts it (policy optionally from `.minds/redact.json`: `allow`, `deny_secrets`, `deny_pii`, `secret_keys`, …), stores the sessions, and appends the Minds session-id trailer to `HEAD`. Invoked by the post-commit hook.

```
minds checkpoint --commit 76a1b3d
```
