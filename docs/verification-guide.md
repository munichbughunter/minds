# Verification Guide — what the audit bundle proves, and what it does not

*Layer 3, R6. For `minds audit --export`.*

This document is deliberately built so that the limits get as much room as the
promises. A proof artifact whose limits you only learn about when you ask does
more damage than none at all.

The most important points therefore also live **in the bundle itself** (`proves` /
`does_not_prove`): the bundle gets passed on; this document stays behind.

## What is inside

```
Change ──▶ Commits ──▶ Sessions ──▶ Attribution ──▶ Verdicts (+ signatures)
                          │                     └──▶ Thread (comments)
                          └──▶ Evidence seals (+ signatures)   [ADR-0011]

rejected_seals ──▶ block seals of withheld sessions
```

Per change id: the commits, the sessions behind them (with agent, model, and the
instruction), the canonical attestation payload per session, the verdicts with their
review payload and — if present — their signature, and the comment thread.
Since schema 2, the bundle also carries each session's **evidence seals** (the
byte-exact seal text of every checkpoint epoch, including signature) and, under
`rejected_seals`, the block seals: sessions whose payload the storage policy
rejected — the seal proves they existed, without disclosing their content.

Generate:

```sh
minds audit --export --out audit.json           # everything reachable from HEAD
minds audit --export --base main --out mr.json  # only this stack
minds audit --export --mode proof --out p.json  # only the proof scaffold
```

Two modes: **`redacted`** (default) carries everything the store yields — the
redacted intents, verdicts, comments, seals. **`proof`** carries only the proof
scaffold: ids, canonical payload texts, seals including signatures, verdict
metadata — no intent, no summaries, no comments. This lets an external party
check *that* something happened, and *how much*, without passing the content along.
A "full" mode deliberately does not exist: the store holds exclusively redacted
sessions (fail-closed) — there is nothing beyond `redacted` to export.

Even `proof` still carries **personal identifiers**: the reviewer (needed to
bind signatures to an identity) plus agent and model names in the canonical
payloads. If you must not pass those on, redact the bundle yourself before
handing it over. Content hashes on read effects exist only for files
**tracked** by git — merely reading a private file leaves no fingerprint in
the bundle.

## What it proves

**Integrity of the content.** Every `id` is the blake3 hash of the canonical form
of its content. Anyone who pulls the session from the store can recompute the hash;
anyone who edits it after the fact gets caught. The same holds for verdicts
and comments.

**Verifiability without this tool.** `attestation_payload` and `review_payload`
are, byte for byte, the texts that get signed. An auditor needs only
`ssh-keygen`:

```sh
jq -r '.changes[].verdicts[] | select(.signature) | .signature' audit.json > v.sig
jq -r '.changes[].verdicts[] | select(.signature) | .review_payload' audit.json \
  | ssh-keygen -Y verify -f allowed_signers -I anna@example.org -n minds -s v.sig
```

**Continuity across rebase and force-push.** Verdicts hang on the change id, not
on the commit hash. A reworked stack does not lose its review history.

**Provable deletion.** A session erased via `minds forget` still stands in the
chain as `"payload": "forgotten"`. The reference stays resolvable, the content is
gone — GDPR deletion, without the history lying.

## What it does not prove

**No completeness.** The capture path is fail-open: `minds hook` would rather lose
an event than disturb the session. A lost event is **silently** absent here.
`minds fsck` makes gaps visible; a bundle without an accompanying `fsck` run says
nothing about completeness.

**No causality line ↔ session.** The mapping comes from two sources: the trailer
in the commit message (`observed`) and a heuristic for imported sessions
(`inferred`). The provenance is marked on every edge — it must not be flattened.
"Inferred" means inferred.

**No statement about the model.** What is recorded is what the agent **reported**.
That a specific model actually produced a specific text is something a client-side
tool cannot prove; that would require an assurance from the provider.

**No statement about the keys.** A signature is only worth as much as the
`allowed_signers` file it is checked against. If it comes from the same repo as
the bundle, it is self-attestation. It must come from a source the verifier
trusts independently (a directory service, a file distributed out of band, a
key ceremony).

**No trust for the unsigned.** An unsigned verdict is content-addressed — it is
unchanged. But nobody vouches for it with a key. If you need a binding
verdict, require `minds review --sign` and check with `minds reviews --signers`.

**No proof of time.** The timestamps come from the clock of the machine that
wrote the entry. They order events; they prove nothing. If you need provable
time, you need a timestamping service — there is none here.

## How a verifier works with it

