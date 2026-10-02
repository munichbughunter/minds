//! Reconciliation of committed bytes with stored write evidence. Pure and
//! read-only: callers supply blobs and observations from the session window.
//!
//! Paths are repo-relative identities, not display strings; consumers must
//! sanitize them before rendering. Blob contents never appear in the result.

use std::collections::BTreeMap;

use minds_core::{ContentHash, Effect, EffectKind, Session, SessionId, ToolCall};
use minds_git::{CommitId, added_line_ranges};

/// Maximum size of any blob or reconstructed content used for line attribution.
pub const LINE_LEVEL_LIMIT: usize = 2 * 1024 * 1024;

/// A commit's derivation, sorted by repository path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub commit: CommitId,
    /// First parent, or `None` for a root commit.
    pub base: Option<CommitId>,
    pub files: Vec<FileRecon>,
    /// Only `Explained` and `ExplainedFsOnly`, never `ReportedOnly`.
    pub explained_lines: u64,
    pub total_changed_lines: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecon {
    pub path: String,
    pub class: ReconClass,
    pub line_level: LineLevel,
    /// Blake3 of the committed blob. For deletions, the hash of empty bytes;
    /// `deleted` distinguishes absence from a committed empty file.
    pub committed: ContentHash,
    pub deleted: bool,
    /// Last observation matching the committed state, even if a later one
    /// contradicts it. Classification always uses the latest observation.
    pub last_observed: Option<ObservedAt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRecon {
    /// One-based line number in the committed file; only changed lines appear.
    pub line: u32,
    pub class: ReconClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconClass {
    /// Latest witness state matches, and a tool claimed those bytes.
    Explained,
    /// Latest witness state matches, without a matching tool claim.
    ExplainedFsOnly,
    /// No witness observation; the latest tool claim matches.
    ReportedOnly,
    /// Evidence is missing or contradicts the committed state.
    Unexplained,
}

impl ReconClass {
    fn explained(self) -> bool {
        matches!(self, Self::Explained | Self::ExplainedFsOnly)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineLevel {
    Available(Vec<LineRecon>),
    Unavailable(Reason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Binary,
    TooLarge,
    ReconstructionMismatch,
    /// A witness reported a hash different from the commit, without bytes
    /// and without a reconstructible claim for that hash.
    MissingContent,
}

/// Caller-supplied change against the first parent. Renames must be supplied
/// as two entries: deletion of the old path and addition of the new path.
#[derive(Debug, Clone)]
pub struct ChangedFile<'a> {
    /// Unique repo-relative path within `ReconInput::changed`.
    pub path: &'a str,
    /// `None` for an addition (all files in a root commit).
    pub base: Option<&'a [u8]>,
    /// `None` for a deletion; `Some(&[])` is a present, empty file.
    pub committed: Option<&'a [u8]>,
    /// Added/modified line count from Git's diff statistics. Used only when a
    /// blob exceeds `LINE_LEVEL_LIMIT`; smaller text diffs are derived here.
    /// Deletions always count zero and binary additions/modifications one.
    pub added_lines: u64,
}

/// A witness observation. `None` means observed absence, not an unknown hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedAt {
    pub hash: Option<ContentHash>,
    pub seq: u64,
    /// RFC 3339, parsed with offset and subsecond precision for ordering.
    pub at: Option<String>,
}

/// Future EA-08 input. Callers restrict observations to the linked sessions'
/// window. No filesystem is consulted, and `content` is optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsObservation {
    pub path: String,
    pub observed: ObservedAt,
    /// Optional bytes for partial line attribution when the commit differs.
    /// Accepted only after verification against `observed.hash`.
    pub content: Option<Vec<u8>>,
}

pub struct ReconInput<'a> {
    pub commit: CommitId,
    pub base: Option<CommitId>,
    pub changed: &'a [ChangedFile<'a>],
    pub sessions: &'a [&'a Session],
    pub observations: &'a [FsObservation],
}

struct Claim<'a> {
    call: &'a ToolCall,
    effect: &'a Effect,
}

