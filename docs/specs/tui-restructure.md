# Spec: `minds inspect` — persistent split view

Status: ready for implementation
Crate: `minds-tui`
Depends on: nothing outside `minds-tui` (no reader/store/core changes)

## Goal

Today, `minds inspect` shows either the session list (Activity) **or** the
drilled-in detail (Graph/Why/Evidence) — never both. Selecting a session
(`Enter`) replaces the whole screen; getting back to the list costs one or
more `Esc`. The user loses the list context every time they look at a
session's detail, and there is no way to skim through sessions and see
their graph update live, the way a file list + preview pane works in most
TUIs (and the way `git-ai`'s and `prtui`'s list+detail layouts work).

This spec makes the session list **permanently visible** in a left column,
with a live-updating detail pane on the right showing the graph of
whatever session is currently under the cursor — no `Enter` required to see
it. Drilling further (subagent, why-chain, evidence report) still works
exactly as today, just rendered in the narrower right column instead of
full-screen.

## Non-goals

- No new data, no new views, no changes to `minds-reader`, `minds-core`, or
  the evidence chain. This is a rendering/layout change only.
- No new TUI framework or major dependency. Stays on `ratatui` 0.30 /
  `crossterm` 0.29 (already in the workspace `Cargo.toml`).
- No change to the `App` state shape's core mechanics (`views: Vec<View>`
  stack, `reduce()` dispatch) — see "Why this design" below for why that
  stack is deliberately left alone.
- No change to what each view (Graph/Why/Evidence) computes or displays —
  only how much horizontal space it gets and whether the list is visible
  alongside it.
- Per-agent color in the session list (below) is the only visual-language
  addition; no other glyph/color scheme changes.

## Current structure (as of this spec)

All paths relative to `crates/minds-tui/src/`.

- **`app.rs`** — `App<'a>` holds `cards: Vec<SessionCard>`, `visible: Vec<usize>`
  (filtered indices into `cards`), `cursor: usize` (index into `visible`),
  and `views: Vec<View>` — a stack of full-screen overlays. `View` is an enum
  with three variants: `Graph { id, graph, rows, cursor, timeline }`,
  `Why { chain, cursor, edge, inspector }`, `Evidence { id, report,
  uninterpreted, cursor }`. `App::top()` returns `self.views.last()`.
  `reduce(action)` dispatches on `self.views.pop()`: `None` routes to
  `reduce_activity` (moves `self.cursor` in the list, or on `Enter`/`w`/`e`
  calls `push_graph`/`push_why`/`push_evidence` to push a new `View`);
  `Some(view)` routes to that view's own action handling, which may itself
  push further views (e.g. `Enter` on a `Subagent` node inside `Graph`
  pushes another `Graph`; `Enter` on a `Commit`/`Change` node pushes `Why`).
- **`view/mod.rs`** — `draw(frame, app)` splits the screen into `head` (2
  rows), `body` (the rest), `foot` (2 rows) via
  `Layout::vertical([Length(2), Min(1), Length(2)])`. It sets
  `app.page` (rows per page) from `body.height`, then matches
  `app.top()`: `None` → `activity::draw(frame, app, body)` (full `body`
  width); `Some(View::Graph{..})` → `graph::draw(frame, app, body, ...)`
  (full `body` width); same for `Why`/`Evidence`. **This is the exact
  match arm this spec changes.**
