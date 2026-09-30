# Spec: `ToolAdapter` for Codex

Status: ready for implementation
Repo: `minds` (Rust workspace, `cargo build --release --bin minds`, Rust 1.85+)
Branch convention: `feat/<slug>` (e.g. `feat/codex-tool-adapter`), one Conventional Commit at the end
Audience: an implementer with no prior context on this conversation — everything needed is below or cited by exact file path.

## 1. Goal

Today, Codex's tool calls (file edits, shell commands) are captured but not *interpreted*: they land in `minds inspect` as "◐ OBSERVED" raw evidence instead of a structured `Effect` (what file, what kind of change). Claude Code already gets this full interpretation via a `ToolAdapter` implementation. This spec adds the equivalent `ToolAdapter` for Codex, so Codex tool calls reach the same interpretation quality as Claude Code's.

## 2. Non-goals

- No daemon, no new capture architecture, no change to the hook pipeline. This plugs into the existing `ToolAdapter` extension point (`crates/minds-capture/src/normalize.rs`) exactly the way `ClaudeAdapter` does.
- No adapters for Cursor, Gemini, or opencode — out of scope for this spec.
- No content-hash/artifact-hash changes — that machinery (`crates/minds-capture/src/adapter.rs::hash_artifacts`) already works generically off whatever `Effect` an adapter produces; nothing there needs touching.
- No changes to `minds enable`'s Codex hook *registration* (`.codex/hooks.json` + `codex_hooks = true` in `.codex/config.toml`, `crates/minds-cli/src/enable.rs`) — registration already works today and is not part of this spec.

## 3. Current state (verified against the repo)

**Registration already works.** `crates/minds-cli/src/enable.rs:1770` (`enable_codex`) writes `.codex/hooks.json` using the *same* JSON structure as Claude Code (`claude_style(root, Which::Codex)`, `enable.rs:1771`) plus the `codex_hooks = true` switch Codex requires to read `hooks.json` at all (`enable.rs:1801`, `ensure_codex_hooks_flag`). This means Codex's hook *envelope* is confirmed identical in shape to Claude Code's: `session_id`, `cwd`, `hook_event_name`, and — for tool events — `tool_name` + `tool_input`. This is not an assumption; it is exercised in three places in the test suite already:
  - `crates/minds-capture/src/normalize.rs:506` (`an_unknown_agent_still_gets_the_prompt_and_an_uninterpreted_tool`)
  - `crates/minds-capture/tests/fixtures.rs:304` (`a_foreign_agents_tool_call_survives_as_uninterpreted`)
  - `crates/minds-cli/tests/end_to_end.rs:2954` (`an_uninterpreted_call_dents_only_the_interpretation_axis`)

All three show the **exact real Codex payload shape already used in this codebase**:
```json
{"session_id":"x2","hook_event_name":"PreToolUse","tool_name":"apply_patch","tool_input":{"diff":"--- a/x"}}
```
So: Codex's file-edit tool is called `apply_patch`, and its `tool_input` carries a single field `diff` containing a **unified diff** (`--- a/<path>` / `+++ b/<path>` header lines), not a `file_path` field like Claude's `Edit`/`Write`.

**What happens today:** `crates/minds-capture/src/normalize.rs:202` (`adapter_for`) has no entry for `"codex"` in its `ADAPTERS` registry (`normalize.rs:199`, currently `&[&ClaudeAdapter]`) — confirmed by the existing test `the_registry_resolves_known_agents_and_only_those` (`normalize.rs:558`), which explicitly asserts `adapter_for("codex").is_none()`. Every Codex tool call therefore falls through to `generic_tool` (`normalize.rs:245`): the raw payload is kept verbatim as evidence (`CaptureStatus::Uninterpreted`, `adapter: "generic"`), no `Effect` is derived, no file path is known structurally.

