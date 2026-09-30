# EA-14 — Intent anchor: the chain starts at an approved requirement

- Commit: `feat(core): intent anchor — versioned requirement as the session's first chain link`
- Branch: `feat/intent-anchor`
- Depends on: EA-06c (EA-05 kind `intent-activate`) · Size: M · Demo: yes

## Goal
A session can start with a chained reference to a specific, content-addressed version of
a requirement. The anchor format, its storage, and how the witness puts it at the head of
a session's chain. Signing and CLI are EA-15.

## Non-goals
CLI (`bind`/`sign`, EA-15), GitLab fetching (EA-16), scope findings (EA-17).

## Read first
ADR-0012 decision 5, `crates/minds-core/src/evidence.rs` (seal text format and its
fail-closed parser — mirror that style), `crates/minds-core/src/attest.rs`
(`check_single_line`), `crates/minds-store` (ref-per-object layout), EA-06c.

## Formats
Anchor text (exactly 4 lines, LF, trailing LF, parser fail-closed like the seal):
```text
minds-intent-v1
source=file:<repo-relative path>@<git blob sha> | issue:<project>#<iid>@<updated_at RFC3339> | prompt
content=b3-<64hex>
scope=<glob>[,<glob>…] | -
```
- `content = derive_key("minds/intent/v1/content", redacted snapshot bytes)` — computed
  over the **redacted** snapshot so it is recomputable from stored material and never an
  oracle for redacted secrets.
- `anchor_id = derive_key("minds/intent/v1/anchor", anchor text)`.
- Snapshot: the requirement text after the redaction pipeline (fail-closed; rejection →
  no anchor, clear error). File source: file bytes as UTF-8 (reject non-UTF-8). Issue
  source: RFC 8785 canonical JSON `{"description":…,"title":…}` (EA-16 produces it).

## Storage
`refs/minds/intents/<64hex anchor_id>` → parentless commit, tree: `anchor` (text),
`snapshot` (redacted), optional `anchor.sig`. Idempotent per id. Synced by `minds sync`.

## Chain integration (witness)
- The witness keeps one "active intent" (from `IntentActivate` frames; persisted in its
  home). When the first hook event of a new `SessionKey` arrives, the witness appends a
  synthetic event **before** it: `raw_kind = "minds.intent"`, payload
  `{"anchor_id":"b3-…","anchor":"<text>","signature":"<armored>"|null}` — observed by the
  witness, chained like any event, so the session's chain starts at the intent.
- An activation received **during** a running session is appended to that session as a
  further `minds.intent` event (the reader reports "intent changed mid-session").
- A1 path (no witness): `minds intent` (EA-15) writes `<git-dir>/minds/intent/active`;
  checkpoint adds the anchor id to the session envelope as an additive field
  `intent_anchor: Option<ContentHash>` (weaker: not chained, the agent can edit the file;
  EA-11 treats it as `bound (unchained)`).

## Reader
`intent_of(session) -> IntentState` with `Unbound | Bound { anchor_id, chained: bool,
signature: SigState, snapshot_matches: bool, changed_mid_session: bool }`.

## Acceptance criteria
- [ ] Anchor text golden tests; parser rejects every malformed variant (missing line, extra line, control chars, bad hash, empty scope).
- [ ] Witnessed session with active intent → first chain item is the `minds.intent` event.
- [ ] Activation mid-session → second `minds.intent` event, reader flags it.
- [ ] Snapshot with a secret → redacted before hashing and storing; the raw secret never reaches the store (corpus test).
- [ ] Legacy sessions → `Unbound`, serialization unchanged.

## Tests
`intent_anchor_text_golden`, `intent_anchor_parser_is_fail_closed`,
`witness_prepends_intent_to_new_session`, `intent_change_mid_session_is_flagged`,
`intent_snapshot_is_redacted_before_hash`, `legacy_session_intent_unbound`.
