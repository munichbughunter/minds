# Track EA — Evidence Assurance (implements ADR-0012)

Goal: answer "if the agent writes its own evidence, it is only signed, not proven" with a
running system, not an argument. Every issue names the `DOES_NOT_PROVE` sentence it retires
(or narrows), so progress is measurable in the product's own vocabulary.

Legend: **[DEMO]** = required for the Loom recording · Size S/M/L · one issue = one
Conventional Commit loop, CI triad as usual.

**Demo-critical path:** EA-0 → EA-S2 → EA-1a → EA-1 → EA-2 → EA-5 → EA-6 → EA-7 → EA-8 → EA-9 →
EA-10 (`container` only) → EA-11 → EA-12 → EA-13 → EA-14 → EA-15 (`--file` only) → EA-18 +
EA-20 (tests-only replay) → EA-22, with EA-4 accepted alongside. Everything else (EA-S1, EA-3,
EA-16, EA-17, EA-19, EA-21, benchmark replay) hardens A2/A3 after the recording.

---

## Phase 0 — Prerequisites and spikes

### EA-0 fix(cli): `minds verify` without argument resolves HEAD **[DEMO]** · S
Known issue from demo prep, still open (`verify_cmd::run` rejects `(None, None, _)`).
- AC: `minds verify` = verify the session(s) linked to `HEAD` via trailer; several sessions → one verdict block each, overall exit = worst.
- AC: `minds recall` brief no longer truncates the intent to its headline (second known issue on the demo path).

### EA-S1 spike: Claude Code `managed` isolation profile · M
Question: can the Bash tool be prevented from writing to the witness directory and connecting to the witness socket, while harness hooks still reach it?
- Test `allowManagedHooksOnly`, sandbox filesystem/network denials (incl. Unix sockets), deny rules on `.claude/settings*.json`, and HTTP hooks vs command hooks (payload parity with `PostToolUse`).
- Outcome: ADR-0012 addendum — `managed` yields A2, or stays A1 with the reasons.

### EA-S2 spike: `container` profile on Linux and macOS · S **[DEMO]**
- Claude Code in a dev container, worktree bind-mounted, socket mounted from the host.
- Verify: host-side watcher sees container writes on the bind mount (inotify on Linux, FSEvents with Docker Desktop/virtiofs on macOS); latency < 1 s.
- Outcome: a `devcontainer.json` + `compose.yaml` template for EA-12.

---

## Phase 1 — Artifact reconciliation (works at A1 today, ships first)

### EA-1a feat(capture): write-time content hash from the tool payload **[DEMO]** · M
Today `adapter::hash_artifacts` hashes write effects by reading the worktree at checkpoint time — that is the file at commit time, not what the agent wrote.
- AC: additive `Effect.written: Option<ContentHash>` derived from the raw payload (`Write`: `tool_input.content`; `Edit`/`MultiEdit`: reconstructed from the payload if it carries enough, else `None` with reason), adapter version bumped.
- AC: golden fixtures from a real Claude Code session for Write/Edit/MultiEdit (redacted).

### EA-1 feat(reader): reconciliation read model **[DEMO]** · M
Retires (for witnessed ranges): *"Not that a session actually produced the lines attributed to it"*.
- `Reconciliation { files: Vec<FileRecon>, explained_lines, total_lines }`, classes `explained | explained_fs_only | reported_only | unexplained`.
- Compare `blake3(committed blob)` with the last observed content hash per path from write effects (and `fs.observed` once EA-8 lands).
- Line level: reconstruct from `Write` payloads, replay `Edit` onto the pre-session blob, accept only if the result hashes to the observed hash; otherwise file level only, flagged `line_level: unavailable`.
- Pure derivation: no ref moves, deterministic, test-pinned like `reinterpret`.
- Tests: agent-only commit → 100 % explained; one human line after the session → exactly that line `unexplained`; file written via shell → `reported_only` at A1.

