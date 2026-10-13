# ADR-0013 — Conformance against a human-signed spec

- Status: accepted
- Date: 2026-10-13
- Affects: `minds-core`, `minds-redact`, `minds-git`, `minds-store`, `minds-reader`, `minds-attest`, `minds-cli`, `minds-tui`
- Related: [ADR-0011](0011-evidence-chain.md) (evidence chain),
  [ADR-0012](0012-witnessed-evidence.md) (witnessed evidence)

## Context

Minds proves *what happened* (sessions, seals, witness observations) and *what was asked*
(intent anchors). It did not say whether the result conforms to what the team decided the
system should look like: crate boundaries, data contracts, "every agent commit carries a
signed intent". Checking that is easy; trusting the check is not. The agent runs as the same
user: it edits the working tree, commits, writes the object database and `refs/minds/*`, and
can put programs on `PATH`. A rule it can change — in the rule file or in a file the rule
only names — is no rule.

## Decision

1. **The spec is evidence, signed by a human — as an envelope.** `.minds/spec.json`
   (`minds-spec-v1`) is read tolerantly and identified by its canonical form (RFC 8785):
   `id = derive_key("minds/spec/v1", canonical)`. `minds spec sign` signs an envelope
   (`minds-spec-signed-v1`): the spec in `HEAD`, a **pin** (hash) of every file the spec names
   only by path (the schemas of `json-schema` rules) and **`at`**, the `HEAD` commit. The
   signature uses the ssh-sig namespace `minds-spec` and counts only from a principal whose
   every `allowed_signers` line is restricted to `namespaces="minds-spec"`. Envelope and
   signatures live under `refs/minds/specs/<id>` (content-addressed, synced, `git fsck`
   clean); envelopes and signatures are added side by side and never replace each other.
2. **Which spec applies is decided by signatures and history — not by files, not by
   the store.** The store is agent-writable: envelopes can be deleted or the ref
   rewound. So an envelope signed at `at` counts for a commit C only if it is **fresh**:
   - `at` ∈ history(C);
   - every descendant of `at` in history(C), C included, carries exactly the signed
     state (spec id and every pinned file);
   - every other commit in history(C) \ history(at) (side branches forked before
     `at`) carries that state or one that already existed in history(at).

   Consequences of that definition:
   - Any change after the signature means DRIFT until the current state is signed.
     This covers a rollback (directly, through a removal, padding, a merge with either
     side first, or back to a state that was valid before a later signature), a looser
     schema pin, a reverted unsigned experiment, and a rewound or emptied store.
   - PASS needs a fresh, validly signed envelope for C's spec. Only envelopes whose
     signing commit and C carry exactly their state cost a signature check.
   - Otherwise DRIFT, with the rules from the newest valid envelope by commit
     generation, never from C's spec. With no valid envelope at all, no rule of the
     unsigned spec is evaluated.
   - Undecidable (walk or read budget, shallow clone, damaged objects, unchecked
     signatures): UNKNOWN.
3. **The result is a derivation.** `minds_reader::conformance::evaluate` computes per rule
   *expected → observed → PASS/DRIFT/UNKNOWN* from the commit's objects and the signed
   envelope. Nothing is written; anyone with the objects and the signer file recomputes the
   same rows. `minds verify` always shows it when a spec exists and never changes the exit code
   for it; `--conformance` turns DRIFT/UNKNOWN into a gate (exit 2, never masking 1/3/4) for CI.
4. **Read without trusting the object database or `PATH`.**
   - Listing and reading go through `Repo::list_blobs_verified`, `read_blob_verified` and
     `parents`: every commit, tree and blob is size-checked at the header and re-hashed; trees
     are checked for git's sort order and for names `git fsck` refuses.
   - Budgets cap entries, path and tree bytes, read bytes and schema steps. Symlinks and
     submodules are reported, and one under a rule's glob is UNKNOWN. Anything unreadable or
     over a limit is UNKNOWN.
   - No `git` process runs, so no repository config, hook or fsmonitor of the agent.
   - Signing and checking use only an `ssh-keygen` whose file and directory chain are
     root-owned and not group/other-writable (Unix). After signing, the signature is
     checked against the given public key and the exact envelope before it is stored.
     Keys without a `minds-spec` line are rejected in-process, and only their
     signatures are kept in the store.
