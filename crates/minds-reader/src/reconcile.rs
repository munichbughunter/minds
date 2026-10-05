//! Reconciliation of committed bytes with stored write evidence. Pure and
//! read-only: callers supply blobs and observations from the session window.
//!
//! Paths are repo-relative identities, not display strings; consumers must
//! sanitize them before rendering. Blob contents never appear in the result.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use minds_core::{ContentHash, Effect, EffectKind, Session, SessionId, ToolCall};
use minds_git::{CommitId, added_line_ranges, removes_lines};

/// Maximum size of any blob or reconstructed content used for line attribution.
pub const LINE_LEVEL_LIMIT: usize = 2 * 1024 * 1024;

/// A commit's derivation, sorted by repository path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub commit: CommitId,
    /// First parent, or `None` for a root commit.
    pub base: Option<CommitId>,
    pub files: Vec<FileRecon>,
    /// Only witness-backed lines (`Explained`, `ExplainedFsOnly`), never
    /// `ReportedOnly`. For "every line with some evidence" see
    /// [`Reconciliation::backed_lines`].
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
    /// Added/modified lines of this file against the base, counted like
    /// `Reconciliation::total_changed_lines` (binary files count one).
    pub changed_lines: u64,
    /// Whether content went missing that the added lines do not show: a
    /// deletion; a binary or oversized modification (unknown amounts); or,
    /// for text, a diff hunk that removes more lines than it adds — against
    /// the base, or against the claimed content (written by the agent, then
    /// removed). A replacement in place shows as added lines and does not
    /// count.
    pub removes: bool,
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

impl Reconciliation {
    /// Folds a reconciliation of further files of the same commit into this
    /// one — for callers that reconcile file by file to bound memory. Paths
    /// must be disjoint; the result stays sorted by path. Absorbing in path
    /// order costs no sort.
    pub fn absorb(&mut self, other: Reconciliation) {
        self.explained_lines += other.explained_lines;
        self.total_changed_lines += other.total_changed_lines;
        let in_order = match (self.files.last(), other.files.first()) {
            (Some(last), Some(first)) => last.path <= first.path,
            _ => true,
        };
        self.files.extend(other.files);
        if !in_order {
            self.files.sort_by(|a, b| a.path.cmp(&b.path));
        }
    }

    /// Changed lines with some evidence: everything not `Unexplained`,
    /// including `ReportedOnly` (a tool claim, no witness).
    pub fn backed_lines(&self) -> u64 {
        self.total_changed_lines
            .saturating_sub(self.unexplained_lines())
    }

    /// Changed lines without any evidence (`Unexplained`). Where line-level
    /// attribution is unavailable, the file-level class applies to all of the
    /// file's changed lines. `ReportedOnly` lines are not counted here: they
    /// are backed by a tool claim, only not by a witness.
    pub fn unexplained_lines(&self) -> u64 {
        self.files.iter().map(FileRecon::unexplained_lines).sum()
    }
}

impl FileRecon {
    /// Changed lines of this file without any evidence; see
    /// [`Reconciliation::unexplained_lines`].
    pub fn unexplained_lines(&self) -> u64 {
        match &self.line_level {
            LineLevel::Available(lines) => lines
                .iter()
                .filter(|l| l.class == ReconClass::Unexplained)
                .count() as u64,
            LineLevel::Unavailable(_) if self.class == ReconClass::Unexplained => {
                self.changed_lines
            }
            LineLevel::Unavailable(_) => 0,
        }
    }
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
    /// Upper bound of added/modified lines (e.g. all lines of the committed
    /// blob). Used only when a blob exceeds `LINE_LEVEL_LIMIT`, and only for
    /// an `Unexplained` file — a backed oversized file weighs one line;
    /// smaller text diffs are derived here. Deletions always count zero and
    /// binary additions/modifications one.
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

/// A witness file observation (EA-08, see [`crate::observations`]). Callers
/// restrict observations to the linked sessions' window. No filesystem is
/// consulted, and `content` is optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsObservation {
    pub path: String,
    pub observed: ObservedAt,
    /// Optional bytes for partial line attribution when the commit differs.
    /// Accepted only after verification against `observed.hash`.
    pub content: Option<Vec<u8>>,
    /// The file was present, but the witness deliberately recorded no hash
    /// (secret file, too large, link target outside the repository) —
    /// `observed.hash` is then `None` without meaning absence. A latest
    /// opaque observation confirms nothing and never erases an earlier
    /// readable contradiction; without one, the file is classified as if
    /// unobserved (claims only). It can therefore classify worse than claims
    /// alone — when the witness last read different bytes.
    pub opaque: bool,
}

