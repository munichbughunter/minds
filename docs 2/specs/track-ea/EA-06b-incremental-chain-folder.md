# EA-06b — Incremental chain folder

- Commit: `feat(core): incremental chain folder — same root as chain_salted, one link at a time`
- Branch: `feat/chain-folder`
- Depends on: — · Size: S · Demo: yes

## Goal
The witness chains each event on receipt. Provide an incremental fold whose result is
**provably identical** to `evidence::chain_salted` over the same items.

## Read first
`crates/minds-core/src/evidence.rs` — `chain`, `chain_salted`, `chain_from`, `Coverage`, tags.

## Design
```rust
pub struct ChainFolder { state: [u8; 32], first_seq: Option<u64>, last_seq: u64,
                         events: u64, pre_chain: u64, gaps: Vec<GapRecord> }
impl ChainFolder {
    pub fn new_salted(salt: &[u8; 32]) -> Self;   // start = derive_key(CTX_CHAIN, salt)
    pub fn new_unsalted() -> Self;                // start = [0; 32] (tests, golden vectors)
    pub fn push(&mut self, item: &ChainItem);     // exactly one iteration of chain_from
    pub fn head(&self) -> ContentHash;            // current state (shown live by the witness)
    pub fn snapshot(&self) -> ChainResult;        // what chain_from would return now
    pub fn to_state(&self) -> FolderState;        // persistable (serde), for crash recovery
    pub fn from_state(s: FolderState) -> Self;
}
```
Refactor `chain_from` to use `ChainFolder` internally so there is exactly one
implementation of the link step.

## Acceptance criteria
- [ ] Property test: for random item sequences, `ChainFolder` pushed item by item == `chain_salted` / `chain`.
- [ ] Existing golden vectors in `evidence.rs` unchanged.
- [ ] `to_state`/`from_state` roundtrip continues to produce the same root.
- [ ] Invariant tests from ADR-0011 still pass unmodified.

## Tests
`folder_equals_batch_fold`, `folder_state_roundtrip_continues_identically`,
`folder_head_changes_on_every_push`.
