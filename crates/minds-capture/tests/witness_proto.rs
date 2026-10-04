use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use minds_capture::witness_proto::{Frame, MAGIC, MAX_FRAME, ProtoError, decode, encode};

// Count requested heap bytes only on the current test thread and only while
// decoding. Fixtures, assertions, and parallel tests do not affect the budget.
thread_local! {
    static ALLOCATED: Cell<Option<usize>> = const { Cell::new(None) };
}

struct CountingAllocator;

fn count(size: usize) {
    ALLOCATED.with(|counter| {
        if let Some(total) = counter.get() {
            counter.set(Some(total.saturating_add(size)));
        }
    });
}

// SAFETY: Every operation forwards the original pointer/layout to System.
// The thread-local counter neither allocates nor changes allocation behavior.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const ID: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const ANCHOR: &str = concat!(
    "minds-intent-v1\nsource=prompt\ncontent=b3-",
    "0000000000000000000000000000000000000000000000000000000000000000",
    "\nscope=-\n"
);
const SIGNATURE: &str = "-----BEGIN SSH SIGNATURE-----\nYQ==\n-----END SSH SIGNATURE-----\n";

fn examples() -> Vec<Frame> {
    vec![
        Frame::Hook {
            agent: "codex".into(),
            event_override: Some("Tool".into()),
            stdin: vec![0, 255, 10],
        },
        Frame::CheckpointRequest {
            commit: None,
            request_id: ID,
        },
        Frame::Ping,
        Frame::IntentActivate {
            anchor: ANCHOR.into(),
            signature: Some(SIGNATURE.into()),
            request_id: ID,
        },
        Frame::Ack {
            request_id: ID,
            status: "ok".into(),
        },
        Frame::Nack {
            request_id: ID,
            reason: "no".into(),
        },
        Frame::Hook {
            agent: "claude-code".into(),
            event_override: None,
            stdin: vec![],
        },
        Frame::CheckpointRequest {
            commit: Some("aB".repeat(20)),
            request_id: ID,
        },
        Frame::CheckpointRequest {
            commit: Some("09".repeat(32)),
            request_id: ID,
        },
        Frame::IntentActivate {
            anchor: ANCHOR.into(),
            signature: None,
            request_id: ID,
        },
        Frame::Ack {
            request_id: ID,
            status: "✓ gültig".into(),
        },
        Frame::Nack {
            request_id: ID,
            reason: String::new(),
        },
    ]
}

fn wire(payload: &[u8]) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn short(value: &[u8]) -> Vec<u8> {
    let mut bytes = (value.len() as u16).to_le_bytes().to_vec();
    bytes.extend_from_slice(value);
    bytes
}

fn measured_decode(bytes: &[u8]) -> Result<(Frame, usize), ProtoError> {
    ALLOCATED.with(|c| c.set(Some(0)));
    let outcome = std::panic::catch_unwind(|| decode(bytes));
    let allocated = ALLOCATED.with(|c| c.replace(None).unwrap());
    let result = outcome.expect("decoder must never panic");
    let budget = bytes
        .get(8..12)
        .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    assert!(
        allocated <= budget,
        "allocated {allocated}, declared {budget}"
    );
    if result.is_err() {
        assert_eq!(allocated, 0, "invalid/incomplete frames must not allocate");
    }
    result
}

#[test]
fn frame_roundtrip_all_kinds() {
    for frame in examples() {
        let encoded = encode(&frame).unwrap();
        assert_eq!(measured_decode(&encoded), Ok((frame, encoded.len())));
    }
}

#[test]
fn frame_golden_vectors() {
    // Independently specified on-disk bytes, not produced by the Rust encoder.
    let vectors = include_str!("fixtures/witness-v1.hex");
    let lines: Vec<_> = vectors
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .collect();
    assert_eq!(lines.len(), 6);
    for (frame, line) in examples().into_iter().zip(lines) {
        let (_, hex) = line.split_once(' ').unwrap();
        let expected: Vec<u8> = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        assert_eq!(hex.len() % 2, 0);
        assert_eq!(encode(&frame).unwrap(), expected);
        assert_eq!(measured_decode(&expected), Ok((frame, expected.len())));
    }
}

