# EA-15 — `minds intent bind` / `sign`: which requirement, approved by whom

- Commit: `feat(cli): minds intent — bind a requirement version and sign it`
- Branch: `feat/intent-cli`
- Depends on: EA-14, EA-09 · Size: M · Demo: yes (`--file` only)

## Goal
The human binds a requirement version and signs it — ideally with a FIDO key that needs a
physical touch, so an agent with full shell access still cannot approve an intent.

## Read first
EA-14, `crates/minds-attest/src/lib.rs` (`ssh_sign_ns`), `crates/minds-cli/src/sign_cmd.rs`,
`review_cmd.rs` (`--sign` UX), `main.rs` `SPECS`.

## CLI
```text
minds intent bind   --file <path> [--scope <glob,glob>]      → prints anchor id + summary
minds intent bind   --issue <project#iid> [--scope …]         (EA-16)
minds intent sign   [<anchor-id>] [--key <path>]              → signs (default: last bound), activates
minds intent show   [<anchor-id>]
minds intent list
```
- `bind --file`: the file must be tracked or at least hashed with `git hash-object`; print
  a warning if its blob is not in `HEAD`'s tree (`requirement not committed — anchor
  refers to a working-tree version`).
- `sign`: key from `--key`, else `user.signingkey`. Detect key type from the `.pub`:
  `sk-ssh-ed25519@openssh.com` / `sk-ecdsa-sha2-nistp256@openssh.com` → `sk (user presence)`,
  else `software key`. For sk keys, **inherit stderr** so the "Confirm user presence"
  prompt from ssh-keygen is visible; keep stdin closed (no passphrase prompts).
- After signing: store `anchor.sig` (EA-14 storage) and activate: if `MINDS_WITNESS_SOCKET`
  (or `--witness-home`) is available send `IntentActivate`; otherwise write the A1 active
  file. Print which path was taken.
- Output (golden):
```text
intent  file:fachliche-anforderung.md@3f9c1e2a  content b3-7a41…
scope   src/sort/**, tests/**
signed  patrick@doering-it (sk-ssh-ed25519, user presence)
active  witness (container)
```

## Verify integration
`minds verify` prints under Assurance reasons: `intent signed, sk key` / `intent signed,
software key` / `intent unsigned` / `intent not bound`; signature checked with
`NS_INTENT` against `--signers`.

## Acceptance criteria
- [ ] Golden output above with a software test key (sk path covered by a unit test on key-type detection and by a manual checklist in the PR description).
- [ ] Sign with a key whose principal is restricted to `minds` (not `minds-intent`) → verify reports `intent signature invalid for minds-intent` (not TAMPERED — an unsigned/invalid intent is an assurance fact, not tampering of evidence).
- [ ] Without witness → A1 active file written, message says so.
- [ ] `agent-help` and `docs/commands.md` updated.

## Tests
`intent_bind_file_golden`, `intent_bind_warns_uncommitted`, `intent_sign_detects_key_type`,
`intent_sign_activates_witness`, `intent_sign_without_witness_writes_active_file`,
`intent_wrong_namespace_is_reported`.
