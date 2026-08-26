# ADR-0007 — `minds forget`: redactable payload

- Status: accepted
- Date: 2026-07-28
- Affects: `minds-store`, `minds-cli`, `minds-reader`
- Related: layer 2 of the v0.2 plan

## Context

A secret or personal data in Git history is in there forever — GDPR erasure and
the Merkle chain are structurally mutually exclusive. That is one of the
fracture points plain Git does not resolve. Minds, however, separates
**reference** (the trailer in the production commit) from **payload** (the
session as content-addressed JSON in the store). This separation is exactly
what makes it possible to delete the content **without** breaking the
reference — something a SaaS vendor would have to rebuild at considerable
cost, and something Git does not offer out of the box.

## Decision: tombstone instead of object deletion

`minds forget <session> [--reason]` replaces the payload with a **tombstone** —
a small marker JSON with the reason. The content-addressed path remains, but
its content is gone.

- **`exists` stays `true`, `get` reports `Forgotten`.** The reference still
  resolves — to a tombstone, not to content. `minds fsck` therefore sees **no**
  orphaned trailer.
- **`why`/`show`/the reader show "forgotten".** Graceful degradation, not an
  error. The reader already counts a forgotten session as "not readable" via
  its `Err(_)` branch — the page does not fall over.
- **Append-only is preserved.** The tombstone is *appended* as a new commit; no
  Git object is deleted. That fits the store's append-only discipline — deleting here
  means *overwriting*, not *removing*.

On read, the tombstone comes **before** the hash test: it deliberately does not
hash to the id (the content is replaced), but it is not a defect (`Corrupt`);
it is an erasure (`Forgotten`).

## Honest limits (deliberately not in v0.2)

1. **History of the context ref.** The old blob survives in the *history* of
   `refs/minds/context` until a history rewrite (BFG/filter-repo, or a
   re-orphan of the ref) purges it. The *current state* is content-free
   immediately — for the reader and every regular access, the session is gone.
2. **Pushed session branches.** A `minds/session/<hash>` branch (child backend)
   already pushed to the forge keeps carrying `session.json`/`session.md`
   until it is removed there separately.
3. **Re-capture.** If exactly the same session is captured again, the same
   content-addressed object is created again and overwrites the tombstone.
   `forget` targets historical data that will not recur.

A later `minds forget --purge` (history rewrite + branch and remote cleanup)
closes 1 and 2. The tombstone is the complete answer for the *current state*
and the honest partial answer for the *history*.

## Consequences

Minds can do what plain Git cannot: remove the content of a change and keep the
reference to it. Alongside signed attribution and reviews as Git objects, this
is a building block that makes the thesis "more into the repo, less into the
platform" concrete for regulated environments.
