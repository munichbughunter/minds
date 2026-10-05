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

/// Protocol identity (first seven bytes) and version (last byte).
pub const MAGIC: [u8; 8] = *b"MWIT\0\0\0\x01";
/// Maximum payload length, including the kind byte (8 MiB).
pub const MAX_FRAME: u32 = 8 * 1024 * 1024;

const HEADER_LEN: usize = 12;
const MAX_AGENT: usize = 64;
const MAX_TEXT: usize = 4 * 1024;

/// A single request or response. Raw hook stdin is preserved byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Hook {
        agent: String,
        event_override: Option<String>,
        stdin: Vec<u8>,
    },
    CheckpointRequest {
        commit: Option<String>,
        request_id: [u8; 16],
    },
    Ping,
    IntentActivate {
        anchor: String,
        signature: Option<String>,
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
        Frame::CheckpointRequest { commit, request_id } => {
            out.push(0x02);
            put_short(&mut out, commit.as_deref().unwrap_or_default());
            out.extend_from_slice(request_id);
        }
        Frame::Ping => out.push(0x03),
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
        Frame::CheckpointRequest { commit, .. } => {
            let commit = commit.as_deref().unwrap_or_default();
            check_commit(commit)?;
            19 + commit.len()
        }
        Frame::Ping => 1,
        Frame::IntentActivate {
            anchor, signature, ..
        } => {
            check_anchor(anchor)?;
            (23 + anchor.len()).saturating_add(signature.as_ref().map_or(0, String::len))
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

    fn request_id(&mut self) -> Result<[u8; 16], ProtoError> {
        Ok(self.take(16, "request_id")?.try_into().unwrap())
    }

    fn finish(self) -> Result<(), ProtoError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ProtoError::BadField("trailing bytes"))
        }
    }
}
