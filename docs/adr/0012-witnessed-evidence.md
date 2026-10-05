# ADR-0012 — Witnessed Evidence: the agent does not write its own record

- Status: accepted
- Date: 2026-10-03
- Affects: `minds-core`, `minds-capture`, `minds-attest`, `minds-reader`, `minds-gitlab`, `minds-cli`, `minds-tui`
- Related: [ADR-0003](0003-hooks-over-transcript-parsing.md) (hooks over transcript parsing),
  [ADR-0008](0008-signed-attribution.md) (signed attribution),
  [ADR-0011](0011-evidence-chain.md) (evidence chain);
  implementation plan: [Track EA](../specs/track-ea/README.md) (evidence assurance)

Acceptance records the architecture and the qualification gates below; it does not
certify an isolation profile or mean that the planned witness commands are implemented.
The spike results in [Decision 2](#decision-2-isolation-profiles--how-the-witness-gets-its-own-trust-domain)
remain binding on the implementation.

## Context

ADR-0011 made the stored record tamper-evident and its gaps provable. It deliberately left
one question open, and an external reviewer put it precisely:

> "Your chain is only as strong as the intent at the start and the capture in the middle.
> If the agent writes its own evidence, it is signed in the end — but not proven."

That is correct for today's design. The agent (Claude Code, Codex, …) runs as the user's
UID with a shell. Everything the capture path can reach, the agent can reach:

| Component | Where it lives today | Can the agent touch it? |
|---|---|---|
| Journal (raw events) | `<git-dir>/minds/journal/`, 0600, same UID | yes — before the checkpoint |
| Session salt, epoch state | `<git-dir>/minds/evidence/state/` | yes |
| Seal signing key | `user.signingkey` (the human's ssh key / agent) | usually yes (ssh-agent) |
| Hook registration | `.claude/settings.json` in the repo | yes |
| Intent | extracted from the first prompt | not provably tied to an approved requirement |

Our own `DOES_NOT_PROVE` list already names the consequences: no integrity between append
and seal; no statement about who controls the keys; no proof that a session produced the
lines attributed to it; no statement about actors outside the hook boundary; no real time.

"Hash-chained and signed" therefore means today: **the record was not changed after
sealing.** It does not mean: **the record is what happened.** This ADR closes as much of
that distance as a client-side, air-gap-capable tool can close — and names the rest.

## Principles

1. **The trust root sits outside the agent's reach.** Evidence is written, chained and
   signed by a component the agent cannot write to, kill, or impersonate with a key.
2. **Corroboration over self-report.** A statement the agent makes about itself (a hook
   event, a claimed test result) counts as *reported*. It becomes stronger only when an
   independent source agrees: a second observer, the committed artifact, a re-execution.
3. **Assurance is recomputed, never stored.** Like `Verified` in ADR-0011, the assurance
   level is derived at verify time from facts (who signed, which scope, what corroborates).
   No byte in the store claims a level.
4. **The three axes stay separate.** Assurance is not a fourth verdict axis. It states *who
   observed*; integrity, coverage and interpretation keep their meaning and exit codes.

## Threat model

| Actor | Assumption |
|---|---|
| Agent | May be buggy, drifting, prompt-injected or adversarial. Has shell, file and network tools within its sandbox. Wants to make the record look better than reality. |
| Human operator | Trusted to declare and approve intent. **Not** trusted to be the only other editor — human edits after the session are expected and must be visible, not forbidden. |
| Host | Trusted at A2 not to be root-compromised. The agent does not have root. |
| CI (self-managed GitLab) | Trusted third party at A3, with protected variables the agent never sees. |
| Out of scope | Root compromise of the host; a manipulated agent harness binary; the model provider misreporting which model ran. |

## Assurance levels

| Level | Name | What it takes | Retires from `DOES_NOT_PROVE` |
|---|---|---|---|
| A0 | claimed | Import, heuristic lineage, legacy | — |
| A1 | observed | Today: harness hooks, same UID, chain at seal, optional user signature | (baseline) |
| A2 | witnessed | Witness in a separate trust domain, live chaining, witness key, second observer (file system), artifact reconciliation, signed intent | integrity between append and seal · who controls the keys · which lines the session produced · (partially) actors outside the hook boundary |
| A3 | reproduced & anchored | A2 + CI re-execution of decisive commands + CI countersignature on first sight of each seal | real time (upper bound) · reported test/benchmark results |

A session can mix levels (e.g. the witness was unreachable for 40 seconds). The level is
reported **per sealed range**; the session's overall level is the weakest range.

## Decision 1: The witness — one writer, outside the agent's trust domain

`minds witness` is a long-running process that is the **only writer** of raw evidence.
It runs in a trust domain the agent cannot write into (Decision 2).

- **Transport:** `minds hook` becomes a thin forwarder: it reads the harness payload from
  stdin and sends it as a length-prefixed, versioned frame over a Unix domain socket
  (`minds-witness-v1`). Timeout budget unchanged (fail-open, exit 0).
- **Fail-open stays:** if the witness is unreachable, the hook writes the local journal as
  today. Those ranges are sealed later with `scope=agent-hooks/v1` (A1), not
  `witness/v1`. The witness records its own start/stop (and whether the previous stop was clean) as chained events, so downtime is provable.
- **Live chaining:** because the witness serializes appends, the ADR-0011 fold is computed
  **incrementally on receipt**. The formula, hash domains and seal format stay identical
  (`minds/evidence/v1/*`) — an external verifier needs nothing new. The append→seal window
  of ADR-0011 decision 1 disappears for witnessed ranges.
- **Storage:** journal, salt and epoch state live in the witness's own directory (0700,
  owned by the witness identity), never under `<git-dir>`. Redaction is unchanged:
  `put` still accepts only `RedactedSession`; the witness produces it at checkpoint.
- **Seal:** scope `witness/v1`; signed by the **witness key** with ssh-sig namespace
  `minds-witness`. The key never leaves the witness domain.
- **Ledger:** the agent can still write `refs/minds/*` in the shared repository — including
  deleting a witnessed seal. The witness therefore keeps an append-only ledger of every seal
  id it produced (in its own home). `minds verify --witness-home <dir>` on the host, and the
  CI anchor (Decision 7), turn a removed witnessed seal into an integrity finding instead of
  a silent coverage gap.
- **Key roles by namespace** (all ssh-sig, air-gap capable, ADR-0008 unchanged):
  `minds-witness` (seals), `minds-intent` (human intent approval), `minds` (reviews,
  attributions, as today), `minds-anchor` (CI). `allowed_signers` restricts every principal
  to its namespaces via `namespaces="…"`, so a witness key cannot sign an approval and a
  human key cannot sign a witnessed seal.

## Decision 2: Isolation profiles — how the witness gets its own trust domain

`minds enable --witness <profile>` is planned to set up the profiles below, subject to
their qualification gates. The profile is a fact the verifier checks (via the signer
principal), not a claim in the seal. A profile name alone never grants A2.

| Profile | Agent runs | Witness runs | Agent is kept out by |
|---|---|---|---|
| `container` (intended default for regulated teams once qualified) | in a dev container, worktree bind-mounted | on the host | the container boundary; only the socket directory is mounted from witness state |
| `user` | as the developer's UID | as a dedicated OS user `minds-witness` | file ownership; socket is write-only for the developer group |
| `managed` (Claude Code; A1 only) | natively | as the developer's UID | candidate system-managed policy and sandbox/file-tool denials; exclusion has not been demonstrated |

**Container qualification (EA-S2).** The [container spike](../spikes/ea-s2-container-profile.md)
provides reusable templates and a repeatable experiment, not successful isolation or
watcher measurements. Use **inotify on native Linux** as the first qualification target;
the recorded Linux run is still outstanding. Keep **macOS container recording disabled**
until FSEvents measurements and an actual host Unix-socket round trip pass on an identified
Docker Desktop version and file-sharing backend. Both platforms' latency, burst-loss,
socket, isolation and hook results remain deferred or not run. Acceptance of this ADR
does not turn those missing measurements into A2 qualification.

The container mounts the worktree and `$MINDS_WITNESS_HOME/run`, never the host home,
journal, salt, keys, ledger, configuration or Docker socket. Mounting the socket's
directory allows socket recreation; its writable bind also lets the agent unlink the
socket (denial of service), without access to the evidence store. The profile assumes
the agent cannot control the host Docker daemon or add mounts. A visible socket inode
does not prove host reachability through Docker Desktop's Linux VM; no TCP relay or
VM-hosted witness is implicitly substituted. EA-10 must validate canonical paths and
keep witness state outside the workspace; linked Git worktrees need separate handling.

**Managed qualification (EA-S1).** The [managed spike](../spikes/ea-s1-managed-profile.md)
decides that `managed` **stays A1 on Linux and macOS**. Native Claude policy enforcement,
signal/debugger denial, managed-hook access and command/HTTP payload parity have not
been established. Same-UID control processes could read/write protected fixture files
and terminate a disposable child, but these were not Claude sandbox experiments.
The policy is a candidate, not an installed or qualified deployment template. Only a
complete native matrix for pinned OS/harness versions can reopen A2 qualification;
missing measurements are never successful denials (see [Addendum EA-S1](#addendum-ea-s1)).

In the `container` and `user` profiles the agent can still send fabricated frames into
the socket: it must accept hooks running as the agent. The candidate `managed` profile
instead aims to deny agent socket access while allowing managed hooks; that distinction
remains unverified. Socket access is not authentication of an event. Principle 2 handles
it: a fabricated write event without a matching file-system observation is
*uncorroborated*; a fabricated test result is *not reproduced*. The agent can add noise to
the record; it cannot remove or rewrite what the witness already chained, and it cannot
make a claim corroborated merely by submitting it. These guarantees require a qualified
isolation boundary; they are not claims about the current A1 managed profile.

## Decision 3: A second observer — the file system

The witness watches the worktree (inotify / FSEvents / ReadDirectoryChangesW via one
cross-platform abstraction) and records, per change, `(path, blake3(content), observed_at)`
as its own event class `fs.observed` in the same chain.

- Correlation happens at read time (derivation, recomputable): a hook write effect with
  content hash H at path P is **corroborated** if the file observer saw H at P within the
  same range.
- Changes the file observer sees without a hook event are recorded, not dropped: typically
  shell redirections, generators, formatters — or a human.
- Same privacy rules as ADR-0011: the secret-file wall suppresses hashes for `.env`,
  keys etc.; untracked private files outside the repo are never fingerprinted; ignored
  paths (`target/`, `node_modules/`) are skipped by `.gitignore`.

**As implemented (EA-08).** `fs.observed` events live in the witness's own stream
(`agent = "witness"`, the same stream as `witness.start`/`witness.stop`), with payload
`{"path","content":"b3-…"|null,"reason":null|"too_large"|"secret_file"|"deleted"|"outside_repo"|"redacted_content"|"unscannable"}`.

- **What is observed.** Paths are repo-relative. A hash is computed only over content the
  redaction pipeline would leave unchanged (`redacted_content` otherwise) and only over
  UTF-8 (`unscannable` otherwise) — the same dictionary-oracle rule as `Effect.written`.
  Secret files are never read. For a symlink the **link text** is fingerprinted (what Git
  commits); a link pointing out of the repository is `outside_repo`, and so is a file with
  a second hard link (it may be a host file). A deletion is recorded only for tracked paths
  or paths this run already observed.
- **What is ignored.** Only `.gitignore` and `.git/info/exclude` decide (no global or
  config-named exclude file); tracked files are never ignored. The rules are evaluated
  **in-process** from bytes the witness reads itself — the witness never runs `git`
  against the agent's worktree, because the agent controls its configuration and index (a
  lazy fetch via a promisor remote would run `core.sshCommand` on the host). An unreadable
  rule file drops the paths it governs.
- **How it reads.** The repository root is opened once as a directory descriptor; every
  access walks below it with `openat(O_NOFOLLOW)`/`fstatat`/`readlinkat`, never crosses a
  file-system boundary, and reads only regular files. The watcher does not follow
  symlinks; nested repositories (submodules) are not observed; new directories are
  searched (bounded), since inotify does not report files created before their watch.
- **Bounded work.** Paths, bytes and appends per event-loop step are capped, also on a
  checkpoint request; what does not fit goes into the next epoch. Lost notifications
  (queue overflow, a swapped root, unreadable ignore rules, a panic, watcher unavailable)
  are chained as `fs.gap` events.

Every witness checkpoint also seals the stream's current epoch: scope `witness-fs/v1`,
signed under `minds-witness`, outcome **`observations_stored`**. The seal format stays at
13 lines; for this outcome the `session=` line carries the **id of the observation
object** (`b3-` + blake3 of its RFC 8785 bytes), not a session id. Older binaries reject
the unknown outcome word rather than misread the id — the same accepted trade-off as
schema 2. The object `{"schema":2,"started_at","first_at","last_at","observations":[{"seq","at","path",
"content","reason"}]}` passes the redaction pipeline fail-closed (`RedactedObservations`,
no public constructor; an observation whose path the pipeline refuses is dropped, not the
whole epoch) and is stored under `refs/minds/observations/<64 hex>`, one ref per object,
carried by `minds sync`. `minds forget` does not apply to it: it holds only redacted paths
and hashes.

The reader accepts an observation object only through a `witness-fs/v1` seal whose
signature verifies under `minds-witness` (the agent can write `refs/minds/observations/`
and seals in a shared repository). `minds verify` takes the trust file only from
`--signers` or `~/.ssh/allowed_signers` — never from the repository's
`gpg.ssh.allowedSignersFile` — and accepts a principal only if every line naming it **or
carrying one of its keys** is restricted to exactly `namespaces="minds-witness"`.
Reconciliation uses observations only for linked sessions that a trusted `witness/v1`
seal names (the witness stamps their times), one window per session. It ends with the
checkpoint's observation epoch: the earliest **trusted** `witness-fs/v1` epoch ending at or
after the session's witnessed range. The witness seals that epoch before the sessions and
closes it with a monotonically stamped `fs.checkpoint` event, so work continued after the
commit lands in the next epoch. From that epoch the signed `previous` chain is followed back
to the first epoch ending before the window starts, or to the chain's start (`previous`
absent: the first checkpoint after a witness start) if that epoch began no later than the
window start; only observations from the epochs in between count. If a link is
missing (deleted), untrusted or a block seal (observations not stored), the session gets
**no** observations — the latest remaining reading would not be the latest. Untrusted seals
are ignored entirely. With a single trusted witness key, deleting or planting seals can
therefore throw a session back to the state without a witness, never turn a line into
`explained` (deleted witnessed seals become
visible through the witness ledger, EA-11/EA-12). Unwitnessed sessions open no window —
their timestamps can be set by the agent — and the gap between two linked sessions belongs
to no window. A session sealed at several checkpoints has one window up to its latest
witnessed range. An opaque latest observation (no hash)
does not erase an earlier contradicting one; otherwise the file is classified as without a
witness. Corroboration window: an observation of H at P within `[claim − 2 s,
claim + 30 s]`.

Known limits: a rewrite of identical content within one epoch is a single observation
(a later claim for it stays uncorroborated); in SHA-256 repositories tracked files that
match an ignore pattern are not observed; `fs.gap` events are chained but not yet surfaced
by `minds verify` (EA-11/EA-12); linked worktrees (`.git` is a file) are not observed.
`explained (fs only)` means the witness saw exactly the committed bytes in the window —
it says nothing about who wrote them (an agent can re-surface existing content with a
`touch` after an epoch boundary). The witness seals the observation epoch **before** the sessions of the same checkpoint;
if that epoch cannot be sealed, the sessions are deferred — a trusted `witness/v1` seal
exists only together with its checkpoint's epoch. The `fs.checkpoint` mark is stamped
monotonically (never before the last session event).

**As implemented (EA-08a).** The observation object (schema 2) carries `started_at`: the
stamp of the epoch's first event in the witness stream (`witness.start` for a run's first
epoch — the watcher is armed before it — otherwise the first event after the previous
seal). It is signed transitively through the object id on the seal's `session=` line; no
seal-format change. Schema-1 objects stay readable (`started_at` absent, never serialized)
but a schema-1 chain start never anchors — nor does an unknown future schema (the reader
anchors on exactly schema 2 until it learns the next one). The window starts
`WINDOW_SLACK` (30 s) before the session's first witnessed stamp, so the first commit of a
run gets observations only if the witness was started at least that long before the
session's first activity; a session starting sooner stays `reported only`. Every witness stamp (lifecycle, `fs.observed`,
`fs.gap`, `fs.checkpoint`, accepted hook events, the stream key of a run) comes from a
monotonic clock: `max(now, ⌊high-water⌋ms + 1 ms)` — one millisecond, so that every stamp is
also later **as RFC 3339 text**, which carries milliseconds. Bursts above 1000 events per
second run ahead of the wall clock briefly. The clock persists a reservation (an upper bound
of every stamp issued, raised to stamp + 1 s before the stamp is appended) in
`evidence/clock` of the witness home (0600, atomic, fsync) — not under `evidence/state/`,
where `clock` would be a valid agent directory. A missing file means first run (the mark is
raised to the newest journal event and the newest seal in the witness ledger); an
unreadable or corrupt one refuses to start (`corrupt witness clock state`), never a silent
reset. To repair it, do not delete it: write a value not before the current time and not
before the last sealed stamp. Stamps are journal times: an `fs.observed` stamp is never
before its classification but may trail it by the bounded queue latency. After a quick restart, stamps may run
up to one second ahead of the wall clock (the safe direction: a later start anchors less
often). A clock that once jumped far ahead keeps the witness there until wall time catches
up; an agent able to push more than 1000 accepted events per second can make it run ahead
the same way (consistent within the witness's time base, but heuristics comparing witness
stamps with commit times may lose matches — the safe direction). A window in which an observation
epoch ended as a block seal (observations not stored) yields no observations, since the
latest remaining reading would not be the latest. The chain rule assumes a single witness key per repository: with several
witnesses pushing to one remote, a deleted epoch of one can be bridged by another's (until
EA-10 pins the key). The
observer hashes up to 16 MiB, capture up to 32 MiB: a file the witness last read
smaller and then only saw as `too_large` stays contradicted by that earlier reading.
Seals do not name their repository: with several witnesses, a principal restricted to
`minds-witness` counts for every repository (pinning the witness key per repository is
EA-10). The default `~/.ssh/allowed_signers` may be writable by an agent running as the
developer's uid — gates such as `--require-explained` should pass `--signers` with a file
the agent cannot write. On Linux the recursive inotify watch also covers ignored
directories (`target/`, `node_modules/`); a large tree can exhaust `max_user_watches`, which
is recorded as `fs.gap` (`unavailable`). Untracked, non-ignored files in the worktree are
fingerprinted even if never committed, which is an offline guessing target for
low-entropy content.

