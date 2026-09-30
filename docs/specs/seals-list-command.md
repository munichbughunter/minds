# Spec: `minds seals` — list Evidence-Chain seals without knowing an id first

Status: ready for implementation
Depends on: ADR-0011 (Evidence Chain) — read `docs/adr/0011-evidence-chain.md` in full before touching this
Audience: this document is self-contained. It assumes no prior conversation context — everything you need is either in this file or in the cited source files.

## 1. Goal

`minds verify --evidence <seal-id>` and `minds sign --seal <seal-id>` both require the caller to already know a seal's id (a `b3-<64hex>` content hash). Today there is no user-facing way to discover which seal ids exist in a repo — the only enumeration primitive, `ContextStore::list_seals()`, is used internally (e.g. by the namespace-fallback in `crates/minds-cli/src/verify_cmd.rs::seals_naming`) but never exposed through the CLI. A user who wants to browse "what evidence do I have" has no entry point except grepping `refs/minds/evidence/` by hand.

Add a new subcommand, `minds seals`, that lists seals in a readable form — id, linked session (if any), event range, gap/signature status, timestamp — so a seal id becomes something you *discover*, not something you must already possess.

## 2. Non-goals

- Do not change seal creation, signing, or the checkpoint flow (`crates/minds-cli/src/checkpoint.rs`, `crates/minds-cli/src/sign_cmd.rs`) at all.
- Do not change `minds verify`'s behavior, output, or exit codes.
- Do not add write paths. This command only reads `refs/minds/evidence/*`; it must never create, sign, or mutate a seal.
- No new store trait methods should be needed — see §5. If you find during implementation that you genuinely need one, stop and reconsider the design before adding trait surface.

## 3. Current state

