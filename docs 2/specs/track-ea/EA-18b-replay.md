# EA-18b — `minds replay`: reported results become checked results

- Commit: `feat(cli): minds replay — re-run decisive test and benchmark commands in CI`
- Branch: `feat/replay`
- Depends on: EA-18a · Size: L · Demo: yes (tests only, unsigned record)

## Goal
In CI, on a clean checkout of the commit, re-execute the session's decisive
test/benchmark commands **safely**, compare with the stored outcomes, and emit a replay
record. A match lifts the claim to "reproduced"; a mismatch is `claim not reproduced`.

## Non-goals
Replaying anything that is not a recognized, normalized runner command. Replaying on
developer machines by default (allowed, but the record is only meaningful from CI).

## Read first
EA-18a, `crates/minds-reader` session access, `crates/minds-attest` (`NS_ANCHOR`),
`crates/minds-store` (ref-per-object pattern).

## Safety rules (hard)
- **No shell.** Execute `argv` directly (`std::process::Command`), never `sh -c`.
- **Allowlist from reviewed config.** `.minds/replay.json` (in the repo, changes go through
  code review) lists allowed runners and argument patterns, e.g.
  `{"schema":1,"runners":{"cargo-test":{"argv0":"cargo","sub":["test","nextest"],"allow_flags":["-p","--package","--release","--lib","--test","--","--exact"]}},"tolerance":{"default_pct":25,"overrides":[{"bench":"sort/*","pct":15}]},"timeouts":{"per_command_s":600,"total_s":1800}}`.
  A decisive command not matching the allowlist → `skipped (not allowlisted)`, reported,
  never executed.
- Environment: cleared except `PATH`, `HOME`, `CARGO_*`, `RUSTUP_*` and an explicit
  allowlist from the config. Working directory: repo root joined with the recorded cwd
  relative to the session's repo root; must stay inside the checkout.

## Decisive commands
The last occurrence of each distinct `(class, normalized argv)` in the session's turns.
(Deterministic; documented as interpretation with a version number.)

## Comparison
- Tests: `failed == 0` status and `passed` count must match; `ignored` differences are
  reported but not a mismatch.
- Benches: per bench name, `|observed − recorded| / recorded ≤ tolerance`.
- Exit code: compared only if the recorded outcome has one.

## Replay record
Canonical JSON (RFC 8785):
`{"schema":1,"commit":…,"session":…,"interpretation_version":…,"results":[{"turn":…,"call":…,"argv":[…],"expected":{…},"observed":{…},"verdict":"reproduced|not_reproduced|skipped","reason":…}],"environment":{"ci":"gitlab","pipeline":…,"image":…}}`
Stored at `refs/minds/anchors/replay/<64hex>`, `record.sig` signed with `NS_ANCHOR` using
the key file from `MINDS_ANCHOR_KEY_FILE`. `--unsigned` allowed (demo cut); the reader
shows unsigned records but EA-11 never counts them for A3.

## CLI output (golden)
```text
replay   3/3 decisive test runs reproduced (cargo test -p sort …)
replay   1 skipped (not allowlisted): python bench.py
```
Exit: 0 all reproduced or skipped; 2 any `not_reproduced`; 4 operational failure.

## Acceptance criteria
- [ ] A fixture whose recorded `cargo test` passed and still passes → reproduced.
- [ ] Manipulated stored outcome (claims 12 passed, reality 11) → `not_reproduced`, exit 2.
- [ ] A decisive `rm -rf /` style command (via fixture) is never executed — test asserts no process spawn (inject a spawner trait).
- [ ] Commands with `..` cwd or absolute cwd outside the checkout → skipped.
- [ ] Record canonical bytes golden; signature verifies under `minds-anchor`.

## Tests
`replay_reproduces_passing_tests`, `replay_detects_false_claim`, `replay_never_runs_unlisted`,
`replay_rejects_cwd_escape`, `replay_bench_tolerance`, `replay_record_golden`,
`replay_record_signature_namespace`.

## Security review focus
This is the only place minds executes recorded commands. Review the allowlist logic, the
environment clearing, and cwd confinement first.