## Decision 4: Artifact reconciliation — the commit must be explained by evidence

`minds verify <session> --commit <rev>` (and the default target `HEAD` when no argument
is given) reconciles the committed tree against the evidence:

For every file changed in the commit, the committed blob's blake3 is compared with the
**last corroborated content** at that path in the session's sealed ranges. Each changed file
is classified; where the recorded tool payloads allow the observed content to be
reconstructed (full writes, or edits replayed onto the pre-session blob, then checked
against the observed hash), each changed **line** is classified as well. File level is
always available, line level is best-effort and says so:

| Class | Meaning |
|---|---|
| `explained` | the committed content equals the last content the agent wrote, corroborated by the file observer |
| `explained (fs only)` | the file observer saw exactly this content, but no tool call claimed it (shell, generator) |
| `reported only` | a tool call claims it, no independent observation (A1 ranges) |
| `unexplained` | the committed content was never observed in the session — a later human edit, another tool, or something outside the boundary |

**Prerequisite found in the code:** today's write-effect hash is computed at checkpoint
time by reading the worktree (`adapter::hash_artifacts`). It fingerprints the file as it is
at commit time, not what the agent wrote — a human edit before the commit would be hashed as
the agent's write. Reconciliation therefore needs a write-time fingerprint: an additive
`Effect.written` hash derived from the tool payload itself (A1, EA-01a), and the witness's
`fs.observed` stream (A2). The existing `content` field keeps its meaning for the evidence DAG.