- `ContextStore::list_seals(&self) -> Result<Vec<ContentHash>>` already exists and is fully implemented for all three backends: `crates/minds-store/src/git_store.rs:1322` (reads `refs_under(SEAL_REF_PREFIX)`), `crates/minds-store/src/in_repo.rs:129`, `crates/minds-store/src/child_repo.rs:163`. The trait default (`crates/minds-store/src/store.rs:353`, doc comment: "Alle abgelegten Seals — für `fsck` und `verify --evidence`") returns an empty `Vec`. Order is **not** guaranteed — it comes straight from git ref iteration, not sorted by time or sequence.
- It is already used today, read-only, by `seals_naming()` in `crates/minds-cli/src/verify_cmd.rs:665` (fallback path when a session's `evidence.json` back-reference is missing) and presumably by `minds fsck`.
- `ContextStore::seal_text(&self, id: &ContentHash) -> Result<Option<String>>` (`crates/minds-store/src/store.rs:319`) reads a seal's bytes, verifies `id == derive_key(text)`, and returns `Err(StoreError::SealMismatch { requested, actual })` on tamper — reuse this, never call `seal_bytes` directly for display (that skips the hash check).
- `ContextStore::seal_signature(&self, id: &ContentHash) -> Result<Option<String>>` (`crates/minds-store/src/store.rs:368` default, real impls in each backend) returns the raw `seal.sig` text or `None` if unsigned. For a listing command you only need presence/absence, not verification (verification needs `--signers`/`--identity`, which `minds verify` already does properly — do not reimplement signature *verification* here, only report *presence*).
- `ContextStore::seals_of(&self, session: SessionId) -> Result<Vec<ContentHash>>` (`crates/minds-store/src/store.rs:348`) returns the seals recorded against one session, in entry order, via the session's `evidence.json` back-reference. Use this for the `--session <id>` filter (see §4) instead of listing everything and filtering client-side.
- `Seal` (`crates/minds-core/src/evidence.rs:365`) has these fields: `root: ContentHash`, `agent: String`, `scope: String`, `first_seq: u64`, `last_seq: u64`, `events: u64`, `gaps: u64`, `pre_chain: u64`, `outcome: SealOutcome`, `previous: Option<ContentHash>`, `last_event_at: String` (RFC 3339). `SealOutcome` (`crates/minds-core/src/evidence.rs:342`) is `Stored { session: String }` or `Rejected`.
- No user-facing command exists today that enumerates seals. `minds verify --evidence <id>` and `minds sign --seal <id>` both take a required, already-known id as their only way in.

## 4. Target behavior

New subcommand: `minds seals`.

```
minds seals                       # all seals, most recent first
minds seals --session <id>        # only the seals recorded against one session
minds seals --limit <n>           # cap the number printed (default: no cap)
```

`--session` and no positional argument at all — this is a listing command, not one that acts on a single named thing (unlike `minds verify <session-id>`). Follow the `Spec` table convention in `crates/minds-cli/src/main.rs` (see §6): `--session` and `--limit` are **value** flags, both optional, 0 positionals.

Example output, no flags, three seals, one unsigned, one with gaps, one block seal:

```
$ minds seals
3 seal(s):

▸ b3-4f9a2c8e1d7b0653  seq 1–42, 42 event(s), 0 gap(s), stored — signature valid
  session  b3-91cf0a4e6d2b8873
  time     2026-09-17T14:32:05Z

▸ b3-1a0e77c9b3f4d210  seq 1–18, 18 event(s), 3 gap(s), stored — unsigned
  session  b3-6bd21f0938ac47e5
  time     2026-09-16T09:11:40Z

▸ b3-8c3d5f0271e9a4b6  seq 1–7, 7 event(s), 0 gap(s), storage_policy_rejected_payload — unsigned
  session  -
  time     2026-09-15T18:02:12Z
```

Sort order: **most recent first**, by `last_event_at` (string-sortable RFC 3339, so a plain descending string sort is correct — no date parsing needed). This is the only sensible default; state it in the command's `--help`/usage text if the project's CLI has a per-command help string convention (check `crates/minds-cli/src/main.rs` near the `SPECS` table and `agent_help` for whether individual commands carry inline help text, and follow whatever exists there).

Per-seal line format: reuse `print_seal_line`'s format from `crates/minds-cli/src/verify_cmd.rs:733` (`"{id}: seq {first}-{last}, {events} event(s), {gaps} gap(s), {outcome} — {signature}"`) for the first line, since that's the project's existing seal-summary format — do not invent a new one. Add `session` and `time` as indented follow-up lines (matching the two-space-indent convention used in `print_session_sealed`, `crates/minds-cli/src/checkpoint.rs:413`).

Id display: full seal ids are 64-hex-character `b3-` hashes (`ContentHash`), which is unreadably long in a list. Reuse the truncation pattern from `crates/minds-cli/src/blame.rs:114` (`short_id`: `b3-` plus first 12 hex chars, `…` suffix) — write an equivalent local helper (or a shared one, see §6) for `ContentHash` if one does not already exist for that type; check whether `ContentHash` already has a `Display` short form before adding a new helper.

If a seal's `outcome` is `Stored { session }`: print `session  <id>`. If `Rejected`: print `session  -` (dash, matching the convention already used in `report_tampered_seal`, `verify_cmd.rs:429-432`).

## 5. Design decision

This is **mostly wiring**, not new logic:

- Enumeration: `store.list_seals()` (all) or `store.seals_of(session_id)` (filtered) — both already exist and are fully implemented.
- Reading + tamper detection: `store.seal_text(&id)` — already handles the hash check and returns `StoreError::SealMismatch` on tamper.
- Parsing: `Seal::parse(&text)` (used throughout `verify_cmd.rs`) — already exists.
- Signature presence: `store.seal_signature(&id)` — already exists, `Some`/`None` is enough here, no verification needed.
- Formatting: adapt `print_seal_line` (rename/extract if you want it shared between `verify_cmd.rs` and the new command — see §6 for whether to share or duplicate).

No new `ContextStore` trait methods, no new storage format, no new hashing. The only genuinely new code is: the CLI subcommand plumbing, the sort-by-`last_event_at` step, the `--session`/`--limit` filtering, and a listing-appropriate print format (header count line + per-seal block instead of the single-seal verdict format `minds verify` uses).

If a seal fails to parse or its hash doesn't match (`StoreError::SealMismatch`), **do not abort the whole listing**. Print an error line for that one seal (reuse the spirit of `report_tampered_seal` in `verify_cmd.rs:395`, but keep it short — one line, not the full tamper report; the full tamper report is `minds verify`'s job) and continue with the rest. A listing command should never let one bad entry hide all the good ones — that would itself be a coverage gap of the *tool*, not of the evidence.

## 6. Implementation plan