#[test]
fn decode_incomplete_is_not_an_error_state() {
    for frame in examples() {
        let bytes = encode(&frame).unwrap();
        for len in 0..bytes.len() {
            assert_eq!(measured_decode(&bytes[..len]), Err(ProtoError::Incomplete));
        }
    }
    // Even a huge declared payload must not reserve memory while waiting.
    let mut header = MAGIC.to_vec();
    header.extend_from_slice(&MAX_FRAME.to_le_bytes());
    assert_eq!(measured_decode(&header), Err(ProtoError::Incomplete));
}

#[test]
fn decode_consumes_exactly_one_frame() {
    let frames = examples();
    let mut stream: Vec<u8> = frames.iter().flat_map(|f| encode(f).unwrap()).collect();
    stream.extend_from_slice(&MAGIC[..4]);
    let mut offset = 0;
    for expected in frames {
        let (frame, consumed) = measured_decode(&stream[offset..]).unwrap();
        assert_eq!(frame, expected);
        offset += consumed;
    }
    assert_eq!(
        measured_decode(&stream[offset..]),
        Err(ProtoError::Incomplete)
    );
}

#[test]
fn decode_rejects_oversized_length() {
    for length in [MAX_FRAME + 1, u32::MAX] {
        let mut header = MAGIC.to_vec();
        header.extend_from_slice(&length.to_le_bytes());
        assert_eq!(measured_decode(&header), Err(ProtoError::TooLarge(length)));
    }
}

#[test]
fn decode_rejects_magic_version_and_kind() {
    for index in 0..7 {
        let mut bytes = encode(&Frame::Ping).unwrap();
        bytes[index] ^= 1;
        assert_eq!(measured_decode(&bytes), Err(ProtoError::BadMagic));
    }
    for version in 0..=255 {
        if version == 1 {
            continue;
        }
        let mut bytes = MAGIC;
        bytes[7] = version;
        assert_eq!(
            measured_decode(&bytes),
            Err(ProtoError::UnsupportedVersion(version))
        );
    }
    for kind in 0..=255 {
        if matches!(kind, 1..=4 | 0x81 | 0x82) {
            continue;
        }
        assert_eq!(
            measured_decode(&wire(&[kind])),
            Err(ProtoError::BadField("kind"))
        );
    }
    assert_eq!(
        measured_decode(&wire(&[])),
        Err(ProtoError::BadField("kind"))
    );
}

#[test]
fn complete_payload_with_missing_fields_or_trailing_bytes_is_malformed() {
    for frame in examples() {
        let encoded = encode(&frame).unwrap();
        let payload = &encoded[12..];
        // Hook stdin and response text are allowed to be empty, all other
        // fields are mandatory even if their string value may be empty.
        let min_len = match &frame {
            Frame::Hook { stdin, .. } => payload.len() - stdin.len(),
            Frame::Ack { status, .. } => payload.len() - status.len(),
            Frame::Nack { reason, .. } => payload.len() - reason.len(),
            _ => payload.len(),
        };
        for len in 0..min_len {
            assert!(matches!(
                measured_decode(&wire(&payload[..len])),
                Err(ProtoError::BadField(_))
            ));
        }
        if matches!(
            frame,
            Frame::Ping | Frame::CheckpointRequest { .. } | Frame::IntentActivate { .. }
        ) {
            let mut extra = payload.to_vec();
            extra.push(0);
            assert_eq!(
                measured_decode(&wire(&extra)),
                Err(ProtoError::BadField("trailing bytes"))
            );
        }
    }
    // Inner lengths must not borrow from an appended, otherwise valid frame.
    for payload in [
        vec![1, 255, 255],
        vec![2, 64, 0],
        vec![4, 0, 0, 255, 255, 255, 255],
    ] {
        let mut stream = wire(&payload);
        stream.extend_from_slice(&encode(&Frame::Ping).unwrap());
        assert!(matches!(
            measured_decode(&stream),
            Err(ProtoError::BadField(_))
        ));
    }
}

#[test]
fn agent_validation_matches_journal_with_a_64_byte_limit() {
    for agent in [
        "",
        ".",
        "..",
        "../codex",
        "c/d",
        "a b",
        "c\n",
        "c\0",
        "c\u{85}",
        "cödex",
        &"a".repeat(65),
    ] {
        let frame = Frame::Hook {
            agent: agent.into(),
            event_override: None,
            stdin: vec![],
        };
        assert_eq!(encode(&frame), Err(ProtoError::BadField("agent")));
        let payload = [vec![1], short(agent.as_bytes()), vec![0, 0]].concat();
        assert_eq!(
            measured_decode(&wire(&payload)),
            Err(ProtoError::BadField("agent"))
        );
    }
    for agent in ["codex", "claude-code", "custom.Agent_2", &"a".repeat(64)] {
        assert!(minds_capture::SessionKey::new(agent, "session").is_ok());
        let frame = Frame::Hook {
            agent: agent.into(),
            event_override: None,
            stdin: vec![],
        };
        assert_eq!(measured_decode(&encode(&frame).unwrap()).unwrap().0, frame);
    }
}

