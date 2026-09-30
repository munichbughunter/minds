# EA-00 — `minds verify` defaults to HEAD; `recall` keeps the full intent

- Commit: `fix(cli): verify resolves HEAD without argument, recall keeps the full intent`
- Branch: `fix/verify-head-recall-intent`
- Depends on: — · Size: S · Demo: yes

## Goal
`minds verify` without arguments verifies the session(s) linked to `HEAD`. `minds recall`
prints the complete intent request, not only its first line. Both are known demo-path bugs.

## Non-goals
No new verify output lines (EA-02/EA-12). No change to the verdict or exit codes.

## Read first
- `crates/minds-cli/src/verify_cmd.rs` — `run()`, the `(None, None, _)` arm
- `crates/minds-cli/src/recall.rs`, `crates/minds-reader/src/brief.rs`
- how trailers are read: `crates/minds-core/src/trailer.rs`, `crates/minds-git/src/trailer.rs`
- `crates/minds-cli/src/main.rs` — `SPECS` table (`verify` positionals)

## Design
- `verify` positional becomes optional. With none: resolve `HEAD`, read `Minds-Session-Id`
  trailers (observed) from the commit message; fall back to the store index edges
  `commit → session`. For each session run the existing `verify_session` block, separated
  by a blank line; the process exit code is the **worst** verdict by the order
  1 > 3 > 2 > 0, operational failure 4 wins over all.
- No session linked to HEAD → print `No session is linked to HEAD (<short sha>).` and exit 3.
- Also accept `minds verify <rev>` where `<rev>` is not a session id: if the argument does
  not parse as `SessionId`, try it as a revision and apply the same resolution.
- `recall`: find where the intent is truncated to the headline and print `intent.request`
  in full, still through `text::sanitize`, wrapped at the terminal width.

## Acceptance criteria
- [ ] `minds verify` in a repo whose HEAD carries one trailer prints the same block as `minds verify <id>` and the same exit code.
- [ ] Two trailers → two blocks, exit = worst.
- [ ] HEAD without trailer and without index edge → message above, exit 3.
- [ ] `minds verify HEAD~1` works; an invalid revision → exit 4 with a sanitized error.
- [ ] `minds recall <file>` shows a multi-line intent completely.
- [ ] `agent-help` output updated (the table test enforces it).

## Tests
`verify_without_argument_uses_head`, `verify_worst_verdict_wins_for_multiple_sessions`,
`verify_head_without_session_is_not_verifiable`, `verify_accepts_revision`,
`recall_prints_full_multiline_intent` (in `crates/minds-cli/tests/end_to_end.rs` style).

## Security review focus
Revision strings are user input: pass them to git as a single argument after `--`-safe
handling; sanitize every echoed string.

## Docs
`docs/commands.md` (verify, recall).