5. **No new dependency.** Rule sources use bounded hand-written subsets:
   - a line-based Cargo manifest reader, which resolves renames and workspace dependencies;
     unknown forms make the manifest unreadable, i.e. UNKNOWN;
   - a lexer-backed `use`-tree scanner;
   - a Draft 2020-12 JSON Schema subset, where an unknown keyword or an unexpected value form
     is UNKNOWN.

   The static-binary promise holds.

## Rule kinds

| Kind | Expected | Observed |
|---|---|---|
| `no-dependency` | crate `A` has no (transitive, workspace) dependency on `B` | the dependency path `A → … → B` |
| `no-import` | no file under `paths` names a Rust path | `file:line uses path` |
| `json-schema` | files match the signed version of a schema | `file: /pointer violates keyword` |
| `graph` | commits changing `paths` carry a session / bind each session to a signed intent | session counts from trailers and the store index |

## Consequences

- A rule the reader does not understand is UNKNOWN, never PASS; CI with `--conformance` fails
  until the spec or minds is updated.
- The store keeps every envelope of a spec (never dropped; at most 64), so older commits
  stay PASS under the envelope that was fresh for them. A squash or rebase (the signing commit leaves the history) and
  a deliberate return to an older spec need a new signature.
- A spec holds at most 64 envelopes; an evaluation places at most 256 envelopes whose
  signing commit exists locally and walks at most 200 000 commits (beyond: UNKNOWN). Pruning
  envelopes no history reaches is follow-up work.
- Two branches with differently signed specs that are merged need a signature on the result —
  with one residual: a state that existed before `at` and is signed again only on a side
  branch forked before `at` looks, from history alone, like a stale branch; merging that
  branch while keeping the newer state stays PASS. A rewound **branch** (reset below the
  newer signing commit) likewise only shows the history that remains: rollback protection
  holds for commits whose history contains the newer signature, i.e. gate the protected
  branch or the merge result in CI.
- Which valid envelope supplies the rule rows under DRIFT follows commit generation, which the
  agent can influence; it never affects the spec row or the gate.
- A spec removed while the store holds no readable envelope gives no report in plain
  `verify` and the TUI; `--conformance` still fails ("not assessed").
- The TUI computes conformance synchronously while idle; on very large histories a selection
  can block for the duration of one evaluation (follow-up: a worker thread).
- CI needs the full history back to the signing commit (`fetch-depth: 0`); in a shallow clone
  the spec row is UNKNOWN.
- A `graph` rule judges one commit (its diff against the first parent); CI must check every
  commit of a change. A branch based before a newer signature is judged against the spec
  of its own history — gate the merge result, not only the branch head.
- `no-import` reads `use` trees and follows aliases, but it is a text match, not a compiler:
  it is not robust against an adversarial agent (macro-generated code, module paths it does not
  resolve). Over its limits it is UNKNOWN. Crate boundaries an agent must not cross belong in
  `no-dependency`.
- A PASS is only as strong as the signer file. Locally that is `~/.ssh/allowed_signers`,
  which the agent can write; CI should pass `--signers` with a file outside the agent's reach.
- Signature checks are bounded per run (envelopes in the history first, at most 64
  `ssh-keygen` checks); beyond that the result is UNKNOWN.
- Graph facts (sessions, intents) are at the assurance level of the sessions: at A1 the agent
  can write sessions itself. The witness does not yet attest the active spec at observation
  time; that is follow-up work (witness by default).
- On Windows, `ssh-keygen` ownership is not checked (the first one outside the checkout is
  used).
- A fresh clone without `refs/minds/specs/*` reports the spec as unsigned ("fetch
  refs/minds/* if it was signed elsewhere") — DRIFT, not silence.
