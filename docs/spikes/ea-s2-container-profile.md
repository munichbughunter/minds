# EA-S2 — Container profile spike

## Status and decision

The reusable templates, repeatable experiment, and provisional backend decision
are implemented. The empirical latency and burst measurements are deferred as
requested; no figures are claimed without actually running Docker.

| Host | Backend | 200 writes: p50 / p95 | 1000-file burst: missing | Socket / isolation / hook | Result |
| --- | --- | --- | --- | --- | --- |
| macOS 26.6, arm64, 2026-10-02 | FSEvents through `fswatch` | Deferred | Deferred | Not run | Docker and `fswatch` absent |
| Linux with native Docker Engine | inotify through `inotifywait` | Deferred | Deferred | Not run | Deferred |

Backend decision for EA-08/EA-10: use **inotify on native Linux** as the first
qualification target. Keep **macOS container recording disabled** until both the
FSEvents measurements and the host-socket round trip pass on an identified Docker
Desktop version and file-sharing backend. This is a conservative rollout decision,
not a measured finding that FSEvents loses events. Linux itself still needs the
recorded run is a follow-up, rather than a prerequisite for reusing these templates.

Docker Desktop runs containers in a Linux VM. File visibility and a socket inode
being visible do not prove that a macOS process can be reached through that socket.
The experiment therefore tests an actual host-side Unix-socket reply independently
of file watching. Docker documents special forwarding for its SSH-agent socket;
that does not establish forwarding for an arbitrary witness socket. See
[Docker Desktop networking](https://docs.docker.com/desktop/features/networking/networking-how-tos/).
If this probe fails, fast FSEvents alone cannot qualify the profile. No TCP relay or
Linux-VM witness is silently substituted for the specified host witness.

## Deliverables and template contract

- [devcontainer.json](../../crates/minds-cli/templates/witness/devcontainer.json)
- [compose.yaml](../../crates/minds-cli/templates/witness/compose.yaml)
- [Experiment](ea_s2_probe.py) and [measurement regression tests](test_ea_s2_probe.py)

EA-10 can include the two templates verbatim with `include_str!` and write them
side by side in `.devcontainer/`. The Dockerfile is inline in Compose, so there is
no third template to emit. Compose must support `build.dockerfile_inline` (2.17+).
The experiment additionally needs Compose's `build --builder` option and Buildx
0.14+ (`default-load`). Use a local Docker daemon: bind mounts on a remote daemon
refer to the remote filesystem, while this experiment observes the local one.

Set these variables in the **host environment before starting Compose or the dev
container editor**. The templates do not inject the witness home into the agent:

| Host variable | Value |
| --- | --- |
| `MINDS_HOST_UID` | `id -u`; must be nonzero |
| `MINDS_HOST_GID` | `id -g`; existing numeric group IDs are supported |
| `MINDS_WORKSPACE_ROOT` | Canonical absolute path to the repository |
| `MINDS_WORKSPACE_NAME` | Basename of that path, matching `${localWorkspaceFolderBasename}` |
| `MINDS_WITNESS_HOME` | Canonical absolute host state directory, **outside the repository** |
| `MINDS_CLAUDE_CODE_VERSION` | Optional exact Claude release; defaults to `stable` |

Required values use Compose's error-on-missing interpolation. Both bind sources
must already exist (`create_host_path: false`). EA-10 must reject a witness home
inside the workspace, resolve symlinks before checking containment, validate a
single path component for the workspace name, and preserve the same basename in
both configurations. These host-side validations are not implemented in `enable`
by this spike.

The image installs Git, socat (for the spike's socket probe), and native Claude
Code as user `agent`. Its UID/GID match the host; dev-container automatic UID
rewriting is disabled. No Node installation is needed. The native installer accepts
a release channel or exact version; see [Claude Code setup](https://code.claude.com/docs/en/setup).
Use an exact release when collecting comparable runs, record `claude --version`,
and rebuild to update; background auto-updates are disabled. Agent credentials are
not mounted or embedded. Interactive Claude authentication is a separate user step.

The container has exactly two configured host binds:

| Host | Container | Access |
| --- | --- | --- |
| Canonical repo root | `/workspaces/<name>` | Read/write |
| `$MINDS_WITNESS_HOME/run` | `/run/minds-witness` | Read/write |

`MINDS_WITNESS_SOCKET=/run/minds-witness/witness.sock` is a service environment
variable, so terminals, `docker compose exec`, Claude, and Git hooks inherit it.
The agent runs without Linux capabilities and with `no-new-privileges`. Compose
selects the named image account `agent` so its passwd entry, home directory, UID,
and primary GID remain consistent. Neither
the Docker socket nor the host home, journal, salt, keys, ledger, or witness config
is mounted. Mount the **directory**, not the socket inode, so recreating the socket
does not leave a stale file mount. The probe uses state/run mode `0700` and socket
mode `0600`, with the host user's UID owning all three; it never widens permissions.

This isolation assumes the agent cannot control the host Docker daemon or add
mounts. It does not defend against a host user deliberately changing the profile.
Because `run/` is writable, the agent can unlink the socket (denial of service);
that does not grant access to the evidence store. A linked Git worktree whose
`.git` points outside the mounted root needs separate EA-10 handling. The spike
uses an ordinary repository with its Git directory inside the mount.

## Reproduce the experiment

Run as a non-root user on Linux with native Docker Engine, Compose, Buildx, and
`inotifywait` (inotify-tools), or on macOS with Docker Desktop, Compose, Buildx,
and `fswatch`. Python 3.10+ is run through `uv`; the script has no Python package
dependencies. The image build needs access to Debian packages and Anthropic's
native installer. No dependencies or Docker installation are performed on the host
by the script.

From the repository root:

```sh
uv run --no-project --no-managed-python --no-cache python -B \
  docs/spikes/ea_s2_probe.py --output ea-s2-result.json
```

The output file is the deliberately retained result artifact and is never
overwritten. It contains UTC date, OS/architecture, Docker/Compose/watcher/Claude
versions, UID/GID, path mapping, individual checks, raw latency samples,
percentiles, and missing burst filenames. Record the Docker Desktop version,
selected file-sharing backend (e.g. VirtioFS), host filesystem, and resource limits
alongside each real result; the runner does not infer those from Docker Engine's
version. No host/container clocks are subtracted from one another.

The runner creates a fresh project under `/tmp/ea-s2-*`, a synthetic state tree
outside its worktree, and a temporary host socket responder. This is a ping probe,
not a witness implementation or its future frame protocol. Use `--scratch-parent`
if Docker Desktop does not share `/tmp`; choose a short canonical path because
Unix sockets have a pathname-length limit. All application probes run as `agent`:

1. Verify Claude is installed and UID/GID match the host.
2. Verify the host state path is absent, `MINDS_WITNESS_HOME` is unset, only
   `witness.sock` appears under `/run/minds-witness`, no Docker socket is exposed,
   and a symlink to the host-only synthetic key cannot be read.
3. Send `ea-s2-ping` through the mounted socket and require the host's
   `ea-s2-pong` reply. Socket visibility alone is insufficient.
4. Create a Git repository, install a throwaway post-commit hook, and commit from
   inside the container. Require its marker on the host and the inherited socket
   path. This verifies Git hook execution, not EA-06d checkpoint delegation.
5. Start the recursive host watcher and wait for an observed readiness write.
   Then measure 200 distinct small writes, followed by 1000 distinct burst files.
   File measurements still run if the socket or isolation check failed.

### Latency and loss methodology

Linux uses `inotifywait -mr -e close_write`; macOS explicitly selects fswatch's
`fsevents_monitor` with a requested 10 ms latency. Both stream NUL-delimited paths
to a host reader thread. The watcher starts after the test directories exist,
avoiding recursive-watch installation races during the burst.

One persistent `docker compose exec -T` shell performs all 200 writes. For each
write, the host records a monotonic timestamp immediately before sending the
filename, and another when its first event is read. This is a **latency upper
bound**, including pipe/exec transport, shell scheduling, file write, event
delivery, and reader scheduling. It excludes repeated Docker process startup and
does not require synchronized VM clocks. It cannot isolate pure filesystem-event
latency. p50/p95 use nearest rank; every sample is retained. Missing events time
out after two seconds and are counted separately, never converted into zeroes.

The burst is a single container shell loop writing 1000 files without pauses. The
host waits up to ten seconds after the writer exits. Count distinct expected paths,
not raw events: duplicates cannot hide loss, and the final directory count alone
cannot prove delivery. A pass requires all 200 events, every observed single-write
upper bound below one second, all 1000 burst events, all 1000 host files, and every
functional check passing. A lower bound or median alone cannot hide tail failures.
This checks retained small files, not rename storms, transient create/delete
pairs, large trees, or queue-overflow recovery; those belong to EA-08.

### Cleanup

On success, failure, SIGINT, or SIGTERM, the runner attempts to remove its Compose
containers/network/volumes/image and its separately named Buildx builder, including
that builder's cache. It removes the BuildKit daemon and Debian base images only
when they were absent before the run. Temporary worktree, synthetic state, and watcher files are deleted
by context managers. Existing projects, images, and shared build caches are never
globally pruned. Cleanup errors are recorded and prevent a successful result.
A killed process (SIGKILL), unavailable Docker daemon, or machine crash can prevent
cleanup; the result's failed command identifies the unique project/builder to
remove. Do not run a global `docker system prune` as cleanup for this spike.

## Path mapping for the witness

For a repo at `/home/alice/projects/demo` (or `/Users/alice/projects/demo` on macOS):

```text
/workspaces/demo  ↔  /home/alice/projects/demo
```

EA-10 passes this as `--path-map /workspaces/demo=/home/alice/projects/demo` to the
host witness. Translate paths by complete path components: `/workspaces/demo2`
must not match `/workspaces/demo`. Resolve and constrain filesystem accesses to
the canonical host root; reject traversal or symlink escapes. Raw hook payloads
remain verbatim evidence; mapping is only for locating host files. Store mapping
and witness state location on the host, not in agent-editable Git configuration.

## Local validation and remaining gate

On the available macOS host, the runner's real preflight returned exit 1 with
`Missing prerequisites: docker, fswatch`. No image was built and no Docker
resources were created. JSON/YAML syntax, Python lint/formatting, measurement
regressions, workspace formatting, Clippy, and workspace tests are checked locally.
Validation result: 4 probe regressions and 1462 workspace tests passed; 2 existing
workspace tests were ignored. `cargo fmt --all --check`, Clippy with `-D warnings`,
Ruff check/format, and JSON/YAML parsing passed. The initial sandboxed workspace
run could not bind a local test server; the complete rerun outside the sandbox
passed. Temporary validation logs and the task-specific uv cache were removed.
The socket, image build, dev-container editor startup, and watcher integrations
remain unverified because this session did not have Docker. Linux and macOS
measurements are follow-up evidence; the templates, checks, and provisional
Linux-first / macOS-disabled rollout decision are ready for EA-10.

Local checks (no Docker needed):

```sh
uv run --no-project --no-managed-python --no-cache python -B -m unittest discover \
  -s docs/spikes -p 'test_ea_s2_probe.py'
uv run --no-project --no-managed-python --no-cache ruff check --no-cache docs/spikes/*.py
uv run --no-project --no-managed-python --no-cache ruff format --check --no-cache docs/spikes/*.py
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The Python regressions exercise nearest-rank statistics, split/NUL event framing,
duplicate handling, missing/slow-event failure, socket/isolation failures, cleanup
failure classification, and protection of existing measurement files. They do
not substitute for Docker measurements. No witness implementation, production
dependency, hook behavior, or `minds enable` code is changed by this spike.
