# ADR-0011 — Evidence Chain: provable observation, provable gaps

- Status: accepted
- Date: 2026-08-24
- Affects: `minds-core`, `minds-capture`, `minds-store`, `minds-cli`, `minds-reader`, `minds-tui`
- Related: ADR-0003 (hooks over transcript parsing), ADR-0004 (store index, evidence classes),
  ADR-0008 (signed attribution), ADR-0010 (one ref per session);
  implementation plan: Track EV

## Context

Minds is cryptographically **addressed**: `SessionId = BLAKE3(canonical session)`, sessions
live content-addressed under `refs/minds/store/`, attribution and reviews are signable.
What an auditor can check with that is the integrity of **what was stored**.

What they cannot check is the **capture scope**: how do they know that nothing is missing
between two events? That a hook did not fail? That no session existed whose storage the
redaction rejected? Today the journal detects sequence gaps on read — but `checkpoint`
discards that finding, deletes the journal afterwards, and a redaction block leaves nothing
behind except a `hook.log` line. A detected gap is not proof of a gap, and an event that
does not exist cannot be signed.

The audit bundle itself says so honestly: "proves no completeness". This ADR turns that
limitation into a checkable statement.

## Principle

> **Minds must never infer the absence of an event from the absence of evidence.**
> No evidence record means *unknown* — unless the observation scope is sealed, and the
> absence is thereby itself a checkable statement.

Three separations follow, valid everywhere:

1. **VALID ≠ COMPLETE.** Integrity ("the records that exist are unchanged") and coverage
   ("the observation scope is captured without known gaps") are separate judgments.
2. **Evidence is immutable. Interpretation is recomputable.** Only what was observed
   (raw-data facts) is hashed and sealed; every interpretation (adapter normalization,
   edge heuristics, status upgrades) is versioned and repeatable without touching the evidence.
3. **Hash every event, sign every checkpoint.** Every journal event carries hashes; what
   gets signed is the seal of a range, never the individual event — the hot path stays
   fail-open and cheap.

More precisely, there are **three trust axes** that are never blended into one status:

```text
              Evidence
                 │
    ┌────────────┼────────────┐
    ▼            ▼            ▼
 Integrity    Coverage    Interpretation
 "changed?"  "missing?"  "what does it mean?"
    │            │            │
 Hashes/seals  Gaps/scope/  Adapters,
 chain         epochs       capture, DAG
```

An unknown tool is an **interpretation** problem, a hook failure a **coverage** problem, a
modified journal an **integrity** problem. `minds verify` states the three axes separately
(`Integrität` / `Coverage` / `Deutung` / `Gesamt` — integrity / coverage / interpretation /
overall); the exit codes remain the CI contract from integrity × coverage — interpretation
never upgrades or downgrades the verdict.

## Decision 1: Evidence hashes in the hot path, chaining at seal time

Every journal event gets two additive fields on append:

- `payload_hash = derive_key("minds/evidence/v1/payload", payload)` — over the payload
  **after** the secretwall (the oracle rule from `lineage.rs` applies unchanged: for secret
  files, no hash over secret content is ever produced, because the wall replaces the
  content beforehand).
- `event_hash = derive_key("minds/evidence/v1/event", encode(seq, at, at_nanos, raw_kind,
  cwd, transcript_path, payload_hash))` — over a **length-prefixed binary encoding**
  (u64-LE length per field, option tags), not over JCS: `at_nanos` exceeds 2^53, and the
  canonical JSON form deliberately rejects that. Only observed facts are hashed — `kind`
  (the classification) is interpretation and stays out.

**No `prev_event_hash` in the event.** Seq assignment is lock-free (`create_new`); the
neighboring event may still be a `.tmp` file while our own append runs. A best-effort prev
link would produce legitimate "prev unknown" markers indistinguishable from tampering — a
verification primitive that regularly emits false positives is worthless. The chain is
built deterministically at seal time over the sorted seq sequence:

```text
h_0 = derive_key("minds/evidence/v1/chain", 0^32 ‖ tag ‖ item_hash_0)
h_i = derive_key("minds/evidence/v1/chain", h_{i-1} ‖ tag ‖ item_hash_i)
```

with `tag 0x01` = event (`item_hash` = `event_hash`) and `tag 0x02` = gap (`item_hash` =
hash of the gap record) — **a gap is itself a chain link**, not silence.

