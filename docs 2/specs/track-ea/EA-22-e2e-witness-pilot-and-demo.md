# EA-22 — End-to-end witness pilot test and the demo scripts

- Commit: `test: witnessed evidence end to end — the Loom scenario as a pilot test`
- Branch: `test/witness-e2e`
- Depends on: all demo specs · Size: M · Demo: yes

## Goal
The Loom script (`docs/demo/loom-drehbuch-witness.md`) becomes an automated test. The
recording may show nothing this test does not cover.

## Tasks
1. Move the Loom script into `docs/demo/` (provided file) and keep its expected outputs in
   sync with the golden texts of EA-02/EA-12/EA-15/EA-18b.
2. `crates/minds-cli/tests/witness_pilot.rs`, style of `pilot.rs`: temp host dir (witness
   home), temp "agent side" dir, real `minds` binary, a witness process started by the
   test, hook payload fixtures from EA-01a/EA-18a fed through `minds hook` with
   `MINDS_WITNESS_SOCKET` set, file writes performed by the test to simulate the agent's
   tool effects (so the fs observer sees them). Scenario:
   - bind + sign intent (software test key), activate on the witness
   - witnessed session: Write/Edit/Bash(`cargo test` fixture)/discarded variant
   - commit → checkpoint delegation → `SESSION SEALED`
   - `minds verify --signers …` → VERIFIED, `A2 witnessed`, `150/150`-style full explanation (exact numbers from the fixture)
   - forged claim via `demo/forge-write-claim.sh` → `1 reported event uncorroborated`, verdict unchanged
   - human edit + amend → `unexplained <file>:<line>`, `--require-explained 100` → exit 2
   - `git reset --hard HEAD@{1}`, `demo/tamper-seal.sh` → TAMPERED with expected/found, exit 1
   - delete a witnessed seal ref + `--witness-home` → TAMPERED (`missing from the repository`)
   - `minds replay --unsigned` → `reproduced`
3. `demo/forge-write-claim.sh <path>`: builds a syntactically valid Claude Code
   `PostToolUse` Write payload with invented content and pipes it into `minds hook
   --agent claude-code`. Header comment: "Demo-Angriff, nicht für Produktivrepos".
4. `demo/tamper-seal.sh`: reads the newest `refs/minds/evidence/<id>`, changes one byte
   in the `events=` line, writes a new parentless commit with the same tree layout,
   moves the ref. Same header comment.
5. `demo/README.md`: prerequisites (Docker, dev container from EA-10, FIDO key optional),
   and the order of commands for the recording.

## Acceptance criteria
- [ ] Test is part of `cargo test --workspace` on Linux; skipped with a clear message on non-Unix.
- [ ] Runtime < 60 s.
- [ ] Every command and expected output line in the Loom script appears in the test (a small check that greps the script's fenced `text` blocks against the test's golden strings).
