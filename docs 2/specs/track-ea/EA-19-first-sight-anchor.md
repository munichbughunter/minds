# EA-19 — `minds anchor`: first-sight countersignature in CI

- Commit: `feat(cli): minds anchor — CI countersigns seals on first sight`
- Branch: `feat/anchor`
- Depends on: EA-09, EA-18b · Size: M · Demo: no

## Goal
Give every seal an independent "existed no later than pipeline #N" statement, signed by a
key the agent never sees, and mirrored into GitLab where the agent cannot delete it.

## Design
- Countersignature text (exactly 5 lines):
```text
minds-anchor-v1
seal=b3-<64hex>
project=<CI_PROJECT_PATH>
pipeline=<CI_PIPELINE_ID>
at=<RFC3339 from the CI clock>
```
  Signed with `NS_ANCHOR` (`MINDS_ANCHOR_KEY_FILE`). Stored at
  `refs/minds/anchors/first-sight/<seal_id>` (tree: `anchor`, `anchor.sig`). If the ref
  exists, do nothing (first sight wins; never overwritten).
- Mirror: with `CI_MERGE_REQUEST_IID` set, `minds-gitlab` posts one idempotent MR note per
  pipeline listing the anchored seal ids (marker pattern as in `Project::mirror`).
- Verify: `anchored: pipeline #N, <at>` per seal; `--online` additionally checks the MR
  note exists (defence against deleted anchor refs).
- Missing CI variables → exit 4 with a clear message; never anchor with a developer key.

## Acceptance criteria
- [ ] Idempotent: second run in the same or a later pipeline writes nothing new.
- [ ] Anchor text golden; signature verifies under `minds-anchor` only.
- [ ] Deleted anchor ref + `--online` → reported as integrity finding (`anchor ref missing, MR note present`).
- [ ] Token and key paths never printed.
