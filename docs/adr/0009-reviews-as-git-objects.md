# ADR-0009 — Reviews as Git objects (layer 3, slice R1 + R5)

- Status: accepted (R1–R6 implemented)
- Date: 2026-07-28, extended 2026-07-29
- Affects: `minds-core`, `minds-store`, `minds-cli`
- Builds on: ADR-0006 (Change-Id), ADR-0008 (signed attribution); `Roadmap.md` layer 3

## Context

GitLab's project memory — reviews, approvals, discussion — lives in Postgres,
not in the repo. Migrate away and you lose half the story. Radicle and git-bug
show the other way: **reviews as Git objects.** That is the big bet of the
thesis "more into the repo, less into the platform" — and the one thing a SaaS
vendor is structurally reluctant to build.

This ADR began with the first cut (**R1** review object, **R5** policy gate)
and since 2026-07-29 covers **R1–R6 in full**. The additions are below under
"The build-out"; the transport question (one ref per session, one push for all)
got its own ADR: [ADR-0010](0010-one-ref-per-session.md).

## Decision 1 (R1): the review object

`minds_core::Review` is a versioned, **content-addressed** envelope: subject
(preferably a **Change-Id**, alternatively a session id), verdict
(`approve` / `reject` / `needs-work`), reviewer identity, summary. The hash is
`blake3` of the canonical form (the same `b3-` text form as sessions).

- **On the Change-Id, not the commit.** Only that way does the verdict survive
  a rebase — exactly the reason layer 2's Change-Id came before layer 3.
- **Its own ref `refs/minds/reviews`.** Reviews live separately from the context
  store: own access rights, own push path, no mixing with the session list.
  The same content-addressed layout (`reviews/<2hex>/<rest>.json`),
  dedup-friendly.
- **Commands:** `minds review <subject> --approve|--reject|--needs-work
  [--summary]` creates, `minds reviews <subject>` lists.

The reviewer identity is the same one used for signing — R1 thereby connects to
the signed attribution from ADR-0008 (a signed verdict is the additive next
step).

## Decision 2 (R5): policy as a binary, not as YAML

`minds fsck --require-review` demands an approve for **every reachable,
agent-authored commit** (carries ≥1 `Minds-Session-Id`) — on its Change-Id or
one of its session ids. If it is missing, the exit code is ≠ 0.

The bundled `ci/minds-review-gate.gitlab-ci.yml` calls only this binary. A
format that cannot hold logic (YAML) should not carry any — the rule lives in
the binary, where it is testable.

## The build-out (rest of R1, R2, R3, R4, R6)

**Signed verdicts (R1 complete).** `review_payload` in `minds-core` is the
canonical, signable text — the same construction as `attestation_payload`. The
signature lives as a **sidecar** next to the review
(`reviews/<2hex>/<rest>.sig`), not inside it: a field in the envelope would be
circular (the hash covers the envelope; the signature is computed over the hash).
Consequence: the hash does not change when someone signs after the fact, and
multiple identities can sign the same verdict. `minds review --sign` signs,
`minds reviews --signers` verifies. **Without `--signers` nothing is verified,
only reported** — "signed" and "valid" must not look alike.

**The thread (R2).** `minds_core::Comment` is an append-only operation,
content-addressed, anchored at `file:line`, at a turn, or at the change as a
whole. The anchor is part of the hash: the same text in two places is two
comments. Merging two logs is a **set union** (`ReviewStore::merge_from`) —
commutative and idempotent, because same path means same content. The display
order comes from the content (time, then hash), so two machines show the same
thread the same way. Comments live under the same ref as the verdicts: a
second ref would be a second place where something can go missing.

**The stack (R3).** `minds stack` shows the changes from a base onward, each
with its state. Because the verdict hangs off the Change-Id, it
survives rebase and force-push — pinned down in the test
`a_force_push_of_the_stack_keeps_every_verdict`.

**The platform as cache (R4).** `minds-gitlab` mirrors verdicts as an MR note,
**one-way and idempotent** via an invisible marker
`<!-- minds:review:<hash> -->`. The reverse direction (`minds gitlab webhook`)
is opt-in, stateless, and writes nothing without `--write`. No HTTP stack in
the binary: `curl` does the transport, just as signing uses `ssh-keygen`. The token comes only from the
environment and goes to `curl` via stdin — never into an argument list.
Operating model: [gitlab-operating-model.md](../gitlab-operating-model.md).

**The audit export (R6).** `minds audit --export` bundles change → commits →
sessions → attribution → verdicts (+ signatures) + thread as portable JSON. It
contains the **canonical payloads**, so it is verifiable without this tool
(`blake3`, `ssh-keygen -Y verify`). The limits are stated **in the artifact**
(`proves` / `does_not_prove`), not just in the docs — the artifact gets passed
along; the docs stay behind. In detail:
[verification-guide.md](../verification-guide.md).

## Consequences

A repo can carry what the platform database used to hold: the verdict on a
change, content-addressed, traveling with the repo, enforceable in CI.
Together with the Change-Id, signed attribution, and `minds forget`, the "more
into the repo" thesis is thereby fully realized: a repo carries its own,
cryptographically verifiable answer to "Who wrote this, on what instruction,
who reviewed it, and why was it merged?" — without a platform, without a
database, verifiable offline.

What deliberately stays open: the signature of a **session** attribution is
written to stdout by `minds sign` but not stored. The audit export therefore
ships the payload and accepts the signature from outside. A sidecar like the
review's would be the additive next step.