Reconciliation is a **coverage** statement ("does the evidence cover the artifact?"), never
integrity. `unexplained` is not an accusation; it is the honest answer to "which of these
lines did the recorded session produce?". It retires the `DOES_NOT_PROVE` line about the
mapping between sessions and lines for witnessed ranges.

## Decision 5: Intent binding — which requirement, approved by whom

A session starts with an **intent anchor**, the first event of the chain:

```text
minds-intent-v1
source=issue:<project>#<iid>@<updated_at> | file:<path>@<blob-sha> | prompt
content=b3-<64hex>          blake3 of the canonical intent text
scope=<glob>[,<glob>…] | -  optional: paths the work is expected to touch
```

- `minds intent bind --issue <id>` (via `minds-gitlab`) or `--file <path>` snapshots the
  requirement. The snapshot travels in the (redacted) session; the anchor binds its hash.
- `minds intent sign` signs the anchor with namespace `minds-intent`. Recommended key type:
  FIDO `sk-ssh-ed25519` with user presence — signing requires a physical touch, so the
  agent cannot sign even with full shell access. Other key types are allowed and reported
  as such.
- `verify` recomputes: the anchor signature, and — if GitLab is reachable — whether the
  issue version still exists in the issue's history. Writes outside `scope` become
  findings (`out of scope`), again on the coverage axis.

