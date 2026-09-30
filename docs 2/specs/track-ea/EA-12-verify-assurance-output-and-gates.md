# EA-12 — `minds verify`: Assurance line, Not-proven line, gates, ledger check

- Commit: `feat(cli): verify reports assurance and limits, gates on level, checks the witness ledger`
- Branch: `feat/verify-assurance`
- Depends on: EA-02, EA-11 · Size: S · Demo: yes

## Goal
The verify block reads exactly like the Loom script: three axes unchanged, then
`Assurance` and `Not proven`, plus uncorroborated claims. Gates for CI.

## Read first
`crates/minds-cli/src/verify_cmd.rs`, `crates/minds-cli/src/fsck.rs`, EA-11, EA-13,
`docs/demo/loom-drehbuch-witness.md` (the target output).

## Output contract (golden)
```text
Verdict         VERIFIED
Integrity       intact     (2 seals, 118 events, chain recomputed)
Coverage        complete   (0 gaps · artifact 150/150 lines explained · 0 out of scope)
  uncorroborated  seq 119  Write src/sort/merge.rs  b3-deadbeef…  no file-system observation
Interpretation  all calls interpreted
Assurance       A2 witnessed   (minds-witness@build-07, profile container; intent signed, sk key)
Not proven      model identity · correctness of the decision · actions outside the boundary
```
- Keep the existing lines' wording where they already exist; only add/extend. If the
  current output differs in labels or order, keep the current labels and adapt the golden
  text — then update the Loom script in the same commit.
- `Assurance` reasons: when below the best possible level, print the first reason:
  `Assurance       A1 observed    (range 2: witness signature not checked — no trusted allowed_signers)`.
- `Not proven` lists the short forms of the level-aware `DOES_NOT_PROVE` entries (EA-13)
  that apply to the achieved level, joined with ` · `, max 3 + `(minds verify --limits)`.
  New bool flag `--limits` prints the full sentences.

## Flags
- `--signers <file>` (exists) is the trusted allowed_signers source for witness/intent/anchor.
- `--witness-home <dir>`: compare the witness ledger with the repo; each ledger seal id
  absent from `refs/minds/evidence/` → `Integrity       VIOLATED  witnessed seal b3-… missing from the repository` → verdict TAMPERED, exit 1.
- `--require-assurance <A0|A1|A2|A3>` on `verify` and `fsck`: below → `Gate …` line, exit 2
  (never masking 1/3/4).

## Acceptance criteria
- [ ] Golden output for: A2 clean fixture, A1 fixture, mixed fixture, forged-claim fixture, human-edit fixture, missing-ledger-seal fixture.
- [ ] Exit codes: frozen contract (W6) holds in all fixtures; gates only ever produce 2.
- [ ] `fsck --require-assurance A2` fails for a repo with any agent-authored commit whose sessions are below A2.

## Tests
`verify_assurance_golden_*` (one per fixture), `verify_ledger_missing_seal_is_tampered`,
`verify_require_assurance_gate`, `fsck_require_assurance_gate`, `verify_limits_flag`.
