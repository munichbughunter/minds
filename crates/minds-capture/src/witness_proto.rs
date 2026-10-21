//! Pure framing for `minds-witness-v1`; no socket I/O or authentication.
//!
//! A client that sent a `CheckpointRequest` keeps its connection fully open until
//! it has read the answer — no half-close (`shutdown(SHUT_WR)`). The witness reads
//! end-of-stream as "the requester is gone" and then does not amend the commit.
//!
//! The 12-byte header is [`MAGIC`] followed by a little-endian u32 length.
//! Length counts **kind + body**, excluding the header, and is capped at
//! [`MAX_FRAME`] independently of the CLI's stdin limit. Hooks are fire-and-forget;
//! clients must not wait for a response to them.
//!
//! Empty optional strings have the same wire representation as `None` and decode
//! to `None`. Request IDs are supplied by the caller. Intent anchors and signatures
//! are opaque UTF-8 here: anchor semantics belong to EA-14 and signature verification
//! belongs to the verifier, never to this capture-time codec.
//!
//! `IntentChallenge` and `IntentActivateProven` belong together on **one**
//! connection: the witness answers the challenge with a one-time nonce that is
//! valid only on that connection; the proof (an SSH signature by the human's
//! pinned security key) covers anchor id and nonce. Whether it holds is the
//! witness's decision, not this codec's.

/// Protocol identity (first seven bytes) and version (last byte).
pub const MAGIC: [u8; 8] = *b"MWIT\0\0\0\x01";
/// Maximum payload length, including the kind byte (8 MiB).
pub const MAX_FRAME: u32 = 8 * 1024 * 1024;

const HEADER_LEN: usize = 12;
const MAX_AGENT: usize = 64;
const MAX_TEXT: usize = 4 * 1024;

/// Longest model id a [`Frame::Model`] may carry (K1): one short line.
pub const MAX_MODEL: usize = 128;
/// Longest agent version a [`Frame::Usage`] may carry: one short word.
pub const MAX_VERSION: usize = 64;
/// Longest answer text a [`Frame::Said`] may carry (64 KiB); the hook clips
/// longer texts before sending.
pub const MAX_SAID: usize = 64 * 1024;
/// How many [`Frame::Said`] turns one event may carry; the hook sends no
/// more, the witness ignores any beyond — a bound on what one connection
/// can make the writer hold.
pub const MAX_SAID_TURNS: usize = 512;

