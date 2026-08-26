# ADR-0005 — Context reinjection: deterministic, honest, 0 tokens

- Status: accepted
- Date: 2026-07-28
- Affects: `minds-core`, `minds-reader`, `minds-capture`, `minds-cli`
- Extends: ADR-0003 (hooks over transcript parsing), ADR-0004 (import & store index)

## Context

The vision names four problems. `minds show`/`why`/`render` solve #1 (the MR as
a guessing game) and #4 (the audit gap). **Problem #3 — "no agent learns from
what the last one did" — was open.** A session's knowledge (working commands,
dead ends, touched files) has long been sitting, redacted, in the store; nobody
was reading it back.

Third-party tools like *Grain* (on entire.io) show the way: distill from the
session history a brief or an `AGENTS.md` that the next agent reads.

## Decision 1: deterministic, not generated

The reinjection extracts **observed facts**, it invents no prose. No model in
the path — same sessions ⇒ byte-identical brief (golden-tested). The optional
LLM summary path remains deferred to M8 as planned, then with content-hash
caching over the `SessionId`.

The pure core is `minds_core::extract` (`Extract::from_sessions`); the Markdown
surface is `minds_reader::brief`. Both without I/O, both golden-tested.

## Decision 2: strong vs. heuristic is visibly separated

Not every signal is equally reliable, and the brief says so:

- **strong**, because read from the normalized `Effect`: working commands
  (Exec), hot files, co-change clusters.
- **heuristic**, because guessed from patterns or free text: rework (churn) and
  corrections (correction language in a user turn) — explicitly labeled
  "(heuristic)" in the brief.

"Conventions" as style rules are deliberately **not** produced: those would
need the code itself, or a model.

## Decision 3: three commands by direction of view

- `minds recall <target>` — retrospective/targeted: the brief behind a file,
  line, or commit. The agent sibling of `why`.
- `minds distill [--path|--out]` — cumulative/repo-wide: an `AGENTS.md`
  **draft**. Merging into an existing file is deliberately left to the human
  (v0.3).
- `minds brief [<file>...] [--hook]` — forward-looking/session start:
  size-capped so the agent input stays small (headroom consideration).

## Decision 4: reinjection into the agent is opt-in

`minds enable --recall` registers a SessionStart hook for Claude Code that
emits `minds brief --hook`; its `hookSpecificOutput.additionalContext` is
prepended by Claude to the new session. **Opt-in**, because it costs agent
tokens. The envelope contract is agent-specific — other agents follow once
their format is verified.

## Honest about the limits

- **`intent.discarded`** is filled deterministically at checkpoint time — from
  the pattern "file written and removed again". Since the Claude adapter knows
  **no** `Delete` effect (deletion runs via `Bash rm`), the removal is also
  read from `rm`/`git rm` commands. A future adapter with a real `Delete`
  effect will slot in automatically.
- **`intent.constraints`** stays empty: there is no reliable deterministic
  signal for it. A guessed constraint would be worse than none.
- The command de-noising (`cd …` dropped, pipe head, truncation) is a named
  heuristic; it groups `cargo clippy … | grep x/y` into **one** fact.

## Consequences

Problem #3 is addressed — without a model, without network, without new state;
the data was already in the store. The quality of the reinjection rises
automatically with the quality of the capture (track A: more agents with real
effects → richer facts).
