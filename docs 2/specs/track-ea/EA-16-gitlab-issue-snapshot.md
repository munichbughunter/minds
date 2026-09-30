# EA-16 — GitLab issue snapshot and version check

- Commit: `feat(gitlab): bind intents to a GitLab issue version`
- Branch: `feat/intent-gitlab`
- Depends on: EA-15 · Size: M · Demo: no

## Goal
`minds intent bind --issue <project#iid>` snapshots title and description with their
`updated_at`; verify can confirm (online) that this version existed.

## Read first
`crates/minds-gitlab/src/lib.rs` (`Project`, token handling via env var, existing HTTP
mechanism — reuse it, no new HTTP dependency), EA-14 formats.

## Design
- `Project::issue_snapshot(iid) -> IssueSnapshot { title, description, updated_at, web_url }`.
- Snapshot bytes for EA-14: RFC 8785 canonical JSON `{"description":…,"title":…}`.
- Source line: `issue:<project>#<iid>@<updated_at>`.
- Version check at verify (only with `--online`, needs the token env var): first check the
  API capabilities of the target GitLab — **do not assume** a description-history endpoint
  exists in every edition. If available, confirm a version whose canonical snapshot
  hashes to the anchor's `content`; else compare against the current issue and report
  `current` / `changed since binding` / `version check unavailable`. Offline → `not checked (offline)`, never "valid".

## Acceptance criteria
- [ ] Mocked HTTP fixtures for: unchanged issue, changed issue, API without history, 401, 404.
- [ ] Token never appears in errors or logs (existing sanitizer tests extended).
- [ ] Verify never upgrades anything based on an offline or failed check.