/// A single request or response. Raw hook stdin is preserved byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Hook {
        agent: String,
        event_override: Option<String>,
        stdin: Vec<u8>,
    },
    /// The model id the hook read from the tail of the transcript at
    /// `Stop`/`SessionEnd` (K1), sent on the same connection **right after**
    /// the `Hook` frame of that event. Only the id travels, never text or
    /// token counts. A witness that does not know this kind drops it and
    /// closes the connection — the event before it has already landed, so
    /// an old witness loses the model, never the event.
    Model {
        agent: String,
        session: String,
        model: String,
    },
    /// What the `SessionEnd` hook summed up from the transcript: the agent
    /// version (empty = unknown) and the token totals. Sent on the same
    /// connection after the `Hook` frame, like [`Frame::Model`]; a witness
    /// that does not know this kind drops it and closes the connection —
    /// the event has already landed. A statement of the hook, not an
    /// observation of the witness.
    Usage {
        agent: String,
        session: String,
        version: Option<String>,
        input_tokens: u64,
        output_tokens: u64,
    },
    /// The agent's answer text of one turn (`turn` counts the human's
    /// prompts from 0), already redacted by the hook with the repository's
    /// policy. One frame per turn, in order, after [`Frame::Usage`]. The
    /// same delivery rule as `Usage`.
    Said {
        agent: String,
        session: String,
        turn: u32,
        text: String,
    },
    CheckpointRequest {
        commit: Option<String>,
        request_id: [u8; 16],
    },
    Ping,
    /// Asks what the file observer is doing. Answered with `Ack` (status
    /// `watching <root> · <n> frames · last <ts>`) or `Nack` (`no file
    /// observer`), request id zero — read-only, accepted on both sockets.
    /// A witness that does not know this kind closes the connection, which
    /// `minds doctor` reports as "not answering".
    Status,
    IntentActivate {
        anchor: String,
        signature: Option<String>,
        request_id: [u8; 16],
    },
    /// Asks for a one-time nonce for [`Frame::IntentActivateProven`].
    IntentChallenge {
        request_id: [u8; 16],
    },
    /// An activation the human approved just now: `proof` signs anchor id and
    /// `nonce` (64 lowercase hex digits).
    IntentActivateProven {
        anchor: String,
        signature: Option<String>,
        nonce: String,
        proof: String,
        request_id: [u8; 16],
    },
    /// Host side, one witness daemon per host: asks the host daemon to
    /// serve the repository at `repo` (an absolute path) — set up its witness
    /// home and key and run its witness. `intent_key` is the `.pub` line of
    /// a security key to pin, if any. Answered with `Ack` (status parts
    /// separated by ` · `: `registered <id>` or `running <id>`, `signers
    /// <line>`, …) or `Nack`. Whether the peer may ask is the daemon's
    /// decision, not this codec's.
    Register {
        repo: String,
        intent_key: Option<String>,
        request_id: [u8; 16],
    },
    /// Host side: stop serving `repo` and remove its witness home. Only the
    /// witness user may ask.
    Unregister {
        repo: String,
        request_id: [u8; 16],
    },
    Ack {
        request_id: [u8; 16],
        status: String,
    },
    Nack {
        request_id: [u8; 16],
        reason: String,
    },
}

/// Longest repository path a [`Frame::Register`] may carry (`PATH_MAX`).
pub const MAX_REPO: usize = 4096;

/// Errors never contain attacker-controlled field contents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtoError {
    #[error("incomplete witness frame")]
    Incomplete,
    #[error("invalid witness magic")]
    BadMagic,
    #[error("unsupported witness version {0}")]
    UnsupportedVersion(u8),
    #[error("witness payload too large: {0} bytes")]
    TooLarge(u32),
    #[error("invalid witness field: {0}")]
    BadField(&'static str),
}

/// Encode a validated frame. Oversized frames are rejected before allocation.
pub fn encode(frame: &Frame) -> Result<Vec<u8>, ProtoError> {
    let length = encoded_length(frame)?;
    let mut out = Vec::with_capacity(HEADER_LEN + length as usize);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&length.to_le_bytes());
    match frame {
        Frame::Hook {
            agent,
            event_override,
            stdin,
        } => {
            out.push(0x01);
            put_short(&mut out, agent);
            put_short(&mut out, event_override.as_deref().unwrap_or_default());
            out.extend_from_slice(stdin);
        }
        Frame::Model {
            agent,
            session,
            model,
        } => {
            out.push(0x07);
            put_short(&mut out, agent);
            put_short(&mut out, session);
            put_short(&mut out, model);
        }
        Frame::Usage {
            agent,
            session,
            version,
            input_tokens,
            output_tokens,
        } => {
            out.push(0x09);
            put_short(&mut out, agent);
            put_short(&mut out, session);
            put_short(&mut out, version.as_deref().unwrap_or_default());
            out.extend_from_slice(&input_tokens.to_le_bytes());
            out.extend_from_slice(&output_tokens.to_le_bytes());
        }
        Frame::Said {
            agent,
            session,
            turn,
            text,
        } => {
            out.push(0x0a);
            put_short(&mut out, agent);
            put_short(&mut out, session);
            out.extend_from_slice(&turn.to_le_bytes());
            put_long(&mut out, text);
        }
        Frame::CheckpointRequest { commit, request_id } => {
            out.push(0x02);
            put_short(&mut out, commit.as_deref().unwrap_or_default());
            out.extend_from_slice(request_id);
        }
        Frame::Ping => out.push(0x03),
        Frame::Status => out.push(0x08),
        Frame::IntentActivate {
            anchor,
            signature,
            request_id,
        } => {
            out.push(0x04);
            put_short(&mut out, anchor);
            let signature = signature.as_deref().unwrap_or_default();
            out.extend_from_slice(&(signature.len() as u32).to_le_bytes());
            out.extend_from_slice(signature.as_bytes());
            out.extend_from_slice(request_id);
        }
        Frame::IntentChallenge { request_id } => {
            out.push(0x05);
            out.extend_from_slice(request_id);
        }
        Frame::IntentActivateProven {
            anchor,
            signature,
            nonce,
            proof,
            request_id,
        } => {
            out.push(0x06);
            put_short(&mut out, anchor);
            put_long(&mut out, signature.as_deref().unwrap_or_default());
            put_short(&mut out, nonce);
            put_long(&mut out, proof);
            out.extend_from_slice(request_id);
        }
        Frame::Register {
            repo,
            intent_key,
            request_id,
        } => {
            out.push(0x0b);
            put_short(&mut out, repo);
            put_short(&mut out, intent_key.as_deref().unwrap_or_default());
            out.extend_from_slice(request_id);
        }
        Frame::Unregister { repo, request_id } => {
            out.push(0x0c);
            put_short(&mut out, repo);
            out.extend_from_slice(request_id);
        }
        Frame::Ack { request_id, status } => {
            out.push(0x81);
            out.extend_from_slice(request_id);
            out.extend_from_slice(status.as_bytes());
        }
        Frame::Nack { request_id, reason } => {
            out.push(0x82);
            out.extend_from_slice(request_id);
            out.extend_from_slice(reason.as_bytes());
        }
    }
    Ok(out)
}

