# EA-17 — Scope findings: did the work stay inside the declared scope?

- Commit: `feat(reader): out-of-scope findings against the intent anchor`
- Branch: `feat/scope-findings`
- Depends on: EA-14, EA-01 · Size: S · Demo: no

## Goal
If the anchor declares `scope=`, every write claim, every file observation and every
changed file of the commit outside the scope is an `out of scope` finding on the coverage
axis. This is the "agent stayed within ticket scope" audit question.

## Design
- Minimal glob matcher in `minds-reader` (`*`, `**`, `?`, literal segments; no brace
  expansion) with exhaustive tests — no new dependency.
- `scope_findings(intent, reconciliation, claims, observations) -> Vec<ScopeFinding { path, source: Claim|Observation|Commit }>`.
- EA-02's Coverage segment `· N out of scope` becomes live; detail lines:
  `  out of scope   docs/README.md  (commit, observation)`.
- No scope in the anchor → segment omitted, no findings.

## Acceptance criteria
- [ ] Glob matcher table tests incl. edge cases (`**` at start/middle/end, dotfiles, trailing slash).
- [ ] Fixture with one out-of-scope write → exactly one finding with correct sources.
- [ ] Findings never change the verdict or exit code; `--require-in-scope` gate (bool) → exit 2.
