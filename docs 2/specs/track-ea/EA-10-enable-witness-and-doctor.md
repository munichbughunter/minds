# EA-10 — `minds enable --witness <profile>` and `minds doctor`

- Commit: `feat(cli): enable --witness sets up an isolation profile, doctor proves it`
- Branch: `feat/enable-witness`
- Depends on: EA-S2, EA-06d, EA-08 · Size: M · Demo: yes (`container` only)

## Goal
One command sets up a witnessed configuration; one command proves the isolation actually
holds. Setup never runs privileged commands itself.

## Non-goals
`managed` profile beyond writing a proposal file (blocked on EA-S1). Multi-repo witnesses.

## Read first
`crates/minds-cli/src/enable.rs` (hook registration, `check_agent_path`, `SessionStart`
handling), EA-S2 templates, `00-conventions.md` (state dir), `main.rs` `SPECS`.

## `minds enable --witness container` (run on the host, in the repo)
1. Run the normal `enable` steps (hooks, Git hooks) — unchanged.
2. `minds witness init --repo <root> --path-map /workspaces/<name>=<root> --profile container`
   and `minds witness keygen` (skip if a key exists).
3. Write `.devcontainer/devcontainer.json` and `.devcontainer/compose.yaml` from the EA-S2
   templates (`include_str!`), mounting `$MINDS_WITNESS_HOME/run` at `/run/minds-witness`
   and setting `MINDS_WITNESS_SOCKET`. Never overwrite existing files: write
   `*.minds-proposed` next to them and say so.
4. Install a user service for the witness: systemd user unit
   `~/.config/systemd/user/minds-witness-<repo-id>.service` (Linux) or a launchd agent
   plist (macOS). Print the exact command to enable it; do not start it silently.
5. Print the `allowed_signers` line from `keygen` and where to put it (a location outside
   the repo that the team distributes out of band).

## `minds enable --witness user`
Print the exact steps (create user `minds-witness`, group `minds-agents`, home ownership,
socket group) — never execute `sudo`. Generate the service unit for that user.

## `minds enable --witness managed`
Only write `managed-settings.minds-proposed.json` with the settings from EA-S1 and a
header comment that the profile yields A1 until EA-S1 says otherwise.

## `minds doctor` (new command)
Checks, each printed as `ok` / `warn` / `fail` with one line of reason:
- hooks registered for detected agents; Git hooks present; store config readable;
- witness: `MINDS_WITNESS_SOCKET` set (agent side) · socket answers `ping` · profile from
  `witness.json` (host side) · key present, 0600;
- **isolation probe (agent side):** try to open `$MINDS_WITNESS_HOME` paths passed via
  `--probe-home <dir>`; success is `fail` ("agent can reach witness home"), `EACCES`/`ENOENT`
  is `ok`. In the container profile the home is simply absent — `ok`;
- exit 0 if no `fail`, else 1.

## Acceptance criteria
- [ ] `enable --witness container` on a fresh repo produces the files above, idempotent on rerun, never overwrites.
- [ ] `doctor` inside the (simulated) agent side reports the isolation probe `ok`; with a readable home it reports `fail` and exits 1.
- [ ] No command in this spec invokes `sudo` (grep test on the source).
- [ ] `agent-help` / `docs/commands.md` updated.

## Tests
`enable_witness_container_writes_templates`, `enable_witness_is_idempotent`,
`enable_witness_never_overwrites_devcontainer`, `doctor_isolation_probe_ok_and_fail`,
`no_sudo_in_enable_witness`.
