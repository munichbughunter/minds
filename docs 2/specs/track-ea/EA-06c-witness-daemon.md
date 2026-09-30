# EA-06c — `minds witness run`: the single writer with live chaining

- Commit: `feat(cli): minds witness — single-writer evidence daemon with live chaining`
- Branch: `feat/witness-daemon`
- Depends on: EA-05, EA-06a, EA-06b, EA-09 · Size: L · Demo: yes

## Goal
A long-running process on the host that receives hook frames, appends them to its own
journal, chains every event on receipt, and — on request — checkpoints and seals with
scope `witness/v1`, signed by the witness key under namespace `minds-witness`. The agent
side cannot reach anything but the socket.

## Non-goals
File observer (EA-08), hook-side forwarding (EA-07), checkpoint delegation protocol from
the container (EA-06d — here only the internal "checkpoint now" entry point), `enable`
integration (EA-10), sealing the witness's own stream with its lifecycle events (EA-08 —
here they are only appended and chained). One witness serves **one repository**.

## Read first
`00-conventions.md` (state dir, W1–W4), `crates/minds-cli/src/hook.rs` (what `record`
does with stdin), `crates/minds-capture/src/hook_event.rs::parse`, `secretwall.rs`,
`checkpoint/core.rs` (EA-06a), `evidence.rs` `ChainFolder` (EA-06b),
`witness_proto.rs` (EA-05), `crates/minds-cli/src/hooklog.rs`.

## CLI
```text
minds witness init  --repo <host-path> [--home <dir>] [--path-map <container>=<host>]
minds witness run   [--home <dir>] [--follow]
minds witness keygen [--home <dir>]            (EA-09 provides the key logic)
minds witness status [--home <dir>]
```
`init` writes `witness.json` (`repo_root`, `path_map`, `profile`, schema version 1),
creates the layout from `00-conventions.md` with 0700/0600. `run` refuses to start if the
home is group/world-writable, not owned by the current user, or a symlink.

## Behaviour of `run`
- Binds `run/witness.sock` (remove a stale socket only if no process answers `ping`),
  socket mode 0660; group ownership as configured by the profile (EA-10).
- Single-threaded event loop (or one acceptor + one writer thread with a channel): **all
  appends are serialized** — the property that makes live chaining sound.
- On `Hook` frame: run `hook_event::parse` on `stdin` (same code as `minds hook`), append
  via `Journal::at(home/journal).append(...)`, compute `event_hash` as today, push
  `ChainItem::Event` into the session's `ChainFolder` (one folder per `SessionKey`, salted
  with that session's salt from `EpochState::at(home/evidence/state)`), persist the
  folder state after each push (atomic write, `create_new` + rename).
- On start: append a chained `witness.start` event to the witness's own stream
  (`agent = "witness"`, `local_id = <start timestamp>`), payload
  `{"previous_stop":"clean"|"unclean"|"none","profile":"container"|"user"|"managed","key":"SHA256:…"}`; on SIGTERM/SIGINT a `witness.stop` event.
  "Unclean" = a persisted marker from the previous run exists without a stop event.
- Checkpoint entry point `checkpoint_now(commit)`: opens the repo at `repo_root`, calls
  `run_checkpoint` with `EvidenceSource { journal: witness journal, epochs: witness epochs,
  scope: SCOPE_WITNESS_V1 }` and `SealSigner::Key { path: home/key/witness_ed25519,
  namespace: "minds-witness" }`, then attaches trailers exactly like `minds checkpoint`.
  The seal root must equal the persisted folder's `snapshot().root` — assert it; a
  mismatch is logged as an integrity error and the checkpoint is deferred.
- Ledger: after every successful seal, append `<seal_id> <scope> <last_event_at>` to
  `home/ledger` (append-only, fsync). `minds witness status` prints the last entries.
  The agent can delete refs in the shared repo; the ledger is what makes that visible
  (consumed by EA-11/EA-12 via `--witness-home`).
- `--follow`: prints one line per event to stdout:
  `seq 000117  PostToolUse  Write src/sort/merge.rs   head 3f9c1e2a` (tool name and path
  from the adapter's interpretation, sanitized; no payload content ever).
- Diagnostics go to `log/witness.log` using the hook.log redaction rules.
- `status`: profile, repo, socket, number of open sessions, last event time, key fingerprint.

## Path mapping
Hook payload `cwd` and paths are in the agent's namespace (`/workspaces/demo`). The
witness stores them **verbatim** (evidence is not rewritten). `path_map` is used only
where the witness itself touches the filesystem (checkpoint: `tracked_files`,
`hash_artifacts`) and by the file observer (EA-08).

## Acceptance criteria
- [ ] Identical event stream through the witness and through the legacy journal → identical chain root, identical seal apart from `scope` (test with a fake clock).
- [ ] Every append updates the persisted folder; kill -9 between events → restart continues the same chain (root at checkpoint equals batch fold over the journal).
- [ ] Two clients sending concurrently → seq strictly increasing, no lost events (1000 frames from 8 threads).
- [ ] Malformed frames are dropped and logged; the daemon never exits because of client input.
- [ ] `run` refuses insecure homes (tests for group-writable, foreign owner, symlink).
- [ ] Seals carry `scope=witness/v1` and `seal.sig` verifies with namespace `minds-witness`.
- [ ] `witness.start` with `previous_stop=unclean` after a simulated crash.
- [ ] Every produced seal id appears exactly once in `home/ledger`.
- [ ] Unix only; on Windows `minds witness` exits 4 with "not supported on this platform".

## Tests
`witness_chain_equals_legacy_chain`, `witness_survives_kill_between_events`,
`witness_serializes_concurrent_clients`, `witness_ignores_malformed_frames`,
`witness_refuses_insecure_home`, `witness_seal_is_scoped_and_signed`,
`witness_records_unclean_restart`, `witness_ledger_lists_every_seal`, `witness_follow_never_prints_payload`.

## Security review focus
Socket handling (stale-socket race, permissions), no payload bytes in logs or `--follow`,
redaction still fail-closed at checkpoint, key file never readable by group/others.
