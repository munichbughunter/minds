# Command Reference

`minds` captures the context of AI-agent coding sessions in Git — intent, attribution, redaction, evidence chain, and reviews — and makes it queryable from the terminal. `minds --help` prints the same command list in the terminal.

## Setup

### minds enable

```
minds enable [--agent <name>] [--child-repo <path>] [--child-remote <url>] [-v] [--ref <name>] [--recall] [--global-hooks]
```

Prepares the repository for Minds: registers the hooks with the agent and the repo, and writes the store config to `.git/config`. Runs silently; `-v`/`--verbose` shows each step. Without `--agent` it configures all known agents (`claude-code`, `codex`, `cursor`, `gemini`, `opencode`, `all`); the command is idempotent and leaves third-party hooks untouched. `--child-repo` places the context in a separate repository instead of in-repo (created bare, or cloned from `--child-remote`); `--recall` (Claude Code) adds an opt-in SessionStart hook that prepends the context brief from previous sessions to a new session (costs tokens); `--global-hooks` confirms a hooks directory outside the repo (e.g. a globally set `core.hooksPath`) — without the flag, enable asks or aborts.

```
minds enable --agent claude-code -v
```

### minds enable --witness

```
minds enable [--agent <name>] --witness container|user|managed
```

Runs the normal `enable` steps first, then sets up a witnessed configuration. No step runs a privileged command, no existing file is overwritten — when a file already exists with different content, `<name>.minds-proposed` is written next to it and the report says so — and no service is started. Reruns are idempotent.

