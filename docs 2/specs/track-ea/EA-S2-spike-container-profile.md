# EA-S2 — Spike: container profile (agent in dev container, witness on host)

- Commit: `docs(spike): container isolation profile for the witness`
- Branch: `spike/container-profile`
- Depends on: — · Size: S · Demo: yes

## Question
Does the `container` profile work end to end on Linux and macOS with Docker (Desktop):
worktree bind-mounted, only `run/` of the witness state dir mounted, host-side watcher
seeing container writes quickly and reliably?

## Tasks
1. A dev container with Claude Code installed, UID matching the host user, worktree
   mounted at `/workspaces/<name>`, `$MINDS_WITNESS_HOME/run` mounted read-write at
   `/run/minds-witness`, `MINDS_WITNESS_SOCKET=/run/minds-witness/witness.sock`.
2. Host: a throwaway Rust or shell watcher (`inotifywait -mr` / `fswatch`) on the worktree.
   Measure latency from `echo x > file` in the container to the host event (p50/p95 over
   200 writes), and check for lost events under a burst (1000 small files).
3. Verify from inside the container: the state dir is invisible; only the socket exists;
   `git commit` inside the container triggers the post-commit hook.
4. Record the container-vs-host path mapping needed (`/workspaces/<name>` ↔ host path).

## Deliverable
- `docs/spikes/ea-s2-container-profile.md` with measurements and findings.
- Templates to be reused by EA-10: `templates/witness/devcontainer.json`,
  `templates/witness/compose.yaml` (placed under `crates/minds-cli/templates/witness/`,
  included via `include_str!` later).
- Decision on the watcher backend per OS (inotify on Linux; FSEvents via Docker Desktop
  on macOS, or "record on Linux only" if unreliable).

## Non-goals
No witness implementation, no `minds enable` changes.