1. Create `crates/minds-cli/src/seals_cmd.rs` (new file, sibling to `verify_cmd.rs`, `review_cmd.rs`). Public entry point: `pub fn run(session: Option<&str>, limit: Option<&str>) -> ExitCode`, mirroring the `run`/inner-function split used in `verify_cmd.rs` (`run` parses/dispatches, a private function does the work and returns `Fallible<()>` or similar, errors get printed with the `minds seals: {err}` prefix and `ExitCode::FAILURE`, following the exact pattern in `blame.rs::run` and `verify_cmd.rs`'s `operational_failure`).
2. In `seals_cmd.rs`:
   - Open context via `crate::context::Context::open()?` (same as `blame.rs`, `verify_cmd.rs`).
   - If `--session <id>` given: parse as `SessionId`, call `ctx.store.seals_of(id)`. Else: call `ctx.store.list_seals()`.
   - If `--limit <n>` given: parse as `usize`, cap the final sorted list at that length (apply the limit *after* sorting, not before — otherwise "most recent first" plus a limit would show arbitrary entries, not the newest ones).
   - For each seal id: `store.seal_text(&id)`, handle `Ok(Some(text))`, `Ok(None)` (skip — referenced-but-missing, same defensive handling as `check_seals` in `verify_cmd.rs:482-495`), `Err(StoreError::SealMismatch{..})` (print a one-line tamper notice, continue — do not panic or abort), and `Err(other)` (propagate, this is an operational failure, same as elsewhere in the codebase).
   - Parse with `Seal::parse(&text)`; on parse failure, treat the same as a hash mismatch — one-line notice, continue.
   - Collect `(id, Seal, has_signature: bool)` for all successfully-read seals.
   - Sort descending by `seal.last_event_at` (plain `str`/`String` comparison — RFC 3339 with a fixed-width zero-padded format sorts correctly as a string; verify this holds for the actual format `checkpoint.rs` writes before relying on it — if any seal predates zero-padding or uses a non-comparable format, sort by parsing to a proper timestamp type instead, but check first before adding a parsing dependency that might not already be in the workspace).
   - Apply `--limit` if given.
   - Print the header line: `println!("{n} seal(s):\n")` — or `"no seals yet"` if empty (see §7).
   - Print each seal per the format in §4.
3. Add the subcommand to `crates/minds-cli/src/main.rs`:
   - Add to the `SPECS` table (near `spec("verify", ...)` around line 305): `spec("seals", &["--session", "--limit"], &[], 0)` — 0 positionals, two optional value flags.
   - Add the dispatch arm (near the `"verify" =>` arm around line 723): `"seals" => seals_cmd::run(parsed.value("--session"), parsed.value("--limit")),` — match the exact accessor method names (`.value()`, `.positional()`, `.has()`) already used for other commands in that match block; read the surrounding 20 lines of `main.rs` before writing this to get the exact method names right, they are not fully shown in this spec.
   - Add `mod seals_cmd;` near the other `mod ..._cmd;` declarations at the top of `main.rs`.
4. Decide whether to extract a shared seal-line-formatting helper used by both `verify_cmd.rs::print_seal_line` and the new command, or to keep them independent. Given the two commands print slightly different things (single verdict-oriented line vs. list-oriented block with session/time), duplication with a shared low-level piece (e.g. the outcome-to-word and gaps/events summary segment) is more likely to be the right call than a shared whole-function — but verify this against how much of `print_seal_line` is actually reusable once you have the real code in front of you, and prefer NOT forcing a shared abstraction if the two output shapes diverge more than expected.
5. Update whatever documents the command list for users/agents — check `crates/minds-cli/src/main.rs`'s `agent_help` function and any USAGE string near the top of the file, since there is a test (mentioned in the `SPECS` doc comment at `main.rs:278`, "Ein Test hält [`agent_help`] mit dieser Tabelle im Gleichschritt") that keeps `agent_help` in sync with `SPECS` — find and satisfy that test.

## 7. Edge cases

- **No seals in the repo at all** (fresh repo, or one entirely captured before the evidence chain, ADR-0011): print `"no seals yet"` (or equivalent) and exit `0` — this is a state, not an error, same philosophy as `minds verify`'s "captured before the evidence chain" case (`verify_cmd.rs:255`).
- **`--session <id>` where the session has zero seals**: same as above, but scoped — e.g. `"no seals for session b3-…"`.
- **`--session <id>` where `<id>` doesn't parse as a `SessionId`**: print a clear error (`minds seals: not a valid session id {id:?}: {err}`, matching the error-message style in `verify_session`) and exit failure — do not silently treat it as "no results".
- **A seal referenced by `list_seals()` but missing from the store** (`seal_text` returns `Ok(None)`): skip silently or note briefly — this mirrors how `check_seals` in `verify_cmd.rs` handles the same situation for `verify`.
- **A tampered seal** (`StoreError::SealMismatch`) among otherwise-healthy seals: report it inline as tampered, keep listing the rest (see §5 — this is the one piece of genuinely new judgment-call logic in this spec).
- **`--limit 0`**: should print zero seals but still show an accurate count/header if you choose to show total-before-limit vs shown-count — pick one and be consistent, document the choice in the command's own doc comment.
- **Very large repos (thousands of seals)**: `list_seals()` reads every seal's text to sort by `last_event_at`, which is O(n) git-object reads. This spec does not require solving that performance problem (no daemon, no cache — see the project's existing no-daemon design philosophy), but do not do anything asymptotically worse than one `seal_text` read per seal.