pub struct ReconInput<'a> {
    pub commit: CommitId,
    pub base: Option<CommitId>,
    pub changed: &'a [ChangedFile<'a>],
    pub sessions: &'a [&'a Session],
    pub observations: &'a [FsObservation],
    /// Absolute repository roots of the verifying checkout. Agents record
    /// absolute effect paths (Claude Code always does); see [`claim_path`]
    /// for how a claim is mapped to repo-relative paths.
    pub roots: &'a [&'a Path],
}

/// The one repo-relative path a claimed effect path names, or `None`.
///
/// Purely lexical. A relative claim is resolved against the session's
/// recorded `cwd` when that is known and absolute, otherwise taken as
/// repo-relative. An absolute claim under one of `roots` (the verifying
/// checkout) names exactly that path.
///
/// Otherwise — another machine, worktree or symlink — the session's own
/// `cwd` and its ancestors (never `/`) are tried as the root spelling at
/// capture time, for claims below that `cwd` only. Which ancestor was the
/// repository root is not recorded, so this fallback answers only when
/// exactly **one** candidate is a path that
/// `known` confirms (callers pass "exists in the base or committed tree");
/// any ambiguity drops the claim (fail-closed). `exact_only` disables the
/// fallback — for deletions, which carry no hash that could catch a wrong
/// mapping.
pub fn claim_path(
    path: &str,
    cwd: Option<&str>,
    roots: &[&Path],
    known: &dyn Fn(&str) -> bool,
    exact_only: bool,
) -> Option<String> {
    let cwd = cwd.map(Path::new).filter(|cwd| cwd.is_absolute());
    let absolute = match cwd {
        Some(cwd) if Path::new(path).is_relative() && plain(path) => {
            cwd.join(path).to_string_lossy().into_owned()
        }
        _ => path.to_owned(),
    };
    if Path::new(&absolute).is_relative() {
        return repo_path(&absolute, &[]);
    }
    if let Some(exact) = repo_path(&absolute, roots) {
        return Some(exact);
    }
    // Only claims below cwd: anything outside it may as well lie outside the
    // repository.
    if exact_only || !cwd.is_some_and(|cwd| Path::new(&absolute).starts_with(cwd)) {
        return None;
    }
    let mut candidates: Vec<String> = cwd
        .into_iter()
        .flat_map(Path::ancestors)
        .filter(|a| a.parent().is_some())
        .filter_map(|root| repo_path(&absolute, &[root]))
        .filter(|candidate| known(candidate))
        .collect();
    candidates.sort();
    candidates.dedup();
    match candidates.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

fn plain(path: &str) -> bool {
    !(path.starts_with('~') || path.starts_with('$') || path.contains('\\'))
}

/// The repo-relative identity of a claimed effect path, or `None` if it
/// cannot name a file of this repository. Purely lexical: no filesystem
/// access, no symlink resolution (callers pass every root spelling they
/// accept). `..`, shell expansion and backslashes never match.
pub fn repo_path(path: &str, roots: &[&Path]) -> Option<String> {
    if !plain(path) {
        return None;
    }
    let candidate = Path::new(path);
    let relative = if candidate.is_absolute() {
        roots
            .iter()
            .find_map(|root| candidate.strip_prefix(root).ok())?
    } else {
        candidate
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

struct Claim<'a> {
    call: &'a ToolCall,
    effect: &'a Effect,
}

/// The write and delete claims of a set of sessions, in merged turn order and
/// keyed by repo-relative path — built once per commit, then queried file by
/// file (callers that bound memory reconcile one file at a time).
#[derive(Default)]
pub struct Claims<'a> {
    by_path: BTreeMap<String, Vec<Claim<'a>>>,
}

fn timestamp(at: Option<&str>) -> Option<jiff::Timestamp> {
    at.and_then(|at| at.parse().ok())
}

impl<'a> Claims<'a> {
    /// Collects the claims of `sessions`; `known` confirms candidate paths
    /// of the capture-time fallback (see [`claim_path`]).
    ///
    /// Turns are merged by parsed timestamp (falling back to session start),
    /// then content-addressed session identity, turn index and call index.
    /// Missing times sort first. Within a session, turn order remains
    /// authoritative even when a clock moves backwards. Ties therefore never
    /// depend on input session order.
    pub fn collect(
        sessions: &[&'a Session],
        roots: &[&Path],
        known: &dyn Fn(&str) -> bool,
    ) -> Self {
        let mut turns = Vec::new();
        for &session in sessions {
            let id = SessionId::of(session).expect("Session has a canonical representation");
            let mut time = timestamp(
                session
                    .lineage
                    .as_ref()
                    .and_then(|l| l.started_at.as_deref()),
            );
            for (turn_index, turn) in session.turns.iter().enumerate() {
                time = time.max(timestamp(turn.at.as_deref()));
                turns.push((time, id, turn_index, turn, session));
            }
        }
        turns.sort_by_key(|(time, id, index, _, _)| (*time, *id, *index));
        let mut by_path: BTreeMap<String, Vec<Claim<'a>>> = BTreeMap::new();
        for (_, _, _, turn, session) in turns {
            let cwd = session.lineage.as_ref().and_then(|l| l.cwd.as_deref());
            for call in &turn.tool_calls {
                if let Some(effect) = &call.effect
                    && matches!(effect.kind, EffectKind::Write | EffectKind::Delete)
                    && let Some(path) = effect.path.as_deref()
                    && let Some(path) =
                        claim_path(path, cwd, roots, known, effect.kind == EffectKind::Delete)
                {
                    by_path
                        .entry(path)
                        .or_default()
                        .push(Claim { call, effect });
                }
            }
        }
        Self { by_path }
    }

    /// Reconciles `changed` against these claims. No I/O, no mutation.
    pub fn reconcile(
        &self,
        commit: CommitId,
        base: Option<CommitId>,
        changed: &[ChangedFile<'_>],
        observations: &[FsObservation],
    ) -> Reconciliation {
        let mut by_path: BTreeMap<&str, Vec<&FsObservation>> = BTreeMap::new();
        for observation in observations {
            by_path
                .entry(&observation.path)
                .or_default()
                .push(observation);
        }
        for observations in by_path.values_mut() {
            observations.sort_by(|a, b| {
                let key = |o: &FsObservation| (timestamp(o.observed.at.as_deref()), o.observed.seq);
                key(a)
                    .cmp(&key(b))
                    .then_with(|| a.observed.hash.cmp(&b.observed.hash))
                    .then_with(|| a.observed.at.cmp(&b.observed.at))
                    .then_with(|| a.opaque.cmp(&b.opaque))
                    .then_with(|| a.content.cmp(&b.content))
            });
        }
        let mut result = Reconciliation {
            commit,
            base,
            files: Vec::new(),
            explained_lines: 0,
            total_changed_lines: 0,
        };
        for file in changed {
            let claims = self
                .by_path
                .get(file.path)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let observations = by_path
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
}

/// Derive reconciliation without I/O or mutation of any evidence. Without
/// access to the trees, only the changed paths confirm fallback candidates
/// (see [`claim_path`]); callers with a repository use [`Claims::collect`].
pub fn reconcile(input: &ReconInput<'_>) -> Reconciliation {
    let changed: BTreeSet<&str> = input.changed.iter().map(|f| f.path).collect();
    let known = |path: &str| changed.contains(path);
    Claims::collect(input.sessions, input.roots, &known).reconcile(
        input.commit,
        input.base,
        input.changed,
        input.observations,
    )
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
        if observation.opaque || observation.observed.hash.as_ref() != state {
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
    // An opaque latest state says nothing about the bytes: fall back to the
    // claims, as without a witness — unless an earlier, readable observation
    // already contradicts the committed state. An opaque touch (a hard link,
    // a secret-looking rewrite) must not erase a contradiction.
    let observation = match observations.last().copied() {
        Some(latest) if latest.opaque => observations
            .iter()
            .rev()
            .copied()
            .find(|o| !o.opaque)
            .filter(|o| o.observed.hash.as_ref() != state),
        latest => latest,
    };
    let class = classify(claims, observation, state);
    let last_observed = observations
        .iter()
        .rev()
        .find(|o| !o.opaque && o.observed.hash.as_ref() == state)
        .map(|o| o.observed.clone());
    let (line_level, changed, explained, removes) =
        line_reconciliation(file, claims, observation, class);
    (
        FileRecon {
            path: file.path.into(),
            class,
            line_level,
            committed,
            deleted: file.committed.is_none(),
            changed_lines: changed,
            removes,
            last_observed,
        },
        changed,
        explained,
    )
}

fn lines_in(ranges: &[std::ops::Range<u32>]) -> u64 {
    ranges.iter().map(|r| u64::from(r.end - r.start)).sum()
}

fn line_reconciliation(
    file: &ChangedFile<'_>,
    claims: &[Claim<'_>],
    observation: Option<&FsObservation>,
    class: ReconClass,
) -> (LineLevel, u64, u64, bool) {
    let Some(committed) = file.committed else {
        return (LineLevel::Available(Vec::new()), 0, 0, file.base.is_some());
    };
    let base = file.base.unwrap_or_default();
    // Without line attribution, any change to existing bytes may remove.
    let opaque_removal = file.base.is_some_and(|b| b != committed);
    if base.contains(&0) || committed.contains(&0) {
        return (
            LineLevel::Unavailable(Reason::Binary),
            1,
            u64::from(class.explained()),
            opaque_removal,
        );
    }
    if base.len().max(committed.len()) > LINE_LEVEL_LIMIT {
        // `added_lines` is an upper bound for oversized blobs: conservative
        // only against the file. Backed files therefore weigh one line, like
        // binaries — a regenerated lockfile must not dilute unexplained
        // lines elsewhere.
        let changed = if class == ReconClass::Unexplained {
            file.added_lines
        } else {
            1
        };
        return (
            LineLevel::Unavailable(Reason::TooLarge),
            changed,
            u64::from(class.explained()),
            opaque_removal,
        );
    }
    let ranges = added_line_ranges(base, committed);
    let changed = lines_in(&ranges);
    let base_removes = removes_lines(base, committed);
    let evidence = line_evidence(file, claims, observation, class);
    let (content, evidence_class) = match evidence {
        Ok(evidence) => evidence,
        Err(reason) => {
            return (
                LineLevel::Unavailable(reason),
                changed,
                if class.explained() { changed } else { 0 },
                base_removes,
            );
        }
    };
    // Claimed lines the commit lacks: written by the agent, then removed or
    // replaced by someone else — invisible to the added-line view below.
    let claim_removes = content
        .as_deref()
        .is_some_and(|content| removes_lines(content, committed));
    let different = content
        .as_deref()
        .map(|content| added_line_ranges(content, committed));
    let mut different = different.as_deref().unwrap_or_default().iter().peekable();
    let mut explained = 0;
    let lines: Vec<LineRecon> = ranges
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
    // Reconstructed content already accounts for removals between base and
    // claim (the agent made them); only what vanished afterwards counts.
    let removes = match content.as_deref() {
        Some(_) => claim_removes,
        None => base_removes,
    };
    (LineLevel::Available(lines), changed, explained, removes)
}

fn line_evidence(
    file: &ChangedFile<'_>,
    claims: &[Claim<'_>],
    observation: Option<&FsObservation>,
    class: ReconClass,
) -> Result<(Option<Vec<u8>>, ReconClass), Reason> {
    if let Some(observation) = observation {
        let Some(expected) = observation
            .observed
            .hash
            .as_ref()
            .filter(|_| !observation.opaque)
        else {
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
    /// Reconcile only the claimants of `commit` ([`crate::Index::claimants`]:
    /// trailer sessions, store-index links only without a trailer). All blobs
    /// and window-filtered witness observations are supplied by the caller.
    pub fn reconcile(
        &self,
        commit: CommitId,
        base: Option<CommitId>,
        changed: &[ChangedFile<'_>],
        observations: &[FsObservation],
        roots: &[&Path],
    ) -> Reconciliation {
        let (sessions, _) = self.claimants(commit);
        reconcile(&ReconInput {
            commit,
            base,
            changed,
            sessions: &sessions,
            observations,
            roots,
        })
    }
}
