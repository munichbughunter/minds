# EA-07 — `minds hook` forwards to the witness, fail-open

- Commit: `feat(capture): hook forwards to the witness with fail-open journal fallback`
- Branch: `feat/hook-forwarder`
- Depends on: EA-05, EA-06c · Size: M · Demo: yes

## Goal
With `MINDS_WITNESS_SOCKET` set, `minds hook` sends the payload as a `Hook` frame and
returns. Any failure falls back to today's local journal write. The hook's contract
(exit 0, no stdout, no repo/config access, bounded time) is untouched.

## Read first
`crates/minds-cli/src/hook.rs` (all three rules), `secretwall.rs`, `witness_proto.rs`.

## Design
- Order in `record`: read stdin (existing cap) → secret wall (unchanged, still on the
  agent side, so raw secrets never cross the socket) → if env var set: connect (non-blocking,
  connect timeout 50 ms), write the frame with a write timeout of 100 ms total → done.
  On any error: log one line to `hook.log` (`witness unreachable: <kind>`) and fall back to
  the existing journal append.
- No retries, no waiting for a response.
- Measure: extend the existing hook latency test (or add one) — p99 with a live witness
  must not exceed today's p99 by more than 5 ms; with the witness down (connection
  refused) likewise.

## Acceptance criteria
- [ ] With witness: event appears in the witness journal, nothing in `<git-dir>/minds/journal`.
- [ ] Without witness (env unset): behaviour byte-identical to today.
- [ ] Env set but socket missing/refusing/hanging → fallback journal written, exit 0, `hook.log` line, total time bounded.
- [ ] Secret-file payloads are rewritten before sending (test with the `.env` fixture from `secretwall.rs`).
- [ ] Still no byte on stdout in all paths (existing test extended).

## Tests
`hook_forwards_to_witness`, `hook_unset_env_is_unchanged`, `hook_falls_back_when_witness_down`,
`hook_falls_back_when_witness_hangs`, `hook_secret_wall_runs_before_forwarding`,
`hook_latency_budget_with_witness`.