#[test]
fn rejects_control_characters_in_single_line_fields() {
    for control in ['\0', '\n', '\r', '\t', '\u{7f}', '\u{85}', '\u{9f}'] {
        let value = format!("before{control}after");
        for (frame, payload, field) in [
            (
                Frame::Hook {
                    agent: "codex".into(),
                    event_override: Some(value.clone()),
                    stdin: vec![],
                },
                [vec![1], short(b"codex"), short(value.as_bytes())].concat(),
                "event_override",
            ),
            (
                Frame::Ack {
                    request_id: ID,
                    status: value.clone(),
                },
                [vec![0x81], ID.to_vec(), value.as_bytes().to_vec()].concat(),
                "status",
            ),
            (
                Frame::Nack {
                    request_id: ID,
                    reason: value.clone(),
                },
                [vec![0x82], ID.to_vec(), value.as_bytes().to_vec()].concat(),
                "reason",
            ),
        ] {
            assert_eq!(encode(&frame), Err(ProtoError::BadField(field)));
            assert_eq!(
                measured_decode(&wire(&payload)),
                Err(ProtoError::BadField(field))
            );
        }
    }
}

#[test]
fn utf8_is_strict_in_every_text_field_but_stdin_is_opaque() {
    for bad in [
        vec![255],
        vec![0xc0, 0xaf],
        vec![0xed, 0xa0, 0x80],
        vec![0xe2, 0x82],
    ] {
        for (payload, field) in [
            ([vec![1], short(&bad), vec![0, 0]].concat(), "agent"),
            (
                [vec![1], short(b"codex"), short(&bad)].concat(),
                "event_override",
            ),
            ([vec![2], short(&bad), ID.to_vec()].concat(), "commit"),
            (
                [vec![4], short(&bad), vec![0; 4], ID.to_vec()].concat(),
                "anchor",
            ),
            (
                [
                    vec![4, 0, 0],
                    (bad.len() as u32).to_le_bytes().to_vec(),
                    bad.clone(),
                    ID.to_vec(),
                ]
                .concat(),
                "signature",
            ),
            ([vec![0x81], ID.to_vec(), bad.clone()].concat(), "status"),
            ([vec![0x82], ID.to_vec(), bad.clone()].concat(), "reason"),
        ] {
            assert_eq!(
                measured_decode(&wire(&payload)),
                Err(ProtoError::BadField(field))
            );
        }
        let frame = Frame::Hook {
            agent: "codex".into(),
            event_override: None,
            stdin: bad,
        };
        assert_eq!(measured_decode(&encode(&frame).unwrap()).unwrap().0, frame);
    }
}

#[test]
fn commit_is_empty_or_a_full_hex_object_id() {
    for commit in [
        "HEAD".into(),
        "a".repeat(39),
        "a".repeat(41),
        "a".repeat(63),
        "a".repeat(65),
        "g".repeat(40),
        "é".repeat(20),
    ] {
        let frame = Frame::CheckpointRequest {
            commit: Some(commit.clone()),
            request_id: ID,
        };
        assert_eq!(encode(&frame), Err(ProtoError::BadField("commit")));
        let payload = [vec![2], short(commit.as_bytes()), ID.to_vec()].concat();
        assert_eq!(
            measured_decode(&wire(&payload)),
            Err(ProtoError::BadField("commit"))
        );
    }
}

#[test]
fn optional_empty_strings_decode_as_none() {
    for (empty, canonical) in [
        (
            Frame::Hook {
                agent: "codex".into(),
                event_override: Some(String::new()),
                stdin: vec![],
            },
            Frame::Hook {
                agent: "codex".into(),
                event_override: None,
                stdin: vec![],
            },
        ),
        (
            Frame::CheckpointRequest {
                commit: Some(String::new()),
                request_id: ID,
            },
            Frame::CheckpointRequest {
                commit: None,
                request_id: ID,
            },
        ),
        (
            Frame::IntentActivate {
                anchor: ANCHOR.into(),
                signature: Some(String::new()),
                request_id: ID,
            },
            Frame::IntentActivate {
                anchor: ANCHOR.into(),
                signature: None,
                request_id: ID,
            },
        ),
    ] {
        assert_eq!(encode(&empty), encode(&canonical));
        assert_eq!(
            measured_decode(&encode(&empty).unwrap()).unwrap().0,
            canonical
        );
    }
}