1. **Gaps first.** Run `minds fsck` and put the output next to the bundle.
   Without it, every statement about coverage is unsupported.
2. **Obtain the keys.** The `allowed_signers` from an independent source, not
   from the repo.
3. **Check the signatures** (see above). Treat the unsigned separately.
4. **Recompute the hashes**, if the store is shipped along — the clone suffices:
   `git cat-file blob refs/minds/store/<hash>:session.json | b3sum`.
5. **Read the provenance.** Distinguish `observed` and `inferred` — and, since
   ADR-0011, the status too: *observed* does not mean *recomputed*. Treat the
   two the same and you have misread the bundle.
6. **Check the seals** (section below): recompute the identity, verify the
   signature, read the coverage. A block seal in `rejected_seals` is a
   statement, not an error: this session existed, its payload was rejected by
   the storage policy.

## Recomputing the Evidence Chain without Minds

The proof does not belong to Minds: seal identity and signature are checkable
with standard tools. The boundary is drawn clearly:

| Component      | Externally checkable?                                |
| -------------- | ---------------------------------------------------- |
| Seal identity  | yes — `seal_id == derive_key(seal text)`             |
| Seal signature | yes — `ssh-keygen -Y verify` against allowed_signers |
| Chain root     | only with the local journal **and** the session salt |

The seal commits cryptographically to chain root and coverage; the underlying
chain can be reproduced only locally. With the bundle alone, a verifier
recomputes the sealed claim — not the chain itself. The hashes are
`blake3::derive_key` with fixed context strings — shown here as Python, because
`b3sum` has no derive_key mode:

```python
# pip install blake3
from blake3 import blake3

def derive(context: str, material: bytes) -> str:
    return blake3(material, derive_key_context=context).hexdigest()
```

**1. The seal identity.** The ref name must be the hash of the text:

```sh
git for-each-ref refs/minds/evidence/            # list the seals
git cat-file blob refs/minds/evidence/<id>:seal  # fetch the text
```

```python
assert derive("minds/evidence/v1/seal", seal_text_bytes) == ref_name_hex
```

**2. The signature** (if `seal.sig` sits next to it) — exactly the stored
bytes, checked like a Git SSH signature:

```sh
git cat-file blob refs/minds/evidence/<id>:seal      > seal.txt
git cat-file blob refs/minds/evidence/<id>:seal.sig  > seal.sig
ssh-keygen -Y verify -n minds -I <identity> \
  -f allowed_signers -s seal.sig < seal.txt
```

**3. The chain root** — recomputable only **locally**, with the journal still at
rest **and** the session salt (`<git-dir>/minds/evidence/state/…/*.salt`;
the fold starts on `derive("minds/evidence/v1/chain", salt)`). That is by
design, not a defect: without the salt, the root would be an offline oracle —
anyone who guesses a short payload could confirm the guess against the root. After the
checkpoint the journal is gone; then the root binds the events as they were
read at the time, and tampering with the *seal* is caught by step 1:

```python
payload_hash = derive("minds/evidence/v1/payload", payload_bytes)
# event_hash: length-prefixed fields (u64 LE) — schema in
# crates/minds-core/src/evidence.rs, context "minds/evidence/v1/event".
# fold: state = derive("minds/evidence/v1/chain",
#                      state ‖ tag ‖ link)   # tag 0x01 event, 0x02 gap,
#                                            # 0x03 pre-chain; start: 32 × 0x00
```

The salt is therefore itself part of the integrity: if it is lost after an
epoch was sealed, Minds does **not** reseal with a new salt — that would
produce a second, diverging root for the same evidence (an epoch fork).
Instead, the checkpoint aborts visibly for this session (`hook.log`), the
journal stays put, and the epoch is treated as no longer reproducible. The
loss is a finding, not something to repair.

**4. Reading the verdict.** `gaps=0`, `pre_chain=0`, `outcome=stored`, and an
epoch chain closed via `previous=` ⇒ complete. Everything else is
`VERIFIZIERT, UNVOLLSTÄNDIG` (verified, incomplete) — and `minds verify
<session-id>` says the same with exit codes (0 verified, 1 tampered,
2 incomplete, 3 not verifiable); for CI gates additionally
`minds fsck --require-seal`.

What even the seal does **not** prove sits in the bundle under `does_not_prove` —
in particular: nothing about events outside sealed ranges, and nothing about
the window between append and seal.

## Retention

The bundle is a snapshot. It does not replace the repo: the source remains
`refs/minds/*`, and that travels with every clone. If you must retain for the
long term, retain the repo — the bundle is the form in which you hand it to a
verifier who does not want to operate Git.
