# ADR-0004 — Transcript import as backfill, linked via a store index

- Status: accepted
- Date: 2026-07-24
- Affects: `minds-capture`, `minds-store`, `minds-cli`, `minds-reader`
- Extends: ADR-0003 (hooks over transcript parsing)

## Context

ADR-0003 replaced after-the-fact transcript parsing with live hooks. That
solves capture *from* the moment `minds enable` ran — but not before. Anyone
setting up Minds late in a repo would have no context for all the work done so
far, even though the agents' transcripts still sit on disk (Claude Code:
`~/.claude/projects/<slug>/<session>.jsonl`).

At the same time, the captured context is supposed to reach a shared GitLab
repo — not just the tiny trailer in the commit message, but the sessions
themselves.

## Decision 1: import is backfill, not a reversal

The import reads existing agent transcripts and builds sessions from them. This
is **not a revocation of ADR-0003**: the live path remains the hook. The import
is the one-time gleaning of what happened *before* setup — best effort,
explicitly marked as such.

- It is triggered **automatically by `minds enable`**, in the background. No
  extra command.
- Claude Code has a real reader (format known). For Codex, Cursor, Gemini, and
  OpenCode the readers are scaffolds with best-guess format assumptions; where
  a format does not fit, **nothing** is imported (fail-open) and that is
  honestly reported, rather than storing something wrong.

## Decision 2: linking via a store index, never via history rewrite

A hook-captured session gets its trailer via `amend` on **HEAD** — safe,
because only the freshest commit is rewritten. An *imported* contribution
belongs to **old** commits. Writing the trailer there would mean rewriting
history from the earliest match onward; on a pushed repo, that breaks every
clone. That is ruled out.

Instead: a **store index** as data next to the sessions.

```text
refs/minds/context/
  sessions/b3/<hash>.json     # the session (as before)
  index.json                  # commit → [ {session, evidence} ]
```

- The index is **not** written into commit messages. Old commits stay byte for
  byte as they are.
- `minds show`/`why`/`render`/`fsck` read **both** sources: the trailer
  (`Evidence::Observed`, the authoritative direction) and the index
  (`Evidence::Inferred`, the heuristic one). The reader shows "inferred" in
  gray.
- The session → commit mapping is heuristic: the files written by the session,
  intersected with the files of a commit within the session's time window.
  Hence `Inferred` and not `Observed` — it is a good guess, not an observation.

The index also makes good on the "store index" that `checkpoint` and `edges`
already referred to (the symbolic resolution session ↔ commit). And
because it lives in `refs/minds/context`, it travels along when the ref is
pushed — exactly what scenario 2 (context on GitLab) needs.

## Decision 3: sessions reach GitLab via a push/fetch refspec

`git push` does not send `refs/minds/context` along by itself. `minds enable`
therefore configures a push/fetch refspec (`remote.<name>.push` and `fetch`),
so the context ref travels with a normal `git push`/`git fetch` — directly for
in-repo, into its own remote for the child repo. That closes the gap against
definition-of-done item 1 ("`minds enable` configures backend **+ refspec**").

## Consequences

**Good.** Set up Minds late and you still see context retroactively. The context
reaches GitLab. None of this rewrites code history.

**Price.** Imported links are guesses and marked as such; a wrongly guessed
mapping is possible and recognizable in the reader as "inferred". The four
non-Claude readers are placeholders until format verification. With
`index.json`, the store gets its first neighbor next to `sessions/` — the
layout anticipated it, and `id_of_path` already ignores it.