#[test]
fn field_limits_are_byte_limits_and_checked_on_both_paths() {
    for len in [4096, 4097] {
        let value = format!(
            "{}{}",
            "é".repeat(len / 2),
            if len % 2 == 1 { "a" } else { "" }
        );
        for (frame, payload, field) in [
            (
                Frame::Ack {
                    request_id: ID,
                    status: value.clone(),
                },
                [vec![0x81], ID.to_vec(), value.as_bytes().to_vec()].concat(),
                "status",
            ),
            (
                Frame::Nack {
                    request_id: ID,
                    reason: value.clone(),
                },
                [vec![0x82], ID.to_vec(), value.as_bytes().to_vec()].concat(),
                "reason",
            ),
            (
                Frame::IntentActivate {
                    anchor: value.clone(),
                    signature: None,
                    request_id: ID,
                },
                [vec![4], short(value.as_bytes()), vec![0; 4], ID.to_vec()].concat(),
                "anchor",
            ),
        ] {
            if len == 4096 {
                assert_eq!(encode(&frame).unwrap(), wire(&payload));
                assert_eq!(measured_decode(&wire(&payload)).unwrap().0, frame);
            } else {
                assert_eq!(encode(&frame), Err(ProtoError::BadField(field)));
                assert_eq!(
                    measured_decode(&wire(&payload)),
                    Err(ProtoError::BadField(field))
                );
            }
        }
    }
    let mut frame = Frame::Hook {
        agent: "codex".into(),
        event_override: Some("e".repeat(u16::MAX as usize)),
        stdin: vec![],
    };
    assert_eq!(measured_decode(&encode(&frame).unwrap()).unwrap().0, frame);
    if let Frame::Hook {
        event_override: Some(event),
        ..
    } = &mut frame
    {
        event.push('e');
    }
    assert_eq!(encode(&frame), Err(ProtoError::BadField("event_override")));
}

#[test]
fn max_frame_counts_kind_and_all_field_overhead() {
    for mut frame in [
        Frame::Hook {
            agent: "codex".into(),
            event_override: None,
            stdin: vec![255; MAX_FRAME as usize - 10],
        },
        Frame::IntentActivate {
            anchor: ANCHOR.into(),
            signature: Some("s".repeat(MAX_FRAME as usize - 23 - ANCHOR.len())),
            request_id: ID,
        },
    ] {
        let encoded = encode(&frame).unwrap();
        assert_eq!(encoded.len(), MAX_FRAME as usize + 12);
        assert_eq!(measured_decode(&encoded).unwrap().0, frame);
        match &mut frame {
            Frame::Hook { stdin, .. } => stdin.push(0),
            Frame::IntentActivate {
                signature: Some(signature),
                ..
            } => signature.push('s'),
            _ => unreachable!(),
        }
        assert_eq!(encode(&frame), Err(ProtoError::TooLarge(MAX_FRAME + 1)));
    }
}

#[test]
fn decode_never_panics() {
    // No proptest dependency exists in minds-capture. Exercise arbitrary input,
    // plausible headers, lies about lengths, and mutations inside valid frames.
    let mut state = 0x719a_33d5_aced_ba98_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let seeds: Vec<_> = examples().iter().map(|f| encode(f).unwrap()).collect();
    for iteration in 0..20_000 {
        let len = (next() % 512) as usize;
        let random: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        let _ = measured_decode(&random);
        let mut framed = wire(&random);
        let _ = measured_decode(&framed);
        let lie = [0, 1, MAX_FRAME, MAX_FRAME + 1, u32::MAX, next() as u32][iteration % 6];
        framed[8..12].copy_from_slice(&lie.to_le_bytes());
        let _ = measured_decode(&framed);
        let mut mutated = seeds[iteration % seeds.len()].clone();
        let index = (next() as usize) % mutated.len();
        mutated[index] = next() as u8;
        let _ = measured_decode(&mutated);
    }
}
