# EA-21 — Docs: verification guide, privacy overview, BetrVG note

- Commit: `docs: witnessed evidence — verification recipe, privacy and works-council notes`
- Branch: `docs/witnessed-evidence`
- Depends on: EA-12, EA-15 (EA-19 if merged) · Size: S · Demo: no

## Tasks
- `docs/verification-guide.md`: new section "Witnessed evidence" — recipes with only
  `git cat-file`, `b3sum`/BLAKE3 `derive_key`, `ssh-keygen -Y find-principals` and
  `-Y verify -n minds-witness|minds-intent|minds-anchor`; how to read the Assurance line;
  what each level proves and does not prove (generated from the EA-13 vocabulary — add an
  `xtask` that prints the table so docs cannot drift).
- `docs/privacy-overview.md`: the file observer (paths + hashes only, secret wall,
  ignored paths, no attribution to persons, no content), observation objects are not
  subject to `forget` and why, where the witness stores raw data (host, 0700).
- New `docs/betrvg-note.md` (German): what the witness observes (the agent's workspace),
  what it does not (keystrokes, timing of people, content), "unexplained" is never a
  statement about a person, recommended configuration for works-council agreements.
- `README.md` and `Roadmap.md` §5: one paragraph on assurance levels.

## Acceptance criteria
- [ ] Every command in the recipes is run in a doc test or a script test.
- [ ] No doc claims more than the EA-13 vocabulary for the stated level.
