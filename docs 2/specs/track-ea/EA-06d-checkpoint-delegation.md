# EA-06d — Checkpoint delegation from the agent side to the witness

- Commit: `feat(cli): checkpoint delegates witnessed sessions to the witness`
- Branch: `feat/checkpoint-delegation`
- Depends on: EA-06c, EA-07 · Size: M · Demo: yes

## Goal
When the agent side commits (post-commit hook inside the container), `minds checkpoint`
asks the witness to checkpoint its sessions and then processes the local fallback journal
as today. The agent side never touches witness state.

## Read first
`crates/minds-cli/src/checkpoint.rs`, `prepare_commit_msg.rs`, `witness_proto.rs`,
EA-06c "Checkpoint entry point".

## Design
- If `MINDS_WITNESS_SOCKET` is set: send `CheckpointRequest { commit, request_id }`, wait
  for `Ack`/`Nack` with a timeout of 15 s (configurable via `MINDS_WITNESS_TIMEOUT_MS`).
  On `Ack`, print the status line(s) the witness returned (the `SESSION SEALED` summary is
  produced by the witness and returned as text). On timeout/`Nack`/connect error: print
  `witness unavailable — witnessed sessions stay open, will be sealed at the next checkpoint`
  to stderr and continue; exit code unchanged (today's semantics).
- Then run the local path unchanged (`scope=agent-hooks/v1`) for any fallback journal
  entries (EA-07).
- Trailer attachment: the witness attaches trailers for its sessions; the local path
  attaches for its own. Both use the same trailer mechanism; attaching twice to the same
  commit must be idempotent — verify and add a test.
- Sessions of the same agent run that exist in both writers produce **two** session ids
  (a witnessed range and an A1 range). Both are linked to the commit; verify shows both
  (EA-11 computes per-range assurance). Do not try to merge them.

## Acceptance criteria
- [ ] Container-simulation test (two processes, temp dirs, socket): commit → witness seals → trailer present → `minds verify` finds the witnessed session.
- [ ] Witness down → commit still succeeds, stderr message printed, fallback sealed as A1.
- [ ] Trailer attachment idempotent when both writers attach.
- [ ] No read of the witness home from the agent-side process (assert via a test that runs the agent side with the witness home `chmod 000`).

## Tests
`checkpoint_delegates_to_witness`, `checkpoint_witness_down_falls_back`,
`trailer_attach_is_idempotent_across_writers`, `agent_side_never_reads_witness_home`.