## 8. Testing requirements

Follow the project's existing test conventions — before writing tests, read the existing test modules in `crates/minds-cli/src/verify_cmd.rs` (search for `#[cfg(test)]` in that file) and/or `crates/minds-cli/src/blame.rs` / `review_cmd.rs` for the harness pattern used (likely a temp-repo fixture helper shared across `minds-cli` tests — find it via grep for `fn test_repo` or similar in `crates/minds-cli/src/` before writing new fixture code; do not duplicate an existing fixture helper).

At minimum, cover:
- Empty repo → "no seals yet", exit 0.
- One stored seal, unsigned → appears, correct fields, sorted correctly with others.
- One stored seal, signed → signature presence shown correctly (not verified, just "present").
- One block/rejected seal (`SealOutcome::Rejected`) → `session  -` shown, no crash.
- Multiple seals → correct descending sort by `last_event_at`.
- `--session <id>` filters to only that session's seals (use `seals_of` semantics, not client-side filtering of `list_seals()`).
- `--limit <n>` caps output correctly, applied after sorting.
- A tampered seal among healthy ones → healthy ones still print, tampered one gets a short inline notice, command does not abort or panic.
- `main.rs`'s `SPECS`/`agent_help` sync test still passes after adding the new command (this test already exists — just don't break it).

## 9. Acceptance criteria

- [ ] `minds seals` with no flags lists all seals in the repo, most recent first, or prints "no seals yet" for an empty repo.
- [ ] `minds seals --session <id>` lists only seals recorded against that session.
- [ ] `minds seals --limit <n>` caps the printed list to `n` entries, applied after sorting.
- [ ] Output format is visually consistent with `minds verify`'s existing seal-line format (reused, not reinvented) plus session/time follow-up lines.
- [ ] A tampered or unparseable seal does not abort the listing; it's reported inline and the rest of the list still prints.
- [ ] No new `ContextStore` trait methods were added (or, if one turned out to be genuinely necessary, it's justified in the PR description against this spec's assumption that none was needed).
- [ ] No write path was touched — `cargo test` for `minds-store` and `minds-cli` passes unchanged, and a manual check confirms `refs/minds/evidence/*` is untouched by running `minds seals` against a repo and diffing `git for-each-ref refs/minds/evidence` before/after.
- [ ] `main.rs`'s `SPECS`/`agent_help` consistency test passes.
- [ ] New unit tests per §8 pass.
- [ ] `cargo clippy --workspace` and `cargo fmt --check` (or whatever the project's CI triade actually runs — check `Cargo.toml`/CI config/the `/feature` skill's "CI-Triade" step for the exact commands) are clean.

## 10. Relevant invariant

From this project's non-negotiable invariants: **"Tolerant lesen, kanonisch schreiben"** (read tolerantly, write canonically). This command is a pure read path — it must never write, sign, create, or mutate anything under `refs/minds/evidence/`, and it must handle malformed/tampered/legacy seal data by reporting it, never by crashing or silently dropping the rest of the listing.