/// Decode the first frame and return its byte count, leaving subsequent frames
/// untouched. A valid but truncated frame returns [`ProtoError::Incomplete`].
/// Once the declared payload is complete, missing internal fields are malformed
/// (`BadField`), not incomplete: reading more bytes cannot repair that frame.
///
/// No allocation occurs until the complete payload is present and validated.
/// The returned strings and byte vector allocate at most `length` bytes in total;
/// untrusted lengths are never used to reserve memory.
pub fn decode(buf: &[u8]) -> Result<(Frame, usize), ProtoError> {
    let magic = buf.get(..8).ok_or(ProtoError::Incomplete)?;
    if magic[..7] != MAGIC[..7] {
        return Err(ProtoError::BadMagic);
    }
    if magic[7] != MAGIC[7] {
        return Err(ProtoError::UnsupportedVersion(magic[7]));
    }
    let length_bytes = buf.get(8..HEADER_LEN).ok_or(ProtoError::Incomplete)?;
    let length = u32::from_le_bytes(length_bytes.try_into().unwrap());
    if length > MAX_FRAME {
        return Err(ProtoError::TooLarge(length));
    }
    let consumed = HEADER_LEN + length as usize;
    let payload = buf
        .get(HEADER_LEN..consumed)
        .ok_or(ProtoError::Incomplete)?;
    let mut cursor = Cursor(payload);
    let kind = cursor.take(1, "kind")?[0];
    let frame = match kind {
        0x01 => {
            let agent = cursor.short_text("agent")?;
            check_agent(agent)?;
            let event_override = cursor.short_text("event_override")?;
            check_line(event_override, u16::MAX as usize, "event_override")?;
            Frame::Hook {
                agent: agent.to_owned(),
                event_override: optional(event_override),
                stdin: cursor.0.to_vec(),
            }
        }
        0x07 => {
            let agent = cursor.short_text("agent")?;
            check_agent(agent)?;
            let session = cursor.short_text("session")?;
            check_session(session)?;
            let model = cursor.short_text("model")?;
            check_model(model)?;
            cursor.finish()?;
            Frame::Model {
                agent: agent.to_owned(),
                session: session.to_owned(),
                model: model.to_owned(),
            }
        }
        0x09 => {
            let agent = cursor.short_text("agent")?;
            check_agent(agent)?;
            let session = cursor.short_text("session")?;
            check_session(session)?;
            let version = cursor.short_text("version")?;
            check_version(version)?;
            let input_tokens = cursor.u64("input_tokens")?;
            let output_tokens = cursor.u64("output_tokens")?;
            cursor.finish()?;
            Frame::Usage {
                agent: agent.to_owned(),
                session: session.to_owned(),
                version: optional(version),
                input_tokens,
                output_tokens,
            }
        }
        0x0a => {
            let agent = cursor.short_text("agent")?;
            check_agent(agent)?;
            let session = cursor.short_text("session")?;
            check_session(session)?;
            let turn = cursor.take(4, "turn")?;
            let turn = u32::from_le_bytes(turn.try_into().unwrap());
            let text = cursor.long_text("text")?;
            check_said(text)?;
            cursor.finish()?;
            Frame::Said {
                agent: agent.to_owned(),
                session: session.to_owned(),
                turn,
                text: text.to_owned(),
            }
        }
        0x02 => {
            let commit = cursor.short_text("commit")?;
            check_commit(commit)?;
            let request_id = cursor.request_id()?;
            cursor.finish()?;
            Frame::CheckpointRequest {
                commit: optional(commit),
                request_id,
            }
        }
        0x03 => {
            cursor.finish()?;
            Frame::Ping
        }
        0x08 => {
            cursor.finish()?;
            Frame::Status
        }
        0x04 => {
            let anchor = cursor.short_text("anchor")?;
            check_anchor(anchor)?;
            let length = cursor.take(4, "signature")?;
            let length = u32::from_le_bytes(length.try_into().unwrap());
            let signature = text(cursor.take(length as usize, "signature")?, "signature")?;
            let request_id = cursor.request_id()?;
            cursor.finish()?;
            Frame::IntentActivate {
                anchor: anchor.to_owned(),
                signature: optional(signature),
                request_id,
            }
        }
        0x05 => {
            let request_id = cursor.request_id()?;
            cursor.finish()?;
            Frame::IntentChallenge { request_id }
        }
        0x06 => {
            let anchor = cursor.short_text("anchor")?;
            check_anchor(anchor)?;
            let signature = cursor.long_text("signature")?;
            let nonce = cursor.short_text("nonce")?;
            check_nonce(nonce)?;
            let proof = cursor.long_text("proof")?;
            check_proof(proof)?;
            let request_id = cursor.request_id()?;
            cursor.finish()?;
            Frame::IntentActivateProven {
                anchor: anchor.to_owned(),
                signature: optional(signature),
                nonce: nonce.to_owned(),
                proof: proof.to_owned(),
                request_id,
            }
        }
        0x0b => {
            let repo = cursor.short_text("repo")?;
            check_repo(repo)?;
            let intent_key = cursor.short_text("intent_key")?;
            check_line(intent_key, MAX_TEXT, "intent_key")?;
            let request_id = cursor.request_id()?;
            cursor.finish()?;
            Frame::Register {
                repo: repo.to_owned(),
                intent_key: optional(intent_key),
                request_id,
            }
        }
        0x0c => {
            let repo = cursor.short_text("repo")?;
            check_repo(repo)?;
            let request_id = cursor.request_id()?;
            cursor.finish()?;
            Frame::Unregister {
                repo: repo.to_owned(),
                request_id,
            }
        }
        0x81 | 0x82 => {
            let request_id = cursor.request_id()?;
            let field = if kind == 0x81 { "status" } else { "reason" };
            let line = text(cursor.0, field)?;
            check_line(line, MAX_TEXT, field)?;
            if kind == 0x81 {
                Frame::Ack {
                    request_id,
                    status: line.to_owned(),
                }
            } else {
                Frame::Nack {
                    request_id,
                    reason: line.to_owned(),
                }
            }
        }
        _ => return Err(ProtoError::BadField("kind")),
    };
    Ok((frame, consumed))
}

