# EA-05 — Witness wire protocol `minds-witness-v1`

- Commit: `feat(capture): witness wire protocol — framed hook payloads over a Unix socket`
- Branch: `feat/witness-protocol`
- Depends on: — · Size: S · Demo: yes

## Goal
A small, versioned, fuzz-hardened framing used between `minds hook` / `minds checkpoint`
(agent side) and `minds witness` (host side). Encoding and decoding only — no sockets here.

## Non-goals
No daemon, no client I/O (EA-06c/EA-07). No authentication: the agent side can always
send frames (ADR-0012 decision 2). No encryption (local socket).

## Read first
`crates/minds-capture/src/hook_event.rs` (what the hook currently parses from stdin),
`crates/minds-capture/src/journal.rs` (`NewEvent`), stdin size caps in `minds-cli/src/hook.rs`.

## Format
```text
magic   8 bytes  "MWIT\x00\x00\x00\x01"   (protocol version 1 in the last byte)
length  u32 LE   payload length, ≤ MAX_FRAME (8 MiB — equal to the hook's stdin cap)
kind    u8       0x01 hook · 0x02 checkpoint-request · 0x03 ping · 0x04 intent-activate
body    bytes    kind-specific, see below
```
- `hook` body: `agent` (u16-LE length + UTF-8, ≤ 64 bytes), `event_override`
  (u16-LE length + UTF-8, may be empty), `stdin` (the remaining bytes, verbatim — the raw
  harness payload after the secret wall ran on the agent side).
- `checkpoint-request` body: `commit` (u16-LE length + 40/64 hex or empty for HEAD),
  `request_id` (16 random bytes).
- `ping` body: empty.
- `intent-activate` body: `anchor` (u16-LE length + the `minds-intent-v1` text, ≤ 4 KiB),
  `signature` (u32-LE length + armored ssh signature, may be empty), `request_id` (16 bytes).
  Answered with `ack`/`nack`. (Consumed by EA-14; the witness accepts any well-formed
  anchor — signatures are judged at verify time, never at capture time.)
- Responses (witness → client) use the same framing with kinds `0x81 ack` (body:
  `request_id` + UTF-8 status line ≤ 4 KiB) and `0x82 nack` (body: `request_id` + reason).
  Hook frames get **no** response (fire-and-forget; the hook must not wait).

## API
```rust
// minds-capture/src/witness_proto.rs
pub const MAGIC: [u8; 8]; pub const MAX_FRAME: u32;
pub enum Frame { Hook { agent: String, event_override: Option<String>, stdin: Vec<u8> },
                 CheckpointRequest { commit: Option<String>, request_id: [u8; 16] },
                 Ping, IntentActivate { anchor: String, signature: Option<String>, request_id: [u8; 16] }, Ack { request_id: [u8;16], status: String }, Nack { request_id: [u8;16], reason: String } }
pub fn encode(frame: &Frame) -> Result<Vec<u8>, ProtoError>;
pub fn decode(buf: &[u8]) -> Result<(Frame, usize /*consumed*/), ProtoError>; // streaming-friendly
pub enum ProtoError { Incomplete, BadMagic, UnsupportedVersion(u8), TooLarge(u32), BadField(&'static str) }
```
Validation: agent name must match the existing agent vocabulary check; UTF-8 strictly
validated; control characters rejected in `agent`, `event_override`, `status`, `reason`.

## Acceptance criteria
- [ ] Roundtrip for every frame kind (golden bytes committed as test vectors).
- [ ] `decode` never panics and never allocates more than `length` bytes (property test with arbitrary bytes, including length lies and `u32::MAX`).
- [ ] Unknown version → `UnsupportedVersion`, unknown kind → `BadField("kind")`.
- [ ] Partial buffers → `Incomplete` (enables reading from a stream).

## Tests
`frame_roundtrip_all_kinds`, `frame_golden_vectors`, `decode_never_panics` (proptest or
a deterministic fuzz loop if proptest is not already a dev-dependency — check first),
`decode_rejects_oversized_length`, `decode_incomplete_is_not_an_error_state`.