**The fold is salted.** The root travels to the forge in the seal, and `seq` and
`last_event_at` sit next to it there in plaintext — unsalted, the root of a
one-event epoch would be an offline oracle: anyone who guesses the payload (a
short password, a PIN in the prompt) could recompute the root and confirm the guess.
Therefore the fold starts on `derive_key(ctx, salt)` with a random 32-byte salt
per session, stored **locally** next to the epoch state (0600, never pushed).
The price is intentional: an outsider cannot recompute the root from guessed
payloads — locally (`fsck`, before the discard) it remains recomputable, because
the salt is readable there.

**The salt does not heal.** Once an epoch is sealed, a missing or corrupted salt
is **not** regenerated: a new salt would seal the same evidence under a second,
diverging root — an epoch fork (same evidence, different cryptographic
identity), contradicting the determinism claim "same events ⇒ same seal".
Instead, the loss itself is the finding: the checkpoint visibly defers the
session (`hook.log`), the journal stays put, the epoch is treated as no longer
reproducible. Only before the first seal may a salt be created or replaced —
at that point no seal has committed to a root yet.

**Named trade-off:** Between append and seal, only the filesystem protects the journal
(0700/0600, symlink refusal) — exactly today's state. A local attacker with write access can forge,
up to the checkpoint, what then gets sealed "cleanly". The self-describing event hashes
make after-the-fact payload swapping detectable on journals *at rest* (`fsck` recomputes);
more — signing hooks — would violate the hot-path budget and remains an outlook item. This
window is listed in the verification guide (`docs/verification-guide.md`) under
"does not prove".

## Decision 2: Coverage as epoch seals with explicit gaps

