# EA-08 — The file-system observer: the witness's second eye

- Commit: `feat(cli): witness file-system observer — independent write observations`
- Branch: `feat/witness-fs-observer`
- Depends on: EA-06c · Size: M · Demo: yes

## Goal
The witness watches the worktree and records every content change as
`fs.observed { path, content, at }` in its own chained stream, sealed with scope
`witness-fs/v1` and stored as a redacted observation object. Reconciliation (EA-01) uses
these observations to turn claims into `explained` and to catch writes nobody claimed.

## Non-goals
Process tracing (exec observation), network observation, anything outside the worktree.

## Read first
`00-conventions.md` (W1, W5, W7), EA-06c, `crates/minds-capture/src/adapter.rs`
(`hash_artifacts` boundaries), `crates/minds-redact/src/secretfile.rs`,
`crates/minds-store/src/store.rs`, `git_store.rs`, `layout.rs`, `crates/minds-core/src/evidence.rs`
(`SealOutcome`, seal parser), `crates/minds-reader/src/reconcile.rs` (EA-01).

## Design

### Watching
- Crate `notify` (check musl build in CI before committing — if it breaks the static
  build, stop and report). Recursive watch on `repo_root` (host path), debounce 100 ms per
  path, coalesce bursts.
- Ignore: `.git/`, anything matched by `.gitignore` / `.git/info/exclude` (evaluate via
  `git check-ignore --stdin` in batches, or gix's exclude stack if available — no guessed
  API, check docs.rs), and the witness home if it happens to be inside the repo (refuse to
  start in that case instead).
- For each settled change: read the file (cap 16 MiB; larger → `content: null`,
  `reason: "too_large"`), blake3 it. Secret files (`minds_redact::is_secret_file`) →
  `content: null`, `reason: "secret_file"`, **no hash computed**. Deletions →
  `content: null`, `reason: "deleted"`. Paths are stored repo-relative (host → repo-relative;
  never the host absolute path).

### Stream and chaining
- The witness's own stream: `SessionKey { agent: "witness", local_id: <stream epoch id> }`
  — the same stream that already holds `witness.start`/`witness.stop` (EA-06c).
- Each observation is a journal event with `raw_kind = "fs.observed"` and payload
  `{"path":…,"content":"b3-…"|null,"reason":null|…}`, chained on append like hook events.

### Sealing and storage
- On every witness checkpoint (EA-06c `checkpoint_now`), the witness stream's current
  epoch is sealed too: scope `witness-fs/v1`, new outcome value `observations_stored`,
  `session=` carries the **observation object id** (document this reuse of the field in
  the seal docs and ADR-0012). Parser: accept the new outcome value (golden tests;
  older binaries reject it — same accepted trade-off as schema 2).
- Observation object: canonical JSON (RFC 8785)
  `{"schema":1,"first_at":…,"last_at":…,"observations":[{"seq":…,"at":…,"path":…,"content":…,"reason":…}]}`,
  paths passed through the redaction pipeline fail-closed. Type-guarded like sessions:
  `RedactedObservations` without a public constructor, produced only by the pipeline;
  `put_observations` accepts only that type. Stored under
  `refs/minds/observations/<64hex>` (one ref per object, ADR-0010 pattern), picked up by
  `minds sync`.
- `minds forget` does not apply to observation objects (they contain only paths and hashes);
  document why in the privacy overview (EA-21).

### Reader integration
- `minds-reader`: `observations_in_window(from, to) -> Vec<FsObservation>` over stored
  observation objects; reconciliation (EA-01) receives them for the linked sessions'
  window (± 30 s).
- New corroboration derivation: for each write claim with `written = H` at path P, find an
  observation of H at P within `[claim_at − 2 s, claim_at + 30 s]` → `Corroborated`,
  otherwise `Uncorroborated`. Exposed on the reader model for EA-11/EA-12.

## Acceptance criteria
- [ ] A write in the (simulated) container appears as `fs.observed` with the correct hash within 1 s.
- [ ] Ignored paths, `.git/`, secret files: no hash ever computed (assert via a counting hasher in tests).
- [ ] Burst of 1000 files → one observation per final state, no crash, bounded memory.
- [ ] Witness checkpoint produces two seals: `witness/v1` (agent session) and `witness-fs/v1` (observations), both signed under `minds-witness`.
- [ ] Observation object roundtrip, canonical bytes golden-tested, `git fsck` clean.
- [ ] Reconciliation fixture: agent Write + matching observation → `Explained`; shell write with observation only → `ExplainedFsOnly`; forged claim without observation → `Uncorroborated`.

## Tests
`fs_observer_records_write`, `fs_observer_never_hashes_secret_or_ignored`,
`fs_observer_coalesces_bursts`, `witness_checkpoint_seals_both_streams`,
`observation_object_canonical_golden`, `corroboration_window_rules`,
`reconcile_with_observations_end_to_end`.

## Security review focus
Symlinks inside the worktree must not be followed out of the repo (hash the link target
only if it resolves inside the repo root; otherwise `reason: "outside_repo"`). No content,
only hashes, ever leaves the witness process.
