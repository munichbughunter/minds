# ADR-0006 — Change-Id: stable change identity

- Status: accepted
- Date: 2026-07-28
- Affects: `minds-core`, `minds-cli`
- Related: ADR-0005 (context reinjection); layer 2 of the v0.2 plan

## Context

The commit hash is not an identity for a *logical* change: `rebase`, `squash`,
`amend`, and `cherry-pick` produce a new hash for the same intent. That breaks
stable references, review continuity across force-pushes, and thinking in terms
of "this change" vs. "this version of this change" — exactly the fracture point
Gerrit and Jujutsu solve with change ids. Minds needs the same stable bracket
so that later reviews (layer 3) can hang off *the change* and not off a
transient commit version.

## Decision 1: Gerrit-compatible format

A Change-Id is `I` + 40 hex characters (`I<40 hex>`) — the same form Gerrit's
`commit-msg` hook produces. Existing expectations and regexes
(`I[0-9a-f]{40}`) thus apply without adjustment. The trailer key stays in the
Minds namespace: `Minds-Change-Id`, consistent with `Minds-Session-Id` and
`Minds-Attribution`.

The type (`minds_core::ChangeId`) follows the same "read tolerantly, write
canonically" line as `SessionId`/`ContentHash`.

## Decision 2: the trailer carries it, not the hash

Like the `Minds-Session-Id` trailer, the Change-Id lives in the **text** of the
commit message. It thereby survives exactly the operations that change the
hash — because `rebase`/`squash`/`cherry-pick` carry the message along. This
shares the entire, already-tested trailer machinery (`extract_all`, squash
tolerance with indented bodies).

## Decision 3: generated in the `prepare-commit-msg` hook

`minds prepare-commit-msg` (called by the `enable` hook) appends a Change-Id if
none is present, and leaves an existing one untouched. Thus the first version
of a change gets its id, and every later one (amend, rebase) keeps it.

**Honest limit:** For an interactive commit *without* `-m`, the message is
still empty at hook time; then nothing is appended (the trailer would otherwise
become the subject). `-m`, `amend`, `rebase`, `cherry-pick`, and `squash` are
covered reliably — exactly the operations whose survival this is about.
A `commit-msg` hook would be the more precise place for the interactive first
commit and remains a possible addition.

**Generation:** from time + process id, well distributed via splitmix64. A
Change-Id is no secret — it needs uniqueness, not unpredictability. A collision
would take two commits in the same nanosecond from the same process.

## Consequences

`minds show` displays a commit's Change-Id. The Change-Id survives rebase and
squash (verified end to end). It is the prerequisite for layer 3 (reviews hang
off the Change-Id, not the commit) and for stacked changes.
