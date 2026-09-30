# EA-03 — Reconciliation in TUI evidence mode and `minds render`

- Commit: `feat(tui): show explained and unexplained lines in evidence mode and HTML`
- Branch: `feat/recon-surfaces`
- Depends on: EA-01 · Size: S · Demo: no

## Goal
The TUI evidence mode (`e`) and `minds render` show the reconciliation of the selected
commit: a summary line (`artifact 148/150 lines explained`) and a per-file list; in the
file view, unexplained lines are marked in the gutter.

## Rules
- Unexplained is **never** styled as an error (no red). Use the theme's neutral "not
  observed" style; legend text `not observed in the session`.
- The TUI stays read-only and consumes only `minds-reader` APIs.
- HTML: static, no JS required; the marker is a CSS class plus a text label for screen readers.

## Acceptance criteria
- [ ] Snapshot tests for the TUI view (`crates/minds-tui/src/view/tests.rs` style) with the EA-01 fixtures.
- [ ] HTML golden test for one explained and one unexplained file.
- [ ] No new dependency.
