# EA-06a — Journal/epoch roots and an extracted checkpoint core

- Commit: `refactor(capture): journal and epoch state at explicit roots, checkpoint core reusable`
- Branch: `refactor/checkpoint-core`
- Depends on: — · Size: M · Demo: yes

## Goal
Make the existing capture pipeline usable by a second writer (the witness) without
duplicating it: journal and epoch state can live at an arbitrary root, and the checkpoint
logic becomes a function parameterized by where evidence comes from, which scope it seals
under, and how the seal is signed. **Pure refactor: behaviour and output unchanged.**

## Non-goals
No witness, no new scope in use, no new CLI flags.

## Read first
`crates/minds-capture/src/journal.rs` (`Journal::open/discover/root`),
`crates/minds-capture/src/epoch.rs` (`EpochState::open`, `STATE_DIR`),
`crates/minds-cli/src/checkpoint.rs` (entire file), `crates/minds-cli/src/sign_cmd.rs`.

## Design
- `Journal::at(root: impl Into<PathBuf>) -> Journal` — the directory that today is
  `<git-dir>/minds/journal`. `Journal::open(git_dir)` becomes `Journal::at(git_dir.join("minds/journal"))`.
  Same for `EpochState::at(root)`; `open(git_dir)` delegates.
- Permission hardening (0700/0600, symlink refusal) applies identically at any root.
- Extract from `checkpoint.rs` into `crates/minds-cli/src/checkpoint/core.rs`:
```rust
pub struct EvidenceSource<'a> { pub journal: &'a Journal, pub epochs: &'a EpochState, pub scope: &'static str }
pub enum SealSigner<'a> { UserConfig /* today: user.signingkey */, Key { path: &'a Path, namespace: &'static str }, None }
pub struct CheckpointEnv<'a> { pub repo: &'a Repo, pub root: &'a Path, pub log_dir: &'a Path,
                               pub store: &'a dyn ContextStore, pub pipeline: &'a RedactionPipeline,
                               pub tracked: Option<&'a BTreeSet<String>> }
pub struct CheckpointOutcome { pub stored: Vec<SessionId>, pub sealed: Vec<SealSummary> }
pub fn run_checkpoint(env: &CheckpointEnv, src: &EvidenceSource, signer: &SealSigner) -> Fallible<CheckpointOutcome>;
```
  `checkpoint()` (the command) becomes: build env, call `run_checkpoint` with
  `scope = SCOPE_AGENT_HOOKS_V1`, `SealSigner::UserConfig`, then attach trailers and
  index edges exactly as today.
- `minds_attest::ssh_sign` gets a sibling `ssh_sign_ns(payload, key, namespace)`;
  `ssh_sign` delegates with `NAMESPACE` (EA-09 adds the namespace constants — here only
  the parameter).

## Acceptance criteria
- [ ] All existing tests pass unchanged, in particular `end_to_end.rs`, `seals.rs`, `audit.rs`, `pilot.rs`.
- [ ] A golden test: a fixed journal checkpointed via the old command path and via `run_checkpoint` produces byte-identical seals and session ids.
- [ ] `Journal::at` on a fresh dir applies 0700; on a symlinked root it refuses.
- [ ] No public behaviour change visible in `git diff` of `docs/`.

## Tests
`checkpoint_core_is_byte_identical_to_command_path`, `journal_at_arbitrary_root_hardens_permissions`,
`epoch_state_at_arbitrary_root_roundtrip`.