A **seal** seals exactly the range the checkpoint actually read — never more. Line-based
format (like `minds-attestation-v1`, line count fixed = 12, fields validated fail-closed
following the #12 pattern):

```text
minds-seal-v1
root=b3-<64hex>          chain root over events and gap records
agent=<agent>
first_seq=<n>            range actually read
last_seq=<n>
events=<n>
gaps=<n>                 missing/corrupted links, each its own chain link
pre_chain=<n>            legacy events without stamped hashes (pre-existing)
outcome=stored | storage_policy_rejected_payload
session=b3-<64hex> | -
previous=b3-<64hex> | -  seal of the previous epoch of the same session
last_event_at=<RFC3339>  from the last event, not a wall clock
```

`seal_id = derive_key("minds/evidence/v1/seal", bytes)`. The seal is deterministic:
same events ⇒ same seal ⇒ idempotent storage.

**Epochs:** After `journal.discard` the same session starts again at `seq 0` — every
checkpoint epoch becomes its own session and its own seal, chained via `previous`
(local state under `<git-dir>/minds/evidence/state/`, never pushed). If the state is
missing (fresh clone), epochs stand unconnected: the verdict then honestly says
"incomplete". `verify` may close epochs heuristically at read time via `lineage.local_id` —
shown as a heuristic, **never** upgrading the verdict. What was lost before the first read
event (a crash before any gap record) is not claimed: the seal claims only
`first_seq..last_seq`; the rest falls under "epoch chain open".

## Decision 3: A redaction block leaves a seal behind

If the fail-closed redaction rejects a session, **no** session object is created —
but from now on there is a seal with `outcome=storage_policy_rejected_payload` and `session=-`. It
contains chain root, counts, agent, time range — **no** intent, no paths, not even the name
of the field the redaction failed on (the `RedactionAudit` stays local). The auditor sees:
a session existed, its range is sealed, the storage policy rejected the payload — integrity
valid, coverage incomplete. If the checkpoint succeeds after a policy fix, the success seal
chains via `previous` onto the block seal: the history stays traceable. The journal stays
put, as before.

## Decision 4: Seals live in their own namespace and survive `forget`

```text
refs/minds/evidence/<64hex seal_id>   parentless commit; tree: seal [+ seal.sig]
refs/minds/store/<64hex session>      session.json, links.json, new: evidence.json
```

`evidence.json` (mutable like `links.json`, never canonical) carries the back-references
session → seals. The seal namespace is decoupled from the payload: `forget` erases
`session.json`, the seal remains as payload-free proof — it contains only hashes
and counts, nothing erasable. Seals are never erased and never force-pushed; `minds sync`
picks up the namespace automatically (one push for everything, ADR-0010). Ref names never
contain a `local_id` derivative — no oracle for externally assigned identifiers on the forge.

## Decision 5: Signature on the seal, optional and best-effort

If `user.signingkey` is configured, the checkpoint signs the exact seal bytes as stored
(`ssh-keygen -Y sign -n minds`, ADR-0008) and places `seal.sig` next to them — the pattern
of the review signatures. Without a key, the seal remains **hash-valid** (integrity comes
from the content-addressed ref name); the signature adds the authorship binding. `minds sign
--seal <id>` retrofits it. A signature failure does not break the checkpoint.

## Decision 6: Evidence in two dimensions — source × status

The previous `Evidence` enum (`Inferred < Declared < Content < Observed`) conflates two
questions: *Where does the statement come from?* and *Was it checked?* It gets split:

```rust
EvidenceSource { Heuristic, HumanDeclared, ContentDerived, Observed }   // Ord = trust
EvidenceStatus { Missing, Unknown, Partial, Verified }                  // Ord
EvidenceMark   { source, status }
```

- **Verified means recomputed** — cryptographically or via content evidence, at
  verify/read time. It is never frozen into stored bytes and never granted without a
  check. That is why legacy values map to `Unknown`: `observed→(Observed, Unknown)`,
  `content→(ContentDerived, Unknown)`, `declared→(HumanDeclared, Unknown)`,
  `inferred→(Heuristic, Unknown)`. No legacy edge was ever recomputed; the principle
  forbids reading the absence of the check as passing it.
- **Read tolerantly, write canonically:** The deserializer accepts the legacy string and
  the object form; writes always use the object form. `SCHEMA_VERSION = 2` — for the
  first time, the bump also genuinely separates readability: newer binaries read all
  older versions, older binaries do not read schema 2. Existing data is not migrated
  (content-addressed = immutable); legacy sessions are the representable state
  "captured before Evidence Chain".
- **Merge instead of `max()`:** Two dimensions have no total order. Rule: `source` first
  (the previous trust order); with equal source, `status` decides; the stronger source
  wins with its complete mark. Invariant: **stored** marks never carry `Missing` —
  `Missing` exists only in verify/reader output.
- Observation and interpretation are additionally separated by `ToolCall.capture`
  (`interpreted | uninterpreted`, with `adapter` and `adapter_version`): a tool call that
  no adapter interprets appears as "observed, not interpreted" instead of silently
  vanishing — including for agents without an adapter of their own (generic fallback).

## Decision 7: One verdict in two axes, fixed exit codes

`minds verify <session-id>` (and `--evidence <seal-id>` for sessionless seals) judges in
the matrix integrity × coverage:

| | Coverage complete | Coverage incomplete/unknown |
|---|---|---|
| Integrity intact | `VERIFIZIERT` (verified) | `VERIFIZIERT, UNVOLLSTÄNDIG` (verified, incomplete) |
| Integrity violated | `MANIPULIERT` (tampered) | `MANIPULIERT` (tampered) |
| No material | — | `NICHT VERIFIZIERBAR` (not verifiable) |

Exit codes (CI contract): **0** VERIFIZIERT · **1** MANIPULIERT · **2** VERIFIZIERT,
UNVOLLSTÄNDIG · **3** NICHT VERIFIZIERBAR. Coverage complete ⇔ `gaps=0 ∧ pre_chain=0 ∧
outcome=stored ∧` epoch chain closed. A legacy session without a seal is `NICHT
VERIFIZIERBAR (vor Evidence-Chain erfasst)` — not verifiable, captured before the Evidence
Chain: a state, not an error. `fsck` gets the counterparts: hash recomputation of journals
at rest and seal checking as **findings**, `--require-seal` as a gate analogous to
`--require-review`.

## Decision 8: Adapters sit ABOVE the chain, interpretation is deterministic (phase 5)

The `ToolAdapter` trait (registry, one adapter per agent, `adapter_version` from the
implementation) interprets journal events and stored calls — it **never** changes their
bytes, hashes, or identity: `Raw evidence → Chain → Adapter → Interpretation`, never the
other way around. Interpretation is deterministic (same evidence + same adapter version ⇒
same interpretation, test-pinned) — otherwise `minds reinterpret` would be worthless.
`minds reinterpret <session>` delivers on "Interpretation is recomputable":
strictly read-only, it shows, for each call, the evidence address (unchanged) and the
stored and current interpretations side by side.

## Decision 9: The evidence DAG is a projection (phase 6)

The chain remains the temporal, append-only provenance. Semantic relationships above it —
content handovers: "B read exactly the bytes A wrote" — are projected **at read time** from
the stored content hashes (write hash == read hash at the same path; for this, since
phase 6, read effects are hashed too — but only for repo-relative paths **tracked** by git:
tracked content is visible to every repo reader anyway; its hash reveals nothing new.
Merely reading a private or repo-external file never produces a fingerprint — an unsalted
content hash over a short file would be the same confirmation oracle the chain root is
salted against. The secret-file exception is unchanged). None of this is stored: recomputable at any
time, deterministically sorted. These edges are the first place that produces
`(ContentDerived, Verified)` — not observed, not claimed, but **recomputed**.

## Decision 10: Proof bundles — and why there is no `full` (phase 7)

`minds audit --export --mode proof` exports only the proof scaffold (ids, canonical
payload texts, seals including signatures, verdict metadata — no intent, no comments):
checkable without passing content along. `redacted` remains the default and the
**maximum** — a `full` mode deliberately does not exist, because the store holds
exclusively redacted sessions (fail-closed); promising more would be empty or a leak.
`proves`/`does_not_prove` are part of the product model, not of the docs: they travel in
the artifact and prevent the drift from "we have evidence" to "we prove that nothing else
happened".

## The invariants (test-pinned)

1. **Every chain link is bound to exactly one predecessor** — in the fold: `h_i` covers
   `h_{i-1}` (`invariant_each_chained_link_is_bound_to_exactly_one_predecessor`).
   Deliberately NO prev link in the event itself (decision 1).
2. **The fold state encompasses the predecessor** — ditto.
3. **The event hash covers the observed facts** — and only those; `kind` is interpretation
   (`invariant_the_event_hash_covers_the_observed_facts_and_only_those`).
4. **A gap is itself verifiable evidence**
   (`invariant_a_gap_is_itself_verifiable_evidence`).
5. **Coverage is always scoped** — a seal without `scope=` does not parse; "complete"
   means complete within the boundary, never "all system activity"
   (`invariant_coverage_is_always_scoped`).
6. **Interpretation never changes raw evidence** — `reinterpret` moves no ref
   (`reinterpret_is_read_only_and_deterministic`).
7. **Legacy stays legacy** — explicit state instead of `None`, never retroactively
   embellished (`invariant_legacy_stays_legacy`, `Provenance::Legacy`).
8. **"Not captured" ≠ "did not happen"** — the principle; gap links, block seals, and the
   `does_not_prove` list are its implementation.

The hash domains are versioned namespaces (`minds/evidence/v1/…`,
`invariant_the_hash_domains_are_versioned_namespaces`): a future `chain-v2` can exist
alongside v1 without reinterpreting historical data.

## Rejected alternatives

- **prev_hash in the event** (vision §4): racy in the lock-free journal, see decision 1.
- **Per-event signatures / a signing hook:** violates the hot-path budget (fail-open, no
  wait time in the agent); the append-seal window is named instead of signed away.
- **Merkle tree over event ranges:** a linear chain suffices at session scale; selective
  partial proofs of individual events are not a current need. Outlook.
- **External time anchors (RFC 3161, OpenTimestamps):** Minds stays offline-/air-gap-capable;
  "when, really" remains unproven and is stated in the verification guide. Outlook.
- **Transparency log / global consistency:** would turn local provenance into a
  global-consistency problem (gossip, witnesses, fork consistency, PKI) — massive
  overengineering for a local evidence system.
- **Retroactive migration of old sessions:** would create false assurance — a chain
  produced today over yesterday proves nothing about yesterday. Legacy stays legacy.
- **Seal only as `seal.json` in the session ref:** fails at the redaction block (no
  session ref exists) and would couple the proof to the payload's fate under `forget`.
- **Mapping legacy to `Verified`:** would devalue the status dimension from day one
  (Verified without a check) and contradict the principle.

## Consequences

Minds can now keep four statements distinct: *observed* (event with hashes in
a sealed range), *not observed* (gap link in the chain, or an open epoch chain), *inferred*
(heuristic, marked as such, never upgraded), and *not capturable* (block seal,
uninterpreted call). The audit bundle gains the statement "for sealed ranges, tampering and
gaps are cryptographically detectable" — and honestly names what still remains unproven:
integrity between append and seal, events outside sealed ranges, real wall-clock time.
External verifiers do not need Minds: `git cat-file`, BLAKE3 `derive_key`, and `ssh-keygen -Y
verify` suffice (recipe in the verification guide, `docs/verification-guide.md`).

The price: schema 2 is unreadable for old binaries (central distribution, no existing
users — accepted), and the chaining guarantee begins only at the seal, not at the append.
