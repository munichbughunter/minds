# EA-18a — Exec outcome extraction: what did the test/benchmark report?

- Commit: `feat(capture): interpret test and benchmark outcomes of shell calls`
- Branch: `feat/exec-outcome`
- Depends on: EA-01a (fixture approach) · Size: M · Demo: yes (tests only)

## Problem (found in the code)
`ToolCall` stores `name`, `arguments`, `capture`, `effect` — **not** what a command
reported. The raw `tool_response` exists only in the journal, which is discarded after
checkpoint. Without a stored outcome, replay has nothing to compare against.

## Goal
At checkpoint, the adapter interprets `Bash` calls of known test/benchmark runners and
stores a small, number-only outcome. Interpretation, versioned, deterministic.

## Non-goals
No replay (EA-18b). No storage of stdout/stderr text. Runners beyond the list below.

## Read first
`crates/minds-capture/src/normalize.rs` (Claude adapter, `effect_for`, versioning),
`adapter.rs` (checkpoint), `crates/minds-core/src/session.rs` (`ToolCall`), EA-01a fixture README.

## Step 1 — fixtures
Record real `PostToolUse` payloads for `Bash` running `cargo test` (pass and fail),
`cargo bench` (criterion), and one unknown command. Commit as fixtures. **Check the actual
`tool_response` shape** (stdout/stderr fields, whether an exit code is present) and write
it down in the fixture README.

## Design
```rust
// minds-core/src/session.rs — additive, hash-stable
pub struct ToolCall { /* … */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ExecOutcome>,
}
pub struct ExecOutcome {
    pub class: ExecClass,            // Test | Bench
    pub runner: String,              // "cargo-test", "cargo-bench-criterion", "pytest", …
    pub command: Vec<String>,        // normalized argv (see below), redacted
    pub exit_code: Option<i32>,      // only if the payload carries it
    pub tests: Option<TestCounts>,   // passed, failed, ignored
    pub benches: Vec<BenchValue>,    // name, value (integer nanoseconds), unit "ns"
}
```
- No floats in the envelope (project invariant): benchmark values are converted to
  integer nanoseconds.
- Runners in scope: `cargo test` / `cargo nextest run` (summary lines), `cargo bench`
  with criterion output (median of the `time: [lo mid hi]` triple), `pytest` (final
  summary line). Unknown → `outcome: None`.
- Command normalization: split the shell string only if it is a single simple command
  (no `;`, `&&`, `|`, redirection, subshell, env assignments except `RUST_LOG=`-style
  `KEY=VALUE` prefixes on an allowlist); otherwise `outcome: None` with capture note
  `compound command not interpreted`. This keeps replay (EA-18b) free of shell parsing.
- Bump adapter version.

## Acceptance criteria
- [ ] Fixtures → exact `ExecOutcome` values (golden).
- [ ] Compound commands → `None`.
- [ ] No text from stdout/stderr stored beyond the parsed numbers and bench names (bench names pass redaction).
- [ ] Old sessions serialize unchanged.

## Tests
`outcome_cargo_test_pass`, `outcome_cargo_test_fail`, `outcome_criterion_bench_ns`,
`outcome_pytest_summary`, `outcome_compound_command_not_interpreted`,
`outcome_no_floats_in_envelope`, `outcome_additive_serialization`.