### EA-2 feat(cli): `minds verify --commit <rev>` and coverage line **[DEMO]** · S
- Default target `HEAD` (with EA-0). Prints `artifact N/M lines explained` in the Coverage line and lists unexplained hunks with `file:line`.
- Gate: `--require-explained <percent>`; failing gate → exit 2 (VERIFIED, INCOMPLETE semantics), never 1.

### EA-3 feat(tui,html): render reconciliation · S
- Evidence mode (`e`) and `minds render` show explained/unexplained per hunk; unexplained never red-as-error, always "not observed".

---

## Phase 2 — The witness (A2)

### EA-4 docs(adr): ADR-0012 witnessed evidence **[DEMO]** · S
Accept after EA-S1/EA-S2 results are folded in.

### EA-5 feat(capture): witness wire protocol **[DEMO]** · S
- Frame: `minds-witness-v1`, u32-LE length, u8 kind (`hook | control`), payload bytes; max frame size capped; unknown version → reject frame, never crash.
- Fuzz test on the decoder (length lies, truncated frames, huge lengths).

### EA-6 feat(cli): `minds witness run` — single writer with live chaining **[DEMO]** · L
Retires: *"Not the integrity between append and seal"* (for `witness/v1` ranges).
- Accepts frames on a Unix socket; assigns seq; appends to its own journal (dir owned by the witness identity, 0700/0600).
- Computes the ADR-0011 fold incrementally on each append; salt and epoch state in the witness dir.
- Records its own lifecycle as chained events (`witness.start` with clean/unclean previous stop, `witness.stop`) so downtime is provable, not silent.
- Keeps an append-only ledger of produced seal ids; `verify --witness-home` flags witnessed seals missing from the repo (the agent can delete refs in a shared repo).
- Performs checkpoint on request (`control: checkpoint`) or on the Git hook path; produces `RedactedSession` + seal with `scope=witness/v1`.
- Test: identical event stream through witness and through the legacy journal → identical chain root (same algorithm, same domains).

### EA-7 feat(capture): hook becomes a fail-open forwarder **[DEMO]** · M
- `minds hook`: if `MINDS_WITNESS_SOCKET` (or config) is set → send frame with the existing timeout budget; on any failure write the local journal exactly as today (A1 range).
- Latency test: p99 of the hook unchanged vs. current baseline.
- `minds checkpoint` seals witness ranges and fallback ranges separately; `previous` links them in order.

### EA-8 feat(witness): file-system observer **[DEMO]** · M
Narrows: *"Not that the agent process was the only actor"*.
- Cross-platform watcher (`notify` crate), recursive on the worktree, `.gitignore`-aware, debounce 100 ms.
- Emits `fs.observed { path, content: blake3 | secret-wall-suppressed, at }` into the same chain.
- Secret-file wall and "no fingerprints outside the repo" apply unchanged (ADR-0011 decision 9).
- Reconciliation (EA-1) consumes it: `explained` requires hook claim **and** fs observation; `explained_fs_only` for fs-only.

### EA-9 feat(attest): signing namespaces and witness key · S **[DEMO]**
Retires: *"Not who controls the signing keys"* (for the witness role).
- Namespaces `minds-witness`, `minds-intent`, `minds-anchor` next to `minds`.
- `minds witness keygen` creates the key inside the witness domain; `allowed_signers` template with `namespaces="minds-witness"` for witness principals.
- `verify` rejects a seal with `scope=witness/v1` that is not signed under `minds-witness` by a principal allowed for it → integrity finding, not a downgrade.

### EA-10 feat(cli): `minds enable --witness <container|user|managed>` **[DEMO]** · M
- `container`: writes `.devcontainer/` (from EA-S2), witness systemd user unit / launchd plist on the host, socket path wiring.
- `user`: prints the exact `useradd`/permissions steps (never runs sudo itself), socket group-writable, witness dir owned by `minds-witness`.
- `managed`: writes a `managed-settings.json` proposal for the admin; only if EA-S1 is green.
- `minds doctor` reports the active profile and whether the agent can reach the witness dir (active probe as the developer UID must fail with EACCES).

