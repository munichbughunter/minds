# EA-02 — `minds verify --commit`, artifact coverage in the Coverage line

- Commit: `feat(cli): verify reconciles the commit — artifact coverage and unexplained hunks`
- Branch: `feat/verify-commit`
- Depends on: EA-00, EA-01 · Size: S · Demo: yes

## Goal
`minds verify` shows how much of the commit is explained by evidence and lists unexplained
lines. A gate can require a minimum.

## Non-goals
No assurance line (EA-12). No change to the verdict word or exit codes except the gate.

## Read first
`crates/minds-cli/src/verify_cmd.rs` (current Coverage printing), `main.rs` `SPECS`,
`crates/minds-reader/src/reconcile.rs` (EA-01).

## Design
- New value flag `--commit <rev>` and `--require-explained <percent>` (0–100, integer).
  Without `--commit`: the commit resolved by EA-00 (HEAD or the given revision); when a
  session id is given directly, the commit is taken from its observed trailer edge; if
  none → reconciliation is skipped with `Artifact        not assessed (no linked commit)`.
- Output, appended to the existing Coverage line and followed by detail lines:
```text
Coverage       complete   (0 gaps · artifact 148/150 lines explained · 0 out of scope)
  unexplained    src/sort/merge.rs:88     not observed in the session
  unexplained    src/sort/merge.rs:91-92  not observed in the session
  file only      Cargo.lock               line level unavailable (reconstruction mismatch)
```
  (`· 0 out of scope` appears only after EA-17; until then omit that segment.)
- Hunks are compressed to ranges; at most 20 detail lines, then `  … N more (minds verify --commit <rev> --all)`; add bool flag `--all`.
- Gate: if explained/total·100 < required → print `Gate           explained 98% < required 100%` and exit 2, unless the verdict is already 1/3/4 (worst wins).

## Acceptance criteria
- [ ] Output matches the golden text above for the fixture from EA-01.
- [ ] `--require-explained 100` on the human-edit fixture → exit 2; on the agent-only fixture → exit 0.
- [ ] TAMPERED stays exit 1 even when the gate fails.
- [ ] Paths are sanitized (`text::sanitize`) before printing.
- [ ] `agent-help` and `docs/commands.md` updated.

## Tests
`verify_prints_artifact_coverage`, `verify_lists_unexplained_ranges`,
`verify_detail_lines_are_capped`, `verify_require_explained_gate`,
`verify_gate_never_masks_tampered`.
