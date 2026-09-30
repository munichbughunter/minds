# Spec: per-line output for `minds blame`

Status: ready for implementation. Target repo: `minds` (Rust workspace, binary `minds`, CLI code under `crates/minds-cli`, git layer under `crates/minds-git`).

This spec is self-contained. It does not assume you have seen any prior discussion — everything you need is either quoted below or points at an exact file/line to read yourself.

## 1. Goal

`minds blame <file>` today aggregates every line of a file by the session that wrote it and prints one summary block per session. That answers "which sessions touched this file", but not "who wrote *this specific* line", the way `git blame` (or the classic per-line agent-attribution view popularized by tools like git-ai) does. Add a `--lines` flag to `minds blame` that prints one annotated line of output per source line, each tagged with the short session id and agent name behind it, in file order.

After this change, `minds blame --lines <file>` lets a user scan a file top to bottom and see, per line, which agent/session wrote it — the git-blame-shaped view — while `minds blame <file>` (no flag) keeps today's session-aggregated summary unchanged.

## 2. Non-goals

- No change to the existing aggregated (default) output of `minds blame` — it must stay byte-for-byte identical to today's behavior when `--lines` is not passed.
- No new blame engine, no changes to `crates/minds-git/src/blame.rs` (`BlameProvider`, `GixBlame`, `ShellBlame`, `AutoBlame`) — these already provide everything needed (`BlameLine { line, commit }`).
- No color/TTY detection, no terminal width wrapping of long source lines — print the full line as-is.
- No JSON/machine-readable output mode — plain text only, this spec.
- No changes to `minds why` or `minds show`.
- Do not touch `crates/minds-capture` or any capture/hook path. This is a pure read/derive feature (see §10).

## 3. Current behavior (as of this spec)

`crates/minds-cli/src/blame.rs` implements `minds blame <file>`:

```rust
pub fn run(target: Option<&str>) -> ExitCode { ... }   // dispatch, no flags today

fn blame(path: &str) -> Fallible<()> {
    let ctx = Context::open()?;
    let head = ctx.repo.head()?.commit()...;            // errors if HEAD has no commit
    let lines = ctx.repo.blame().blame_file(head, path)?;   // Vec<BlameLine>, sorted by line asc
    // ... groups lines by session, prints:
    // "{path} — {total} lines, {with_context} with captured context ({pct}%)\n"
    // then per session, ranked by line count desc:
    // "▸ {headline}"
    // "  {count} line(s) · {agent.name} · {model.id} · {short_id}"
    // then, if any lines have no session: "\n{without} line(s) without captured context"
}
```

Key types already available and unchanged by this spec:

- `minds_git::BlameProvider::blame_file(at: CommitId, path: &str) -> Result<Vec<BlameLine>>` — `BlameLine { line: u32 /* 1-based */, commit: CommitId }`, returned sorted ascending by `line` (one entry per source line; `crates/minds-git/src/blame.rs:44` `Repo::blame()` returns an `AutoBlame` that implements this).
- `Context::open() -> Fallible<Context>` (`crates/minds-cli/src/context.rs:84`) — opens the repo + configured store for the current working directory.
- `Context::linked_sessions(&self, commit: CommitId) -> Fallible<(Vec<(SessionId, Session)>, Skipped)>` (`context.rs:112`) — resolves a commit to its linked sessions (trailer + store index merged), tolerantly skipping forgotten/unreadable ones and reporting the skip count via `Skipped`.
- `minds_core::{Session, SessionId}` — `Session.agent.name: String`, `Session.model.id: String`, `Session.intent.request: String`.
- The existing private helper `fn short_id(id: SessionId) -> String` in `blame.rs` (`"b3-"` + 12 hex chars + `"…"` if longer) — reuse it, don't duplicate.
- `ctx.repo.tree_of(commit) -> Result<TreeId>` and `ctx.repo.read_blob(tree, path) -> Result<Option<Vec<u8>>>` (`crates/minds-git/src/objects.rs:66` and `:85`, both `pub`) — use these to fetch the file's raw content at `head` for per-line printing; do not add a new public API, they already exist and are exactly what the existing `blob_at` private helper in `minds-git/src/blame.rs` uses internally.

CLI argument parsing is hand-rolled, not clap (see `crates/minds-cli/src/main.rs:6-20` for the rationale comment). Commands are declared in a `const SPECS: &[Spec] = &[...]` table (`main.rs:270`) which a test keeps in sync with `agent_help.rs`. The current blame entry is:

```rust
spec("blame", &[], &[], 1),   // main.rs:292 — no value_flags, no bool_flags, 1 positional
```

and the dispatch arm:

```rust
"blame" => blame::run(parsed.positional(0)),   // main.rs:631
```

Compare with `why`, which already has a bool flag:

```rust
spec("why", &[], &["--full"], 1),                          // main.rs
"why" => why::run(parsed.positional(0), parsed.has("--full")),  // main.rs
```

`Parsed::has("--flag") -> bool` and `Parsed::positional(n) -> Option<&str>` are the accessors used throughout `main.rs` — use the same ones.

USAGE text for blame lives in `main.rs` around line 114-117:

```
  minds blame <file>
        Overview of which session sits behind which lines of a file,
        aggregated by session, with context coverage in percent.
```

`crates/minds-cli/src/agent_help.rs:34` has a matching machine-readable entry:

```rust
{"name": "blame", "usage": "minds blame <file>", "summary": "Attribution per line, aggregated by session, with context coverage."},
```

A test (find it by searching `agent_help` tests, likely in `agent_help.rs` or a dedicated test module) asserts `agent_help`'s command list stays in sync with `SPECS` — both must be updated together or that test will fail.

## 4. Target behavior

New invocation: `minds blame --lines <file>`.

Exact example, given a 3-line file `src/retry.rs` where line 2 was written by a session with short id `b3-a1b2c3d4e5f6` and agent name `claude-code`, and lines 1 and 3 have no captured context:

```
$ minds blame --lines src/retry.rs
src/retry.rs — 3 lines, 1 with captured context (33%)

-                -            1  eins
b3-a1b2c3d4e5f6  claude-code  2  ZWEI
-                -            3  drei
```

Precise format rules:

1. **Header line** (always printed, identical wording to the existing aggregate mode's first line): `"{path} — {total} lines, {with_context} with captured context ({pct}%)\n"` — reuse exactly, followed by a blank line.
2. **One row per source line**, in ascending line-number order (the order `BlameLine` entries already come in). Four columns separated by a single space each, computed as follows:
   - **Column A — short session id**: `short_id(id)` for the first session id linked to that line's commit (same "first of the linked sessions" tie-break rule the aggregate mode already uses — see §6), or the literal `"-"` if the line has no session with non-empty `intent.request` (mirrors the existing `without` counting logic).
   - **Column B — agent name**: `session.agent.name` for that same session, or `"-"` if there is none.
   - **Column C — line number**: the 1-based line number, right-aligned.
   - **Column D — source text**: the raw line content, printed as-is (no trimming, no escaping, preserve internal tabs/whitespace).
3. **Column widths**: Column A and Column B are left-aligned and padded to the width of the *widest value that occurs in this file's output* (i.e. compute `max_a = max(len(col_a_value))` over all rows, same for `max_b`), so the columns line up — this is the same dynamic-width idea `git blame`'s author column uses. Column C is right-aligned and padded to the width of `total.to_string().len()` (so a 900-line file gets 3-digit line numbers, a 9-line file gets 1 digit). Column D has no fixed width (it's the last column, print to end of line).
4. If `without > 0` (some lines have no session), do **not** print the trailing `"\n{without} line(s) without captured context"` summary line from the aggregate mode — that information is now visible per-row via the `"-"` markers, and repeating it as a footer would be redundant. (This is a deliberate behavior difference between the two modes — state it explicitly in the module doc comment so it isn't "fixed" later by accident.)
5. If the `Skipped` note (forgotten/unreadable sessions) is non-empty, still print it to stderr exactly as today (`eprintln!("minds blame: {note}")`) — this is orthogonal to `--lines` and unaffected.
6. Exit codes and error messages for the error paths already handled today (missing file argument, HEAD has no commit, path not resolvable in blame) are unchanged and apply identically whether or not `--lines` is passed.

## 5. Design decision

**Add a `--lines` bool flag to the existing `minds blame` command.** Do not add a new subcommand, and do not change the default output.

Justification: `minds why --full` already establishes the pattern of a bool flag toggling a command's verbosity/output shape without changing its identity (§3). A new subcommand would fragment `agent-help`'s command list and force callers to remember two command names for what is fundamentally one question ("who wrote this file") answered at two granularities. `--lines` is also the more literal, self-documenting name compared to alternatives like `--porcelain` (git-specific jargon this project doesn't otherwise use) or `--verbose` (misleading — this isn't "more of the same output", it's a structurally different view).

## 6. Implementation plan

Work through these files in order:

1. **`crates/minds-cli/src/blame.rs`**
   - Change `pub fn run(target: Option<&str>) -> ExitCode` to `pub fn run(target: Option<&str>, lines: bool) -> ExitCode`, threading `lines` into the internal `blame(path: &str, lines: bool) -> Fallible<()>` (or split into `blame_summary` / `blame_lines` helpers called from a small dispatcher — your call, keep `blame()`'s existing body as the summary path with minimal diff).
   - Reuse the existing per-line loop that already builds `commit_cache: BTreeMap<CommitId, Vec<SessionId>>` and `session_of: BTreeMap<SessionId, Session>` (lines 52-82 of the current file) — do not duplicate the session-resolution logic. Restructure so both the aggregate path and the new per-line path can share this resolution step (e.g. resolve once into a `Vec<(BlameLine, Option<SessionId>)>`, then branch on `lines` for how to print it).
   - For the per-line path, additionally fetch file content: `let tree = ctx.repo.tree_of(head)?;` then `let content = ctx.repo.read_blob(tree, path)?.unwrap_or_default();` (the file is known to exist at `head` at this point because `blame_file` already returned non-empty results — but handle the `None`/empty case defensively per §7). Split `content` into lines preserving the same line-numbering convention `BlameLine.line` uses (1-based, see the `line_count` doc comment in `minds-git/src/blame.rs:349-361` for how the last line without a trailing newline is still counted as a line — split accordingly, e.g. via `content.split(|&b| b == b'\n')`, and drop a single trailing empty element caused by a final `\n`, matching that same counting rule).
   - Compute the two dynamic column widths (`max_a`, `max_b` from §4.3) in one pass over the resolved rows, then format each row in a second pass.
   - Keep `short_id` as-is and reuse it for column A.
   - Update the module doc comment at the top of the file to mention the new `--lines` mode and its "no footer line" difference from §4.4.

2. **`crates/minds-cli/src/main.rs`**
   - Change the SPECS entry: `spec("blame", &[], &[], 1)` → `spec("blame", &[], &["--lines"], 1)` (line ~292).
   - Change the dispatch arm: `"blame" => blame::run(parsed.positional(0))` → `"blame" => blame::run(parsed.positional(0), parsed.has("--lines"))` (line ~631).
   - Update the USAGE text block (around line 114-117) to document the new flag, following the style of the neighboring `why`/`show` entries which document `--full` — e.g. add a line like `"  --lines   one annotated line per source line, instead of the session summary"` under the existing `minds blame <file>` description.

3. **`crates/minds-cli/src/agent_help.rs`**
   - Update the `"blame"` entry's `"usage"` field to `"minds blame [--lines] <file>"` (matching however neighboring flagged commands format optional flags in their `usage` string — check `"why"`'s entry in the same file and mirror its exact convention) and adjust `"summary"` if needed so it still reads correctly with the flag mentioned or omitted.
   - Locate and run the test that asserts `agent_help` output stays in sync with `SPECS` (search for it — likely `agent_help.rs` itself or a test file that imports both) to confirm nothing else needs updating.

## 7. Edge cases to handle

- **Line with no session** (no linked session, or all linked sessions have empty `intent.request` — same filter the aggregate mode already applies): render `-`/`-` for columns A/B, per §4.2.
- **Multiple sessions linked to the same commit**: attribute the line to the *first* session in `linked_sessions`'s returned order — this matches the existing aggregate-mode tie-break ("Mehrere Sessions am selben Commit: die Zeile der ersten zuschreiben", `blame.rs` comment near line 76). Do not introduce a different tie-break for `--lines`; the two modes must agree on which session "owns" a given commit's lines.
- **Empty file** (0 lines): `blame_file` returns an empty `Vec`, `total == 0`. Current code already errors before this (`"{path} cannot be resolved in blame (not in the commit?)"` when `lines.is_empty()`) — this is unchanged; `--lines` never sees an empty `lines` Vec because the existing early return still fires first.
- **File without a trailing newline**: the git-blame engines already count the final line correctly (`minds-git/src/blame.rs` test `a_file_without_a_trailing_newline_keeps_its_last_line`). Your content-splitting logic in step 1 must agree with this line count — verify by testing against a fixture file built the same way.
- **Binary / non-UTF8 file content**: `read_blob` returns raw `Vec<u8>`. Do not assume valid UTF-8. Use `String::from_utf8_lossy` (or print byte slices via `std::io::Write` directly) when rendering column D so a binary file doesn't panic or crash the command — a lossy/garbled rendering is acceptable, a crash is not.
- **Very long source lines**: print in full, no truncation or wrapping (§2 non-goal).
- **`--lines` combined with a missing/invalid `<file>` argument**: identical error handling to today — the flag doesn't change argument validation.
- **Rename-tracking divergence between `GixBlame`/`ShellBlame`** noted in `minds-git/src/blame.rs`'s module doc: out of scope for this spec — `--lines` consumes whatever `ctx.repo.blame().blame_file(...)` already returns, same as the aggregate mode; do not special-case renamed files.

## 8. Testing requirements

Follow this project's existing test conventions: tests live in `#[cfg(test)] mod tests` at the bottom of the same file they test (see `crates/minds-cli/src/why.rs` and `crates/minds-git/src/blame.rs` for the pattern), use `TempRepo` fixtures (`crate::fixture::TempRepo` in `minds-git`, or whatever fixture helper `minds-cli`'s existing tests for `blame`/`why`/`show` use — grep for `TempRepo` usage inside `crates/minds-cli` to find the right one before assuming), and assert on exact output strings where feasible (the project favors literal string equality over fuzzy assertions).

Add tests to `crates/minds-cli/src/blame.rs`'s existing test module (or create one if none exists there yet — check first) covering:

1. `minds blame --lines` on a file where every line has captured context — assert the exact multi-line output string, including column alignment, matches expectations for a small fixture (e.g. 2-3 lines from 1-2 sessions).
2. A file with a mix of lines with and without captured context — assert the `-`/`-` placeholder rows render correctly and no trailing "N line(s) without captured context" footer is printed (§4.4).
3. Column-width alignment: a fixture with at least two different session-id/agent-name lengths, asserting the shorter values are padded to match the longest (this is the detail most likely to be implemented wrong — test it explicitly, not just visually).
4. The default (no `--lines`) output remains byte-identical to before this change — reuse/adapt whatever test already exists for the aggregate path (if one exists; if not, add one).
5. `minds blame --lines` on a file with a line that has no trailing newline at EOF — assert the last line is still rendered (mirrors the `minds-git` blame test of the same shape).
6. A `main.rs`-level or integration-level test (if the project has CLI integration tests — check for a `tests/` directory at the `minds-cli` crate root or workspace root) verifying `--lines` is accepted by the parser and an unknown flag like `--linez` is still rejected (regression-proofing the SPECS table entry).

Also re-run whatever test keeps `agent_help.rs` and `SPECS` in sync (§6 step 3) and fix it if it fails.

## 9. Acceptance criteria

- [ ] `minds blame <file>` (no flag) output is unchanged, verified by a passing regression test.
- [ ] `minds blame --lines <file>` on a file with N lines prints exactly N annotated rows, in ascending line-number order, plus the unchanged header line.
- [ ] Lines without captured context render `-` in both the session-id and agent-name columns.
- [ ] Column A and Column B are left-padded/aligned to the widest value present in that invocation's output; Column C is right-aligned to the digit width of the file's total line count.
- [ ] No "N line(s) without captured context" footer is printed in `--lines` mode.
- [ ] `minds blame --linez <file>` (typo) is rejected by the parser with the same "unknown flag" behavior every other command's parser already gives (verifies the SPECS table change is correct).
- [ ] `agent-help`'s blame entry and the sync test both pass.
- [ ] `cargo test -p minds-cli` and `cargo test -p minds-git` both pass with no new warnings under whatever lint level the project's CI enforces (check `Cargo.toml`/CI config for `#![deny(warnings)]` or clippy gates before finishing).

## 10. Relevant invariants

This project's non-negotiable invariants (from its feature-development conventions) that apply here:

- **"Evidence vs. Ableitungen: Ableitungen sind jederzeit rekonstruierbar und betreten niemals den Capture-Pfad (`minds-capture`)."** — `minds blame --lines` is a pure read/derive operation: it reads already-committed git history and already-stored sessions, computes a view, and prints it. It must not write anything, must not touch `crates/minds-capture`, and must remain fully re-derivable from existing state (rerun it twice, get the same answer, given the same HEAD).
- **"Tolerant lesen, kanonisch schreiben."** — reading is tolerant here too: a line with a forgotten/unredacted/corrupt session must render as `-`/`-` (via the existing `Skipped`-tracking `get_skipping`/`linked_sessions` machinery), not crash or abort the whole command, exactly as the aggregate mode already behaves for the sessions it can't read.