fn timestamp(at: Option<&str>) -> Option<jiff::Timestamp> {
    at.and_then(|at| at.parse().ok())
}

/// Derive reconciliation without I/O or mutation of any evidence.
///
/// Turns are merged by parsed timestamp (falling back to session start), then
/// content-addressed session identity, turn index and call index. Missing times
/// sort first. Within a session, turn order remains authoritative even when a
/// clock moves backwards. Ties therefore never depend on input session order.
pub fn reconcile(input: &ReconInput<'_>) -> Reconciliation {
    let mut turns = Vec::new();
    for session in input.sessions {
        let id = SessionId::of(session).expect("Session has a canonical representation");
        let mut time = timestamp(
            session
                .lineage
                .as_ref()
                .and_then(|l| l.started_at.as_deref()),
        );
        for (turn_index, turn) in session.turns.iter().enumerate() {
            time = time.max(timestamp(turn.at.as_deref()));
            turns.push((time, id, turn_index, turn));
        }
    }
    turns.sort_by_key(|(time, id, index, _)| (*time, *id, *index));
    let mut claims: BTreeMap<&str, Vec<Claim<'_>>> = BTreeMap::new();
    for (_, _, _, turn) in turns {
        for call in &turn.tool_calls {
            if let Some(effect) = &call.effect
                && matches!(effect.kind, EffectKind::Write | EffectKind::Delete)
                && let Some(path) = effect.path.as_deref()
            {
                claims.entry(path).or_default().push(Claim { call, effect });
            }
        }
    }
    let mut observations: BTreeMap<&str, Vec<&FsObservation>> = BTreeMap::new();
    for observation in input.observations {
        observations
            .entry(&observation.path)
            .or_default()
            .push(observation);
    }
    for observations in observations.values_mut() {
        observations.sort_by(|a, b| {
            let key = |o: &FsObservation| (timestamp(o.observed.at.as_deref()), o.observed.seq);
            key(a)
                .cmp(&key(b))
                .then_with(|| a.observed.hash.cmp(&b.observed.hash))
                .then_with(|| a.observed.at.cmp(&b.observed.at))
                .then_with(|| a.content.cmp(&b.content))
        });
    }
    let mut result = Reconciliation {
        commit: input.commit,
        base: input.base,
        files: Vec::new(),
        explained_lines: 0,
        total_changed_lines: 0,
    };
    for file in input.changed {
        let claims = claims.get(file.path).map(Vec::as_slice).unwrap_or_default();
        let observations = observations
            .get(file.path)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let (recon, changed, explained) = reconcile_file(file, claims, observations);
        result.total_changed_lines += changed;
        result.explained_lines += explained;
        result.files.push(recon);
    }
    result.files.sort_by(|a, b| a.path.cmp(&b.path));
    result
}

