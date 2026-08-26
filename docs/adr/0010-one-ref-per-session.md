# ADR-0010 — One ref per session, and one push for all

- Status: accepted
- Date: 2026-07-29
- Affects: `minds-store`, `minds-git`, `minds-cli`
- Builds on: ADR-0004 (import and store index), ADR-0009 (reviews as Git objects)

## Context

Testing surfaced that `git push` takes noticeably longer with Minds enabled.
Measured against gitlab.com: the `pre-push` hook cost **1.7–1.9 s on every
push**, regardless of whether there was any new context at all. The cause was a
second, serial `git push` inside the hook — a full connection setup, only to
hear "Everything up-to-date" in the majority of cases.

Behind it lay a second, larger problem. The entire store hung on **one** ref
(`refs/minds/context`):

- Every checkpoint rewrote its tree with *all* N sessions.
- Every checkpoint additionally read `index.json`, added one line, and wrote it
  back whole.
- Two agents checkpointing at the same time ran into a compare-and-swap.
- Two machines that both checkpointed diverged on push and had to be merged.

For a solo developer this is invisible. For the agent fleet Minds aims at, it
is the construction that breaks first.

## What others have built

`entireio/cli` solves the same case in the `pre-push` hook — **synchronously**,
but with three optimizations: it compares the local ref against the
remote-tracking ref and, absent a difference, skips every network operation; it
keeps a flock-protected push queue instead of "offering everything"; and it
sends all due refs in *one* round trip. Deliberately **no** `ls-remote` in the
hook, on the grounds that a network round trip there could trigger an SSH
security-key prompt.

`entireio/cli` has also been through exactly the migration at issue here: away
from the long-lived branch `entire/checkpoints/v1`, toward one ref per
checkpoint under `refs/entire/`. Their reasoning in
`docs/architecture/ref-checkpoint-backend.md`:

> "That branch is a serialization point: every condensation rewrites its tip, every
> push races on one ref, and the whole history travels together."

`entireio/forgemark` measures the same idea from the server side: its default
strategy gives every agent its **own ref**, "so it isolates the server's
per-repo ref-update path".

## Decision 1: the payload gets one ref per session

A session lives under `refs/minds/store/<full hash>`; its tree carries
`session.json`. As a result:

- **Writing is O(1).** The tree has one entry, no matter how large the store is.
- **No race.** The ref name *is* the content hash. Two agents with different
  sessions touch different refs; two with the same session write the same
  tree, and the second run is a no-op.
- **No divergent push.** A session ref is created exactly once.

`refs/minds/store/` and not `refs/minds/sessions/`: the latter continues to
carry the *browsable* branches of the child backend (shortened hash, with
`session.md`). Payload and view are two things.

Empirically verified, refuting an earlier assumption in the code: **GitLab
accepts refs outside `refs/heads/*`** — `refs/minds/context` lives there. The
new namespace can therefore be pushed identically. Because it is not under
`refs/heads/`, a forge can neither pick it as the default branch nor put it in
the user's branch list.

## Decision 2: the edges live with their session

Instead of a shared `index.json`, every session ref carries a `links.json` with
*its* share of the commit index. The overall index is the union over all
session refs; it is read, never written.

The trade is deliberate: the hot path (checkpoint) becomes O(1) and
conflict-free; in return, the cold paths (`show`, `why`, `fsck`, `render`)
read N small blobs instead of one big one. A repo that only checkpoints is left
with **not a single shared-written ref**.

Existing repos and the import still write an `index.json`; the read path knows
both places and unions them. Likewise, `get` reads the payload first at the
session ref and then in the old context tree — nobody has to migrate.

`forget` purges in **both** places. Hitting only one would be the worst kind of
mistake this command can make: it would report "forgotten", and the plain text
would still sit in the other tree.

## Decision 3: `minds sync` instead of Git commands in the hook

The hook is now just `minds sync --remote "$1" || true`. The binary:

1. **decides without the network** whether anything is due — via its own
   tracking refs under `refs/minds/remotes/<remote>/*`. For `refs/heads/*` Git
   keeps the books itself, for `refs/minds/*` it does not; so we keep them
   ourselves, as refs and not as a file beside them. If one is lost, the
   consequence is a superfluous, idempotent push.
2. sends **all** due refs in *one* `git push --no-verify --porcelain`.
3. **never** pushes with `--force`. If the review log is rejected, the remote
   state is fetched and **merged in** (`ReviewStore::merge_from`, conflict-free,
   because an entry's path is its content hash) and pushed again — again
   fast-forward.
4. reports progress on stderr. A push that stays silent for ten seconds looks
   like a hanging push.

**Synchronous, not detached.** A background process would be faster, but has no
terminal — credential helpers, SSH passphrase, and security-key touch need
exactly that. A sync that silently fails authentication in the background is
worse than one that takes two seconds and says so.

The fetch refspec distinguishes two cases: the payload is fetched directly
(content-addressed, cannot overwrite anything), while **reviews** land in the
tracking namespace and never overwrite the local log — a `git fetch` must not
sweep away a locally created, not-yet-pushed verdict. Merging happens at the
next `minds sync`.

## Result (measured)

| Operation | before | after |
|---|---|---|
| `pre-push`, nothing new | 1.86 s | **0.02 s** |
| `pre-push`, new context | 1.86 s | 2.03 s (one connection) |
| context + reviews | two connections | **one** |
| checkpoint writes | tree with N entries + index | one tree with two entries |
| shared-written refs | 1 | **0** |

Fixed along the way: `refs/minds/reviews` was **never** pushed before — layer 3
was thus unusable for teams.

## Consequences

- Coming from an older version requires nothing: reading happens in both
  places. New writes go only to the new one. A repo thus converges on its own,
  without a migration ever running.
- The cold paths become O(N) in the session refs. At a few thousand sessions
  that is a ref scan and N small blob reads. Should that hurt, a cached index
  is an *addition* — derived, rebuildable at any time, and thus no regression
  to a shared-written ref.
- The fetch now pulls a glob (`refs/minds/store/*`). If you need only a
  single session, you can fetch it individually — that is the advantage one
  ref per session brings, and the foundation for later on-demand loading.