- **`view/activity.rs`** — draws the session table (`SESSIONS` block) into
  whatever `Rect` it's given: columns `TIME · SESSION · AGENT · [SIZE] ·
  SEAL · VERDICT`, `SIZE` column only shown when `area.width >= 120`
  (`show_size` flag). Already handles arbitrary widths and empty states.
- **`view/graph.rs`**, **`view/why.rs`**, **`view/evidence.rs`** — each
  takes a `Rect` and renders into it; none assume a specific width today
  beyond internal `Layout::vertical` splits and text clipping via
  `view::clip`. They already degrade gracefully on narrow terminals (e.g.
  `why.rs`'s inspector/gaps panels only render `if area.height >= N`).
- **`theme.rs`** — one function per "meaning axis" (evidence source+status,
  provenance, claim, verdict, tool kind, node kind), each returning
  `(glyph, word, Style)`. Explicit rule in the module doc: "Farbe trägt nie
  allein" (color never carries alone) — every color has a glyph and a word
  next to it. `AGENT` (`Color::Magenta`) is currently one fixed color for
  *all* agents; there is no per-agent-name color.
- **`input.rs`** — pure `KeyEvent -> Action` mapping, unaware of layout.
  **Not touched by this spec** (see below).
- **`view/tests.rs`** — render-probe tests: build a `ratatui::Terminal` over
  `ratatui::backend::TestBackend`, call the view's `draw` function, then
  assert on substrings of the rendered buffer's lines (not full-buffer
  equality — layout may shift, the content assertions must not). Fixtures
  build `Inspection`/`Session`/`SessionCard` values by hand plus a
  throwaway `git init`'d temp repo via a local `repo()` helper.

## Why this design (not a full prtui-style panel/tab rewrite)

`minds-tui`'s `View` stack is a deliberate, tested state machine: pushing a
`Graph` when you drill into a subagent, or a `Why` when you drill into a
commit from inside a `Graph`, lets the user go many levels deep (subagent →
its own graph → a commit inside that → its why-chain → an evidence link →
the why-chain of *that* commit) and walk back out one `Esc` at a time. That
is a genuinely different requirement from `prtui`'s flat `MainTab` (Diff /
Conversation / Timeline / Claude / Comments) — those are five fixed facets
of *one* selected PR, not an arbitrarily deep drill-down across sessions
and commits. Replacing the stack with a tab enum would either lose that
depth or reinvent the stack inside the tab. So: **keep the stack
mechanics and `reduce()` exactly as they are** (no changes to `app.rs`'s
action handling, no changes to `input.rs`). The only change is *rendering*:
show the list next to whatever is on top of the stack, instead of instead
of it.

## Target structure

### Layout (terminal width ≥ 100 columns)

```
┌─ SESSIONS (12) ─────────────┬─ SESSION b3-e4f5a6b7c8d…  GRAPH ──────────┐
│ TIME   SESSION       AGENT  │ claude-code · opus · 25.07. 14:10Z · ...  │
│ 14:10Z >Fix retry     ●     │ ┌ YOU ───────────────────────────────┐   │
│ 13:41Z  Add backoff   ◆     │ │ Fix retry handling for 429 errors  │   │
│ ...                          │ └─────────────────────────────────────┘  │
│                              │ ● Fix retry handling                     │
│                              │ ┗━ ◉ claude-code · opus                  │
│                              │    ┣━ ◇ READ src/retry.rs                │
│                              │    ┣━ ✎ EDIT src/retry.rs                │
│                              │    ┗━ ▶ EXEC cargo test                  │
├──────────────────────────────┴───────────────────────────────────────────┤
│ ↑↓ select  Enter descend  w why  e evidence  1·2·3 zoom  ? help  q quit  │
└────────────────────────────────────────────────────────────────────────┘
```

- Left column: fixed width, `Constraint::Length(42)` (enough for the
  `TIME · SESSION · AGENT` columns without `SIZE`; the list never shows the
  `SIZE` column in split mode — there isn't room, and it was already
  conditional on width). Always renders `activity::draw`.
- Right column: `Constraint::Min(1)` (the remainder). Renders:
  - If `app.views` is empty: the **Graph of `app.selected()`**, i.e. call
    the same graph-building/rendering path as `push_graph` would, but
    *without* pushing onto the stack — a read-only live preview that
    updates every time `cursor` moves. See "Implementation plan" for how
    to do this without duplicating `push_graph`'s state.
  - If `app.views` is non-empty: `app.top()` (`Graph`/`Why`/`Evidence`) as
    today, just rendered into the narrower right-column `Rect` instead of
    the full `body`.
  - If `app.visible` is empty (no sessions / no search match): the right
    column stays blank (existing empty-state text already renders in the
    left column via `activity::draw`; do not duplicate it on the right).

### Narrow-terminal fallback (terminal width < 100 columns)

Below 100 columns, fall back to **exactly today's behavior**: the list (or
top-of-stack view) takes the full `body` width, nothing side by side. This
mirrors the existing precedent in `activity.rs` (`show_size = width >=
120`) and in `why.rs` (inspector/gaps panels gated on `area.height`).
Rationale: `why.rs`'s gaps panel needs `area.height < 14` to hide itself
entirely, and its lines already assume enough width to not be clipped to
uselessness — squeezing it into ~55 columns (100 minus a 42-wide list column)
is worse than a temporary full-screen view. Use `body.width` (the whole
body rect, not `frame.area()`, since `head`/`foot` are fixed height, not
width) as the threshold.

### Per-agent color in the session list

Add one new `theme.rs` function:

```rust
/// A small, fixed, deterministic palette so distinct agents are visually
/// distinct in the session list — the word (agent name) is always printed
/// alongside, so this never becomes the only signal (see module doc).
const AGENT_PALETTE: [Color; 6] = [
    Color::Magenta,      // index 0 — same as today's single AGENT color
    Color::Cyan,
    Color::Indexed(214), // amber
    Color::Green,
    Color::Indexed(75),  // sky blue
    Color::Indexed(212), // pink
];