**The reference implementation to mirror** is `ClaudeAdapter` in the same file:
- Trait impl: `normalize.rs:165-195`
- Tool→Effect mapping: `claude_effect`, `normalize.rs:343-365`
- Interpreted-vs-not allowlist: `claude_tool_is_interpreted`, `normalize.rs:294-299`
- Registered in `ADAPTERS`, `normalize.rs:199`

**Open assumption — Codex's exec/shell tool.** The repo has zero ground truth today on what Codex calls its shell-execution tool (Claude Code's equivalent is `Bash`). Public OpenAI Codex CLI documentation (outside this repo, verify before implementing) uses tool names in the `shell`/`local_shell`/`exec` family for command execution, separate from `apply_patch` for file edits. **This must be verified against a real Codex hook payload (or current Codex CLI source/docs) before finalizing the parser** — do not guess-ship this mapping without checking. If unconfirmed, ship without a specific exec-tool mapping (it falls through to `EffectKind::Other`, matching how `ClaudeAdapter` already treats any tool name it doesn't recognize — never an error, only "less detail", see `normalize.rs:341-342`).

## 4. Target state

A `CodexAdapter` registered in `ADAPTERS` (`normalize.rs:199`) such that:
- `adapter_for("codex")` returns `Some(&CodexAdapter)`.
- An `apply_patch` tool call produces `ToolFacts { name: "apply_patch", arguments: <verbatim tool_input JSON>, effect: Some(Effect { kind: EffectKind::Write, path: Some(<first file path found in the diff>), content: None }), capture: Capture { status: CaptureStatus::Interpreted, adapter: "codex", adapter_version: CODEX_ADAPTER_VERSION } }`.
- Any other/unrecognized Codex tool name falls back to `EffectKind::Other` with `CaptureStatus::Uninterpreted` — same graceful-degradation contract as `ClaudeAdapter` for tools like `Glob`/`WebFetch` (see `claude_tool_is_interpreted`, `normalize.rs:294-299`, and the test `a_known_claude_tool_is_interpreted_an_unknown_one_is_not`, `normalize.rs:569`).
- `minds inspect` then shows Codex `apply_patch` calls as fully interpreted (file path + write effect visible), not "◐ OBSERVED".

## 5. Design

New code lives in `crates/minds-capture/src/normalize.rs`, next to `ClaudeAdapter` — do not create a new file; the existing module already holds one adapter per agent plus the shared trait and registry, and the diff-parsing helper is small enough to stay local (mirrors how `claude_tool`/`claude_effect` sit next to `ClaudeAdapter`).

### 5.1 Version constant

```rust
/// Versionsstand der Codex-Deutung. Bump bei jeder Änderung an
/// `codex_effect` oder der Diff-Pfad-Extraktion (ADR-0011: Deutung ist
/// wiederholbar, eine gespeicherte Deutung bleibt ihrem Stand zuordenbar).
pub const CODEX_ADAPTER_VERSION: u32 = 1;
```
(Same pattern as `CLAUDE_ADAPTER_VERSION`, `normalize.rs:60`.)

### 5.2 The adapter struct + trait impl

```rust
/// Der Codex-Adapter.
pub struct CodexAdapter;

impl ToolAdapter for CodexAdapter {
    fn agent(&self) -> &'static str {
        "codex"
    }

    fn version(&self) -> u32 {
        CODEX_ADAPTER_VERSION
    }

    fn tool_facts(&self, event: &JournalEvent) -> Option<ToolFacts> {
        parse::<Tool>(event).and_then(codex_tool)
    }

    fn interpret_stored(&self, name: &str, arguments: &str) -> Option<StoredInterpretation> {
        let raw = RawValue::from_string(arguments.to_string()).ok();
        let effect = codex_effect(name, raw.as_deref());
        let status = if codex_tool_is_interpreted(name) {
            CaptureStatus::Interpreted
        } else {
            CaptureStatus::Uninterpreted
        };
        Some(StoredInterpretation {
            effect,
            status,
            adapter: "codex",
            adapter_version: CODEX_ADAPTER_VERSION,
        })
    }
}
```
This mirrors `ClaudeAdapter`'s two methods 1:1 (`normalize.rs:165-195`) — the `Tool` struct (`tool_name`/`tool_input` fields, `normalize.rs:382-386`) is agent-agnostic and can be reused as-is for Codex parsing (same envelope, confirmed in §3).

### 5.3 Tool → Effect mapping

```rust
/// Kennt die Codex-Deutung dieses Tools eine Wirkung?
pub fn codex_tool_is_interpreted(tool_name: &str) -> bool {
    matches!(tool_name, "apply_patch")
    // Extend once the exec/shell tool name is confirmed (see spec §3
    // "Open assumption"), analogous to how ClaudeAdapter treats "Bash".
}

/// Baut aus Codex' `tool_name` + `tool_input` einen [`ToolFacts`].
fn codex_tool(t: Tool) -> Option<ToolFacts> {
    let name = t.tool_name?;
    let arguments = t
        .tool_input
        .as_ref()
        .map(|r| r.get().to_owned())
        .unwrap_or_default();
    let effect = codex_effect(&name, t.tool_input.as_deref());
    let status = if codex_tool_is_interpreted(&name) {
        CaptureStatus::Interpreted
    } else {
        CaptureStatus::Uninterpreted
    };
    Some(ToolFacts {
        capture: Capture {
            status,
            adapter: "codex".into(),
            adapter_version: CODEX_ADAPTER_VERSION,
        },
        name,
        arguments,
        effect: Some(effect),
    })
}

/// Die agent-spezifische Abbildung Tool-Name + `tool_input` → [`Effect`] für
/// Codex.
pub fn codex_effect(tool_name: &str, tool_input: Option<&RawValue>) -> Effect {
    match tool_name {
        "apply_patch" => {
            let diff = tool_input
                .and_then(|r| serde_json::from_str::<CodexPatch>(r.get()).ok())
                .and_then(|p| p.diff);
            let path = diff.as_deref().and_then(first_diff_path);
            Effect {
                kind: EffectKind::Write,
                path,
                content: None,
            }
        }
        _ => Effect {
            kind: EffectKind::Other,
            path: None,
            content: None,
        },
    }
}

/// Codex' `apply_patch`-Eingabe, nur das interessante Feld.
#[derive(Debug, Default, Deserialize)]
struct CodexPatch {
    diff: Option<String>,
}

/// Der erste Dateipfad aus einem Unified Diff (`--- a/<path>` /
/// `+++ b/<path>`-Kopfzeilen). Codex kann in einem `apply_patch`-Aufruf
/// mehrere Dateien ändern; wie bei Claudes `Effect` (ein Pfad pro Aufruf,
/// vgl. `claude_effect`) trägt der Adapter nur den ersten — eine benannte
/// Grenze, keine falsche Aussage: der Aufruf bleibt vollständig als
/// Roh-Beweismittel in `arguments` erhalten.
///
/// Bevorzugt die `+++ b/`-Zeile (Zielpfad nach der Änderung); fehlt sie,
/// die `--- a/`-Zeile. `/dev/null` (reine Löschung) wird übersprungen.
fn first_diff_path(diff: &str) -> Option<String> {
    fn strip_prefix_path(line: &str, marker: &str) -> Option<String> {
        let rest = line.strip_prefix(marker)?.trim();
        if rest == "/dev/null" {
            return None;
        }
        // Unified-Diff-Pfade tragen ein a/ bzw. b/ Präfix.
        let path = rest.strip_prefix("a/").or_else(|| rest.strip_prefix("b/")).unwrap_or(rest);
        Some(path.to_string())
    }

    let plus = diff.lines().find_map(|l| strip_prefix_path(l, "+++ "));
    plus.or_else(|| diff.lines().find_map(|l| strip_prefix_path(l, "--- ")))
}
```

### 5.4 Registry

```rust
const ADAPTERS: &[&dyn ToolAdapter] = &[&ClaudeAdapter, &CodexAdapter];
```
(`normalize.rs:199`, one-line change.)

## 6. Implementation plan (ordered)

1. In `crates/minds-capture/src/normalize.rs`: add `CODEX_ADAPTER_VERSION`, `CodexAdapter`, `codex_tool_is_interpreted`, `codex_tool`, `codex_effect`, `CodexPatch`, `first_diff_path` as specified in §5.
2. Add `&CodexAdapter` to the `ADAPTERS` array (`normalize.rs:199`).
3. Export `CodexAdapter` (and `CODEX_ADAPTER_VERSION` if any downstream code needs it) from `crates/minds-capture/src/lib.rs` the same way `ClaudeAdapter`/`CLAUDE_ADAPTER_VERSION` are already exported (check `lib.rs:59` for the current `pub use normalize::{...}` line and extend it).
4. Update the existing test `the_registry_resolves_known_agents_and_only_those` (`normalize.rs:558-566`) — `adapter_for("codex").is_none()` must become `.is_some()`, and add the equivalent version assertion for `CodexAdapter` that already exists for `ClaudeAdapter`.
5. Update `a_foreign_agents_tool_call_survives_as_uninterpreted` (`crates/minds-capture/tests/fixtures.rs:293`) — this test's premise ("codex is a foreign agent with no adapter") is now false. Either remove it or repurpose it to cover a genuinely unknown third agent (rename the fixture agent from `"codex"` to something like `"some-other-agent"` so the *behavior under test* — graceful uninterpreted fallback — still has coverage for agents without an adapter).
6. Update `an_uninterpreted_call_dents_only_the_interpretation_axis` (`crates/minds-cli/tests/end_to_end.rs:2938`) similarly — its premise is now false for Codex specifically. Either retarget it to a different unknown agent, or add a new sibling test asserting Codex *is* now fully interpreted (`Interpretation complete`, not the uninterpreted-axis message) — see §9 below.
7. Check `crates/minds-tui/src/view/tests.rs:761` (uses `name: "apply_patch"`) — read the surrounding test to see if it currently asserts "uninterpreted" display for `apply_patch`; if so, update the expectation once Codex is interpreted.
8. Update `README.md`'s agent-support table (`README.md:88-90`) — move Codex out of the "hooks register, prompt is captured; tool calls are stored as raw evidence" row into a row matching Claude Code's "complete" status (or a new intermediate wording if tool-call coverage is narrower than Claude's — see §3's open assumption about the exec/shell tool). Same for `docs/for-testers.md:157` and `docs/pilot-guide.md:104`, which state the same thing.
9. Run `cargo test --workspace` and fix any other test that hardcodes "codex has no adapter" as an assumption (search `grep -rn '"codex"' crates/ --include=*.rs` for anything missed).

## 7. Fail-closed / fail-open requirements

Two invariants, both **non-negotiable** for this project (see `.claude/skills/feature/SKILL.md`):

- **Hooks stay fail-open.** `minds hook` (`crates/minds-cli/src/hook.rs`) always returns exit 0, wraps everything in `catch_unwind`, and does nothing expensive (rule 3 in `hook.rs:24-27`). This spec's new code (`codex_tool`, `codex_effect`, `first_diff_path`) runs **only** at checkpoint time (`crates/minds-capture/src/adapter.rs`, via `normalize::facts`), never inside `minds hook` itself — same cold-path placement as `claude_tool`/`claude_effect` already have. Do not call any of this new code from `hook.rs`.
- **Redaction stays fail-closed.** This adapter must never itself decide what's safe to emit — it only extracts structure (a file path string) from already-captured evidence; the existing redaction pipeline (`crates/minds-redact`) runs downstream on the built `Session` regardless of which adapter produced it, unchanged by this spec. `arguments` must stay the **verbatim** `tool_input` JSON (never reformatted/re-serialized) — same rule `ClaudeAdapter` follows (`normalize.rs:71-74`) — so the later hash-stability/canonical-form guarantees hold.
- `first_diff_path` must **never panic** on malformed/empty diff text — it returns `None` for anything it can't parse (empty string, no `---`/`+++` lines, binary-diff markers, etc.). Add a test for at least one malformed input (see §9).

## 8. Testing requirements

Mirror the existing `ClaudeAdapter` test suite in `normalize.rs`'s `#[cfg(test)] mod tests` (`normalize.rs:396-600`) — for each Claude-side test, add the Codex equivalent:

| Claude test (existing) | Codex equivalent (new) |
|---|---|
| `a_read_becomes_a_read_effect_with_path` (`normalize.rs:428`) | `apply_patch_becomes_a_write_effect_with_path` — payload `{"tool_name":"apply_patch","tool_input":{"diff":"--- a/src/retry.rs\n+++ b/src/retry.rs\n@@ ...\n"}}"`, assert `EffectKind::Write`, `path == Some("src/retry.rs")` |
| `an_unknown_tool_is_other_not_an_error` (`normalize.rs:469`) | a Codex tool name that isn't `apply_patch` (e.g. `"unknown_tool"`) yields `EffectKind::Other`, no panic |
| `arguments_keep_the_raw_tool_input` (`normalize.rs:481`) | same assertion for Codex: `arguments` contains the raw diff text unmodified |
| `interpretation_is_deterministic` (`normalize.rs:536`) | same two-calls-equal assertion for `CodexAdapter.interpret_stored` |
| `a_known_claude_tool_is_interpreted_an_unknown_one_is_not` (`normalize.rs:568`) | `apply_patch` → `Interpreted`; some other Codex tool name → `Uninterpreted` |
| (new, no Claude equivalent) | `first_diff_path` unit tests: multi-file diff returns the *first* file's path; a diff touching only `/dev/null` (pure delete via `--- a/x` + `+++ /dev/null`) returns the `a/` path since the `+++` side is skipped; empty string returns `None`; a diff with only `--- a/x` and no `+++` line still returns `Some("x")` |

Also add/adjust the integration-level fixtures per §6 steps 5–6 (`crates/minds-capture/tests/fixtures.rs`, `crates/minds-cli/tests/end_to_end.rs`) so there is at least one end-to-end test proving a real `minds hook --agent codex` → `minds checkpoint` → `minds verify`/`minds inspect` round-trip shows `apply_patch` as interpreted, not observed — follow the exact structure of `an_uninterpreted_call_dents_only_the_interpretation_axis` (`end_to_end.rs:2938-2970`) but assert the *interpreted* outcome instead.

## 9. Acceptance criteria

- [ ] `adapter_for("codex")` returns `Some`, with `version() == CODEX_ADAPTER_VERSION`.
- [ ] A journal built from a Codex `PreToolUse` event with `tool_name: "apply_patch"` and a unified-diff `tool_input.diff` produces a `ToolCall` with `capture.status == Interpreted`, `capture.adapter == "codex"`, and `effect == Some(Effect { kind: Write, path: Some(<extracted path>), content: None })`.
- [ ] A Codex tool call with an unrecognized `tool_name` still produces a `ToolCall` (never silently dropped), `capture.status == Uninterpreted`, `effect.kind == Other` — no panic, no error.
- [ ] `first_diff_path` never panics on malformed input; covered by a dedicated unit test.
- [ ] `cargo test --workspace` passes, including the updated/retargeted tests from §6 steps 5–6.
- [ ] `minds inspect` on a repo with real Codex hook activity shows `apply_patch` calls as fully interpreted (not "◐ OBSERVED") — verify manually with `minds enable --agent codex` in a scratch repo plus a synthetic `minds hook --agent codex` call (pattern: `end_to_end.rs:2938-2970`).
- [ ] README.md / docs/for-testers.md / docs/pilot-guide.md agent-support tables updated to reflect Codex's new status (§6 step 8).
- [ ] The open assumption about Codex's exec/shell tool name (§3) has been explicitly checked against current Codex CLI documentation before this is considered done — either a mapping was added with confirmed ground truth, or the spec's follow-up note was left in place for a later spec rather than silently guessed.