fn encoded_length(frame: &Frame) -> Result<u32, ProtoError> {
    let length = match frame {
        Frame::Hook {
            agent,
            event_override,
            stdin,
        } => {
            check_agent(agent)?;
            let event = event_override.as_deref().unwrap_or_default();
            check_line(event, u16::MAX as usize, "event_override")?;
            (5 + agent.len() + event.len()).saturating_add(stdin.len())
        }
        Frame::Model {
            agent,
            session,
            model,
        } => {
            check_agent(agent)?;
            check_session(session)?;
            check_model(model)?;
            7 + agent.len() + session.len() + model.len()
        }
        Frame::Usage {
            agent,
            session,
            version,
            ..
        } => {
            check_agent(agent)?;
            check_session(session)?;
            let version = version.as_deref().unwrap_or_default();
            check_version(version)?;
            23 + agent.len() + session.len() + version.len()
        }
        Frame::Said {
            agent,
            session,
            text,
            ..
        } => {
            check_agent(agent)?;
            check_session(session)?;
            check_said(text)?;
            13 + agent.len() + session.len() + text.len()
        }
        Frame::CheckpointRequest { commit, .. } => {
            let commit = commit.as_deref().unwrap_or_default();
            check_commit(commit)?;
            19 + commit.len()
        }
        Frame::Ping | Frame::Status => 1,
        Frame::IntentActivate {
            anchor, signature, ..
        } => {
            check_anchor(anchor)?;
            (23 + anchor.len()).saturating_add(signature.as_ref().map_or(0, String::len))
        }
        Frame::IntentChallenge { .. } => 17,
        Frame::IntentActivateProven {
            anchor,
            signature,
            nonce,
            proof,
            ..
        } => {
            check_anchor(anchor)?;
            check_nonce(nonce)?;
            check_proof(proof)?;
            (29 + anchor.len() + nonce.len() + proof.len())
                .saturating_add(signature.as_ref().map_or(0, String::len))
        }
        Frame::Register {
            repo, intent_key, ..
        } => {
            check_repo(repo)?;
            let intent_key = intent_key.as_deref().unwrap_or_default();
            check_line(intent_key, MAX_TEXT, "intent_key")?;
            21 + repo.len() + intent_key.len()
        }
        Frame::Unregister { repo, .. } => {
            check_repo(repo)?;
            19 + repo.len()
        }
        Frame::Ack { status, .. } => {
            check_line(status, MAX_TEXT, "status")?;
            17 + status.len()
        }
        Frame::Nack { reason, .. } => {
            check_line(reason, MAX_TEXT, "reason")?;
            17 + reason.len()
        }
    };
    if length > MAX_FRAME as usize {
        return Err(ProtoError::TooLarge(
            u32::try_from(length).unwrap_or(u32::MAX),
        ));
    }
    Ok(length as u32)
}