/// Deterministic color for an agent name — same name always gets the same
/// color within one run (and across runs, since it's a pure function of
/// the string). Not cryptographic; a simple FNV-1a-style fold is enough.
pub fn agent_color(name: &str) -> Color {
    let mut hash: u32 = 2166136261;
    for b in name.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    AGENT_PALETTE[(hash as usize) % AGENT_PALETTE.len()]
}
```

Use it in `view/activity.rs::row_of` for the `AGENT` cell:

```rust
Cell::from(Span::styled(
    clip(&card.summary.actor, 22),
    base.fg(theme::agent_color(&card.summary.actor)),
)),
```

(Currently: `base.fg(theme::AGENT)`.) Leave every other use of
`theme::AGENT` (graph header, node glyphs, `theme::lane`) unchanged — those
represent "this is agent territory" as a class, not a specific agent, and
changing them is out of scope. `card.summary.actor` is already the
human-readable "agent · model" string used today; hash on that exact
string so the same agent+model combination always gets the same color.

### Keybindings

No change. `Tab`/panel-focus-switching is **not** introduced — the list
always has focus for `Up`/`Down`/`Enter`/`w`/`e`/search when `views` is
empty (exactly as today), and the top-of-stack view has focus when `views`
is non-empty (exactly as today). The only behavioral difference the user
sees is that the *previously invisible* live graph preview is now visible
before they press `Enter`.

## Implementation plan

1. **`crates/minds-tui/src/view/mod.rs`** — replace the single `match
   app.top() { ... }` block (lines ~39–60 today) with:
   - Compute `let split = body.width >= 100;`
   - If `split`: `let [list_area, detail_area] =
     Layout::horizontal([Constraint::Length(42), Constraint::Min(1)]).areas(body);`
     then always call `activity::draw(frame, app, list_area)`, and in
     `detail_area` render either the live graph preview (new helper, see
     step 2) when `app.top().is_none()`, or the existing
     `graph::draw`/`why::draw`/`evidence::draw` call (unchanged arguments)
     when `app.top()` is `Some(..)`.
   - If `!split`: keep exactly today's code path (single `body`-width
     render, `None => activity::draw(frame, app, body)`, `Some(view) =>`
     the matching full-width draw call).
   - `app.page` calculation (today: `body.height - 2or3`) stays based on
     `body.height` — height doesn't change with the split, only width, so
     no change needed there. Double check `graph::draw`'s use of
     `area.width` (used for `clip()` calls) — it already reads `area.width`
     from whatever `Rect` it's passed, so passing `detail_area` instead of
     `body` is sufficient; no signature changes needed in `graph.rs`,
     `why.rs`, or `evidence.rs`.

2. **Live graph preview without pushing state.** `App::push_graph` builds a
   `View::Graph` and pushes it onto `self.views`. For the live preview we
   need the *same rendering*, but must not mutate `self.views` (that would
   break `Esc`/`Back` semantics — an unpushed preview must not require an
   extra `Esc` to leave). Two options; pick (a):
   - **(a) Preferred:** add a read-only helper `App::preview_graph(&self,
     id: SessionId) -> Option<(SessionGraph, Vec<layout::Row>)>` that does
     what `push_graph` does (`self.inspection.graph(id)`, then
     `layout::rows(&graph, self.zoom)`) but returns the value instead of
     pushing it, with `cursor: 0` and `timeline: false` fixed (the preview
     is not independently navigable — only the pushed view is). In
     `view/mod.rs`, when `split && app.top().is_none()`, call
     `app.selected()` to get the `SessionId`, then
     `app.preview_graph(id)`, then call `graph::draw(frame, app,
     detail_area, id, &rows, 0, false)` — the exact same function
     `push_graph`'s result would use, just with a throwaway `cursor`/
     `timeline`. If `app.selected()` is `None` (empty list) or
     `preview_graph` returns `None` (unreadable session), render nothing
     in `detail_area` (or a one-line placeholder — see edge cases).
   - Do **not** cache the preview in `App` — recomputing `layout::rows` on
     every redraw for the *currently selected* card only (not all cards)
     is O(session size), same cost `push_graph` already pays on `Enter`,
     and redraws are already tick-gated at 250 ms (`app.rs`'s `TICK`
     constant) — no measurable cost.

3. **`crates/minds-tui/src/theme.rs`** — add `AGENT_PALETTE` and
   `agent_color()` as specified above, near the existing `pub const AGENT:
   Color` definition.

4. **`crates/minds-tui/src/view/activity.rs`** — in `row_of`, change the
   `AGENT` cell's style from `base.fg(theme::AGENT)` to
   `base.fg(theme::agent_color(&card.summary.actor))` (keep `base` — the
   degraded-row dimming must still apply; `Style::fg` on an already-dimmed
   base should still read as dimmed, same as today's pattern elsewhere in
   this file, e.g. the `session` `Line` above it).

5. **Footer/help text** (`view/mod.rs::footer`, `view/help.rs::TEXT`) — no
   functional change needed (keybindings are unchanged), but reword the
   `None` case's footer hint if it still says something like "Enter graph"
   implying the graph isn't visible yet. Check the exact current string in
   `footer()`'s `keys` match (`None => "↑↓ select  Enter graph  w why  e
   evidence  ..."`) and change `"Enter graph"` to `"Enter descend"` for
   consistency with the `Graph` case's existing wording, since Enter no
   longer *reveals* the graph (it's already visible) — it now means
   "descend into the row under the cursor" (subagent, commit, etc.), which
   is what it already does once you're inside a pushed `Graph` view.

## Edge cases

- **Empty session list** (`app.cards.is_empty()`): `activity::draw` already
  renders its own empty-state message in `list_area`; leave `detail_area`
  blank (a `Paragraph::default()` or simply skip rendering — `ratatui`
  leaves unrendered areas as the terminal's default background).
- **Search matches nothing** (`app.visible.is_empty()` but
  `app.cards` non-empty): same as above — `activity::draw` shows its "No
  match for the search" text in `list_area`; blank `detail_area`.
- **Selected session unreadable/degraded** (`card.is_degraded()`):
  `app.selected()` still returns the card, but `preview_graph` should
  check `card.is_degraded()` first and return `None` without calling
  `self.inspection.graph(id)` (mirrors `reduce_activity`'s existing
  `.filter(|c| !c.is_degraded())` guard on `Enter`/`w`/`e` — a degraded
  card has no graph to show). Render a one-line placeholder in
  `detail_area` in this case: reuse the same "Degraded: the payload is
  unreadable…" sentence already used in `footer()` for this state.
  Requires reading `mod.rs`'s existing degraded-status string to avoid
  duplicating it verbatim in two places — factor it into a small `const`
  or function both call, or simply call the existing footer-building logic
  (`app.selected().filter(is_degraded)` branch) as a shared helper.
- **Very long session history** (hundreds of cards): unaffected —
  `activity::draw` already scrolls/pages via `offset()`; the split only
  changes the column width passed to it (`list_area.width`, fixed at 42
  instead of full `body.width`), which only affects whether the `SIZE`
  column shows (it won't, `list_area.width` < 120) and how much the
  `SESSION` headline gets clipped to (`headline_w` computation in
  `activity.rs` already derives from `table_area.width`, so it adapts
  automatically — no change needed there).
- **Narrow terminal (< 100 cols)**: falls back to full-width single-pane,
  exactly today's behavior (see "Narrow-terminal fallback" above). Verify
  the threshold against `why.rs`'s and `evidence.rs`'s own internal width
  assumptions — neither hardcodes a minimum below which they render
  garbage, both already clip/wrap, so no further changes needed there.
- **Terminal resize mid-session**: already handled — `app.rs`'s `run()`
  loop redraws every `TICK` (250 ms) or on key input, and `view::draw` is
  called fresh every frame with the current `frame.area()`, so the
  split/no-split decision re-evaluates every redraw automatically. No new
  resize-handling code needed.
- **`NO_COLOR` / monochrome terminals**: `agent_color()` only adds a
  `Style::fg`; the agent name text and existing glyphs are unaffected, so
  the "glyph and word carry meaning, color is decoration" rule still holds
  for this addition specifically — it was never meaning-bearing on its own
  (unlike `theme::evidence`'s glyph+word+color triple), it's purely a
  skimming aid layered on top of already-fully-legible information.

## Testing requirements

Follow the existing `view/tests.rs` pattern (`ratatui::Terminal` over
`TestBackend`, assert on rendered-line substrings, reuse the file's
`session()`/`filled()`/`repo()` fixture helpers — read the full file before
writing new tests, only the first ~130 lines were inspected while writing
this spec).

Add tests for:
1. `split` mode (backend width ≥ 100): rendering `draw(frame, &mut app)`
   with `app.views` empty shows **both** a `SESSIONS` block substring
   *and* the selected card's graph content (e.g. the headline text) in the
   same frame — this is the core behavior change, assert both are present
   simultaneously (today's equivalent test, if any exists, likely asserts
   only one or the other because they were never simultaneous).
2. `split` mode with `app.views` non-empty (e.g. after
   `app.reduce(Action::Enter)` then a `Why`/subagent push): the `SESSIONS`
   block substring is *still* present alongside the pushed view's content
   — proves the list doesn't disappear on drill-down.
3. Narrow mode (backend width < 100, e.g. 80): same setup as (1) — assert
   the `SESSIONS` block is present but the graph content is *not* in the
   same frame (single-pane fallback), matching today's behavior.
4. Degraded selected card in split mode: assert the placeholder text
   appears in `detail_area` and no panic/`unwrap` on a `None` graph.
5. Empty `app.cards` in split mode: assert no panic, empty-state message
   present, no graph content.
6. `theme::agent_color`: pure unit test (no rendering) — same name yields
   the same color across two calls; two different names are *not required*
   to differ (palette has only 6 entries, collisions are expected and
   fine) but assert the function is total (never panics) for at least one
   empty-string input (`agent_color("")`) since `card.summary.actor` could
   theoretically be empty for a degraded/legacy card.
7. `view/activity.rs::row_of`: existing tests (if any assert on the
   `AGENT` cell's style being exactly `theme::AGENT`) will need updating
   to assert `theme::agent_color(name)` instead — check for this before
   assuming all existing tests still pass unmodified.

Run `cargo test -p minds-tui` and confirm the full existing suite still
passes unmodified except for the one `AGENT`-color assertion noted in (7).

## Acceptance criteria

- [ ] `minds inspect` in a terminal ≥ 100 columns wide shows the session
      list and the selected session's graph simultaneously, with no
      `Enter` needed.
- [ ] Moving the list cursor (`↑`/`↓`/`j`/`k`) updates the graph preview
      live, with no additional keypress.
- [ ] `Enter`, `w`, `e` still work exactly as before (pushing
      Graph/Why/Evidence views), now rendered in the right column instead
      of full-screen, and the list stays visible while any of them is on
      top of the stack.
- [ ] `Esc`/`Back` pops the view stack exactly as before; from the empty
      stack, `Esc` still clears the search or quits, unchanged.
- [ ] Terminals narrower than 100 columns behave exactly as `main` does
      today (single full-width pane, no regression).
- [ ] Agents with different names/models show visibly different colors in
      the `AGENT` column of the session list; the agent name text is
      always present regardless of color support (`NO_COLOR`-safe).
- [ ] `cargo test -p minds-tui` passes.
- [ ] `cargo clippy -p minds-tui -- -D warnings` passes (repo-wide CI gate
      — check `xtask`/CI config for the exact invocation used elsewhere in
      the repo before assuming this flag set is correct).
- [ ] No changes outside `crates/minds-tui/`.

## Notes for the implementing session

- This spec was written without running `minds inspect` interactively (no
  populated session history was available in the working environment at
  spec-writing time) — the layout mockup and edge-case list are derived
  entirely from reading `app.rs`, `layout.rs`, `lib.rs`, `input.rs`,
  `theme.rs`, `filter.rs`, and every file under `view/` in full, plus the
  first ~130 lines of `view/tests.rs` for test conventions. **Before
  implementing, run `cargo run -p minds-cli -- inspect` (or build a small
  fixture repo with a couple of captured sessions) to visually confirm the
  mockup reads correctly at a real terminal size, and adjust the exact
  `Constraint::Length(42)` list-column width and the `100`-column split
  threshold by eye if they look cramped or wasteful — those two numbers
  are reasonable starting points, not hard requirements.**
- Read `view/tests.rs` in full (885 lines) before adding tests — only its
  first ~130 lines (fixture setup) were read while writing this spec; the
  actual per-view test bodies were not inspected and may already cover
  some of the cases listed above under different names.
