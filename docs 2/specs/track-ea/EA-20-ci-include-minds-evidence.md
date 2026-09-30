# EA-20 — CI include `ci/minds-evidence.gitlab-ci.yml`

- Commit: `feat(ci): minds-evidence stage — verify, replay, anchor`
- Branch: `feat/ci-evidence`
- Depends on: EA-12, EA-18b (EA-19 optional) · Size: S · Demo: yes (tests-only replay)

## Goal
One include, no logic in YAML (R5 principle): the job only calls the binary.

## Design
Model it on `ci/minds-review-gate.gitlab-ci.yml` (header comment style, German).
```yaml
stages: [minds-evidence]
minds:evidence:
  stage: minds-evidence
  rules:
    - if: '$CI_PIPELINE_SOURCE == "merge_request_event"'
  script:
    - git fetch origin '+refs/minds/*:refs/minds/*'
    - minds verify --signers "$MINDS_ALLOWED_SIGNERS" --require-assurance "${MINDS_REQUIRED_ASSURANCE:-A2}"
    - minds replay ${MINDS_REPLAY_FLAGS}
    - if [ -n "$MINDS_ANCHOR_KEY_FILE" ]; then minds anchor && git push origin 'refs/minds/anchors/*'; fi
```
Variables documented in the header: `MINDS_ALLOWED_SIGNERS` (file variable, from a
trusted source), `MINDS_REQUIRED_ASSURANCE`, `MINDS_REPLAY_FLAGS` (e.g. `--unsigned` for
the demo), `MINDS_ANCHOR_KEY_FILE` (protected, masked file variable), GitLab token env
for MR notes.

## Acceptance criteria
- [ ] YAML lint passes (existing CI checks, if any).
- [ ] `docs/gitlab-operating-model.md` gets a section "Evidence stage".
- [ ] A dry-run script in `xtask` or a test renders the job's commands against a fixture repo and asserts exit codes (0 for the clean fixture, 2 for the false-claim fixture).
