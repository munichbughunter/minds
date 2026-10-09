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
`blake3::derive_key` with fixed context strings. On the command line, that is
`b3sum --derive-key <context> --no-names < file` (b3sum 1.x); the
[witnessed-evidence recipes](#checking-witnessed-evidence-by-hand) use exactly
that. For scripting, the same in Python:

```python
# pip install blake3   (or: uv run --with blake3 python3 …)
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
For a `witness/v1` or `witness-fs/v1` seal that the verdict checks, a
missing signature is `TAMPERED` even without a signer file. An invalid or
untrusted signature, or a wrong namespace, is `TAMPERED` with the reason
`witness seal not signed under minds-witness` (exit 1). The verdict
checks a `witness-fs/v1` seal in two cases: when you name it with
`minds verify --evidence <seal-id>`, or when its `session=` names the
session. A real file-system seal names its observation object instead.
In session and commit verification, a real file-system seal with a missing
or invalid signature is therefore not counted. Its observations drop out
of the window, and the session cannot reach A2. Pass `--signers`
explicitly (see [Witnessed evidence](#witnessed-evidence)). Otherwise, for
the verdict, the signer file comes from the repository's
`gpg.ssh.allowedSignersFile` or from `~/.ssh/allowed_signers`, and an agent
can write both. Observations and the level never read the repository's
config, only `--signers` or `~/.ssh/allowed_signers`. When no signer file
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

## Witnessed evidence

*ADR-0012. For seals with `scope=witness/v1` or `scope=witness-fs/v1`, intent
anchors, and CI countersignatures.*

Up to A1, the agent's own hooks write the record, and the agent can reach
everything they write. A **witness** is a separate process that, in the
`container` and `user` profiles, runs in another trust domain. A witness
started under the developer's own account (`managed`, or by hand) shares the
agent's. It receives the agent's hook events over a socket, chains them
live, watches the worktree's file system, and seals both under its own key
(`minds-witness`). Two more signers can join it: a human who approves the
requirement the session works against (`minds-intent`), and CI, which
countersigns the seals of the sessions behind the commits it builds and
replays the decisive commands (`minds-anchor`). The **assurance level** says which of these
observers stand behind a sealed range. `minds verify` computes it each time
it reads the evidence. No stored object claims a level.

**State of the implementation — read this before relying on a level.** The
witness does not yet record its isolation profile and evidence in signed
material (the `witness.start` facts stay in the witness's own journal).
`minds verify` therefore never grants A2 from real material today: a
fully witnessed range stops at A1, at the latest with the reason
`witness profile unknown`. A3 requires A2. The levels, gates and sentences
below are implemented and tested, and witness seals, intents and
countersignatures are already checked. The recipes in this section work on
real material today. The top two levels are not yet reachable.

### Reading the `Assurance` line

`minds verify` prints two lines under the verdict:

```text
Assurance      A1 observed    (range 1: observed by the agent's hooks only (scope agent-hooks/v1))
Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)
```

- **The level** is the session's **weakest** sealed range. A session can mix
  levels, for example when the witness was unreachable for a while and the
  agent side sealed that stretch itself (`agent-hooks/v1`).
- **The parenthesis** names the first reason the level is not higher, and the
  range it applies to (`range N` counts the `Seal` lines of the block in
  time order). A reason that affects the whole session has no range, for
  example `integrity broken — seal material was altered`,
  `epoch chain open — a sealed range is missing`, or a seal the witness
  ledger lists but the repository no longer has. Any of these caps every range at A0.
- From A2 upwards, the parenthesis instead names the witness principal and
  profile and the intent signer kind, e.g.
  `(minds-witness@build-07, profile user; intent signed, sk key)`.
- **`Not proven`** lists the first three limits that apply at the level, all of which hold at every level.
  `minds verify --limits` prints every limit that still applies at the level.
- The level is not a verdict axis. It does not change the exit code (0
  VERIFIED · 1 TAMPERED · 2 VERIFIED, INCOMPLETE · 3 NOT VERIFIABLE · 4
  operational failure). Only an explicit gate does:
  `--require-assurance A2` turns an otherwise VERIFIED result below A2
  into exit 2. It never masks 1, 3 or 4.
- **The level describes the session, not the commit.** The agent can write a
  trailer that links a witnessed session to a different commit. Whether the
  session explains a commit's lines is the coverage axis
  (reconciliation), not the level. A gate on the level alone does not
  replace it.

**Always pass `--signers <file>` with a trust file from a source you
trust.** Without it, the level falls back to `~/.ssh/allowed_signers`. The
witness-seal check of the verdict falls back further, to
`gpg.ssh.allowedSignersFile` from the repository's Git config **first**.
An agent running as your user can write both, and then a seal it signed
itself can pass. Every statement in this section about `TAMPERED` and
about levels assumes an explicit `--signers`.

What each step requires (all of it checked against that trust file):

| Level | Requires |
|---|---|
| A0 claimed | anything less than A1: no seal, captured before the evidence chain, altered seal material, an open epoch chain, a ledgered seal missing from the repository, a `witness/v1` seal that is unsigned or not validly signed under `minds-witness`, a seal scope that seals no session, or `inferred` provenance |
| A1 observed | a sound seal of scope `agent-hooks/v1`, or a witness seal that misses an A2 condition |
| A2 witnessed | scope `witness/v1`, signed under `minds-witness` by a trusted signer; a gap-free window of trusted `witness-fs/v1` seals over the session; a qualified profile with proven isolation for every witness run in the window; the session end seen by the witness; an intent bound from the session start, chained by the witness, unchanged, its snapshot matching the anchor, and signed under `minds-intent` |
| A3 reproduced | A2, plus a signed replay record (`minds-anchor`) with at least one decisive command, every one reproduced, none skipped, and no `claim not reproduced`; plus a `minds-anchor` first-sight countersignature on every seal of the session |

A signer counts for a role only if **every** line of the trust file that
names the principal or carries its key is restricted to exactly that role's
namespace (`namespaces="minds-witness"`, `"minds-intent"` or
`"minds-anchor"`). A key without `namespaces=`, or with several, never
counts. `cert-authority` lines never count. A principal pattern counts
only if its line is itself restricted to exactly that namespace. This rule
applies to the **level**. The verdict only checks that a `witness/v1`
seal's signature verifies under `minds-witness`. A seal signed by an
unrestricted key in your trust file therefore stays VERIFIED, but it caps
the session at A0 (`witness seal not signed under minds-witness`).

### What each level proves — and what it does not

This table is generated from the one vocabulary that the audit bundle, the TUI
and `minds verify --limits` all print (`cargo xtask proof-table`). A test
fails when it drifts. Refer to sentences by `id`.

<!-- BEGIN generated: cargo xtask proof-table — do not edit by hand -->

**Proves — ✓: the promise holds at that level**

| id | A0 | A1 | A2 | A3 | sentence |
|---|:-:|:-:|:-:|:-:|---|
| `session_content_addressed` | ✓ | ✓ | ✓ | ✓ | Every session id is the blake3 hash of its canonical content — the content can be recomputed against it. |
| `attestation_payload` | ✓ | ✓ | ✓ | ✓ | The attestation_payload is byte-for-byte the text `minds sign` signs; a shipped signature is verifiable against it. |
| `review_payload` | ✓ | ✓ | ✓ | ✓ | The review_payload binds the hash of the verdict; a valid signature over it shows who reviewed. |
| `verdicts_survive_rebase` | ✓ | ✓ | ✓ | ✓ | Verdicts attach to the change id and therefore survive rebase and force-push. |
| `forgotten_visible` | ✓ | ✓ | ✓ | ✓ | A forgotten session stays visible as a reference (payload: forgotten) — deletion is provable, not traceless. |
| `seal_verifiable` | – | ✓ | ✓ | ✓ | Seal identity and signature are externally verifiable: seal_id = blake3::derive_key("minds/evidence/v1/seal", text). The seal commits cryptographically to chain root and coverage; the underlying chain is reproducible only with the local journal and session salt (ADR-0011). |
| `block_seal` | – | ✓ | ✓ | ✓ | A block seal (rejected_seals) proves that a session existed whose payload the storage policy rejected — without disclosing its content. |
| `witness_chaining` | – | – | ✓ | ✓ | Events were chained live by the witness, outside the agent's trust domain, and sealed under its key (minds-witness): a write between append and seal breaks a witness-signed chain. |
| `intent_approved` | – | – | ✓ | ✓ | The intent the session worked against is bound by version, and its approver signed it (minds-intent). |
| `fs_observed` | – | – | ✓ | ✓ | The witness observed the worktree's file-system changes during the session, gap-free and under its key (witness-fs/v1) — a second observer besides the agent's own report. Whether they explain a given commit's lines is a separate check (`minds verify`, coverage axis). |
| `results_reproduced` | – | – | – | ✓ | The decisive results (tests, builds) are real: CI re-ran the decisive commands and reproduced the outcomes the agent reported. |
| `first_sight_bound` | – | – | – | ✓ | An upper time bound: CI countersigned every seal on first sight (minds-anchor), so the evidence existed no later than that. |

**Does not prove — ✓: the limit still applies at that level**

| id | A0 | A1 | A2 | A3 | sentence |
|---|:-:|:-:|:-:|:-:|---|
| `model_identity` | ✓ | ✓ | ✓ | ✓ | Not which model produced the answers: the model name is what the agent reported. |
| `decision_correct` | ✓ | ✓ | ✓ | ✓ | Not that the decision was right: the evidence shows what happened, not whether it was the correct thing to do. |
| `outside_boundary` | ✓ | ✓ | ✓ | ✓ | Not what happened outside the observation boundary: activity that neither the hooks nor the witness observe is not recorded. |
| `root_compromise` | ✓ | ✓ | ✓ | ✓ | Not integrity against whoever controls the host (root, the witness account or its key): they can rewrite the evidence and the witness alike. |
| `record_complete` | ✓ | ✓ | ✓ | ✓ | Not that the record is complete: the hot path is fail-open, and a lost event is silently absent here (`minds fsck` makes gaps visible). |
| `lines_attributed` | ✓ | ✓ | ✓ | ✓ | Not that a session actually produced the lines attributed to it — the mapping comes from trailers (observed) and heuristics (inferred); the provenance is stated on every edge. |
| `transcript_reported` | ✓ | ✓ | ✓ | ✓ | Not that a model did what the transcript says — what is recorded is what the agent reported. |
| `reported_results` | ✓ | ✓ | ✓ | – | Not that reported results (tests, builds) are real: they are what the agent reported until a CI replay reproduces them. |
| `who_controls_keys` | ✓ | ✓ | – | – | Not who controls the signing keys. Without an allowed_signers file from a trusted source, a signature is only a self-attestation. |
| `who_controls_keys_witnessed` | – | – | ✓ | ✓ | Not who controls the human signing keys: witness key control is shown; human key custody still depends on allowed_signers. |
| `unsigned_entries` | ✓ | ✓ | ✓ | ✓ | Not that unsigned entries are genuine: they are content-addressed, but nobody vouches for them with a key. |
| `bundle_chain` | ✓ | ✓ | ✓ | ✓ | Not that the bundle alone can recompute the chain: the chain root is reproducible only with the local journal and session salt — the bundle proves the sealed claim (identity, signature, coverage), not the chain itself. |
| `bundle_level_self_reported` | ✓ | ✓ | ✓ | ✓ | Not the assurance level as a portable fact: it is assessed when the evidence is read, from the repository and the trusted signers at hand; a level stated elsewhere (an exported bundle, a report) cannot be recomputed from that document alone — re-run `minds verify --signers` against the repository. |
| `outside_sealed_ranges` | ✓ | ✓ | ✓ | ✓ | Not that nothing happened outside sealed ranges — a seal claims only the sequence range its epoch actually read. |
| `append_to_seal_window` | ✓ | ✓ | – | – | Not the integrity between append and seal: until the checkpoint, only the file system protects the journal; a local write before sealing is undetectable (ADR-0011, decision 1). |
| `only_actor` | ✓ | ✓ | – | – | Not that the agent process was the only actor: subprocesses, network access and plugins outside the hook boundary (scope in the seal) are not captured — coverage means complete within the boundary, never system activity. |
| `only_actor_witnessed` | – | – | ✓ | ✓ | Not that the agent was the only actor: changes in the worktree are observed; processes, network and other machines are not. |
| `uninterpreted_effects` | ✓ | ✓ | ✓ | ✓ | Not the effect of uninterpreted tool calls: capture=uninterpreted means observed, but the effects are not normalized — the interpretation axis is separate from integrity and coverage. |
| `wall_clock_time` | ✓ | ✓ | ✓ | – | Not real wall-clock time: timestamps come from the hook's local clock, with no external time anchor. |
| `wall_clock_time_anchored` | – | – | – | ✓ | Not the exact time: the CI anchor gives an upper bound, there is no lower bound. |

<!-- END generated: cargo xtask proof-table -->

Read the `Does not prove` half as carefully as the other half. Even at A3,
the evidence says nothing about which model ran, whether the decision
was right, what happened outside the observation boundary, or what a
root user on the witness host did.

### Checking witnessed evidence by hand

The recipes need only `git`, `b3sum` (BLAKE3, version 1.x, for
`--derive-key`) and OpenSSH's `ssh-keygen`. They expect these shell
variables, all paths **absolute**, because step 0 changes directory:

```text
REPO              a mirror you created yourself (see below); only read, never written
WITNESS_SIGNERS   allowed_signers with the witness keys only
INTENT_SIGNERS    allowed_signers with the intent approvers' keys only
ANCHOR_SIGNERS    allowed_signers with the CI keys only
SEAL              a witness/v1 seal id: 64 hex characters, the name below refs/minds/evidence/
FS_SEAL           (step 3) a witness-fs/v1 seal id
INTENT            (step 4) an intent anchor id, the name below refs/minds/intents/
```

**One trust file per role, from a source you trust, never from the
repository.** `ssh-keygen -Y verify -n <namespace>` only checks the
namespace the *signer* chose. A key that your file lists without a
`namespaces=` restriction passes under every namespace. That includes a
developer's commit-signing key, which an agent running in the same session
can use through `ssh-agent`. With one file per role and every line
restricted to that role's namespace, such a key can never become a witness,
an approver or CI. Step 0 checks the restriction field by field. The
namespace option must be the line's **only** option, directly after the
principal. Anything else, including a principal with spaces, fails the
check:

```text
# WITNESS_SIGNERS
minds-witness@build-07 namespaces="minds-witness" ssh-ed25519 AAAA…
# INTENT_SIGNERS
anna@example.org namespaces="minds-intent" sk-ssh-ed25519@openssh.com AAAA…
# ANCHOR_SIGNERS
ci@gitlab.example.org namespaces="minds-anchor" ssh-ed25519 AAAA…
```

**The restriction does not protect the role keys themselves.** It limits
what a listed key is trusted *for*. It does not stop whoever can reach that
key from signing with it. The witness key belongs to the witness's own
account, and the CI key to a protected CI variable (`MINDS_ANCHOR_KEY_FILE`).
Keep the approver's `minds-intent` key out of any `ssh-agent` the agent
can reach. Prefer a security key with user presence (`sk-…`). If
`minds verify` reports `intent signed, software key`, the approval is only
as good as the custody of that file.

**Run the check where no agent has ever run.** Everything below, and
`minds verify --signers` as well, is only as trustworthy as the account
that runs it. An agent running as your user can put its own `git`, `b3sum`
or `ssh-keygen` early in your `PATH` (`~/.cargo/bin`, `~/.local/bin`, shell
rc files). It can edit trust files in your home directory, and it can
prepare or change a mirror there. Use a reviewer account or machine, or a
CI job, that no agent has used. The recipes run `git`, `b3sum`, `ssh-keygen`,
`awk`, `sed`, `grep`, `head`, `mktemp` and `test`. Check where each comes
from (`command -v …`). `b3sum` in particular is often installed into
`~/.cargo/bin`, so install it for the reviewer account, not an agent's.
Keep the mirror and the trust files outside any account an agent worked
in.

**Read from a mirror you created yourself.** A plain `git clone` does not
fetch `refs/minds/*`. Any repository the agent could write to has a Git
config the agent controls: a pager, an `include.path`, or a promisor remote
that `cat-file` would contact. `git clone --mirror --no-local <url>
"$HOME/audit/repo.git"` fetches every ref into a new bare repository with a
fresh config and no worktree. Then set `REPO="$HOME/audit/repo.git"`. Prefer
the forge's URL to a local path. Cloning from a path runs `upload-pack`
inside the source repository, under its config. Use a current, patched Git:
older releases had code-execution bugs exactly when cloning a prepared
local repository.

**Run the recipes in an empty scratch directory, not inside a clone.**
They write files such as `seal.txt`, and a repository you are auditing can
commit a symlink by that name. Step 0 changes into a fresh directory.
If its `ok:` line is missing, stop there. Every `git` call reads the
mirror through `-C "$REPO"`. A repository controls the text of its own
objects, so the recipes never print free text from it. They show only lines
that match a strict format: seal ids as 64 hex characters, `scope=`,
`session=b3-<hex>`, `project=`, `pipeline=` and `at=`, all without spaces.
No line the repository supplies can contain `ok:`. The one value the
recipes take from the repository as an argument, the observation id in
step 3, must be exactly 64 hex characters, and the error output of that
call is discarded. Step 0 holds the ids you pass in (`SEAL`, `FS_SEAL`,
`INTENT`) to the same rule, in case you copied one from an untrusted
document such as an unsigned audit bundle. Otherwise the repository could print fake `ok:` lines
(by escape sequence, by terminal line wrap, or inside an error message).
To count without reading, run all steps as one script and pipe it through
`2>&1 | grep -c '^ok: '`.

Every check prints one `ok:` line when it passes. A check that fails
prints no `ok:` line, only the tool's own error if it has one, so count
the `ok:` lines rather than skimming for errors: steps 0–5 print 16. Every
command below runs as-is in an automated test
(`crates/minds-cli/tests/doc_recipes.rs`). That test also makes sure each
of the following loses its `ok:` line:
- an unrestricted key,
- a key whose comment imitates the restriction,
- a comment line that OpenSSH reads as a key,
- an empty trust file,
- a `cert-authority` line,
- a key under the wrong namespace,
- an edited seal,
- a `witness-fs/v1` seal passed as `SEAL`,
- exchanged or missing observations,
- an edited intent snapshot,
- an intent signed under the wrong namespace,
- a countersignature moved to another seal.

The test also checks two things about a prepared repository: it never gets
a symlink in the clone written through, and it cannot make any line other
than a real `ok:` line contain `ok:`.

<!-- BEGIN recipe: witnessed-evidence (executed by crates/minds-cli/tests/doc_recipes.rs) -->

**0. Scratch directory and trust files.** A trust file passes when it has
at least one key line and every key line is
`<principal> namespaces="<role namespace>" <key type> <key> [comment]`, with
one principal per line:

```sh
: "${REPO:?}" "${WITNESS_SIGNERS:?}" "${INTENT_SIGNERS:?}" "${ANCHOR_SIGNERS:?}"
: "${SEAL:?}" "${FS_SEAL:?}" "${INTENT:?}"
case "$SEAL" in *[!0-9a-f]*) SEAL=invalid ;; esac; [ ${#SEAL} -eq 64 ] || SEAL=invalid
case "$FS_SEAL" in *[!0-9a-f]*) FS_SEAL=invalid ;; esac; [ ${#FS_SEAL} -eq 64 ] || FS_SEAL=invalid
case "$INTENT" in *[!0-9a-f]*) INTENT=invalid ;; esac; [ ${#INTENT} -eq 64 ] || INTENT=invalid
SCRATCH=$(mktemp -d) && cd "$SCRATCH" && echo "ok: working in a fresh scratch directory"
restricted() {
  awk -v ns="namespaces=\"$2\"" '
    /^[ \t\r]*(#|$)/ { next }
    { keys++; if ($1 ~ /,/ || $2 != ns || $3 !~ /^(ssh-|ecdsa-|sk-)/) bad = 1 }
    END { exit !(keys > 0 && !bad) }' "$1"
}
restricted "$WITNESS_SIGNERS" minds-witness && echo "ok: WITNESS_SIGNERS holds minds-witness keys only"
restricted "$INTENT_SIGNERS" minds-intent && echo "ok: INTENT_SIGNERS holds minds-intent keys only"
restricted "$ANCHOR_SIGNERS" minds-anchor && echo "ok: ANCHOR_SIGNERS holds minds-anchor keys only"
```

**1. The seal is the text it names.** The ref name is the domain-separated
BLAKE3 hash (`derive_key`) of the seal text, so a seal cannot be edited
without changing its name:

```sh
git -C "$REPO" --no-pager for-each-ref --format='%(refname:lstrip=3)' refs/minds/evidence/ \
  | grep -x '[0-9a-f]\{64\}'
git -C "$REPO" cat-file blob "refs/minds/evidence/$SEAL:seal" > seal.txt
test "$(b3sum --derive-key minds/evidence/v1/seal --no-names < seal.txt)" = "$SEAL" \
  && echo "ok: seal id matches its text"
grep -qx 'scope=witness/v1' seal.txt && echo "ok: a witness/v1 seal"
grep -x -e 'scope=[a-z0-9/-]\{1,40\}' -e 'session=b3-[0-9a-f]\{64\}' seal.txt
```

**2. A witness key signed it.** Witness seals carry `seal.sig`, signed under
`minds-witness`. In `minds verify`, a witness seal that the verdict checks
is `TAMPERED` without a valid one. In session verification, a file-system
seal without one is not counted: its observations drop out of the window,
and the session cannot reach A2 (see above).
`find-principals` only suggests who signed. The proof is `verify`:

```sh
git -C "$REPO" cat-file blob "refs/minds/evidence/$SEAL:seal.sig" > seal.sig
PRINCIPAL=$(ssh-keygen -Y find-principals -f "$WITNESS_SIGNERS" -s seal.sig | head -n 1)
ssh-keygen -Y verify -f "$WITNESS_SIGNERS" -I "$PRINCIPAL" -n minds-witness -s seal.sig < seal.txt \
  && echo "ok: witness signature by $PRINCIPAL"
```

**3. What the file observer saw.** A `witness-fs/v1` seal names its
observation object in `session=`. The object's name is its plain BLAKE3
hash. The object holds repo-relative paths and content hashes, never file
content. It is the second observer next to the agent's own report:

```sh
git -C "$REPO" cat-file blob "refs/minds/evidence/$FS_SEAL:seal" > fs-seal.txt
git -C "$REPO" cat-file blob "refs/minds/evidence/$FS_SEAL:seal.sig" > fs-seal.sig
test "$(b3sum --derive-key minds/evidence/v1/seal --no-names < fs-seal.txt)" = "$FS_SEAL" \
  && echo "ok: file-system seal id matches its text"
grep -qx 'scope=witness-fs/v1' fs-seal.txt && echo "ok: file-system seal"
PRINCIPAL=$(ssh-keygen -Y find-principals -f "$WITNESS_SIGNERS" -s fs-seal.sig | head -n 1)
ssh-keygen -Y verify -f "$WITNESS_SIGNERS" -I "$PRINCIPAL" -n minds-witness -s fs-seal.sig < fs-seal.txt \
  && echo "ok: file-system seal signed by $PRINCIPAL"
OBSERVATIONS=$(sed -n 's/^session=b3-\([0-9a-f]\{64\}\)$/\1/p' fs-seal.txt)
case "$OBSERVATIONS" in "" | *[!0-9a-f]*) OBSERVATIONS=invalid ;; esac
git -C "$REPO" cat-file blob "refs/minds/observations/$OBSERVATIONS:observations.json" \
  > observations.json 2>/dev/null \
  && test "$(b3sum --no-names < observations.json)" = "$OBSERVATIONS" \
  && echo "ok: observations are the ones the witness sealed"
```

**4. The intent is the approved version.** An intent anchor is four lines:
the format version, the source with its version, the hash of the
**redacted** requirement snapshot, and the expected scope. The anchor names
the snapshot. The ref name is the anchor, and the approver signs the
anchor under `minds-intent`:

```sh
git -C "$REPO" cat-file blob "refs/minds/intents/$INTENT:anchor" > anchor.txt
git -C "$REPO" cat-file blob "refs/minds/intents/$INTENT:snapshot" > snapshot.txt
git -C "$REPO" cat-file blob "refs/minds/intents/$INTENT:anchor.sig" > anchor.sig
test "$(b3sum --derive-key minds/intent/v1/anchor --no-names < anchor.txt)" = "$INTENT" \
  && echo "ok: intent id matches its anchor"
test "content=b3-$(b3sum --derive-key minds/intent/v1/content --no-names < snapshot.txt)" \
  = "$(grep '^content=' anchor.txt)" && echo "ok: anchor names this snapshot"
PRINCIPAL=$(ssh-keygen -Y find-principals -f "$INTENT_SIGNERS" -s anchor.sig | head -n 1)
ssh-keygen -Y verify -f "$INTENT_SIGNERS" -I "$PRINCIPAL" -n minds-intent -s anchor.sig < anchor.txt \
  && echo "ok: intent approved by $PRINCIPAL"
```

**5. A CI key countersigned the seal.** The countersignature lives under the
seal's own id. It names the seal, the pipeline and the time on CI's clock,
and it is signed under `minds-anchor`:

```sh
git -C "$REPO" cat-file blob "refs/minds/anchors/first-sight/$SEAL:anchor" > first-sight.txt
git -C "$REPO" cat-file blob "refs/minds/anchors/first-sight/$SEAL:anchor.sig" > first-sight.sig
grep -qx "seal=b3-$SEAL" first-sight.txt && echo "ok: countersignature names this seal"
PRINCIPAL=$(ssh-keygen -Y find-principals -f "$ANCHOR_SIGNERS" -s first-sight.sig | head -n 1)
ssh-keygen -Y verify -f "$ANCHOR_SIGNERS" -I "$PRINCIPAL" -n minds-anchor -s first-sight.sig < first-sight.txt \
  && echo "ok: countersigned by $PRINCIPAL"
grep -x -e 'project=[A-Za-z0-9._/-]\{1,200\}' -e 'pipeline=[0-9]\{1,20\}' \
  -e 'at=[0-9TZ:.+-]\{1,40\}' first-sight.txt
```

<!-- END recipe: witnessed-evidence -->

`head -n 1` takes the first principal `find-principals` names. If one key
appears under several principals in a role file, try each. With one key
per line and one file per role, the case does not arise.

**What the recipes do not establish.** They check each object on its own:
- **The seal belongs to your session.** A seal from another session or
  repository, signed with the same witness key, passes steps 1–2 just as
  well. Compare the `session=` line from step 1 with the session you are
  auditing. Nothing in steps 1–5 links `FS_SEAL` to `SEAL` either.
- **The intent was bound at the start of this session.** The binding is a
  link in the witness chain. The chain root can only be recomputed with
  the witness's journal and session salt.
- **The observation window covers the whole session without gaps.** That
  needs the epoch chain (`previous=`, `gaps=`).
- **The `at=` time is the first sight and correct.** Step 5 shows that a
  CI key signed a text naming this seal and that time. The time is CI's
  clock. "First sight" is what `minds anchor` does, not something the
  signature proves.
- **The level.** It also depends on the witness's profile and isolation
  evidence, the replay record, and the witness ledger.
- **That the ref exists exactly as named.** If `refs/minds/…/<id>` is
  missing, Git falls back to similarly named refs such as
  `refs/heads/refs/minds/…/<id>`, which anyone who can push a branch can
  create. Content and signatures are still checked, but a step can print
  `ok:` while `minds verify` reports the object missing.

`minds verify <commit> --signers <trust file> [--witness-home <dir>]` checks
all of that, except the `at=` caveat, which stays a limit at every level
(`wall_clock_time_anchored`). Name the commit, not the session: a replay
record only counts against the commit it replayed. Verify uses one combined
trust file in which every line carries its
role's namespace. The level computation enforces that restriction itself. Its result
is still a statement about the material it was given, read with the trust
file you supplied: `bundle_level_self_reported` holds at every level.

Replay records (`refs/minds/anchors/replay/<id>`, blobs `record` and
`record.sig`) work the same way: the id is the plain BLAKE3 hash of
`record`, and the signature is under `minds-anchor`.

## Retention

The bundle is a snapshot. It does not replace the repo: the source remains
`refs/minds/*`, and that travels with every clone. If you must retain for the
long term, retain the repo — the bundle is the form in which you hand it to a
verifier who does not want to operate Git.
