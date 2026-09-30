# Track EA — implementation specs (ADR-0012 Witnessed Evidence)

One file = one agent run = one Conventional Commit. Hand a spec to Claude Code with the
existing feature loop:

```text
/feature docs/specs/track-ea/EA-06c-witness-daemon.md
```

The agent must read, in this order, before touching code:
1. `00-conventions.md` (this folder) — invariants, vocabulary, formats shared by all specs
2. `docs/adr/0011-evidence-chain.md` and `docs/adr/0012-witnessed-evidence.md`
3. the spec itself, including every file listed under **Read first**

The archive `minds-track-ea.zip` is laid out like the repository — unzip it at the repo
root: `docs/adr/0012-witnessed-evidence.md`, `docs/specs/track-ea/` (these specs plus
`ISSUES.md`, the GitHub issue texts), `docs/demo/loom-drehbuch-witness.md`. Commit that as
part of EA-04 before the first implementation run.

## Order and dependencies

`[DEMO]` = needed for the Loom recording. Run top to bottom; a spec may start only when
everything in its **Depends on** line is merged.

| # | Spec | Commit type | Size | Depends on | Demo |
|---|---|---|---|---|---|
| 1 | EA-00 verify defaults to HEAD, recall keeps the full intent | fix | S | — | ✔ |
| 2 | EA-S2 spike: container profile | spike | S | — | ✔ |
| 3 | EA-S1 spike: Claude Code managed profile | spike | M | — | |
| 4 | EA-04 accept ADR-0012 | docs | S | EA-S2 | ✔ |
| 5 | EA-01a write-time content hash | feat(capture) | M | — | ✔ |
| 6 | EA-01 reconciliation read model | feat(reader) | M | EA-01a | ✔ |
| 7 | EA-02 verify --commit, artifact coverage | feat(cli) | S | EA-00, EA-01 | ✔ |
| 8 | EA-03 reconciliation in TUI/HTML | feat(tui) | S | EA-01 | |
| 9 | EA-06a journal/epoch roots, checkpoint core | refactor | M | — | ✔ |
| 10 | EA-06b incremental chain folder | feat(core) | S | — | ✔ |
| 11 | EA-05 witness wire protocol | feat(capture) | S | — | ✔ |
| 12 | EA-09 signing namespaces, witness key | feat(attest) | S | — | ✔ |
| 13 | EA-06c witness daemon | feat(cli) | L | EA-05, EA-06a, EA-06b, EA-09 | ✔ |
| 14 | EA-07 hook forwarder | feat(capture) | M | EA-05, EA-06c | ✔ |
| 15 | EA-06d checkpoint delegation | feat(cli) | M | EA-06c, EA-07 | ✔ |
| 16 | EA-08 file-system observer | feat(cli) | M | EA-06c | ✔ |
| 17 | EA-10 enable --witness, doctor | feat(cli) | M | EA-S2, EA-06d, EA-08 | ✔ |
| 18 | EA-11 assurance computation | feat(reader) | M | EA-01, EA-08, EA-09 | ✔ |
| 19 | EA-12 verify output and gates | feat(cli) | S | EA-02, EA-11 | ✔ |
| 20 | EA-13 level-aware PROVES/DOES_NOT_PROVE | feat(core) | S | EA-11 | ✔ |
| 21 | EA-14 intent anchor | feat(core) | M | EA-06c | ✔ |
| 22 | EA-15 intent bind / sign | feat(cli) | M | EA-14, EA-09 | ✔ |
| 23 | EA-17 scope findings | feat(reader) | S | EA-14, EA-01 | |
| 24 | EA-16 GitLab issue snapshot | feat(gitlab) | M | EA-15 | |
| 25 | EA-18a exec outcome extraction | feat(capture) | M | EA-01a | ✔ |
| 26 | EA-18b replay | feat(cli) | L | EA-18a | ✔ (tests only) |
| 27 | EA-19 first-sight anchor | feat(cli) | M | EA-09, EA-18b | |
| 28 | EA-20 CI include `minds-evidence` | feat(ci) | S | EA-12, EA-18b | ✔ |
| 29 | EA-21 docs: verification guide, privacy, BetrVG | docs | S | EA-12, EA-15 | |
| 30 | EA-22 end-to-end witness pilot test + demo scripts | test | M | all ✔ above | ✔ |

## Rules for the agent on every spec

- If the code contradicts a statement in a spec, **the code wins**: stop, report the
  mismatch with file and line, propose the adjusted design, and continue only with the
  adjusted design documented in the commit body.
- Never widen scope. Items under **Non-goals** stay out even if they look easy.
- Every acceptance criterion maps to at least one named test. The commit body lists
  `AC → test` pairs.
- Output strings shown in specs are the contract for tests and the Loom script. Change
  them only together with the golden test and the spec.
