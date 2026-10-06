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

Since schema 3, the bundle states its **assurance level** (ADR-0012) and speaks
only for that level:

```json
"assurance": { "level": "A1", "name": "A1 observed", "trusted_signers_available": false },
"proves":         [ { "id": "seal_verifiable",       "text": "…" }, … ],
"does_not_prove": [ { "id": "append_to_seal_window", "text": "…" }, … ],
```

- `assurance.level` is the **weakest** session in the bundle — a bundle never
  promises more than its weakest part. Each session carries its own
  `assurance` (`level`, and the first `reason` it is not higher, in the same
  words as `minds verify`). A bundle without any agent session is `A0` with
  `assurance.reason` saying so. The level is computed at export, never stored.
- **The level is the exporter's statement, not a fact the bundle proves.** The
  bundle is unsigned, and the level cannot be recomputed from it: the witness
  observations, the trusted signers and the witness ledger are not part of it
  (`bundle_level_self_reported` says so in every bundle). Re-run
  `minds verify --signers <trusted file> [--witness-home <dir>]` against the
  repository before you rely on a level. `audit` does not check the witness
  ledger.
- `proves` / `does_not_prove` are exactly the sentences that hold at that
  level, from the one vocabulary every surface speaks (bundle, TUI,
  `minds verify --limits`). Each has a stable `id`; refer to sentences by id,
  never by position.
- `trusted_signers_available` says whether a trusted `allowed_signers` plus
  `ssh-keygen` was **available** for checking witness signatures
  (`--signers <file>`, else `~/.ssh/allowed_signers` — never the repo's git
  config); it does not mean that a witness signature was actually checked.
  Without it there is no A2: the bundle then states at most the A1 level and
  its limits. The default `~/.ssh/allowed_signers` is only trustworthy if the
  agent cannot write it (an agent running as your user can) — in CI, pass
  `--signers` explicitly. A `--signers` file that cannot be read is an error.

What changes between levels (ids):

| Level | Limits retired | Limits narrowed | Promises added |
|---|---|---|---|
| A2 witnessed | `append_to_seal_window` | `who_controls_keys` → `who_controls_keys_witnessed`, `only_actor` → `only_actor_witnessed` | `witness_chaining`, `intent_approved`, `fs_observed` |
| A3 reproduced | `reported_results` | `wall_clock_time` → `wall_clock_time_anchored` | `results_reproduced`, `first_sight_bound` |

`model_identity`, `decision_correct`, `outside_boundary`, `root_compromise` and
`bundle_level_self_reported` hold at every level. So does `lines_attributed`:
the level says who observed the session, not which commit's lines came from
it — line reconciliation is the coverage axis of `minds verify`, not an input
to the level. At A0 (no sound seal) the seal promises (`seal_verifiable`,
`block_seal`) are absent, even if the bundle lists block seals under
`rejected_seals`.

Generate:

```sh
minds audit --export --out audit.json           # everything reachable from HEAD
minds audit --export --base main --out mr.json  # only this stack
minds audit --export --mode proof --out p.json  # only the proof scaffold
minds audit --export --signers trusted_signers  # trusted signers for witness seals (A2 possible)
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

**2. The signature** — exactly the stored bytes, checked like a Git SSH
signature. Legacy `agent-hooks/v1` seals use the `minds` namespace:

```sh
git cat-file blob refs/minds/evidence/<id>:seal      > seal.txt
git cat-file blob refs/minds/evidence/<id>:seal.sig  > seal.sig
ssh-keygen -Y verify -n minds -I <identity> \
  -f allowed_signers -s seal.sig < seal.txt
```

Seals with `scope=witness/v1` or `scope=witness-fs/v1` **must** carry
`seal.sig` and verify under `minds-witness`. Discover the candidate principal
from the independently trusted signer file, then verify the payload and namespace:

```sh
ssh-keygen -Y find-principals -f allowed_signers -s seal.sig
# Use a returned principal; if there are several, try each until one verifies.
ssh-keygen -Y verify -n minds-witness -I 'minds-witness@host' \
  -f allowed_signers -s seal.sig < seal.txt
```

Principal discovery alone does not prove validity. `minds verify --evidence
<seal-id> --signers allowed_signers` performs both steps and reports
`witness-signed (minds-witness@host)` on success. Session and revision
verification apply the same rule to every witness seal. Witness principals
come from `allowed_signers`, independently of `--identity` and `user.email`.
OpenSSH's `find-principals` stops at the first matching key entry. Minds
discovers candidates per entry so a namespace restriction on an earlier entry
cannot hide a later valid witness entry; when checking manually, use the
dedicated witness entry for discovery if your file repeats a key.
A missing signature is `TAMPERED` even without a signer file. An invalid or
untrusted signature, or a wrong namespace, is `TAMPERED` with the reason
`witness seal not signed under minds-witness` (exit 1). When no signer file
is available, a present signature is reported as `signature not checked`
without changing the verdict. An explicitly configured but unreadable signer
file, or an unavailable verification executable, is an operational failure
(exit 4).

Create a dedicated witness key on the host:

```sh
minds witness keygen --home /path/to/private/witness-home > witness.allowed_signers
```

This creates `key/witness_ed25519` and `key/witness_ed25519.pub` with mode
0600 inside private directories (0700), refuses existing keys (including
symlinks), and prints exactly one trust-file entry:

```text
minds-witness@host namespaces="minds-witness" ssh-ed25519 AAAA…
```

Distribute that public entry through your trusted channel. Keep the private
key in the witness domain. Home resolution is `--home`, then
`MINDS_WITNESS_HOME`, then `$XDG_STATE_HOME/minds-witness/<repo-id>` (with
`$HOME/.local/state` as the XDG fallback). The repo id is the first 16 hex
characters of BLAKE3 over the canonical worktree root path. Outside a
worktree, provide `--home` or `MINDS_WITNESS_HOME`. Existing home and key
directories must be owned by the current user and private; keygen refuses
symlinked directories and does not relax permissions.

The signing namespaces separate roles:

| Namespace | Purpose |
| --- | --- |
| `minds` | Reviews, attributions, legacy seals |
| `minds-witness` | Witness and filesystem-witness seals |
| `minds-intent` | Intent approvals |
| `minds-anchor` | CI anchors and replay records |

Use `namespaces="…"` restrictions in `allowed_signers` to enforce each
key's role. A witness-only entry rejects a review signed with the same key
under `minds`. Existing human signatures remain in `minds`.

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
`VERIFIED, INCOMPLETE` — and `minds verify
<session-id>` says the same with exit codes (0 verified, 1 tampered,
2 incomplete, 3 not verifiable); for CI gates additionally
`minds fsck --require-seal`.

What even the seal does **not** prove sits in the bundle under `does_not_prove` —
in particular: nothing about events outside sealed ranges
(`outside_sealed_ranges`), and, below A2, nothing about the window between
append and seal (`append_to_seal_window`).

## Retention

The bundle is a snapshot. It does not replace the repo: the source remains
`refs/minds/*`, and that travels with every clone. If you must retain for the
long term, retain the repo — the bundle is the form in which you hand it to a
verifier who does not want to operate Git.
