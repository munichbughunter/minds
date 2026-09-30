# EA-04 — Accept ADR-0012

- Commit: `docs(adr): ADR-0012 witnessed evidence`
- Branch: `docs/adr-0012`
- Depends on: EA-S2 (and EA-S1 if done) · Size: S · Demo: yes

## Tasks
- Add `docs/adr/0012-witnessed-evidence.md` (provided), status `accepted`, date of merge.
- Fold in spike results: container profile findings (EA-S2); managed profile decision
  (EA-S1, or mark as "pending spike EA-S1 — yields A1 until decided").
- Cross-link: ADR-0011 "Consequences" gets one sentence pointing to ADR-0012 for the
  append→seal window and key control; `Roadmap.md` §5 "Known gaps" mentions Track EA.
- Add `docs/specs/track-ea/` (this folder) to the repo.

## Acceptance criteria
- [ ] Markdown links resolve (`xtask` link check if present, otherwise manual).
- [ ] No statement in the ADR contradicts the spike documents.
