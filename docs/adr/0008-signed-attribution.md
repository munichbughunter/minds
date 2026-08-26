# ADR-0008 — Signed attribution: proof instead of claim

- Status: accepted
- Date: 2026-07-28
- Affects: `minds-core`, `minds-cli`
- Related: layer 2 of the v0.2 plan; ADR-0006 (Change-Id), ADR-0007 (forget)

## Context

In Git, `author` is an unsigned free-text field. In a world where agents
commit, that is exactly the foundation on which **nothing** can be proven — for
the regulated target audience (verifiability of AI involvement in code), that
is the core problem. The `SessionId` already establishes the **integrity** of
the content (content-addressed). What is missing is the **attribution**: that a
particular key holder vouches that this session — with this agent and model —
is genuine.

## Decision 1: `ssh-sig`, not sigstore

Signing uses `ssh-keygen -Y sign/verify` (SSH signatures) — the same mechanism
Git uses to sign SSH commits. The reason is the target audience: `ssh` is
already everywhere, **no network, no OIDC, air-gap capable**. sigstore/gitsign
would be more modern, but need an online trust chain that self-managed and
air-gapped shops do not have. sigstore remains a later option behind the same
narrow seam interface.

## Decision 2: what is signed is a canonical attestation payload

`minds_core::attestation_payload(id, session)` produces a deterministic text:

```
minds-attestation-v1
session=b3-<hash>
agent=<name> <version>
model=<provider>/<id>
```

Because the `SessionId` is the hash of the canonical session (agent and model
included), the signature binds the **entire content**; agent and model
additionally appear in plain text so a human can read the pledge. If the
content changes, the id changes — the payload no longer fits, and the
signature breaks. The version prefix makes a later format update cleanly
distinguishable.

## Decision 3: `sign`/`verify` as commands of their own

- `minds sign <session> [--key]` writes the armored signature to stdout (key
  from `--key` or `git config user.signingkey`).
- `minds verify <session> --sig <file> [--signers] [--identity]` reconstructs
  the payload from the (hash-checked) session in the store and verifies;
  defaults for `--signers`/`--identity` come from `git config`
  (`gpg.ssh.allowedSignersFile`, `user.email`).

Deliberately **not** in v0.2: automatic signing at checkpoint time and a
`Minds-Attribution-Sig` trailer on the commit. That is pure wiring (like the
session trailer) on the same foundation — the cryptographic core and its
verifiability are complete here and tested end to end (a real signature
verifies; a third-party or tampered signature fails).

## Consequences

"Agent X, model Y wrote these lines, and person Z vouches for it" becomes
**provable**. Together with `minds forget` (GDPR) and the Change-Id, this is
the attribution/trust part of the thesis "more into the repo, less into the
platform" — and the foundation on which layer 3 (reviews as Git objects) builds
signed verdicts.