### EA-11 feat(core): assurance computation · M **[DEMO]**
- `Assurance { level: A0..A3, per_range: Vec<(SealId, Level, Reasons)> }`, computed in `minds-reader` from scope, signer principal/namespace, fs corroboration, intent signature, replay and anchor records. Never serialized into evidence.
- Overall = weakest range; reasons are shown ("range 2: witness unreachable 14:03–14:04 → A1").

### EA-12 feat(cli): Assurance line, `Not proven` line, gates **[DEMO]** · S
- Output exactly as in ADR-0012 decision 8; exit codes unchanged.
- `--require-assurance A2` on `verify` and `fsck`.

### EA-13 feat(core): level-aware `PROVES` / `DOES_NOT_PROVE` **[DEMO]** · S
- Each sentence gets `min_level`; bundle, TUI and HTML print the set that applies to the verified material.
- Test: an A1 bundle still prints the append→seal limitation; an A2 bundle does not, but prints the remaining A2 limits.

---

## Phase 3 — Intent binding

### EA-14 feat(core): intent anchor as genesis event **[DEMO]** · M
- `minds-intent-v1` text format (fixed line count, fail-closed parser like the seal), `source`, `content`, `scope`.
- First chain link of the session; sessions without anchor show `intent: unbound (prompt only)`.

### EA-15 feat(cli): `minds intent bind` / `minds intent sign` **[DEMO]** · M
- `bind --file <path>` (demo: `fachliche-anforderung.md`, LOG-2417) and `bind --issue <id>` (EA-16).
- `sign` uses namespace `minds-intent`; detects key type and reports `sk (user presence)` vs `software key`.
- `verify` shows `intent signed by <principal> (sk key)` and whether the snapshot hash matches.

### EA-16 feat(gitlab): issue snapshot and version check · M
- Fetch title, description, `updated_at`; canonicalize; hash. At verify time, if reachable, confirm that version via the issue's description history; offline → `not checked (offline)`, never "valid".

### EA-17 feat(reader): scope findings · S
- Writes outside the anchor's `scope` globs → `out of scope` findings in Coverage; count in the verdict block. (The "agent stayed within ticket scope" audit use case.)

---

## Phase 4 — CI: reproduction and anchor (A3)

### EA-18 feat(cli): `minds replay` **[DEMO: tests only]** · L
Retires: reported test/benchmark results as unchecked claims.
- Selects decisive commands via the adapter's classification (`test`, `bench`), re-runs them in CI in a clean checkout with a timeout.
- Tests: exit code + pass/fail counts. Benchmarks: value within tolerance (default ±25 %, `minds.toml` per pattern).
- Demo cut: tests only, record unsigned; benchmarks and signing follow.
- Emits a signed replay record (`minds-anchor`) under `refs/minds/anchors/`; reader lifts matching events to `(Observed, Verified)`, mismatches become `claim not reproduced`.

### EA-19 feat(cli,gitlab): `minds anchor` — first-sight countersignature · M
Narrows: *"Not real wall-clock time"* (upper bound).
- CI countersigns unseen seal ids with the protected CI key; stores under `refs/minds/anchors/<seal_id>`; mirrors an MR note via `minds-gitlab`.
- `verify` prints `anchored: pipeline #N, <time>`.

### EA-20 feat(ci): include template stage `minds-evidence` **[DEMO: tests-only replay]** · S
- One job: `minds verify --require-assurance A2 && minds replay && minds anchor`. No YAML logic beyond calling the binary (R5 principle).

---

## Phase 5 — Documentation and demo

### EA-21 docs: verification guide, privacy overview, BetrVG note · S
- Recipe to verify witness seals, intent signatures and anchors with `ssh-keygen -Y verify` and `b3sum` only.
- Privacy overview: the file observer records paths and hashes of the agent's workspace; "unexplained" is never attributed to a person.

### EA-22 test: end-to-end witness demo as a pilot test **[DEMO]** · M
- Scripted scenario in `crates/minds-cli/tests/`: bind + sign intent (software key in test) → witnessed session with fixture events → commit → human edit → verify shows the unexplained line → tamper a seal → TAMPERED → forged frame → `uncorroborated`.
- The Loom must show nothing this test does not cover.
