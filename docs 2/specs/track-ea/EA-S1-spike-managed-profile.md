# EA-S1 — Spike: can Claude Code managed settings isolate the witness?

- Commit: `docs(spike): managed-settings isolation for the witness`
- Branch: `spike/managed-profile`
- Depends on: — · Size: M · Demo: no

## Question
With Claude Code running natively as the developer's UID, can managed settings guarantee
that **the Bash tool (and Write/Edit tools) cannot** read/write the witness state dir or
connect to `run/witness.sock`, while **hooks can**?

## What to test (record versions, OS, exact settings)
1. `managed-settings.json` at the OS system path with `allowManagedHooksOnly: true`;
   project-level `.claude/settings.json` hooks must no longer load.
2. Managed hooks of type `command` calling `minds hook`, and alternatively HTTP hooks
   (payload parity with command hooks for `PostToolUse`, `UserPromptSubmit`, `Stop`,
   `SessionStart`; record differences field by field).
3. Sandbox: filesystem write/read denials for `$MINDS_WITNESS_HOME`; network/Unix-socket
   denial for `run/witness.sock`. Try from Bash: `cat`, `ls`, `socat`, `python -c` socket
   connect, `nc -U`. Try Write/Edit tools on the state dir.
4. Can the agent disable or edit managed settings, kill the witness process, or `ptrace`
   it? (same UID!) Record results on Linux and macOS.
5. Does a hook process inherit sandbox restrictions? (If yes, hooks cannot reach the
   socket either → profile fails.)

## Deliverable
`docs/spikes/ea-s1-managed-profile.md` containing: a results matrix (action × tool × OS →
allowed/denied, evidence command and output), the exact managed settings used, and a
decision: **A2 possible** (list conditions) or **stays A1** (list the blocking results).
Append a short "Addendum EA-S1" section to ADR-0012 with the decision.

## Non-goals
No production code. No changes to `minds enable`.

## Done when
The decision is recorded and every "denied" result has a reproducible command.