What stays unproven, stated in the artifact: that the intent was good, complete or
unambiguous. minds proves *which* intent was in force and *who* approved it.

## Decision 6: Reproduction in CI — reported results become checked results

`minds replay <session>` runs in CI on a clean checkout of the commit:

- It selects the **decisive commands**: tool calls the adapter classifies as test or
  benchmark runs whose results preceded a keep/discard decision (interpretation, versioned,
  deterministic — ADR-0011 decision 8).
- Tests: exit code and pass/fail counts must match. Benchmarks: the measured value must lie
  within a declared tolerance (default ±25 %, configurable per command pattern).
- The result is a signed **replay record** (namespace `minds-anchor`, stored under
  `refs/minds/anchors/`) — a derivation about evidence, never a change to it. A matching
  result lifts the event's status to `(Observed, Verified)` at read time; a mismatch is the
  finding `claim not reproduced`.

## Decision 7: First-sight anchor — an upper bound for time

The CI job countersigns every seal id it has not seen before (`minds anchor`), with the CI
key from a protected variable, and stores the countersignature under
`refs/minds/anchors/<seal_id>`; `minds-gitlab` mirrors it as an MR note. This proves "this
seal existed no later than pipeline #N" — backdating after that point is detectable. It is
not a transparency log and needs no network beyond the self-managed GitLab
(ADR-0011's rejection of global logs stands).

## Decision 8: Reporting and gates

`minds verify` prints a separate block below the verdict:

```text
  Verdict         VERIFIED
  Integrity       intact     (3 seals, 214 events, chain recomputed)
  Coverage        complete   (0 gaps · artifact 148/150 lines explained · 0 out of scope)
  Interpretation  2 calls uninterpreted
  Assurance       A2 witnessed   (witness@build-07, profile container; intent signed, sk key)
  Not proven      model identity · correctness of the decision · actions outside the boundary
```

- Exit codes stay the ADR-0011 CI contract. Assurance never changes them.
- New gates: `minds verify --require-assurance A2`, `minds fsck --require-assurance A2`,
  `--require-explained <percent>`.
- `PROVES` / `DOES_NOT_PROVE` become level-aware: each sentence carries the minimum level
  at which it holds, and every surface (bundle, TUI, HTML) prints the set that applies.

## What remains unproven even at A3

- That a specific model produced a specific output (needs provider attestation).
- That the agent's stated reasons are its actual reasons — reasoning text is *reported*.
- That the decision was technically right.
- Effects outside the observer boundary: network calls, other machines, processes the file
  observer cannot see (e.g. changes outside the worktree).
- Anything on a root-compromised host.

## Rejected alternatives

- **Signing inside the hook with a key the agent can reach** — self-attestation with extra
  steps; this is exactly the objection.
- **Trusting the transcript file** — written by the harness in the agent's domain, and a
  narrative, not an observation (ADR-0003).
- **eBPF / auditd process tracing** — Linux-only and privileged; a strong future
  observer for A3 on build hosts, not the default. Outlook.
- **TEE / remote attestation of the witness** — right direction for hosted fleets, too much
  operational weight for the target audience today. Outlook.
- **Blocking human edits after the session** — would make minds a gate on people rather
  than a record of agents; `unexplained` is the honest alternative.

## Consequences

- minds gains an operated component (the witness). The `container` profile is the intended
  default for teams using dev containers, subject to the Linux-first qualification and
  macOS recording gate in Decision 2.
- The file observer sees human edits in the worktree. It records only paths and hashes,
  no content, keystrokes or timing beyond the change event; attribution to a person is
  never made. This must be stated in the privacy overview and the §87 BetrVG material:
  the witness observes the *agent's* workspace and reports *unexplained changes*, not people.
- Hot path: one socket write instead of one file write; the budget is unchanged, verified
  by the existing hook latency test.
- The seal format and chain are unchanged, so every existing verifier recipe keeps working;
  only the scope value and the signing namespace are new.

## Addendum EA-S1

2026-10-02 — **The `managed` profile stays A1 on Linux and macOS.**
[EA-S1](../spikes/ea-s1-managed-profile.md) records the controls, candidate managed
settings, reproduction protocol and outstanding native measurements. Same-UID
0700/0600 permissions alone do not protect witness state; local control processes
could read/write it and terminate a disposable child. Those controls are not
Claude sandbox results. System-policy enforcement, agent signal/debugger denial,
hook access and command/HTTP payload parity have not been established on either
platform. Missing measurements must not be interpreted as denied operations.

Do not qualify `managed` ranges for A2 until the spike's complete native results
demonstrate agent exclusion and working managed hooks for the pinned OS/harness
versions. This is a conservative qualification decision, not a proof that all
managed configurations are incapable of isolation. EA-04 accepts this ADR with the A1
cap intact. No production behavior changes in EA-S1.
