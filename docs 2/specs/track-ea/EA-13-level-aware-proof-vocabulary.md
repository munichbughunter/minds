# EA-13 — Level-aware `PROVES` / `DOES_NOT_PROVE`

- Commit: `feat(core): proof vocabulary knows at which assurance level each sentence holds`
- Branch: `feat/level-aware-proofs`
- Depends on: EA-11 · Size: S · Demo: yes

## Goal
One canonical vocabulary, now with levels, so every surface (audit bundle, TUI, HTML,
verify) prints exactly the promises and limits that apply to the verified material.

## Read first
`crates/minds-core/src/evidence.rs` (`PROVES`, `DOES_NOT_PROVE`), all their consumers
(`grep -rn "DOES_NOT_PROVE\|PROVES" crates`), `docs/verification-guide.md`.

## Design
```rust
pub struct ProofSentence { pub id: &'static str, pub short: &'static str, pub text: &'static str,
                           pub holds_from: Level, pub holds_until: Option<Level> }
pub const PROVES_V2: &[ProofSentence]; pub const DOES_NOT_PROVE_V2: &[ProofSentence];
pub fn proves_at(level: Level) -> impl Iterator<Item = &'static ProofSentence>;
pub fn limits_at(level: Level) -> impl Iterator<Item = &'static ProofSentence>;
```
(`Level` is a `minds-core` mirror of the reader's `Assurance` ordinal, to keep `core` free
of reader types.) Keep `PROVES`/`DOES_NOT_PROVE` as derived constants for A1 so existing
consumers compile unchanged; migrate consumers to the level-aware API.

Mapping of today's limits (id → retired at):
- `append_to_seal_window` → retired at A2 (witness live chaining)
- `who_controls_keys` → narrowed at A2: replace by "witness key control is shown; human key custody still depends on allowed_signers"
- `lines_attributed` → retired at A2 (reconciliation with observations)
- `only_actor` → narrowed at A2: "changes in the worktree are observed; processes, network and other machines are not"
- `wall_clock_time` → narrowed at A3: "upper bound from the CI anchor; no lower bound"
- `reported_results` (new, A1–A2) → retired at A3 (replay)
- `model_identity`, `decision_correct`, `outside_boundary`, `root_compromise` (new) → never retired
New `PROVES` at A2: witness chaining, intent version + approver, reconciliation; at A3:
reproduced decisive results, first-sight time bound.

## Acceptance criteria
- [ ] Audit bundle (`minds audit --export`) carries the level and the level's sentence sets (golden).
- [ ] An A1 bundle still states the append→seal limitation; an A2 bundle does not, but states the A2 limits.
- [ ] TUI evidence mode shows the level-appropriate limits (snapshot).
- [ ] Every sentence has a stable `id` (used by tests and docs).

## Tests
`proof_sentences_have_unique_ids`, `a1_limits_include_append_window`,
`a2_limits_exclude_append_window`, `audit_bundle_carries_level_golden`.
