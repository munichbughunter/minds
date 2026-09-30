# EA-01a — Write-time content hash from the tool payload

- Commit: `feat(capture): write-time content hash from the tool payload`
- Branch: `feat/write-time-hash`
- Depends on: — · Size: M · Demo: yes

## Problem (found in the code)
`adapter::hash_artifacts` fills `Effect.content` for write effects by **reading the file
from the worktree at checkpoint time**. That fingerprints the file as committed, not what
the agent wrote. A human edit between the agent's write and the commit is hashed as the
agent's write. Reconciliation (EA-01) cannot be built on that.

## Goal
Every interpreted write carries an additional, **write-time** fingerprint derived only from
the observed payload bytes: `Effect.written`. `Effect.content` keeps its current meaning
(used by the evidence DAG, ADR-0011 decision 9).

## Non-goals
No change to `Effect.content`, to DAG edges, or to read effects. No reconciliation logic.
Codex/Gemini adapters: only if their payload carries full content; otherwise `None`.

## Read first
- `crates/minds-capture/src/adapter.rs` (`hash_artifacts`, `read_artifact`, checkpoint path)
- `crates/minds-capture/src/normalize.rs` (Claude adapter, `effect_for`, `adapter_version`)
- `crates/minds-core/src/lineage.rs` (`Effect`, `ContentHash`, hash-stability rules)
- `crates/minds-capture/src/secretwall.rs` (payload rewriting for secret files)

## Step 1 — fixtures first
Record a real Claude Code session in a scratch repo (hooks enabled) that performs `Write`,
`Edit`, `MultiEdit` (and `NotebookEdit` if cheap). Copy the raw `PostToolUse` journal
payloads, replace any sensitive text, and add them as golden fixtures under
`crates/minds-capture/tests/fixtures/claude-code/`. Document in the fixture README which
Claude Code version produced them. **Do not guess the payload shape.**

## Design
```rust
// minds-core/src/lineage.rs — additive, hash-stable
pub struct Effect {
    // … existing fields …
    /// blake3 der Bytes, die das Tool laut beobachtetem Payload geschrieben hat —
    /// Schreibzeitpunkt, nicht Checkpoint-Zeitpunkt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written: Option<ContentHash>,
    /// Warum `written` fehlt, wenn es fehlt (Interpretation, kein Beweis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_unavailable: Option<WrittenUnavailable>,
}
pub enum WrittenUnavailable { SecretFile, OutsideRepo, PayloadWithoutContent, ReconstructionFailed }
```
Derivation (in the Claude adapter, at checkpoint, from the raw journal payload):
- `Write`: `blake3(tool_input.content as UTF-8 bytes)`.
- `Edit` / `MultiEdit`: if the payload carries the pre-edit file content (check fixtures,
  e.g. an `originalFile`-like field in `tool_response`), apply the replacement(s) exactly
  as the tool does (`replace_all` semantics included) and hash the result; if the payload
  also carries a post-edit form, hash that instead and assert equality in tests. If
  neither is present → `None` + `PayloadWithoutContent`.
- Same fail-closed boundaries as `hash_artifacts`: no hash for secret files, absolute or
  `..` paths → `None` + reason.
- Bump the Claude adapter's `adapter_version`. `minds reinterpret` must show the new field
  for newly captured sessions; legacy sessions stay without it (legacy stays legacy).

## Acceptance criteria
- [ ] Sessions without `written` serialize byte-identically to before (existing test `additive_fields_do_not_change_canonical_form` extended).
- [ ] Write fixture → `written == blake3(content)`.
- [ ] Edit/MultiEdit fixtures → `written` equals the hash of the file the tool produced (asserted against a fixture copy of the resulting file), or a documented `None` reason if the payload lacks content.
- [ ] A human edit after the agent's `Write` and before checkpoint → `content ≠ written`.
- [ ] Secret file / outside path → `written = None` with the right reason, no hash computed.
- [ ] Interpretation is deterministic: same payload + same adapter version ⇒ same `written`.

## Tests
`written_hash_for_write_payload`, `written_hash_for_edit_payload`,
`written_hash_for_multiedit_payload`, `human_edit_before_checkpoint_diverges`,
`written_is_never_computed_for_secret_or_outside_paths`,
`written_is_deterministic_per_adapter_version`.

## Security review focus
The secret wall must run before any hashing; no oracle for short files outside the repo.