fn check_agent(value: &str) -> Result<(), ProtoError> {
    if value.len() > MAX_AGENT || crate::journal::check_component(value, "agent").is_err() {
        return Err(ProtoError::BadField("agent"));
    }
    Ok(())
}

fn check_line(value: &str, max: usize, field: &'static str) -> Result<(), ProtoError> {
    if value.len() > max || value.chars().any(char::is_control) {
        return Err(ProtoError::BadField(field));
    }
    Ok(())
}

/// A model id is one short, non-empty word of printable ASCII (K1) — the
/// same rule on every side: the hook before it sends, the decoder, and the
/// witness before it writes. `pub`, so the hook applies it before encoding
/// and never trades the event for a bad id.
pub fn check_model(value: &str) -> Result<(), ProtoError> {
    if value.is_empty() || value.len() > MAX_MODEL || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ProtoError::BadField("model"));
    }
    Ok(())
}

/// An agent version is empty (unknown) or one short word of printable ASCII
/// (`2.0.76`) — the same rule on every side, like [`check_model`].
pub fn check_version(value: &str) -> Result<(), ProtoError> {
    if value.len() > MAX_VERSION || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ProtoError::BadField("version"));
    }
    Ok(())
}

/// An answer text is at most [`MAX_SAID`] bytes and carries no control
/// characters besides line breaks and tabs — a terminal reads it later, and
/// an escape sequence is not an answer. Empty is allowed: a turn without
/// text keeps its place in the order.
pub fn check_said(value: &str) -> Result<(), ProtoError> {
    if value.len() > MAX_SAID
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(ProtoError::BadField("text"));
    }
    Ok(())
}

