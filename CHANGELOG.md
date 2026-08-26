# Changelog

All notable changes to Minds are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning follows [Semantic Versioning](https://semver.org/).

> **No 1.0 yet.** As long as the leading digit is `0`, there is no compatibility
> promise: any MINOR version (`0.1` → `0.2`) may break the CLI surface and the
> store layout. PATCH versions (`0.1.0` → `0.1.1`) contain fixes only.
>
> **`schema_version` in the stored objects (session, review) is a separate
> matter.** The binary's version versions the *surface*; the schema versions the
> *stored object* — and an object lives as long as the repo. The rule: a newer
> binary reads all older schema versions; the schema only increments on a
> breaking change to the payload, never for an additional field.

## [0.3.0] — 2026-08-26 — "A Gap Is a Link in the Chain"

*Until now, Minds proved what was stored. Now it also proves which scope it
was observing at the time — and that a gap is something other than silence.
A MINOR version, because the surface grows (`verify`, `sign --seal`,
`reinterpret`, evidence mode in the TUI) and schema 2 means a read break for
older binaries. Minds also leaves the Unix world for the first time: the
release includes a native Windows binary.*

### Added

- **The Evidence Chain** ([ADR-0011](docs/adr/0011-evidence-chain.md)) — Minds
  no longer proves just what was stored; it proves **which scope it
  observed**: on append, every journal event carries two stamped hashes
  (payload and observed facts, `blake3::derive_key` with domain separation);
  the checkpoint folds events **and gaps** into a chain and stores it as a
  **seal** under `refs/minds/evidence/<seal_id>` — content-addressed,
  epoch-chained (`previous=`), optionally ssh-signed (`user.signingkey`,
  retrofittable with `minds sign --seal`). A gap is a link in the chain, not
  silence; a seal survives `minds forget` as payload-free proof. When
  redaction rejects a session, an auditable trace exists for the first time:
  a **block seal** (`outcome=storage_policy_rejected_payload`) without
  intent, paths, or field names. `minds verify <session-id>` renders its
  verdict from the **integrity × coverage** matrix (exit codes: 0 VERIFIED,
  1 TAMPERED, 2 VERIFIED/INCOMPLETE, 3 NOT VERIFIABLE), `minds verify
  --evidence <seal-id>` checks sessionless seals, `minds fsck` recomputes
  journals still on disk (tampering = a finding) and gains
  `--require-seal` as a gate. `minds audit --export` (bundle schema 2)
  carries the seals byte-for-byte, `rejected_seals` included; the recipe
  for verifying without Minds is in the
  [verification guide](docs/verification-guide.md).
- **Observed does not mean interpreted:** tool calls carry `capture`
  (`interpreted`/`uninterpreted`, adapter including version). An agent
  without its own adapter no longer loses its tool level — the generic
  fallback receives name and raw arguments as redacted evidence. In
  `minds inspect`, such a call appears as `◐ OBSERVED`.
- **`minds inspect` shows evidence states:** a verdict column in the
  activity view (`◈ sealed · ! incomplete · ✗ TAMPERED · · unsealed`),
  seal verdict and coverage in the session header, withheld sessions as
  rows of their own, new gap kinds in the why chain (`SealedGap`,
  `UnsealedRange`, `PayloadRejected`) — together with the sentence that
  carries the system: *Missing evidence does not prove that nothing
  happened.* The pipe gains a verdict column (11 fields — **caution:**
  existing `awk`/`cut` consumers must account for the new column 8).

- **Three axes of trust instead of one status:** `minds verify` speaks to
  integrity ("was it altered?"), coverage ("do we know whether something is
  missing?"), and interpretation ("what does it mean?") separately — an
  unknown tool is an interpretation problem, not an integrity problem.
  Coverage is **always scoped**: the seal carries its observation boundary
  (`scope=agent-hooks/v1`), and "complete" means complete within that
  boundary, never "all system activity". Exit codes remain the CI contract
  of integrity × coverage.
- **`ToolAdapter` trait and `minds reinterpret`** — adapters sit above the
  chain (registry per agent, `adapter_version` from the implementation) and
  interpret deterministically: same evidence + same version ⇒ same
  interpretation, test-pinned. `minds reinterpret <session>` shows — strictly
  read-only — each call's evidence address, its stored interpretation, and
  the current one: interpretation is reconstructible, evidence immutable.
- **The evidence DAG as a projection:** read effects now carry content
  hashes as well (secret exception unchanged), and the reader projects
  content handovers from them — "B read exactly the bytes A wrote" — as the
  first producers of `(content_derived, verified)`: recomputed, not
  observed. In the session graph as `⇄ HANDOVER` nodes; none of it is
  stored.
- **`minds audit --export --mode proof`** — only the proof skeleton (ids,
  canonical payloads, seals including signatures, verdict metadata), no
  intent, no comments. `redacted` remains the default and the maximum; a
  `full` mode deliberately does not exist (the store holds only redacted
  data). `does_not_prove` now also names actors outside the hook
  boundary, the effects of uninterpreted tools, and the real wall-clock time.
- **Legacy is a state, not a `None`:** `Provenance::{Legacy, Chained}` in
  the read model; old sessions show `· legacy` instead of a blank and are
  never retroactively attributed a chain. The session header in the TUI
  shows epoch `k/n` and interpretation/handover counters. The eight chain
  invariants from ADR-0011 are pinned as named tests.

- **Evidence mode in `minds inspect` (`e`)** — a session's evidence report
  on three levels: the **verdict** (integrity, coverage, epochs, signature,
  interpretation, limits — one line per axis), the **explanation** beneath
  the focus (observation boundary with `✓ captured` vs. `— not captured,
  no gap`; epochs as a timeline with seal status and `previous` resolution;
  `○ NOT SIGNED` as a state of its own — unsigned ≠ invalid), and the
  **cryptography** (seal ids, roots, algorithm). The guiding sentence
  always states the boundary: *"Cryptographically verified within the
  recorded observation boundary."* Legacy sessions show their honest
  sentence instead of an empty skeleton. All of it is carried by the new
  **`EvidenceReport` read model** in `minds-reader` — the same verdict
  computation as `minds verify`, the TUI recomputes nothing;
  `proves`/`does_not_prove` are pulled into `minds-core::evidence` as
  canonical vocabulary (one source for audit bundle, TUI, docs).
- **Evidence edges in the why chain are focusable (#132)** — ↑/↓ first walk
  the edges of the EVIDENCE link, Enter on an edge jumps into the why chain
  of exactly that commit (Esc carries back), and the inspector explains the
  focused edge instead of all of them. The "Enter ↵" hint thus sits at the
  edge and once again promises a real action (#131).

- **Native Windows binary (x86_64):** the release additionally builds
  `minds-<version>-x86_64-pc-windows-msvc.zip` — same archive layout as the
  Unix targets, just as a zip with `minds.exe`. `install.sh` stays
  Unix-only; on Windows, installation means unpacking and putting
  `minds.exe` on the PATH (under WSL, the Linux route still applies). A new
  CI gate (`cargo check` on `windows-latest`) keeps the Windows build
  green going forward.

### Changed

- **Salt loss no longer heals:** if the session salt is missing or corrupted
  after an epoch has already been sealed, the checkpoint no longer creates
  a new salt — a regenerated salt would seal the same evidence under a
  second, diverging root (epoch fork) and break "same events ⇒ same seal".
  Instead, the loss itself is the finding: the session is visibly deferred
  (`hook.log`), the journal stays on disk, the epoch counts as no longer
  reproducible. Before the first sealing, salt creation remains unchanged.
  In addition, `proves`/`does_not_prove` in the audit bundle and the
  verification guide sharpen the proof model: seal identity and signature
  are externally checkable; the seal commits to chain root and coverage,
  the underlying chain is only reproducible with the local journal and
  session salt.
- **Schema 2 — evidence in two dimensions:** `Edge.evidence` (and the edges
  in `links.json`/store index) is now an `EvidenceMark` made of a
  **source** (`heuristic < human_declared < content_derived < observed`)
  and a **status** (`missing < unknown < partial < verified`). "Verified"
  strictly means *recomputed* — legacy values read leniently as status
  `unknown`: the absence of the check is not a pass. Glyphs carry the
  status as a modifier (`● ?` observed-unchecked, `● ✓` recomputed).
  **Break:** a schema-1 binary does not read schema-2 sessions (newer
  binaries read all older versions; existing data is not migrated).

## [0.2.0] — 2026-08-24 — "The Making, Made Visible"

*The first release with an interface. Until now, Minds answered the question
of why line by line — `why`, `show`, `recap`, each its own invocation. Now
there is the one place where the chain can be looked at: `minds inspect`. A
MINOR version, because the CLI surface grows (new command, new default
feature `tui`); the store layout and the schemas remain unchanged — a 0.1.3
repo reads without migration.*

### Added

- **`minds inspect`** — the making of a change, in the terminal. An
  activity list of sessions (time, intent, agent, evidence, verdict), a
  session's graph as a trace — intent → agent → READ/EDIT/EXEC → change →
  review — in three zoom levels with details under the cursor, and the why
  chain of a line or a commit with an inspector that explains every edge:
  trailer evidence or a recomputed conjecture (file intersection, time
  window). A conjecture never looks like evidence — glyph **and** word
  differ, not just the color. Strictly read-only; forgotten or broken
  sessions are degraded rows, not a crash. If stdout is not a terminal, the
  lines come tab-separated and without ANSI — `minds inspect retry | grep`
  shows what the screen shows. Its own crate `minds-tui` behind the Cargo
  feature `tui` (default); `--no-default-features` builds the CLI without
  it.
- **Gaps as a statement of their own.** `WhyChain::gaps()` in the reader
  names where the chain is not substantiated — no commit, no change-id, no
  context, only conjectured attribution, forgotten session, no verdict. The
  interface marks every link with ✓ or ⚠, shows the "N GAPS" block with
  reasoning, and explains the evidence already on focus: the evidence
  sentence says *why* ("reconstructed from file overlap and temporal
  proximity — no explicit provenance record"), not just "somehow
  uncertain". In the pipe, the gaps appear as `gap` lines.

### Changed

- `minds-reader` now carries the read model that the CLI and the interface
  share:
  `Inspection` (load once, then ask), cards, graph, why chain; the `Index`
  holds the evidence **per edge**, the change-id per commit, and degraded
  entries with a cause instead of a bare counter. `sanitize`/`sanitize_path`
  live in the reader so that every interface defuses untrusted text the same
  way. `minds-git` reads a commit's author time (`Repo::commit_time`).

### Known limitations

*The list under 0.1.3 continues to apply unchanged — it remains the handover
state. Nothing new is added: `minds inspect` is strictly read-only, works
only on stored, redacted data, and shares the known limits of `why`/`show`
(linked worktrees show the main tree's commit,
[#20](https://github.com/munichbughunter/minds/issues/20)).*

## [0.1.3] — 2026-08-22 — "Invisible, Even Under Load"

*The release after the first manual test run. Its findings share
one trait: none of them was a leak, but all of them were promises that only
held as long as someone remembered them — redaction at the source instead of
the sink, terminal hardening that existed only for the log, and a push that
waited on a second transport. Now the promises are structural: `hook.log`
and the display defuse **where the writing happens**, and the context
transport no longer hinges on the user's push.*

The price of that last point is below under "Known limitations": the context
arrives seconds after the push, no longer guaranteed with it.

### Changed

- **`git push` no longer waits on the context transport**
  ([#85](https://github.com/munichbughunter/minds/issues/85)). With minds
  refs due, the pre-push hook opened a second full transport before the
  user's push — measured at ~1.5 s against GitHub, almost entirely
  connection setup. Now the hook calls `minds sync --detach`: the planning
  stays in the foreground (local, ~0.02 s), the push is taken over by a
  detached process without a terminal. **The context thus arrives at the
  remote seconds after the push, no longer guaranteed with it** — whoever
  needs that runs `minds sync` by hand before pushing. The background
  process runs in its own session without a terminal; whatever cannot work
  that way — the SSH passphrase of a key without an agent, the touch of a
  security key — makes it fail; alongside the log entry, it leaves a
  marker: the next push then runs synchronously in the foreground again,
  where the error is visible and authentication can succeed. Without due
  refs, the cost stays at zero.

### Security

- **`minds show`/`minds why` defuse stored untrusted text before terminal
  output** ([#116](https://github.com/munichbughunter/minds/issues/116)).
  The render layer printed prompt, agent and model names, constraints, file
  paths, and above all the edge endpoints (`edges[].to`, verbatim from the
  other side's hook payload) raw — the redaction from #35 looks for
  secrets, not control characters, and so ANSI sequences, bidi and
  zero-width characters reached the reader's terminal unchanged. Now every
  untrusted value goes through the same hardening at the sink as `hook.log`
  (`text::sanitize`, paths via `sanitize_path`); which paths get defused
  and which deliberately do not is in the module docs of `render.rs`. The
  full prompt (`--full`) keeps its lines and is indented under the branch
  instead of tearing the tree apart.
- **`hook.log` redacts credentials at the sink, no longer only at the
  source** ([#92](https://github.com/munichbughunter/minds/issues/92)).
  Until now, only `minds sync` invoked URL redaction before an error text
  went to the log; the other write sites (`checkpoint`, `hook`, `brief`,
  `prepare-commit-msg`, the parse errors) relied on their text carrying no
  remote URL — a promise that only held as long as every future caller
  remembered it. Now every line runs through the same redaction before it
  reaches the file, and a test proves that for every source without the
  author's involvement. Redaction happens **before** truncation and always
  over the whole text, so that no halved token — and no PEM key without its
  `-----END` marker — slips past shape detection; a message beyond 256 Ki
  characters is therefore not cut midway but replaced wholesale with a
  marker.
- **Signing no longer drops predictable, world-readable files into /tmp**
  ([#26](https://github.com/munichbughunter/minds/issues/26)). During
  signing/verifying, payloads and signatures landed directly in `/tmp` with
  a predictable name (`minds-sign-<pid>-<nanos>`) and default permissions
  (0644) — world-readable on multi-user systems plus a symlink race, even
  though attestation payloads can contain intent text, exactly the data the
  redaction otherwise protects. Now everything is created in a private temp
  directory (0700, random name) with files in mode 0600 and `create_new`
  semantics. The availability check also no longer calls `ssh-keygen`
  without arguments (which started the interactive keygen mode) but checks
  non-interactively whether `-Y sign` is supported. The ssh-sig logic now
  lives as its own crate `minds-attest`, so that the CLI, `minds-gitlab`,
  and a future CI verifier share the same trust model instead of
  duplicating it.
- **The reconcile branch of `minds sync` no longer listens to server text**
  ([#71](https://github.com/munichbughunter/minds/issues/71)). Whether a
  failed push was a ref divergence — the only case in which `sync` fetches
  remote review states and merges them into the local store — was decided
  by a substring search in the mixed stdout+stderr of `git push`; a remote
  could force that branch with a "rejected" in any `remote:` line. Now the
  decision rests on the `--porcelain` structure of stdout: only a rejection
  determined by git itself in the local comparison (`[rejected]` with
  non-fast-forward/fetch first/stale info) counts as divergence; a
  `[remote rejected]` — whose reason comes verbatim from the server, say
  from a pre-receive hook — no longer does. stdout and stderr are no longer
  mixed; the error message comes from stderr, credentials are still
  removed.
- **The GDPR erasure of an already-pushed session ref now reaches the
  forge** ([#102](https://github.com/munichbughunter/minds/issues/102)).
  Since the tombstones became parentless (#14), an erased ref was no longer
  a fast-forward; `minds sync` (which never pushes with `--force`) let the
  forge keep the plaintext as the current, browsable ref tip — erased
  locally, visible remotely, with a success message. Now `forget` deletes
  the push bookkeeping (`refs/minds/remotes/*`) of the erased session refs
  instead of moving it to the tombstone, and `sync` transfers exactly these
  refs with a `+` refspec: only if the state to be pushed is demonstrably a
  tombstone on a session-exclusive ref (fail-closed, checked against the
  content) and the last-pushed state was not one — never plaintext over
  plaintext, every other ref stays strictly fast-forward and real
  divergence remains deferred. Verification includes the proof of
  parentlessness — a tombstone with history would otherwise travel with its
  content. The transfer is reported at push time and noted in `hook.log`;
  if the forge rejects the force-push (protected branch, server hook),
  **that** too is reported on every run until the erasure is through —
  instead of silently suggesting success. `forget` now takes the same lock
  as `sync`, so that a running push does not recreate the just-deleted
  tracking ref at the plaintext, and only promises the force-push for the
  places that actually get it. The `--force` promise in `agent-help`,
  `--help`, and the privacy overview is sharpened accordingly. Deliberately
  unchanged: the shared context ref of an existing repo is never
  force-pushed (it also carries the other sessions), and the store-ref
  tombstone keeps its `links.json` as it has since #14 (edges
  `commit → session`, no payload) — it travels along with the erasure
  push.
- **The backfill from `minds enable` writes to `hook.log`, no longer raw
  into an `import.log` next to it**
  ([#69](https://github.com/munichbughunter/minds/issues/69)). The
  background import appended stdout and stderr unchanged to a second file
  in the same directory — without the promises `hook.log` has had since #10
  and that `fsck` and `docs/for-testers.md` point to: control characters
  were passed through, the file grew without bound, and it sat there with
  umask permissions. Now the backfill is a hook path like `checkpoint`
  (`Source::Import`): its errors go into the same file defused, capped,
  rotated, and with 0600, `fsck` points to it, a panic leaves only its
  location (the process holds the raw transcripts in memory), and
  `import.log` is no longer created — an existing one from older
  states gets cleaned up by `enable`. Noticed along the way: a transcript
  without read permission was previously a mere *note*, like "no
  importer", and thus equally silent; now it is a finding and appears in the
  log. The happy path stays quiet — otherwise `fsck` would show a hint after
  every `enable` pointing at a file containing nothing fixable.

- **The raw-data journal keeps its permissions promise on every level**
  ([#49](https://github.com/munichbughunter/minds/issues/49)). The event
  files were 0600, but `create_dir_all` created the directories above them
  with umask permissions — other local users saw agent names and session
  identifiers. Now every journal level is created directly with 0700 (no
  umask window), every append heals existing journals along the way —
  test-backed — and the `.next` hint sits at 0600. After the `rename` of an
  event, the **directory** is synced too — without that, a power failure
  could make an event vanish even though the hook had reported success
  (cost trade-off in the code; filesystems without directory fsync remain
  functional). Two refinements from the reviews: hardening starts at
  `journal/`, not at `minds/` — otherwise, in a group-shared repo, the
  hardening would strip the second user of the lock and the error
  channel — and a
  journal level redirected via symlink is refused rather than written to or
  chmodded, the same invariant `hook.log` already defends.

- **Signable payloads can no longer be forged via free-text fields**
  ([#12](https://github.com/munichbughunter/minds/issues/12)). The
  line-based plaintexts over which `minds sign` and `review --sign` sign
  were built from unvalidated fields — a `reviewer` of
  `anna@example.org\ndecision=approve` produced a payload with two
  `decision=` lines: the human-readable promise was forgeable even though
  the hash binds correctly. Both payload functions are now fail-closed and
  reject not just line breaks (incl. NEL) but also the characters that hide
  or reinterpret text (bidi overrides, Unicode tags, zero-width, BOM — the
  same sentinel predicate as the log defusing; NFD names like `Müller` in
  decomposed form stay valid). The line count is test-pinned as an
  invariant. The error names the field and never quotes the value. Also
  from the reviews: `minds audit` visibly degrades an affected entry
  (`unsignable`) instead of aborting repo-wide, the `reviews` status line
  reports "signature not checkable" instead of "valid", and
  `gitlab webhook --write` rejects a network payload whose fields would not
  yield a signable payload — poisoned data never comes into existence in
  the first place.

### Added

- **Integration tests for the pilot-scope commands**
  (part of [#51](https://github.com/munichbughunter/minds/issues/51)).
  Deliberately not all twelve uncovered commands — exactly the paths the
  pilot partner cannot debug themselves: `prepare-commit-msg` through a
  real `git commit` (including amend, which must create neither a new nor a
  second change-id), `blame`/`recap`/`search` each with happy path and a
  named failure case, the `brief --hook` JSON envelope as a contract
  surface (schema **and** session content — Claude Code parses it
  silently), and `gitlab mirror` across the whole CLI route against a local
  HTTP stub with real `curl`: flags land in the right URL segment, the note
  in the body, the token as a header — and a missing token variable is
  called out by name. The rest of #51 (incl. `verify`, `gitlab webhook`,
  `distill`) remains open and documented in the issue.

- **Pilot guide and privacy overview**
  (`docs/pilot-guide.md`, `docs/privacy-overview.md`). The one page for the
  pilot partner's internal approval and the guide for the pilot scope —
  every load-bearing promise verified against the code, the known gaps
  named with issue numbers instead of hidden.

### Removed

- **The checked-in `site/` is out of the repository**
  ([#60](https://github.com/munichbughunter/minds/issues/60)). 58 generated
  HTML files — the default output of `minds render` — went stale against
  the code with every commit and contradicted the reader's own
  self-description: "rebuilt on every run, stateless". `site/` is now in
  the `.gitignore`;
  `minds render` still produces the output locally. The remaining stray
  files from the issue (`hello.txt`, `test.txt`, `retest_szenario_1.txt`,
  `test-szenario-3`) were already untracked and ignored.

### Fixed

- **Concurrent edge writers no longer lose each other's writes**
  ([#4](https://github.com/munichbughunter/minds/issues/4)). Two
  simultaneous checkpoints of the same session — the `post-commit` hook and
  one call by hand suffice — could silently lose a `commit → session`
  edge: `why`/`show` no longer found the session through that commit
  afterwards, even though both calls reported success. The fix went deeper
  than the issue: `GitStore::link` merged outside the CAS loop (the
  described lost update), but the test against real threads showed that
  even the compare-and-swap beneath it did not hold — gix (0.85) verifies
  the expected value of a ref transaction against a state read **before**
  the lock — two writers both got `Ok`. Now reading, merging, and writing
  share one observed commit (`update_blob_in_ref`), and the ref switch
  verifies under its own cross-process lock — staleness becomes `RefRaced`
  and a fresh attempt, never a silent overwrite. That protects the same
  path for `put` and `forget` as well. The retry limit rises from three to
  ten attempts — now that the check actually holds, lost races are the
  normal case under load. Three findings from the reviews in the same
  commit: the lock lives in the **shared** git directory (`common_dir`) so
  linked worktrees take the same one — otherwise the serialization would be
  ineffective in exactly the topology it is needed for; a lock file left
  behind after a hard process end names its path plus the remedy in the
  error message; and an unreadable `links.json` is no longer silently
  replaced by a fresh list on write, but fails with a name — reading stays
  tolerant.

- **`gitlab mirror` sends the body again — header and body separated**
  ([#7](https://github.com/munichbughunter/minds/issues/7)). Token header
  (`--header @-`) and JSON body (`--data-binary @-`) shared the same
  stdin — but curl reads `-H @-` until EOF: the POST went out with an empty
  body (GitLab: "body is missing"), mirroring simply did not work, and the
  note content traveled over the wire as a broken HTTP header. The body now
  goes through a short-lived temp file (0600 on Unix), stdin belongs to the
  token alone — which still never appears in the argument list and never
  touches the disk. On top of that, the error message now quotes the server
  response (`--fail-with-body` puts GitLab's `message` on stdout, trimmed
  to 500 characters) — before, the actual cause, say "404 Project Not
  Found", stayed invisible. Four new tests drive real `curl` against a
  local HTTP stub and cover the path for the first time, among them the
  invariant "the token appears in no error message".

### Known limitations

*The handover-state list. It supersedes the older lists under v0.1.1 and
v0.1.0 — those stand as history; only this one reads as "applies
today".*

- **Linked git worktrees:** capture and `fsck` are correct there, but
  `minds show` and `minds why` show the main tree's commit
  ([#20](https://github.com/munichbughunter/minds/issues/20)).
- **No native Windows binary** — the supported route is WSL.
- **Tool level complete only for Claude Code.** Other agents (Codex,
  Cursor, Gemini, opencode): the prompt is captured, the tool and file
  levels are not yet interpreted.
- **The review layer needs two people on one repo** — solo, capture,
  `why`, and `recall` remain testable; reviews do not.
- **Two edge cases remain around `forget`** — since 0.1.3 the erasure also
  reaches already-pushed refs (#102, above under Security); still open are
  the collision edge case of the browse branch
  ([#100](https://github.com/munichbughunter/minds/issues/100), direction:
  over-erase, no leak) and the raw-data window in the journal
  ([#49](https://github.com/munichbughunter/minds/issues/49)) — details in
  the [privacy overview](docs/privacy-overview.md).
- **`minds import` uses the built-in default policy**, not the repo's own
  `.minds/redact.json` — project-specific denylists do not apply during
  backfill.
- **The context arrives at the remote seconds after the push, no longer
  guaranteed with it**
  ([#85](https://github.com/munichbughunter/minds/issues/85)) — the
  transport has run in the background since 0.1.3. A pipeline that
  immediately follows the push can narrowly miss fresh reviews; whoever
  needs the guarantee runs `minds sync` by hand before pushing.
- **`minds gitlab webhook` has no token verification**
  ([#8](https://github.com/munichbughunter/minds/issues/8)) — it ships as
  a local command (default: dry run), but do not use it; the CI gate
  (`fsck --require-review`) is not yet recommended as a pipeline gate.
- **No self-update** — version changes go through `install.sh` with
  `MINDS_VERSION`.

## [0.1.2] — 2026-08-12 — "The Wall Holds — Front and Back"

*The release on which approval turns — not enthusiasm. Redaction is the
one promise where a failure is not annoying but harmful; and whatever slips
through must remain removable. Hence two halves: **at the front** the wall —
no secret reaches the store, on neither of the two entry paths — **at the
back** the erasure — `minds forget` keeps what the first README page
promises, and lets reinjection keep working afterwards.*

Almost every fix here grew its own follow-up findings in code or security
review — three times they were regressions the fix itself would have
introduced. Where it counted, the numbers were measured against the built
binary, not just the test suite.

### Security

**At the front — redaction:**

- **No more panic on multibyte characters in the value**
  ([#1](https://github.com/munichbughunter/minds/issues/1)).
  `PASSWORD=hunter€2` crashed in the Windows path detection because it was
  byte-indexed in the middle of a UTF-8 character. The check now works on
  char boundaries.

- **`curl -u user:pass` gets redacted**
  ([#2](https://github.com/munichbughunter/minds/issues/2)). New short-flag
  detector for authentication flags, in both forms (`-u user:pass` and
  `-uuser:pass`), as its own switch `short_flags` in the policy — on by
  default.

- **JSON-escaped secrets no longer leak partially**
  ([#3](https://github.com/munichbughunter/minds/issues/3)). Tool arguments
  always sit JSON-serialized in the envelope — exactly the input class the
  patterns did not cover: an escaped quote in the value left `ter2` of
  `hun\"ter2` behind, and a PEM with a literal `\n` did not match at all.
  The reviews found four follow-up findings of the same class; three of
  them the fix itself would have introduced — among other things, the path
  exception flipped from a partial to a total leak.

- **The token rules now know the most likely shapes**
  ([#33](https://github.com/munichbughunter/minds/issues/33)). Anthropic
  (`sk-ant-`), OpenAI (`sk-proj-`, `sk-svcacct-`, `sk-admin-`), SendGrid,
  and the **GitLab family** (`glcbt-`, `glptt-`, `glft-`, `glimt-`,
  `glagent-`, `glsoat-`) — the latter was almost entirely missing, in a
  product that targets GitLab. Two measured findings along the way: the
  non-overlapping prefilter search let the Anthropic key of all things slip
  through completely (`sk-` won against `sk-ant-`), and the length caps sat
  below reality, so token tails survived. Against false positives, the new
  rules demand structure (type section, digit section, word start) — prose
  *about* keys stays readable.

- **Tokens in URL queries no longer reach `hook.log`**
  ([#73](https://github.com/munichbughunter/minds/issues/73)). The form
  documented at GitLab, `?private_token=…`, has no `@` and went verbatim
  into a file that is never deleted. The diagnostics sink now applies the
  redaction policy to each `name=value` pair individually — plus the
  shape-based token detector over the whole text — so that host and error
  cause stay readable. `token` falls into the strict tier in this sink, so
  that prefix-less tokens (self-hosted GitLab before 16.x) are caught too.

- **The secretfile wall knows the common credentials files**
  ([#34](https://github.com/munichbughunter/minds/issues/34)).
  GCP service accounts, `credentials.json`, FIDO SSH keys, `htpasswd`,
  `.netrc.gpg`, the directories `gcloud` and `/etc/wireguard/` — and files
  for which the wall is the **only** layer because no detector can catch
  their contents: Ansible vault password files, `.dockercfg`,
  `rclone.conf`, `.s3cfg`, `.boto`. The most severe finding came from the
  review and concerned the fix itself: the segment-boundary check let
  decorated variants (`credentials.bak`, `config-prod`) fall through —
  byte-identical credentials. The rule is inverted: the remainder behind
  the pattern only disqualifies if it turns the file into something else.

- **Redaction now truly checks every text field of the envelope**
  ([#35](https://github.com/munichbughunter/minds/issues/35)).
  Until now, the timestamps (`turns[].at`,
  `lineage.started_at`, `lineage.ended_at`), the identifier
  `lineage.local_id`, and the endpoints of the provenance edges were
  exempt — on the grounds that nothing could be there. On the hook path
  that was true; on **import** these values come from an external
  transcript file, and the
  endpoint of an edge comes directly from the other side's payload.

  Visible consequence: in rare cases `[redacted:…]` can now stand where a
  value stood before, and the denominator in the redaction message grows.
  Where something is found there for the first time, the envelope changes
  and with it the `SessionId` — a repeated import of the same session then
  stores it under a second identifier.

- **The wall applies on both entry paths**
  ([#93](https://github.com/munichbughunter/minds/issues/93)).
  `minds import` built the tool arguments directly from the transcript — a
  `Write` to a credentials file carried the full content in `input`, and it
  sat verbatim in the store. Check, heuristic, and replacement form now
  live in exactly one place (`secretwall`), with a byte-identical envelope
  form on the hook and import paths. Three additional findings in the same
  commit: the hook path had always lost the marker and the omission reason
  in the envelope; the pipeline redacted its own omission reason (`secret`
  in the field name); doubly serialized input slipped past the wall while
  the verbatim copy carried the content through. `minds import` now reports
  how many tool calls were omitted behind the wall.

- **A corpus of realistic envelopes and two property tests guard the
  regression boundary**
  ([#36](https://github.com/munichbughunter/minds/issues/36)).
  30,000 deterministic inputs each, without a new dependency: no panic on
  arbitrary UTF-8, an injected secret never survives, and
  `redact(redact(x)) == redact(x)`. It was the idempotence invariant, of all
  things, that found a bug 1037 existing tests did not see: a JSON-serialized
  `.env` content flipped the category between two runs (`secret` → `pii`),
  `redact_session` rejected the session as `Unstable` — a silent capture
  outage. An already-redacted placeholder is no longer hit a second time.

**At the back — erasure:**

- **`forget` also erases the session branch**
  ([#5](https://github.com/munichbughunter/minds/issues/5)). The browsable
  branch (`refs/minds/sessions/<hex>`) carries `session.json` **and** a
  rendered `session.md` — and was left in place on deletion: "forgotten"
  reported, plaintext still on the forge, readable by anyone with repo
  access. `forget` now checks and erases all three places (store ref,
  session branch, context tree), replaces the branch tree completely, and
  names every erased place in its output.

- **A repeated `put` no longer revives a forgotten session**
  ([#6](https://github.com/munichbughunter/minds/issues/6)). A capture on a
  second machine or an import overwrote the tombstone with plaintext — a
  GDPR erasure with a success message that did not hold. The store ref is
  now written with an atomic guard (a `forget` in the window wins), the
  session branch checked in three stages; import and checkpoint skip
  forgotten ones without failing.

- **The tombstone is a parentless root commit**
  ([#14](https://github.com/munichbughunter/minds/issues/14)). Before, the
  plaintext remained reachable via `<ref>~1` and traveled to all
  clones on every sync. Now **no** ref reaches it anymore, and it is
  definitively gone after `git gc`; the push bookkeeping
  (`refs/minds/remotes/*`) is also detached from the plaintext — otherwise
  it would keep it gc-immune. If the multi-place erasure aborts,
  `ForgetIncomplete` names the erased and the still-open places — a repeated
  `forget` completes idempotently. A ref already **pushed** to the forge
  still needs a force-push
  ([#102](https://github.com/munichbughunter/minds/issues/102)).

### Fixed

- **Reinjection survives erased and broken sessions**
  ([#83](https://github.com/munichbughunter/minds/issues/83)). A single
  forgotten session aborted `brief`, `distill`, and `recall` permanently —
  whoever used the GDPR erasure lost context reinjection entirely, and the
  SessionStart hook failed at every session start. Now the degrade contract
  of `show`/`why` applies here too: skips are counted instead of aborting,
  every affected command names the number before its output
  (`minds brief: 1 forgotten session skipped`; only broken ones point to
  `minds fsck`), and `brief --hook` writes the hint to the hook.log instead
  of into the session.

## [0.1.1] — 2026-08-10 — "The Hook Actually Fires"

*Eleven repairs to the one promise that carries all the others: that a
commit gets captured. No new functionality — and still the release without
which every feature would have stayed invisible.*

The common thread is **silent falsehood**. Almost every bug here reported
success and did nothing: `enable` wrote hooks into a directory Git never
reads from; the hooks could not find `minds` and kept quiet; a typo switched
off the CI gate and left the pipeline green; a checked-in third-party entry
prevented registration without anyone seeing it. What connects these cases
is not their cause but their construction — they break nothing, they just
stop working.

`minds fsck` is therefore the command that grew the most in this release: it
now names every one of these states.

### Fixed

- The agent registrations have gained a **source of truth**, and detection
  reads two words instead of a substring. Two failure classes traced back
  to it. First: a checked-in entry that merely mentions `minds hook` in
  its text — `echo "minds hook is nice"` — counted as a registration; the
  real capture hook was never installed — silently, for every
  colleague who cloned the repo
  ([#78](https://github.com/munichbughunter/minds/issues/78)). Second: a
  changed invocation **never reached existing installations**, because
  every existing registration passed as "already there"
  ([#68](https://github.com/munichbughunter/minds/issues/68)). Both live at
  the same spot in the code, and a halfway fix would have been worse than
  none: an exact comparison without a reliable ownership test would have
  overwritten configuration minds does not own.

  Now the rule is: the first word must end in `minds` — bare or as a path —
  and the second must be exactly `hook` or `brief`; for the recall entry
  additionally `--hook`, because `minds brief docs/ > brief.md` is a
  legitimate SessionStart hook the user wrote — it belongs to them. What
  gets compared is the **argument part**: a hand-pinned path stays — `minds`
  never wrote one there, and for the agent registrations it is the only
  remedy against the PATH blindness from
  [#25](https://github.com/munichbughunter/minds/issues/25). An entry minds
  owns but with old wording is corrected **in place** (order, `matcher`,
  and the user's extra keys remain), entries it does not own stay
  untouched, and the replacement is reported — even without `-v`, because
  someone may have changed that line by hand. An existing recall entry is maintained even
  **without** `--recall`: the switch governs creation, not maintenance —
  otherwise an `fsck` hint would remain that no `minds enable` fixes.
- An **outdated OpenCode plugin of minds' own** gets updated again. It
  carries the mark behind `//`, but the comparison ran against the shell
  variant with `#` — the test was thus *always* wrong: the plugin counted
  as someone else's file and stayed stale forever.
  ([#68](https://github.com/munichbughunter/minds/issues/68))
- `minds enable` now says when it **could not register anything** at a
  spot because something it does not own is there: a `hooks` that is not an
  object, an event that is not an array, a `minds.ts` it did not write.
  Until now these cases passed as "unchanged" — a reassurance that was not
  true, because the agent then does not journal. And a broken event no longer drags the
  other six down with it.
  ([#68](https://github.com/munichbughunter/minds/issues/68),
  [#78](https://github.com/munichbughunter/minds/issues/78))

- **`minds brief --hook` no longer loses its errors.** The SessionStart
  hook registered by `minds enable --recall` reads
  `minds brief --hook 2>/dev/null || true`: stderr went nowhere and the exit
  code was swallowed. If `brief` failed, the session started **without**
  the context minds wanted to hand it — the same silent outage as
  [#10](https://github.com/munichbughunter/minds/issues/10), only on the
  read path instead of the capture path. The errors now go to
  `<git-dir>/minds/hook.log`, and a panic does too: since
  [#54](https://github.com/munichbughunter/minds/issues/54) the process
  silenced the panic handler but had nothing in place to catch the panic —
  it was suppressed *and* recorded nowhere. This path gives the log only the
  **location**, not the message: `brief` holds redacted sessions in memory.
  Without `--hook` everything stays as it was — a human is at the terminal,
  and the error belongs on stderr.
  ([#68](https://github.com/munichbughunter/minds/issues/68))

- `minds enable` works in **linked worktrees** (`git worktree add`). There,
  `.git` is a file with a `gitdir:` reference; it was not resolved, and
  `enable` reported "no git repository found" — inside what was plainly a
  repository, with a message that was factually wrong. Agents increasingly
  work in worktrees. The hooks land in the **shared** git directory, where Git
  executes them for all working trees; `enable` says so, too, because
  nobody reads that out of a path. A commit in the worktree thus produces a
  checkpoint, and `minds fsck` reports it as healthy there. The same
  resolution incidentally makes `minds enable` usable in **submodules** —
  there, too, `.git` is a file; the hooks land in the submodule's
  `.git/modules/<name>`, the super-repo stays untouched.
  ([#21](https://github.com/munichbughunter/minds/issues/21))

  > *What this does not yet cover:* `minds show` and `minds why` show the
  > **main tree's** commit in a worktree, because the root there is
  > determined via `<git-dir>/..` and in a worktree that yields
  > `…/.git/worktrees`. Capture and checking are correct, the lookup is not
  > yet — the path there is
  > [#20](https://github.com/munichbughunter/minds/issues/20), which unifies
  > the same computation across eleven call sites.

- A **panic in `minds hook`** no longer writes anything to stderr.
  `catch_unwind` did catch it, but too late: Rust's default handler had
  already printed `thread 'main' panicked at …` plus the backtrace hint —
  and the hook's stderr belongs to the agent — Claude Code hands it back to
  the model. A Rust backtrace in the middle of the user's session is
  exactly the noise the hook is supposed to avoid. Now the handler is
  silent, and the **location** of the panic (`hook.rs:99:9`) goes to
  `<git-dir>/minds/hook.log`, where diagnostics belong — the location
  alone, not the panic message: it could embed payload, and `hook.log` is
  the file that ends up in a bug report. The cold path
  (`checkpoint`, `sync`, `prepare-commit-msg`) gains the same but keeps the
  message — no recorded data sits in memory there; and whoever calls one of
  these commands in the **terminal** still sees their panic.
  ([#54](https://github.com/munichbughunter/minds/issues/54))
- An argument that is **not UTF-8** no longer crashes `minds`.
  `std::env::args()` panics on it — in the first line, before any
  precaution of minds' own, with a backtrace on stderr and exit 101. For
  `minds hook`
  that was the worst conceivable place: the agent registration calls it
  without `2>/dev/null`. Such arguments are now converted lossily and run
  into the ordinary "unknown flag" message.
  ([#54](https://github.com/munichbughunter/minds/issues/54))

- A git hook that has lost its **execute bit** is repaired again by `minds
  enable` — and named by `minds fsck`. Git skips a non-executable hook file
  **silently**: no error, no message, just no checkpoint. The bit gets lost
  in ordinary ways (a `git archive`/tarball, a copy across a filesystem
  without mode bits, an overly broad `chmod -R`), and until now the hook
  stayed dead forever afterwards: `enable` returned on text-identical
  content before it even opened the file and reported "unchanged"; `fsck`
  compared only the block text and reported "installed". Both now actually
  look. The
  repair is **reported**, even without `-v`: `chmod -x` is also the way to
  deliberately silence a hook, and revoking that decision wordlessly would
  be a surprise. If the filesystem knows no execute bits at all (CIFS,
  exFAT), `enable` aborts with that reason instead of reporting the same
  repair on every run.
  ([#52](https://github.com/munichbughunter/minds/issues/52))
- `minds enable` no longer appends its shell lines to a hook with a
  **non-shell interpreter**. To a file with `#!/usr/bin/env python3` the
  minds block was previously simply appended — the hook threw syntax
  errors from then on, and the `|| true` in the block catches nothing in
  Python. Now `enable` aborts with a reason and names the interpreter
  instead of damaging a hook it does not own; Bourne relatives (`sh`, `bash`,
  `dash`, `ksh`, `zsh` …), the wrappers `env` and `busybox`, and files
  without a shebang continue to be amended. The check runs **before** the
  first change: an unowned hook at one of the three places no longer aborts
  the run midway, with agent configuration on disk and no store config. And
  `minds fsck` reports the same file as rejected — including the reason —
  instead of saying "installed" or advising a `minds enable` that would be
  guaranteed to abort.
  ([#52](https://github.com/munichbughunter/minds/issues/52))
- The Codex switch `codex_hooks = true` is set with an **exact match**. The
  matching ran on a prefix comparison and thus also hit an unrelated key like
  `codex_hooks_timeout = 30` — whose line was *replaced* by
  `codex_hooks = true`: user configuration destroyed, and the actual
  switch still missing. In addition, the switch now counts as what it is —
  top-level: a `codex_hooks` line under `[profiles.test]` belongs to a
  different table and stays untouched, and a missing switch is inserted
  **before** the first table instead of at the end of the file, where it
  would have landed inside the last table. And where the line logic reaches
  its limit — multi-line values, arrays across several lines — there is no
  guessing: `enable` says the switch needs setting by hand instead of
  possibly writing it into a literal.
  ([#52](https://github.com/munichbughunter/minds/issues/52))

- `minds enable` no longer writes into a hook directory **outside
  the repo** without asking — and a checked-in symlink can no longer redirect
  it there unnoticed. The decision is made on the **resolved location**, not
  on how the path is spelled: whether `core.hooksPath` points outside
  directly (set globally, `init.templateDir`) or a symlink in the working copy
  (`.husky` → elsewhere, also with a trailing slash, path alias, or a link
  in an ancestor) — if the target lies outside the working copy and the git
  directory, `enable` asks, because hooks there apply to **all**
  repositories that use the directory. Interactively as a prompt (default:
  no), in scripts via `--global-hooks`; without consent, `enable` aborts
  **before** anything is written. A symlinked `.git` and a shared
  `.git/hooks`, by contrast, stay prompt-free — a checkout cannot place
  anything there. `minds fsck` now also names a directory outside the
  repo. ([#66](https://github.com/munichbughunter/minds/issues/66),
  [#64](https://github.com/munichbughunter/minds/issues/64))
- The argument parser has become **strict**. An unknown flag used to be
  noise: `minds fsck --require-reviews` (typo) ran through as a bare `fsck`
  and returned exit 0 — the CI policy gate was thus silently switched off,
  the pipeline green. And a value flag blindly took the next argument:
  `minds review I… --summary --sign` created the review with the summary
  "--sign" — **unsigned**, with a success message. Now every subcommand
  aborts on a flag it does not know and names the known ones; a value flag
  followed by another flag is an error instead of a mix-up. Positional
  arguments and flags are order-independent — `minds verify --sig s.sig
  b3-…` finds the subject, not the signature file. `--help` now also works
  behind a subcommand. The exception remains `minds hook`: a recorder does
  not abort over a configuration error — the error goes to
  `<git-dir>/minds/hook.log`, the run continues with what is usable.
  ([#11](https://github.com/munichbughunter/minds/issues/11))
- `minds agent-help` now names **all** public commands. Eight were missing
  (`hook`, `checkpoint`, `blame`, `metrics`, `forget`, `sign`, `verify`,
  `gitlab`) — for a map whose sole purpose is completeness. A test now
  compares it against the parser's command table; whoever adds a command
  without maintaining the map goes red instead of silently incomplete.
  ([#11](https://github.com/munichbughunter/minds/issues/11))
- The git hooks written by `minds enable` no longer depend on the `PATH`.
  GUI clients (VS Code, Fork, Tower) and minimal CI shells start Git
  without the shell's profile — `~/.local/bin` is missing there, the call
  `minds …` went nowhere, and `|| true` turned that into a **silent total
  outage**: committing worked, nothing was captured, permanently and
  without a hint. `enable` now remembers the binary's location in the local
  `.git/config` (`minds.binary`); the hooks resolve it first and only then
  search the `PATH`. The remembered location **wins** — which means an
  outdated global `minds` can no longer shadow the hooks either; the
  version whose `enable` wrote the hooks is the one that runs. If the
  binary moves, the PATH search takes over again, and `minds fsck` says
  that a `minds enable` renews the entry. The hook text itself still
  contains **no** absolute path: since the hook file can live in the
  working copy, a home path in it would be versioned — checked in for
  everyone, broken on every other machine. Here too: existing installations
  need `minds enable` once.
  ([#25](https://github.com/munichbughunter/minds/issues/25))
- Errors from the **hook path** no longer disappear. `checkpoint`,
  `prepare-commit-msg`, and `sync` run from git hooks that throw their
  output away — their errors were thus invisible, even though the docs
  promised a log. The most expensive case: a typo in `.minds/redact.json`
  aborts `checkpoint` *fail-closed*, from then on no session is ever
  checked in again, the journal grows, and nowhere does it say why. Now all
  four hook paths write their errors to `<git-dir>/minds/hook.log` — where
  `minds hook` has always logged.
  ([#10](https://github.com/munichbughunter/minds/issues/10))
- The `pre-push` hook no longer dumps its error messages raw into the push
  output. Alone among the three hooks, it did not redirect stderr;
  an unreachable remote thus wrote five lines between the output of
  `git push` on **every** push, for an operation the user never initiated.
  The message now goes to the log; stdout stays untouched — that is where
  the success message lives.
  ([#10](https://github.com/munichbughunter/minds/issues/10))
- A **panic** in `checkpoint`, `prepare-commit-msg`, or `sync` lands in the
  log instead of vanishing with the discarded stderr. `minds hook` already
  had this safety net; the cold path lacked it, and with the stderr
  redirection a crash there would have been completely silent.
  ([#10](https://github.com/munichbughunter/minds/issues/10))

  > **Existing installations need `minds enable` once.** The body of a hook
  > lives in the hook file, not in the binary — an update alone does not
  > replace it. `minds fsck` now says on its own when a block comes from an
  > older version, instead of letting it pass as "installed".
- `minds enable` now installs the git hooks in the **effective** hook
  directory. Repos with `core.hooksPath` (husky, lefthook, pre-commit,
  global hooks via `init.templateDir`) previously had the hook written to
  `.git/hooks` — a directory Git never reads there. `enable` reported
  success, and yet no commit produced a checkpoint. If the directory is
  relocated, `enable` says so even without `-v` — and says it differently
  when it lies *outside* the repo, because an `enable` then affects all of
  the user's repositories.
  ([#9](https://github.com/munichbughunter/minds/issues/9))
- `minds enable` aborts instead of writing to an unusable place — and it
  does so **before** the first file is created, so no
  half-configured repo is left behind. The affected cases: a set-but-empty
  `core.hooksPath` (Git then runs no hooks at all and does not report it),
  a value that resolves to the working-copy root (the hooks would sit as
  executable files among the source code), and a directory that cannot be
  written to. The message names the path and what to do in each case.
  ([#9](https://github.com/munichbughunter/minds/issues/9))

### Security

- `enable` no longer writes the **agent configurations**
  (`.claude/settings.json`, `.cursor/hooks.json`, `.gemini/settings.json`,
  `.codex/hooks.json`, `.codex/config.toml`, `.opencode/plugin/minds.ts`)
  through a symlink. These files live in the versioned working copy — a
  merged PR could place a link in their stead, visible in the diff only as
  a mode change to `120000`, and `enable` then overwrote the link's target
  completely. Now **every directory between repo root and file** is
  checked, not just the file itself: `.claude` as a link to `~/.claude` was
  the more effective attack — the leaf below it is a regular file, and
  `enable` would have written into the user's *global* configuration.
  Special files are rejected as well (a FIFO left `enable` hanging
  indefinitely), as are files beyond any plausible configuration size.
  Writing goes
  through the same route as for the hooks — sibling file with `create_new`,
  then `rename` — and the check runs **before** the first change: a link on
  the third configuration no longer aborts after the first two are written.
  ([#65](https://github.com/munichbughunter/minds/issues/65))
- An existing agent configuration **keeps its file permissions**, and a
  write-protected one is no longer replaced. Replacing via `rename` swaps
  the inode and with it the permissions: a `settings.json` with `0600` —
  because an API key sits in it — would otherwise have come back as `0644`,
  readable by every local account on a multi-user or CI machine.
  ([#65](https://github.com/munichbughunter/minds/issues/65))
- Untrusted text that `minds` prints or logs — paths from the working copy,
  `core.hooksPath`, the wording of errors from other tools — is now fully
  defused.
  Until now `char::is_control` governed this, and that is only the Unicode
  category `Cc`: **U+2028** (LINE SEPARATOR) and **U+2029** (PARAGRAPH
  SEPARATOR) fell through. Rust's `str::lines` does not break on them,
  browsers and Python's `splitlines()` do — in the job log of a GitLab
  pipeline a line could thus be forged, say an `fsck: all good`. The
  invisible format characters (`Cf`) slipped through as well, among them
  the Unicode tags `U+E0020`–`U+E007F`. Instead of a hand-maintained
  list, `char::escape_debug` itself is now consulted — it covers `Cc`,
  `Cf`, `Zl`, `Zp`, and `Zs`, supplemented by the invisible variation
  selectors (`U+E0100`–`U+E01EF` & co.) and the typographic quotation
  marks `fsck` brackets its paths in. A path can thus neither open a line,
  nor close the bracket, nor carry invisible text.
  ([#10](https://github.com/munichbughunter/minds/issues/10))
- **The content of `.minds/redact.json` does not reach the log.** By
  design, this file holds literal secrets — `deny_secrets` for the internal
  hostname, `allow` for values falsely detected as secrets. The most
  obvious typo there (forgotten array brackets) is not a syntax error but a
  data error, and `serde_json` quotes the value: `invalid type: string
  "glpat-…", expected a sequence`. As long as that landed on a discarded
  stderr, it was fleeting; with the log it would have become permanent. The
  message now names kind, line, and column — enough to repair, nothing to
  leak. ([#10](https://github.com/munichbughunter/minds/issues/10))
- **Credentials from a remote URL do not reach the log.** `git push` writes
  the URL into its error message, and in the **username** position Git does
  not redact a token: `https://glpat-…@gitlab.com/…` thus became
  `fatal: could not read Password for 'https://glpat-…@gitlab.com'` — and
  since the log entry above, that went to disk, into a file `fsck` points
  to and that gets attached to a bug report. The authority part is now cut
  out at the source, before the text becomes a message; host and path
  remain so the diagnosis stays usable.
  ([#10](https://github.com/munichbughunter/minds/issues/10))
- A `git` child process no longer inherits **any trace switches**
  (`GIT_TRACE`, `GIT_TRACE_CURL`, `GIT_CURL_VERBOSE` …). With them, Git
  logs its entire traffic to stderr — including `Authorization: Basic …`,
  and that is not a URL that could be cut out. A `GIT_TRACE=1` in the
  developer's shell would otherwise have sufficed to put a token
  permanently into the log. The variables are **removed**, not set to `0`:
  Git checks `GIT_CURL_VERBOSE` for existence, so a `=0` switched the dump
  on. ([#10](https://github.com/munichbughunter/minds/issues/10))
- `hook.log` is not written through a symlink — neither when the file
  itself is one, nor when `<git-dir>/minds` is. After opening, device and
  inode of the file handle are checked against the name; only then are the
  permissions touched.
  ([#10](https://github.com/munichbughunter/minds/issues/10))
- When writing a **hook file**, `minds enable` no longer follows a symlink.
  Since the hook directory follows `core.hooksPath`, it can live in the
  working copy and thus be **versioned** — a checked-in symlink
  `.husky/post-commit → ~/.aws/credentials` would previously have led to
  `enable` writing the minds block through the link into the private file
  and setting it from `0600` to `0755`. Now a symlink at this spot is
  rejected; writing goes through a sibling file and `rename` (the name is
  replaced, never written through), the permissions are set on the open
  file handle, and files beyond any plausible hook size are not even read in.
  `minds fsck` reads the hook files through the same route and reports a
  rejected hook with its reason.
  ([#9](https://github.com/munichbughunter/minds/issues/9))

  The two gaps explicitly named back then have since been closed: the hook
  **directory** redirected via a symlink is covered by the location rule
  ([#66](https://github.com/munichbughunter/minds/issues/66),
  [#64](https://github.com/munichbughunter/minds/issues/64), see *Fixed*),
  and the **agent configurations** have gone through the same protected
  write path as the hooks since
  [#65](https://github.com/munichbughunter/minds/issues/65) (see below).

### Added

- `minds fsck` now also checks the **agent registrations**. Until now it
  only looked at the git hooks; whether the agent journals at all was
  invisible to the report. Three states are reported: a configuration with
  no minds entry at all (the case a checked-in third-party entry produces —
  [#78](https://github.com/munichbughunter/minds/issues/78)), entries from
  an older version, and an incomplete registration. The recall entry gets a
  sentence of its own when it is outdated — its **absence**, however, does
  not: `--recall` is opt-in, and what nobody wanted is not missing.

  At most one line per agent, never per event, and a file that does not
  exist at all stays quiet. A hint, not a finding: the exit code stays 0,
  because it is the CI gate.
  ([#68](https://github.com/munichbughunter/minds/issues/68))
- `minds fsck` checks the hooks from the effective hook directory and
  reports when `post-commit` or `prepare-commit-msg` are missing there —
  including a hint when the minds block sits in the ignored `.git/hooks`
  instead. A hint, not a finding: the exit code stays 0, because not every
  repo wants hooks.
  ([#9](https://github.com/munichbughunter/minds/issues/9))
- `minds fsck` points to `<git-dir>/minds/hook.log` when entries are there
  — with their count and the path, but **without the wording**: the output
  of `fsck` lands in the CI log, and an error text from the hook path can
  carry an excerpt from the not-yet-redacted recording. A hint, not a
  finding — an old entry must not stop a pipeline.
  ([#10](https://github.com/munichbughunter/minds/issues/10))

  The log limits itself: at 1 MiB it rolls over to `hook.log.1`, and more
  than two files never exist. Multi-line error messages stay *one*
  entry (control characters are written as escape sequences), and the file
  is created with `0600`; an existing one with looser permissions is
  tightened on the next write, and writing never goes through a symlink.
- The `pre-push` hook now reports its **progress** on stdout instead of
  stderr — otherwise the stderr redirection from above would have
  swallowed the success messages along with the errors: what was sent, and
  above all how many review verdicts from others a push adopted. The latter
  matters, because exactly this merge fills the review store that
  `minds fsck --require-review` reads as a CI gate. If it fails, that now
  goes to the log instead of nowhere.
  ([#10](https://github.com/munichbughunter/minds/issues/10))
- `minds fsck` distinguishes an **outdated** minds block from a healthy
  one: the body between the marks is compared against the one this version
  would write. Until now, the mere presence of the mark counted as
  "installed" — a hook from an older `minds` thus looked healthy even
  though Git executes it and it no longer does what it should. The advice
  is `minds enable`; a hint, not a finding.
  ([#10](https://github.com/munichbughunter/minds/issues/10))

### Known limitations

*The state as of v0.1.1. The list under v0.1.0 describes the state back
then and is not rewritten retroactively — parts of what it says no longer
apply today (the PATH dependency, for instance, is gone with
[#25](https://github.com/munichbughunter/minds/issues/25)).*

- **Redaction has known gaps.** `curl -u user:pass` is not redacted
  ([#2](https://github.com/munichbughunter/minds/issues/2)), JSON-escaped
  secrets and PEM keys with a literal `\n` leak partially
  ([#3](https://github.com/munichbughunter/minds/issues/3)),
  `sk-ant`/`sk-proj` are missing from the token rules
  ([#33](https://github.com/munichbughunter/minds/issues/33)), and a
  multibyte character in the value (`PASSWORD=hunter€2`) triggers a panic
  ([#1](https://github.com/munichbughunter/minds/issues/1)). **That is the
  focus of the next version.** Anyone working today with someone else's
  code, or with particularly sensitive code, should know this.
- **`minds forget` does not erase everywhere.** The session branch on the
  forge remains ([#5](https://github.com/munichbughunter/minds/issues/5)),
  a repeated `put` revives the session
  ([#6](https://github.com/munichbughunter/minds/issues/6)), and the
  plaintext stays reachable as a parent commit
  ([#14](https://github.com/munichbughunter/minds/issues/14)). In addition,
  `recall`, `distill`, and `brief` deliver **nothing at all** once a
  session has been erased
  ([#83](https://github.com/munichbughunter/minds/issues/83)).
- **`minds gitlab mirror` does not work.** The note body goes over the wire
  as a header, GitLab rejects with "body is missing"
  ([#7](https://github.com/munichbughunter/minds/issues/7)).
- **In a linked worktree**, `minds show` and `minds why` show the main
  tree's commit. Capture and `fsck` are correct there, the lookup is not
  ([#20](https://github.com/munichbughunter/minds/issues/20)).
- **A `git push` opens two network connections** when there are new
  sessions to transfer: one for the context, one for the code. Noticeable
  against distant remotes
  ([#85](https://github.com/munichbughunter/minds/issues/85)). Without new
  sessions the hook costs nothing.
- **No Windows binary.** The builds cover macOS (Apple Silicon and Intel)
  and Linux (x86_64 and ARM64, musl/static). On Windows, use WSL or build
  from source.
- **The tool level is Claude-Code-specific.** For Codex, Cursor, Gemini,
  and opencode the prompt is captured, but tool calls, touched files, and
  model/token details are not interpreted.
- **The review layer needs two people on one repo** to be exercised at
  all.

## [0.1.0] — 2026-07-29

The first published version — and the first delivered through an installer
instead of as a hand-built archive.

Minds writes the context of an agent session to where it belongs: into Git
itself, next to the code. What prompted a change, who wrote it, and who
reviewed it sits content-addressed and signed under `refs/minds/` and travels
with the repo — no database, no cloud, verifiable offline and in an air gap.

> Earlier, hand-built archives already carried the same version number but
> predate this day. `minds --version` today reports only `0.1.0` and does
> not distinguish the two — anyone who still has an old archive on the PATH,
> please reinstall.

### Added

**Capture**

- **Hook-based capture.** `minds enable` registers agent and git hooks;
  idempotent and careful with entries it does not own. The hot path
  (`minds hook`)
  writes every event to the local journal and always exits 0; the cold path
  (`minds checkpoint`) interprets, redacts, stores, and appends the
  session-id trailer to the commit. See
  [ADR-0003](docs/adr/0003-hooks-over-transcript-parsing.md).
- **Five agents registrable:** `claude-code`, `codex`, `cursor`, `gemini`,
  `opencode`. Interpretation of the tool level is initially limited to
  Claude Code (see *Known limitations*).
- **Redaction, fail-closed.** Secrets and personal data are stripped
  *before* a byte reaches the store — when in doubt, Minds blocks rather
  than risks it. Rules extensible via `.minds/redact.json`.
- **Import of existing history** with heuristic session → commit
  attribution; conjectured attributions are marked as *conjectured* instead
  of stated as fact. See
  [ADR-0004](docs/adr/0004-import-and-store-index.md).

**Storage**

- **Content-addressed store** (`SessionId = blake3(canonical_json)`) with
  two backends behind one trait: in-repo under `refs/minds/` and as a
  separate child repo.
- **One ref per session.** No jointly written ref, hence no serialization
  point for writing and pushing: the ref name *is* the content hash, two
  agents touch different refs, and a repo that only commits pays 0.02 s for
  the hook. See [ADR-0010](docs/adr/0010-one-ref-per-session.md).
- **Browsable session branches.** Every session appears as
  `minds/session/<hash>` with `session.json` (authoritative) and
  `session.md` (rendered) — GitLab thus shows the branch as a readable page
  without any reader deploy.
- **`minds forget <session> [--reason]`** — GDPR erasure: the payload is
  replaced by a tombstone, the hash reference stays resolvable, erasure
  happens at all storage locations. `why`, `show`, and `fsck` stay green
  and degrade honestly instead of breaking. See
  [ADR-0007](docs/adr/0007-forget-redactable-payload.md).

**Lookup**

- **`minds why <file>:<line>`** — the session behind a single line,
  resolved via blame and trailer.
- **`minds show [<commit>] [--full]`** — intent and attribution behind a
  commit.
- **`minds blame <file>`** — attribution per line, aggregated by session,
  with context coverage in percent.
- **`minds recap`** and **`minds search <query>`** — the most recent
  sessions at a glance; intent, conversation, and files searchable.
- **`minds render`** builds a stateless HTML page: click a line, see the
  prompt behind it, conversation and tool calls expandable.
- **`minds fsck`** checks that every trailer resolves and reports journal
  gaps.

**Context reinjection**

- **`minds recall <target>`**, **`minds brief [<file>…]`**, and
  **`minds distill [--path] [--out]`** hand the captured context back to
  the next agent — as a brief for a line, as a size-capped start block, or
  as an AGENTS.md draft from the repo history. Deterministic, no LLM call,
  no tokens; same input yields byte-identical output. Optionally automatic
  at session start via `minds enable --recall`. See
  [ADR-0005](docs/adr/0005-context-reinjection.md).

**Identity and proof**

- **Change-Id** as the stable identity of a logical change, created and
  preserved via `prepare-commit-msg` (trailer `Minds-Change-Id`). Survives
  rebase, squash, amend, and cherry-pick. See
  [ADR-0006](docs/adr/0006-change-id.md).
- **Signed attribution.** `minds sign <session>` signs the canonical
  attribution via `ssh-sig` (no network, air-gap-capable), `minds verify`
  checks it and exits with a code ≠ 0 on tampering. "Agent X, model Y wrote
  these lines" becomes a proof instead of a claim. See
  [ADR-0008](docs/adr/0008-signed-attribution.md).
- **`minds audit --export`** bundles the provenance chain
  (change → session → attribution → verdict) as a portable JSON file with
  the canonical payloads and signatures — checkable without this tool. What
  the bundle proves and what it does not is in
  [docs/verification-guide.md](docs/verification-guide.md).

**Review**

- **Reviews as git objects.**
  `minds review <subject> --approve|--reject|--needs-work` stores a
  content-addressed, optionally signed verdict under `refs/minds/reviews/`;
  `minds reviews <subject>` lists verdicts and checks signatures. The
  verdict is anchored to the change-id and thus survives rebase, squash, and
  force-push. See [ADR-0009](docs/adr/0009-reviews-as-git-objects.md).
- **Review thread.**
  `minds comment <subject> --on <file:line|turn:n> "<text>"` — an
  append-only log of content-addressed entries. Two reviewers commenting
  offline produce no conflict but a union.
- **`minds stack`** shows the dependent changes from a base with their
  respective review state.
- **GitLab bridge, one-way and idempotent.**
  `minds gitlab mirror <subject> --mr <nr>` mirrors verdicts as an MR note
  (optionally as an approval); `minds gitlab webhook` interprets an MR
  comment (`/minds approve|reject|needs-work`) as a verdict — opt-in,
  stateless, no service. The token comes exclusively from the environment.
  Operating model in
  [docs/gitlab-operating-model.md](docs/gitlab-operating-model.md).
- **Policy as a binary instead of YAML.** `minds fsck --require-review`
  demands a valid verdict for every agent-written change and goes red
  otherwise. Plus a reusable CI include
  (`ci/minds-review-gate.gitlab-ci.yml`) that does nothing but call the
  binary.

**Operations**

- **`minds sync [--remote]`** sends context and reviews to the remote in
  one connection — all due refs at once, never with `--force`. Without new
  refs the call costs no connection. Merges whatever verdicts from others a
  `git fetch` brought along.
- **`minds metrics [--format prometheus|openmetrics|json]`** projects the
  store on demand into the Prometheus text format — no daemon, no second
  copy of the data. Plus an importable Grafana dashboard
  (`dashboards/minds.json`)
  and an opt-in CI include (`ci/minds-metrics.gitlab-ci.yml`).
- **`minds agent-help`** outputs the command map as machine-readable JSON —
  for agents, not for humans.

### Security

- **The secret wall on the hot path is agent-agnostic.** The file path is
  drawn from the union of known field variants (`file_path`,
  `notebook_path`, `path`, `absolute_path`, `filepath`, …) plus a heuristic
  over the field name. Fail-closed thus applies to all agents, not just the
  one whose field names we knew first.

### Known limitations

- The git hooks entered by `minds enable` call `minds` **without a path**
  and catch every failure with `|| true` — a recorder must not make a
  commit fail. If the binary is **not on the `PATH`**, the hooks therefore
  run **silently** into the void: committing keeps working, there is no
  error message, but also no change-id on the commit and no captured
  session. The same applies when an **outdated** `minds` sits on the
  `PATH` — it serves the hooks and may write an older store layout.
  `minds enable` does not check either of these yet; it can be verified
  with `command -v minds` and `minds --version`.
- Interpretation of the **tool level is still Claude-Code-specific**. For
  `gemini`, `codex`, `cursor`, and `opencode` the prompt is captured, but
  tool calls, touched files, and model/token details are not yet
  interpreted. Which agent gets full support next depends on the test
  group's needs.
- The **review layer needs at least two people on one repo** to be
  exercised at all.
- The reader (`minds render`) shows sessions, files, and the conversation;
  **overview tiles and charts are still missing**, even though
  `minds metrics` already delivers the numbers.
- The release contains **Linux x86_64** (musl, static) and — as soon as a
  Mac runner is registered — **macOS for Apple Silicon and Intel**.
  **Windows and ARM Linux are currently not built**; there, the route is
  `cargo build --release --bin minds`.
