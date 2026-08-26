# ADR-0003 — Capture via agent hooks over transcript parsing

- Status: accepted
- Date: 2026-07-23
- Affects: `minds-capture`, `minds-cli`
- Supersedes: the M5 sketch (reading the transcript files after the fact)

## Context

The first M5 sketch wanted to parse the agents' log files after the fact: one
`SessionAdapter` trait per agent, fed from its transcript. Building it surfaced
three problems, the third of which was decisive.

1. **Transience.** Claude Code deletes its transcripts after 30 days. Whatever
   is not imported in time is irretrievably gone. A capture that runs after
   the fact loses evidence just by waiting.

2. **Format diversity.** Every agent has its own log, and they change
   constantly. That is the grunt work the vision already names as the heaviest
   burden.

3. **Ordering across agents — the real reason.** A transcript parser only ever
   sees *one* transcript. It therefore cannot, in principle, know that Codex
   wrote a review between two Claude turns. The statement central to Minds —
   "Claude plans, Codex reviews, Claude implements the review points" — cannot
   be proven from a single log, only *suspected*.

## Decision

We adopt the hook approach of [entire.io](https://entire.io), implemented in
Rust: **Minds installs hooks in the agent itself** and receives every event live
via a tiny command `minds hook`.

- If *every* agent hook calls the same binary, which writes to *the same*
  journal, then events from different agents are recorded by **one observer
  with one clock**. The edge between them thus becomes `Evidence::Observed`
  instead of `Inferred` — observed, not guessed.
- The journal lives under `<git-dir>/minds/journal/` (0600, no Git object, not
  in the worktree). The hook is **fail-open** and always exits 0; it must never
  abort the user's session.
- The transcript does not become superfluous; it changes roles: the hook
  provides timing, ordering, and causality; the transcript provides the rich
  content (full text, thinking, token counts). The two halves are only merged
  at checkpoint time (cold, fail-closed).

### Installation per agent (`minds enable`)

| Agent       | Target                                                          |
|-------------|-----------------------------------------------------------------|
| Claude Code | `.claude/settings.json` (`hooks`)                               |
| Codex       | `.codex/hooks.json` + `codex_hooks = true` in `config.toml`     |
| Cursor      | `.cursor/hooks.json`                                            |
| Gemini      | `.gemini/settings.json`                                         |
| OpenCode    | TypeScript plugin                                               |

In addition, Git hooks (`post-commit`/`prepare-commit-msg`): a checkpoint is
created when you or the agent commit. All merges are **idempotent** — a second
`minds enable` changes nothing, and someone else's configuration in the same
files is preserved.

## Consequences

**Good.** Evidence is created live and completely; ordering across agents is
observed, not guessed; a new agent costs one hook registration instead of a
transcript parser.

**Price.** Fail-open means: an event *can* be missing. That is why every event
carries a gapless sequence number, and `ReadOutcome::gaps` makes anything
missing visible — honestly incomplete beats silently complete. `minds enable`
has to know five agent formats and merge them cleanly; that is the remaining
grunt work, but one that occurs once per agent, not on every format change.

**Additive at the core.** The core extensions (`Lineage`, `Turn.parent/at`,
`ToolCall.effect`, `Vec<Edge>`) all carry `skip_serializing_if`. A session
without lineage serializes byte-identically to before M5 and keeps its
`SessionId`; `SCHEMA_VERSION` stays at 1.