fn hash(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

fn claims_state(claims: &[Claim<'_>], state: Option<&ContentHash>) -> bool {
    claims.iter().any(|claim| match state {
        Some(hash) => {
            claim.effect.kind == EffectKind::Write && claim.effect.written.as_ref() == Some(hash)
        }
        None => claim.effect.kind == EffectKind::Delete,
    })
}

fn classify(
    claims: &[Claim<'_>],
    observation: Option<&FsObservation>,
    state: Option<&ContentHash>,
) -> ReconClass {
    if let Some(observation) = observation {
        if observation.observed.hash.as_ref() != state {
            ReconClass::Unexplained
        } else if claims_state(claims, state) {
            ReconClass::Explained
        } else {
            ReconClass::ExplainedFsOnly
        }
    } else if claims
        .last()
        .is_some_and(|last| claims_state(std::slice::from_ref(last), state))
    {
        ReconClass::ReportedOnly
    } else {
        ReconClass::Unexplained
    }
}

fn reconcile_file(
    file: &ChangedFile<'_>,
    claims: &[Claim<'_>],
    observations: &[&FsObservation],
) -> (FileRecon, u64, u64) {
    let committed = hash(file.committed.unwrap_or_default());
    let state = file.committed.map(|_| &committed);
    let observation = observations.last().copied();
    let class = classify(claims, observation, state);
    let last_observed = observations
        .iter()
        .rev()
        .find(|o| o.observed.hash.as_ref() == state)
        .map(|o| o.observed.clone());
    let (line_level, changed, explained) = line_reconciliation(file, claims, observation, class);
    (
        FileRecon {
            path: file.path.into(),
            class,
            line_level,
            committed,
            deleted: file.committed.is_none(),
            last_observed,
        },
        changed,
        explained,
    )
}

fn line_reconciliation(
    file: &ChangedFile<'_>,
    claims: &[Claim<'_>],
    observation: Option<&FsObservation>,
    class: ReconClass,
) -> (LineLevel, u64, u64) {
    let Some(committed) = file.committed else {
        return (LineLevel::Available(Vec::new()), 0, 0);
    };
    let base = file.base.unwrap_or_default();
    if base.contains(&0) || committed.contains(&0) {
        return (
            LineLevel::Unavailable(Reason::Binary),
            1,
            u64::from(class.explained()),
        );
    }
    if base.len().max(committed.len()) > LINE_LEVEL_LIMIT {
        return (
            LineLevel::Unavailable(Reason::TooLarge),
            file.added_lines,
            if class.explained() {
                file.added_lines
            } else {
                0
            },
        );
    }
    let ranges = added_line_ranges(base, committed);
    let changed = ranges.iter().map(|r| u64::from(r.end - r.start)).sum();
    let evidence = line_evidence(file, claims, observation, class);
    let (content, evidence_class) = match evidence {
        Ok(evidence) => evidence,
        Err(reason) => {
            return (
                LineLevel::Unavailable(reason),
                changed,
                if class.explained() { changed } else { 0 },
            );
        }
    };
    let different = content
        .as_deref()
        .map(|content| added_line_ranges(content, committed));
    let mut different = different.as_deref().unwrap_or_default().iter().peekable();
    let mut explained = 0;
    let lines = ranges
        .into_iter()
        .flatten()
        .map(|line| {
            while different.peek().is_some_and(|r| r.end <= line) {
                different.next();
            }
            let matches = content.is_some() && !different.peek().is_some_and(|r| r.contains(&line));
            let class = if matches {
                evidence_class
            } else {
                ReconClass::Unexplained
            };
            explained += u64::from(class.explained());
            LineRecon {
                line: line + 1,
                class,
            }
        })
        .collect();
    (LineLevel::Available(lines), changed, explained)
}

fn line_evidence(
    file: &ChangedFile<'_>,
    claims: &[Claim<'_>],
    observation: Option<&FsObservation>,
    class: ReconClass,
) -> Result<(Option<Vec<u8>>, ReconClass), Reason> {
    if let Some(observation) = observation {
        let Some(expected) = observation.observed.hash.as_ref() else {
            return Ok((None, ReconClass::Unexplained));
        };
        let evidence_class = if claims_state(claims, Some(expected)) {
            ReconClass::Explained
        } else {
            ReconClass::ExplainedFsOnly
        };
        // A matching witness hash authenticates these exact committed bytes;
        // no tool payload is needed for filesystem-only evidence.
        if class.explained() {
            return Ok((file.committed.map(<[u8]>::to_vec), evidence_class));
        }
        if let Some(content) = &observation.content {
            if content.len() > LINE_LEVEL_LIMIT {
                return Err(Reason::TooLarge);
            }
            if hash(content) != *expected {
                return Err(Reason::ReconstructionMismatch);
            }
            return Ok((Some(content.clone()), evidence_class));
        }
        if !claims_state(claims, Some(expected)) {
            return Err(Reason::MissingContent);
        }
        return reconstruct(file.base, claims, expected).map(|bytes| (Some(bytes), evidence_class));
    }
    let Some(last) = claims.last().filter(|c| c.effect.kind == EffectKind::Write) else {
        return Ok((None, ReconClass::Unexplained));
    };
    let Some(expected) = last.effect.written.as_ref() else {
        return Ok((None, ReconClass::Unexplained));
    };
    reconstruct(file.base, claims, expected).map(|bytes| (Some(bytes), ReconClass::ReportedOnly))
}

fn reconstruct(
    base: Option<&[u8]>,
    claims: &[Claim<'_>],
    expected: &ContentHash,
) -> Result<Vec<u8>, Reason> {
    let mut current = base.map(<[u8]>::to_vec);
    let mut matching = None;
    let mut failure = Reason::ReconstructionMismatch;
    for claim in claims {
        if claim.effect.kind == EffectKind::Delete {
            current = None;
            continue;
        }
        let candidate = replay(claim.call, current.as_deref());
        match candidate {
            Ok(bytes) if claim.effect.written.as_ref() == Some(&hash(&bytes)) => {
                if claim.effect.written.as_ref() == Some(expected) {
                    matching = Some(bytes.clone());
                }
                current = Some(bytes);
            }
            result => {
                if let Err(reason) = result {
                    failure = reason;
                }
                current = None;
            }
        }
    }
    matching.ok_or(failure)
}

/// Only reconstruct payloads with known full-write or literal-edit semantics.
/// The stored write-time hash is always the final authority, including when
/// redaction or tool-specific normalization changed the saved arguments.
fn replay(call: &ToolCall, base: Option<&[u8]>) -> Result<Vec<u8>, Reason> {
    use Reason::ReconstructionMismatch as Mismatch;
    // JSON escaping can expand each content byte up to six bytes. Bound JSON
    // parsing too, independently of the cap on the reconstructed result.
    if call.arguments.len() > LINE_LEVEL_LIMIT * 6 + 4096 {
        return Err(Reason::TooLarge);
    }
    let value: serde_json::Value = serde_json::from_str(&call.arguments).map_err(|_| Mismatch)?;
    match call.name.as_str() {
        "Write" => {
            let content = value
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or(Mismatch)?;
            if content.len() > LINE_LEVEL_LIMIT {
                return Err(Reason::TooLarge);
            }
            Ok(content.as_bytes().to_vec())
        }
        "Edit" | "MultiEdit" => {
            let mut content = std::str::from_utf8(base.ok_or(Mismatch)?)
                .map_err(|_| Mismatch)?
                .to_owned();
            if call.name == "MultiEdit" {
                let edits = value
                    .get("edits")
                    .and_then(|v| v.as_array())
                    .ok_or(Mismatch)?;
                for edit in edits {
                    content = replace(&content, edit)?;
                }
            } else {
                content = replace(&content, &value)?;
            }
            Ok(content.into_bytes())
        }
        _ => Err(Mismatch),
    }
}

fn replace(original: &str, edit: &serde_json::Value) -> Result<String, Reason> {
    use Reason::ReconstructionMismatch as Mismatch;
    let old = edit
        .get("old_string")
        .and_then(|v| v.as_str())
        .ok_or(Mismatch)?;
    let new = edit
        .get("new_string")
        .and_then(|v| v.as_str())
        .ok_or(Mismatch)?;
    let all = match edit.get("replace_all") {
        None => false,
        Some(value) => value.as_bool().ok_or(Mismatch)?,
    };
    if old.is_empty() || !original.contains(old) {
        return Err(Mismatch);
    }
    let count = if all {
        original.matches(old).count()
    } else {
        1
    };
    if original
        .len()
        .saturating_add(count.saturating_mul(new.len().saturating_sub(old.len())))
        > LINE_LEVEL_LIMIT
    {
        return Err(Reason::TooLarge);
    }
    Ok(if all {
        original.replace(old, new)
    } else {
        original.replacen(old, new, 1)
    })
}

impl crate::Index {
    /// Reconcile only sessions linked to `commit` by this index. All blobs and
    /// window-filtered witness observations are supplied by the caller.
    pub fn reconcile(
        &self,
        commit: CommitId,
        base: Option<CommitId>,
        changed: &[ChangedFile<'_>],
        observations: &[FsObservation],
    ) -> Reconciliation {
        let sessions: Vec<_> = self
            .sessions_of(commit)
            .iter()
            .filter_map(|id| self.session(*id))
            .collect();
        reconcile(&ReconInput {
            commit,
            base,
            changed,
            sessions: &sessions,
            observations,
        })
    }
}