- **`container`** (run on the host, in the repository): `minds witness init` with `--path-map /workspaces/<name>=<root> --profile container` and `minds witness keygen` (skipped when the key pair exists; an incomplete pair stops `enable` — remove `key/witness_ed25519*` and rerun) in the witness home (`MINDS_WITNESS_HOME`, otherwise `$XDG_STATE_HOME/minds-witness/<repo-id>`; it must lie outside the repository, symlinks resolved). Writes `.devcontainer/devcontainer.json` and `.devcontainer/compose.yaml` from the EA-S2 templates, which mount only `$MINDS_WITNESS_HOME/run` at `/run/minds-witness` and set `MINDS_WITNESS_SOCKET`. Installs a user service — `~/.config/systemd/user/minds-witness-<repo-id>.service` on Linux, `~/Library/LaunchAgents/dev.minds.witness.<repo-id>.plist` on macOS — and prints the exact command to enable it. If a different service file already exists, it is left alone, the template goes next to it as `*.minds-proposed`, and the report asks you to review and move it into place before starting. The service file must not lie inside the repository. Prints the host variables Compose needs and the witness key's `allowed_signers` line: keep it in a file outside the repository that the team distributes out of band, and pass that file to `minds verify --signers`. The report also shows the pins `init` recorded (git dir, store, policy source). The service runs the `minds` binary you ran `enable` with; a binary inside the repository (e.g. `target/release/minds`) or a writable one with more than one hard link is refused, because the agent could replace it and the service would run it on the host. A child-repo store is pinned only from `--child-repo` given to this `enable`, never from `.git/config` alone. `.devcontainer/` is part of the isolation boundary and lives in the agent-writable repository: review every change to it; `minds doctor` on the host fails when `.devcontainer/` holds anything but the two unchanged templates (and `*.minds-proposed`), e.g. a `.env` or a second `devcontainer.json`, or when `.devcontainer.json` exists at the repository root. On macOS, container recording stays unqualified until the EA-S2 measurements pass.
- **`user`**: prints the exact steps for a dedicated OS user `minds-witness` and group `minds-agents` (install minds from a verified release artifact, never from the agent's account; home ownership with group traverse on `/var/lib/minds-witness`; sharing the repository with the group as its owner, never as root, inside a block that stops on a planted symlink; socket group; `witness init --profile user --socket-group minds-agents`) including the systemd unit for that user. Nothing is executed.
- **`managed`**: writes only `managed-settings.minds-proposed.json` (the EA-S1 settings, aimed at this repository's witness home, `.claude/` and binary) with a `$comment` header: the profile yields A1 until EA-S1 says otherwise. Paths with glob or rule metacharacters are refused, since a rule could not protect them. The file lies in the agent-writable repository, so the report prints its BLAKE3 digest: compare it before installing.

```
minds enable --agent claude-code --witness container
```

### minds doctor

```
minds doctor [--probe-home <dir>]
```

Checks the setup, one line per check — `ok`, `warn` or `fail` with one line of reason; exit 0 without `fail`, otherwise 1. Checks the agent hook registrations, the Git hooks and the store config (same wording as `minds fsck`), then the witness:

- **Agent side** (`MINDS_WITNESS_SOCKET` set or `--probe-home` given): the socket variable is set, the witness answers `ping`, and the **isolation probe** — `doctor` tries to open the witness home given with `--probe-home` and the private files in it (non-blocking; a FIFO does not hang it). If anything opens, the result is `fail` ("agent can reach the witness home"). A home that exists but is refused (`EACCES`, e.g. the `user` profile) is `ok` — that is proof. A home that is simply absent (the `container` profile) is only `warn`: absence in another mount namespace proves nothing. On Linux, `doctor` also reads `/proc/self/mountinfo` and fails if a mount exposes the witness home other than its `run/` — a heuristic that does not recognise a mount of a parent directory or a source on another partition, so "nothing found" stays `warn`. Check the container's mounts yourself. An empty `--probe-home` is `fail`, and so is an unclear error (anything but `EACCES`/`ENOENT`/`ENOTDIR`) with `--probe-home`. Without it, `doctor` probes `MINDS_WITNESS_HOME` or the default location: nothing there is at most `warn`, since a missing default path proves little; something that opens is still `fail`. Nothing is read; an opened path is closed immediately.
- **Host side** (`witness.json` in the witness home): profile, pins (`fail` without them), whether the witness answers, the key private, and — for the `container` profile — whether `.devcontainer/` holds exactly the two unchanged templates plus at most their two proposals (`devcontainer.json.minds-proposed`, `compose.yaml.minds-proposed`), all as regular files, and the repository root has no `.devcontainer.json` (`fail` if not). It also looks for code a `git` on the host would run — hooks other than the minds blocks (and `*.sample`), and `.git/config` keys known to run commands (`core.hooksPath`, `core.fsmonitor`, `core.sshCommand`, `filter.*`, `diff.*.textconv`, `include.path`, `!`-aliases and others) — and reports findings as `warn`. This is a heuristic, not proof: a list cannot cover everything an agent can put into `.git` (`commondir`, `config.worktree`, new git keys, nested repositories). Do not run git on the host in a tree the agent wrote. A `.env` at the repository root is a `warn`. If a witness dev container is set up but no witness home is found, that is `fail`, not "not configured". The home is derived from the repository directory itself, never via `git rev-parse` (which follows `core.worktree`). Run it right before every reopen or rebuild of the container. A host-side `ok` is a snapshot of the files, not proof of how the running container was built; an agent-side `ok` comes from a binary running in the agent's environment and is advisory.

```
minds doctor --probe-home /home/alice/.local/state/minds-witness/86f649449fc18941
```

## Daily use

### minds show

```
minds show [<commit>] [--full]
```

Shows the intent and attribution of the session(s) behind a commit (default `HEAD`). The output is compact; `--full` adds the prompt, all files, and edges.

```
minds show HEAD~2 --full
```

### minds why

```
minds why <file>:<line> [--full]
```

Shows the session behind a single line, resolved via `git blame` and the session trailer.

```
minds why src/lib.rs:42
```

### minds blame

```
minds blame [--lines] <file>
```

Shows which session is behind which lines of a file, aggregated per session, with context coverage as a percentage. `--lines` prints one annotated row per source line instead — short session id, agent name, line number, and the source text, in file order; lines without captured context carry a `-` in both attribution columns.

```
minds blame crates/minds-core/src/evidence.rs
minds blame --lines src/retry.rs
```

```
src/retry.rs — 3 lines, 1 with captured context (33%)

-                -            1  fn retry() {
b3-a1b2c3d4e5f6  claude-code  2      backoff(3);
-                -            3  }
```

### minds recap

```
minds recap [--limit <n>] [--all]
```

Lists the most recent sessions at a glance. Shows 10 by default; `--limit` changes the count and `--all` shows everything.

```
minds recap --limit 25
```

### minds search

```
minds search <query>
```

Searches the intent, transcript, and files of the captured sessions.

```
minds search "retry backoff"
```

### minds inspect

```
minds inspect [<query> | <file>:<line>]
```

Shows how a change came to be, in the terminal: a session list, the graph of a session (intent → agent → effects → change → review), and the why-chain of a line. On a terminal at least 120 columns wide the list stays in a left column and the graph of the session under the cursor is previewed on the right; `Enter`, `w` and `e` open Graph, Why and Evidence beside the list. Evidence mode includes an `ARTIFACT` section: the reconciliation of the commits that carry the session (`artifact X/Y lines explained`) and a per-file list where unexplained lines are named as `not observed in the session`. Strictly read-only. When stdout is not a terminal, lines are emitted tab-separated for `grep`/`fzf`.

```
minds inspect src/main.rs:10
```

### minds recall

```
minds recall <target>
```

Condenses the session(s) behind a file, a line (`<file>:<line>`), or a commit into a short context brief. Deterministic and costs 0 tokens — the agent-facing sibling of `why`.

Includes the complete intent request, preserving multiple lines. Terminal output is
sanitized and wrapped to the terminal width (`COLUMNS` or 80 columns as a fallback).

```
minds recall src/lib.rs:42
```

## Agent context

### minds brief

```
minds brief [<file>...]
```

Emits a size-capped context block for the start of an agent session. Without paths it covers the whole repository.

```
minds brief src/lib.rs src/main.rs
```

### minds distill

```
minds distill [--path <directory>] [--out <file>]
```

Condenses the history of the repository (or of a path) into an `AGENTS.md` draft: commands, hot files, dead ends, corrections. Without `--out` it writes to stdout.

```
minds distill --path crates/minds-cli --out AGENTS.md
```

### minds agent-help

```
minds agent-help
```

Prints a machine-readable command map as JSON — meant for agents, not humans.

```
minds agent-help | jq .
```

## Reviews

### minds review

```
minds review <subject> --approve|--reject|--needs-work [--summary <text>] [--sign] [--key <path>]
```

Records a review verdict as a Git object under `refs/minds/reviews`. `<subject>` is a change id (`I…`) or a session id (`b3…`). `--sign` signs the verdict (ssh-sig), turning a claim into evidence; the key comes from `--key` or `git config user.signingkey`.

```
minds review I4f2a9c --approve --summary "LGTM" --sign
```

### minds reviews

```
minds reviews <subject> [--signers <file>] [--identity <id>]
```

Shows the verdicts and the comment thread for a change id or session id. With `--signers`, signatures are verified instead of merely reported.

```
minds reviews I4f2a9c --signers .minds/allowed_signers
```

### minds comment

```
minds comment <subject> [--on <file:line|turn:<n>>] "<text>"
```

Appends a comment to the review thread. The thread is an append-only log of content-addressed entries — two reviewers working offline produce a union, not a conflict.

```
minds comment I4f2a9c --on src/lib.rs:42 "Prefer a bounded channel here."
```

### minds stack

```
minds stack [--base <ref>]
```

Shows the dependent changes above the base and the review state of each. Because the verdict is attached to the change id, it survives rebase and force-push.

```
minds stack --base origin/main
```

## Evidence & compliance

### minds seals

```
minds seals [--session <id>] [--limit <n>]
```

Lists Evidence-Chain seals — id, linked session (if any), event range, gap/signature status, timestamp — so a seal id becomes something you discover, not something you must already know before `verify --evidence` or `sign --seal`. Most recent first; `--session` scopes to one session's seals, `--limit` caps how many print (applied after sorting). A tampered or unreadable seal is reported inline and does not hide the rest of the list.

```
minds seals --session b3a1f0e --limit 5
```

### minds witness init / run / status

```sh
minds witness init --repo /host/project --home /private/witness \
  --path-map /workspaces/project=/host/project [--profile container|user|managed] \
  [--socket-group <group>] [--child-repo <path>] [--policy-rev <rev>]
minds witness keygen --home /private/witness
minds witness run --home /private/witness --follow
minds witness status --home /private/witness
```

The Unix-only daemon owns one repository. `init` creates a schema-version-2
`witness.json` and private journal, epoch, key, ledger, log and runtime directories.
Without `--profile`, omitting `--path-map` selects `user` and a map selects
`container`; `container` requires a map, `user` and `managed` refuse one, and
`--socket-group` (name or number; `user` only) lets that group traverse the home
and `run/` (mode 0710) to reach the socket. The map must connect an absolute agent
repository root to the canonical host root. Profile selection alone does not
establish an assurance level. `minds enable --witness` runs `init` for you.

`init` **pins** what the witness trusts, so nothing the agent can edit later
redirects it, and prints what it pinned:

- the canonical git directory (exactly `<root>/.git`; linked worktrees are refused);
- the store: in-repo with the context ref from the repository config (only under
  `refs/minds/`), or a child repository — only when you name it with
  `--child-repo <path>` (outside the observed repository). If the repository
  config selects a child repository you did not name, `init` stops instead of
  pinning a path the agent may have written;
- a witness-owned redaction policy: `.minds/redact.json` from a **commit** —
  HEAD, or the revision you name with `--policy-rev` — preferably the full id of
  the reviewed commit, which the agent cannot redirect (a full id is taken as a
  commit even if a ref of that name exists; ref names like `origin/main` are
  agent-writable locally) — never the worktree. It is read through the pinned git directory
  without config includes and without `refs/replace/*`, size-checked before it is
  read, and verified: commit, every tree on the path and the blob are re-hashed
  from the bytes read and must match their ids (gix does not verify on read, and
  the agent may own `.git/objects`). It is floored at the strict default. Only "the file is not in that commit"
  (or "no commit yet") falls back to the strict default; any other error stops
  `init`. A policy that would redact its own placeholders (e.g. a term like
  `redacted`) is refused — it would make every redacted session unstable. Local
  refs belong to the agent too, so the report names the full commit and blob id
  (also when `init` finds the home unchanged): compare them with the version your
  team reviewed. Trees on the path must be sorted without duplicates (as `git fsck`
  requires), commits and trees are size-limited, and `init` refuses when
  `refs/replace` or `info/grafts` exists — your own `git` would show other content. On a pinned home, a `--policy-rev` whose policy differs is
  refused, not ignored.

The running witness never reads `.minds/redact.json` or the store keys in
`.git/config` again. `init` is idempotent: the same configuration is left as it is
(pins included); a different one — including a `--child-repo` other than the
pinned store — is never overwritten, and the error names what differs. A schema-1 `witness.json` (from before the pins) still loads, but the
witness refuses to run or seal with it — fail-closed, since the team's extra
redaction terms would otherwise be dropped silently. Running `init` again with the
same arguments adds the pins (stop the witness first).

`run` requires a generated key and refuses symlinked, foreign-owned or insufficiently
private state. A process lock enforces a single writer even during stale-socket
recovery. Only `run/witness.sock` (0660) belongs in the agent's namespace; keep
the rest of the state directory on the host.

**Checkpoint requests run in a worker process.** The witness starts its own binary
(`minds witness __checkpoint`, internal) per request, with a 60-second deadline,
CPU, file-size, open-file and core limits (plus a data-segment limit on Linux), and its own process group. It opens the
pinned git directory once, without following `include.path`/`includeIf`, and reads
tracked paths from the index directly instead of running `git ls-files`. A worker
that blocks (a FIFO swapped in after the layout check, the gix-ref lock loop), dies
(`SIGBUS` from a truncated mapped pack) or overruns the deadline costs only that
request — the witness keeps running and the sessions stay open. At the deadline the
worker first gets `SIGTERM` (it removes the lock files gix holds) and `SIGKILL` two
seconds later; lock files still left behind are named in `log/witness.log`. The
worker's diagnostics reach the witness log through the witness, deduplicated and
redacted; an overlong line is dropped, never cut. The rate limit applies **per
client**, where the client is the requested commit (and the commit the trailers
were retrofitted to): one run per second each, so an agent requesting checkpoints
for its commit every second does not make the human's post-commit request for a
new commit see `rate limited`. A **failed** run blocks every client for that
second, so made-up commit ids get at most one run per second. Every repeated line
in `log/witness.log` is deduplicated within a minute and the count is written
later; a panic prints a fixed line instead of the panic message.

Honest limit: the layout check before a run cannot stop the agent from swapping an
entry inside `.git` for a symlink between the check and the worker's write — the
worker runs as the witness user. Closing that needs file access relative to an open
directory descriptor, which gix does not offer.

Hook frames are parsed and secret-file-filtered using the same code as local hooks.
Each journal append advances a salted, atomically persisted chain. Restart checks
the persisted prefix before completing interrupted updates. `witness.start` records
whether the previous run stopped cleanly; SIGINT and SIGTERM append `witness.stop`.
`--follow` shows sequence, event kind, interpreted tool/path and chain head, without
prompts, arguments or tool responses. Redacted diagnostics go to `log/witness.log`.
`status` shows the profile, repository, socket health, open hook sessions, most recent
event, key fingerprint and the ten latest ledger entries.

The checkpoint checks the live root before sealing, signs under `minds-witness` with
scope `witness/v1`, records each seal once in the fsynced ledger, and attaches the
standard session trailers. Path mapping affects artifact reads only; stored hook paths
and payloads retain the agent namespace. Host transcripts are never loaded from
hook-supplied paths. Intent requests currently receive `not supported`.

On non-Unix platforms, `minds witness` exits 4 with
`not supported on this platform`.

### minds witness keygen

```
minds witness keygen [--home <directory>]
```

Creates an Ed25519 key at `<home>/key/witness_ed25519` with mode 0600 and
prints its `namespaces="minds-witness"` allowed-signers entry to stdout.
Refuses existing private/public keys and symlinks. Home defaults to
`MINDS_WITNESS_HOME`, otherwise `$XDG_STATE_HOME/minds-witness/<repo-id>`
(`$HOME/.local/state` when XDG is unset). Existing home and key directories
must be private and owned by the current user. No repository is needed when
the home is explicit. See the [verification guide](verification-guide.md)
for trust-file distribution and independent signature verification.

### minds verify

Witness-scoped seals require a signature in `minds-witness` by a principal
discovered in the trusted `allowed_signers` file. Success prints
`witness-signed (<principal>)`; missing, invalid or incorrectly namespaced
signatures produce `TAMPERED`. Without a signer file, present signatures
print `signature not checked`. See the [verification guide](verification-guide.md).

```
minds verify [<session|rev>] [--signers <file>] [--identity <id>]
             [--commit <rev>] [--require-explained <percent>] [--all]
             [--witness-home <dir>] [--require-assurance <A0|A1|A2|A3>] [--limits]
minds verify <session> --sig <file> [--signers <file>] [--identity <id>]
minds verify --evidence <seal-id>
```

Renders the evidence verdict: integrity × coverage across the seals of a session. With no argument, verifies the sessions linked to `HEAD`. A target that is not a session id is resolved as a Git revision (for example `HEAD~1`). Sessions come from `Minds-Session-Id` trailers, falling back to the store's commit-to-session index when no session trailers are present.

Multiple sessions produce the same blocks as individual session checks, separated by a blank line. Exit codes: 0 VERIFIED, 1 TAMPERED, 2 INCOMPLETE, 3 NOT VERIFIABLE, 4 operational failure. The worst result wins in the order **4 > 1 > 3 > 2 > 0**. If HEAD has no linked session, prints `No session is linked to HEAD (<short sha>).` and exits 3. Invalid revisions exit 4.

With `--sig` a session id is required; it checks a signed attribution and exits non-zero when the signature is invalid. `--evidence` yields the verdict for a single seal, even without a session (redaction block).

**Artifact coverage.** The Coverage line also reconciles the commit against the stored write evidence: how many added or modified lines of the commit are explained by the sessions this run verifies, followed by the places that are not.

```
Coverage       complete within the boundary (boundary: agent-hooks/v1 — activity outside it is not captured · 0 gaps · artifact 148/151 lines explained)
  unexplained    src/sort/merge.rs:88     not observed in the session
  unexplained    src/sort/merge.rs:91-92  not observed in the session
  file only      Cargo.lock               line level unavailable (reconstruction mismatch)
```

- **Which commit.** `--commit <rev>` if given, otherwise the resolved revision (`HEAD` by default); for a session id, the newest commit reachable from `HEAD` (topological order) whose `Minds-Session-Id` trailer names the session. Without such a commit the block prints `Artifact       not assessed (no linked commit)`. In a shallow clone whose first parent is missing, or a partial clone missing a tree or blob, it prints `not assessed (first parent not in this clone (shallow))`, `not assessed (tree not in this clone (partial))` or `not assessed (blob not in this clone (partial))` — verify never fetches. None of these changes the verdict. Objects are read without replace refs (`refs/replace`) and checked against their ids; a replaced object is an error, never a substitute.
- **Which evidence.** Only the sessions whose verdict this run prints contribute claims — the target session, or the sessions of the target revision. `--commit` chooses the commit, not the sessions: `minds verify <session> --commit <rev>` reconciles `<rev>` against that one session. `minds verify <session> --require-explained N` judges the session's own trailer commit, not `HEAD`; to gate `HEAD`, use `minds verify --require-explained N`.
- **Which paths.** Agents record absolute paths. A claim under this checkout's root names exactly that file. In a clone elsewhere (CI), the session's recorded working directory stands in for the root at capture time; because the repository root itself is not recorded, such a claim only counts if it lies below that directory and exactly one candidate path exists in the commit's trees. Deletions — and therefore renames, which count as deletion plus addition — are never explained this way, and sessions started in a subdirectory may stay unexplained outside the capture checkout: fail-closed.
- **What "explained" means.** A line counts as explained when evidence backs it — today a tool claim whose write-time hash matches the committed bytes (*reported only*). That is the agent's own statement, chained into the seals by the hooks; without a valid seal signature (`--signers`) or a witness (EA-08) it is a self-declaration of whoever controls the evidence refs, not an observation. Lines without matching evidence (human edits, shell writes) are *unexplained*. A claim is bound to the bytes it wrote, not to the commit's parent: a linked session that once wrote exactly these bytes explains them, even if a later commit had changed them in between. Contiguous lines are compressed to ranges; at most 20 detail lines are printed, then `… N more (minds verify --commit <rev> --all)`. *file only* marks files judged as a whole (binary, too large, reconstruction mismatch); such a file weighs one line when backed and all of its lines when unexplained. Changes that are not added lines are listed too: deletions, removed lines (a removal not replaced in place, from the base or from what the agent wrote), submodule pointers (also when `.gitmodules` says `ignore`), mode changes, new executables and symlinks. Paths are sanitized before printing (and shortened beyond 256 characters); file contents never appear. Because every verdict-mode run reads the commit's blobs, a damaged object or a replaced object (`refs/replace`) now makes `minds verify` exit 4 even without the artifact flags.

`--require-explained <percent>` (an integer from 0 to 100) turns this into a gate. It fails when explained × 100 / changed falls below the requirement (`Gate           explained 98% < required 100%`) and — for any requirement above 0 — when an unexplained change is not captured by added lines: a deletion, removed lines, a binary or oversized file, a submodule pointer, a mode change or a symlink (`Gate           1 unexplained change(s) beyond added lines — required 100%`). A requirement above 0 also fails when nothing can be assessed (no commit, shallow or partial clone, several trailer commits for a session id — pass `--commit`); `--require-explained 0` requires nothing. Only `100` is a hard statement: below it, a large backed file under 2 MiB (a regenerated lockfile) can outweigh unexplained lines elsewhere. A failed gate exits 2 — the same code as VERIFIED, INCOMPLETE, so a pipeline that tolerates 2 also skips the gate; the printed `Gate` line tells them apart — unless the verdict is already 1, 3 or 4, which always win. With several sessions, each block repeats the commit's artifact lines; the `Gate` line appears once at the end.

The gate judges **one commit** against its first parent. A branch with several commits needs one run per commit — otherwise a human commit sandwiched below an agent commit at `HEAD` goes unchecked:

```
for c in $(git rev-list --reverse origin/main..HEAD); do
  minds verify "$c" --require-explained 100 || exit $?
done
```

The flags apply only to the evidence verdict, not to `--sig` or `--evidence`. A human commit without a linked session exits 3 (NOT VERIFIABLE) in that loop — which also stops it.

**Assurance and limits.** Below the three axes each block states who observed the material and what it does not prove:

```
Integrity      intact
Coverage       complete within the boundary (boundary: witness/v1 — activity outside it is not captured · 0 gaps · artifact 1/1 lines explained)
  uncorroborated  turn 1 call 1  Write src/sort/merge.rs  b3-deadbeef…  no file-system observation
Interpretation complete
Assurance      A1 observed    (range 1: witness signature not checked — no trusted allowed_signers)
Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)
Overall        VERIFIED
```

- **`Assurance`** is computed at read time, never stored: `A0 claimed`, `A1 observed`, `A2 witnessed`, `A3 reproduced` — the weakest sealed range of the session. Below A2 the parenthesis names the first reason the level is not higher, with the range it comes from (ranges count from 1 in time order). From A2 on it names the verified witness principal, the profile and the intent signature. Witness signatures count only against a trusted signer file: `--signers`, otherwise `~/.ssh/allowed_signers` — never `gpg.ssh.allowedSignersFile` from the repository — and only for principals restricted to `namespaces="minds-witness"`. The witness profile and its isolation proof are not yet part of the signed material, and intent binding does not exist yet; A2 is therefore not reachable today (`witness profile unknown`, `intent not bound`). A session linked only through the store index with an inferred (heuristic) edge is `A0`. The level says who observed the session, not whether the session explains the commit — that is the Coverage axis. **A1 is no promise against the agent:** an agent can write its own `agent-hooks/v1` seals, trailers and index edges, so `--require-assurance A1` guards against missing or broken capture, not against a malicious agent; only the witness levels (A2+) are observed outside the agent's control. A signer file the agent can write (one inside the repository, or `~/.ssh/allowed_signers` when the agent runs under your user) makes witness signatures self-attestations — keep the trusted file out of the agent's reach. All seals that name the session count, not only those in its back-reference (`evidence.json`, which the agent can rewrite); a seal found only through the namespace is printed as a `Note`.
- **`uncorroborated`** lists write claims of a witnessed session that no witness observation confirms (same path, same bytes, between 2 s before and 30 s after the claim). The location is turn and call in the stored session; the hash is the first 8 hex digits of the claimed write-time hash — already stored in the repository and never formed for secret files, but in a job log it lets a reader confirm a guess for a file with very little content. Only sessions with a witness observation window get these lines; they are capped like the artifact lines (`--all`). An uncorroborated claim does not lower the level and does not change the verdict.
- **`Not proven`** lists the short forms of the limits that hold at the achieved level, at most three, then `(minds verify --limits)`. `--limits` prints every limit as a full sentence instead.

`--witness-home <dir>` compares the witness ledger (`<dir>/ledger`, the append-only list of seals the witness produced) with `refs/minds/evidence/`. A seal in the ledger that is missing from the repository makes the block `Integrity      VIOLATED  witnessed seal b3-… missing from the repository` and the verdict TAMPERED (exit 1) — for every session of the run, since the ledger names no session (also when the revision has no linked session). A ledger seal whose ref holds different bytes is reported the same way (`… altered in the repository`). A torn last line (the witness crashed while appending; it appends nothing afterwards) is not counted and prints `Note           witness ledger ends in a torn line — seals after it are not ledgered`. An unreadable ledger, or a line other than the last that is not `<seal-id> <scope> <last_event_at>`, exits 4. The ledger is read, never written.

`--require-assurance <A0|A1|A2|A3>` is a gate on the weakest session of the run: `Gate           assurance A1 observed < required A2 witnessed`, exit 2 — never masking 1, 3 or 4. Any other value exits 4.

```
minds verify --signers ~/trusted/allowed_signers
minds verify HEAD~1
minds verify --require-explained 100
minds verify b3a1f0e --commit HEAD~2 --all
minds verify --signers ~/trusted/allowed_signers --require-assurance A2
minds verify --witness-home "$MINDS_WITNESS_HOME" --limits
```

### minds sign

```
minds sign <session> [--key <path>]
minds sign --seal <seal-id> [--key <path>]
```

Signs the attribution of a session (ssh-sig) and writes the signature to stdout. The key comes from `--key` or `git config user.signingkey`; `--seal` signs a single seal instead of a session.

```
minds sign b3a1f0e --key ~/.ssh/id_ed25519 > attribution.sig
```

### minds fsck

```
minds fsck [--require-review] [--require-seal]
           [--require-assurance <A0|A1|A2|A3> [--signers <file>]]
```

Checks that every trailer resolves and reports journal gaps. Exits 1 when orphaned trailers exist. `--require-review` also requires an approval for every agent-authored change — a policy gate for CI. `--require-assurance` requires every session that a reachable `Minds-Session-Id` trailer names to reach at least that assurance level (computed as in `minds verify`, with `--signers` as the trusted signer file and without a ledger). Each session below it is listed (`below A2: <session> — A1 observed (range 1: …)`), followed by `Gate           assurance A1 observed < required A2 witnessed`; a failed assurance gate exits 2, any other finding still exits 1. Only sessions named by a reachable trailer are gated: a history without such a trailer passes (the agent controls commit messages — combine with `minds verify --require-assurance` per commit). An invalid level exits 1, like every fsck usage error; `--signers` without `--require-assurance` is refused.

```
minds fsck --require-review
minds fsck --require-assurance A2 --signers ~/trusted/allowed_signers
```

### minds forget

```
minds forget <session> [--reason <text>]
```

GDPR deletion: replaces the payload of a session with a tombstone. The reference stays resolvable while the content disappears from the store.

```
minds forget b3a1f0e --reason "customer data in prompt"
```

### minds reinterpret

```
minds reinterpret <session>
```

Re-interprets the preserved tool calls of a stored session with the current adapter state. Strictly read-only — the evidence remains unchanged.

```
minds reinterpret b3a1f0e
```

### minds audit

```
minds audit --export [--out <file>] [--base <ref>] [--mode redacted|proof]
```

Bundles the provenance chain (change → session → attribution → verdict) into a portable JSON file. It contains the canonical payloads and signatures and is verifiable without this tool. Without `--out` it writes to stdout.

```
minds audit --export --base origin/main --mode proof --out audit.json
```

## Sync & integration

### minds sync

```
minds sync [--remote <name>] [--detach] [-v]
```

Pushes context and reviews to the remote — all pending refs in one connection, never with `--force`; the only exception is transmitting a GDPR deletion (tombstone ref). Invoked by the pre-push hook; with no new refs the call opens no connection. `--detach` (used by the hook) hands the transport to a background process so the user's push does not wait on it.

```
minds sync --remote origin -v
```

### minds gitlab mirror

```
minds gitlab mirror <subject> --mr <nr> [--url <base>] [--project <id>] [--token-env <var>] [--approve]
```

Mirrors the verdicts of a change to GitLab as an MR note — one-way and idempotent; the repository remains the source of truth. The token is read only from the environment (default `MINDS_GITLAB_TOKEN`), never passed as an argument.

```
minds gitlab mirror I4f2a9c --mr 137 --project 42
```

### minds gitlab webhook

```
minds gitlab webhook [--write] [--secret-env <var>]
```

Reads a GitLab webhook payload from stdin and interprets an MR comment (`/minds approve|reject|needs-work`) as a verdict. Without `--write` it only shows what would be created; the feature is opt-in and runs no service. If `MINDS_GITLAB_WEBHOOK_SECRET` (or the variable named by `--secret-env`) holds a secret, the `X-Gitlab-Token` header is required: the receiver passes it through in `MINDS_GITLAB_WEBHOOK_TOKEN`, the comparison is timing-safe, and on mismatch the payload is discarded.

```
minds gitlab webhook --write < payload.json
```

### minds metrics

```
minds metrics [--format prometheus|openmetrics|json]
```

Exports metrics from the store: throughput, iteration, continuity, streak, redaction, and context coverage. The default format is Prometheus, ready for Grafana.

```
minds metrics --format json
```

### minds render

```
minds render [--out <directory>]
```

Builds a static HTML site from the context (default `./site`): click a line to see the prompt behind it. Stateless.

Each session page reconciles the commits the session takes part in — commits whose trailer names it, or, for commits without a trailer, those linked through the store index (then marked `claims from inferred links (no trailer)`). The reconciliation is per commit: when a trailer names several sessions, all their claims count (`claims of N sessions`), as in `minds verify <rev>`. The page shows `artifact 148/150 lines explained` and a per-file list (`explained`, `explained (fs only)`, `reported only`, `unexplained`). In the changes of the session and on a file page, unexplained lines carry a neutral gutter mark (`◦`, with a screen-reader label `not observed in the session`) — never an error colour. A file page marks lines only while the file at HEAD is byte-identical to the reconciled commit; otherwise the session page shows the marks in the commit's diff. No JavaScript required.

```
minds render --out public
```

## Plumbing — called by hooks, rarely by hand

### minds hook

```
minds hook --agent <name> [--event <name>]
```

Accepts an agent hook event on stdin and stores it in the local journal. Always exits 0.

```
minds hook --agent claude-code --event PostToolUse < event.json
```

### minds checkpoint

```
minds checkpoint [--commit <id>]
```

Interprets the journal, redacts it (policy optionally from `.minds/redact.json`: `allow`, `deny_secrets`, `deny_pii`, `secret_keys`, …), stores the sessions, and appends the Minds session-id trailer to `HEAD`. Invoked by the post-commit hook.

```
minds checkpoint --commit 76a1b3d
```