/// The session id a [`Frame::Model`] names: what a journal session key
/// accepts (`[A-Za-z0-9._-]`, 1–128 bytes); the writer checks it again.
fn check_session(value: &str) -> Result<(), ProtoError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(ProtoError::BadField("session"));
    }
    Ok(())
}

/// A repository path: absolute, non-empty, one line, at most [`MAX_REPO`]
/// bytes. Whether it is a repository is the daemon's decision.
fn check_repo(value: &str) -> Result<(), ProtoError> {
    if value.is_empty() || !value.starts_with('/') {
        return Err(ProtoError::BadField("repo"));
    }
    check_line(value, MAX_REPO, "repo")
}

fn check_commit(value: &str) -> Result<(), ProtoError> {
    if !value.is_empty()
        && (!matches!(value.len(), 40 | 64) || !value.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(ProtoError::BadField("commit"));
    }
    Ok(())
}

fn check_anchor(value: &str) -> Result<(), ProtoError> {
    if value.len() > MAX_TEXT {
        return Err(ProtoError::BadField("anchor"));
    }
    Ok(())
}

/// The nonce of a challenge: exactly 64 lowercase hex digits (32 bytes).
pub const NONCE_HEX: usize = 64;

fn check_nonce(value: &str) -> Result<(), ProtoError> {
    if value.len() != NONCE_HEX
        || !value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(ProtoError::BadField("nonce"));
    }
    Ok(())
}

/// An armored SSH signature is a few hundred bytes; 16 KiB is generous.
fn check_proof(value: &str) -> Result<(), ProtoError> {
    if value.is_empty() || value.len() > 16 * 1024 {
        return Err(ProtoError::BadField("proof"));
    }
    Ok(())
}

fn text<'a>(bytes: &'a [u8], field: &'static str) -> Result<&'a str, ProtoError> {
    std::str::from_utf8(bytes).map_err(|_| ProtoError::BadField(field))
}

fn optional(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn put_short(out: &mut Vec<u8>, value: &str) {
    // All callers validated the byte length before allocating the output.
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn put_long(out: &mut Vec<u8>, value: &str) {
    // The total frame length (≤ MAX_FRAME) was checked before.
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

/// A bounded, borrowing cursor; inner lengths cannot escape the payload slice.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize, field: &'static str) -> Result<&'a [u8], ProtoError> {
        let value = self.0.get(..len).ok_or(ProtoError::BadField(field))?;
        self.0 = &self.0[len..];
        Ok(value)
    }

    fn short_text(&mut self, field: &'static str) -> Result<&'a str, ProtoError> {
        let len = self.take(2, field)?;
        let len = u16::from_le_bytes(len.try_into().unwrap());
        text(self.take(len as usize, field)?, field)
    }

    fn long_text(&mut self, field: &'static str) -> Result<&'a str, ProtoError> {
        let len = self.take(4, field)?;
        let len = u32::from_le_bytes(len.try_into().unwrap());
        text(self.take(len as usize, field)?, field)
    }

    fn request_id(&mut self) -> Result<[u8; 16], ProtoError> {
        Ok(self.take(16, "request_id")?.try_into().unwrap())
    }

    fn u64(&mut self, field: &'static str) -> Result<u64, ProtoError> {
        Ok(u64::from_le_bytes(self.take(8, field)?.try_into().unwrap()))
    }

    fn finish(self) -> Result<(), ProtoError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ProtoError::BadField("trailing bytes"))
        }
    }
}
