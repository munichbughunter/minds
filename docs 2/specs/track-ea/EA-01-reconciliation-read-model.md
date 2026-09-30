# EA-01 — Reconciliation read model: which committed lines did the session produce?

- Commit: `feat(reader): artifact reconciliation — commit against observed writes`
- Branch: `feat/reconciliation`
- Depends on: EA-01a · Size: M · Demo: yes

## Goal
A pure, deterministic derivation in `minds-reader` that classifies every file and, where
possible, every changed line of a commit as `explained`, `explained (fs only)`,
`reported only` or `unexplained`, based on the session's stored write evidence
(`Effect.written`) and — once EA-08 exists — the witness's file observations.

## Non-goals
No CLI output (EA-02), no TUI (EA-03), no assurance level (EA-11). Nothing is stored.

## Read first
- `crates/minds-reader/src/model.rs`, `index.rs` (`evidence_report`), `graph.rs`
- `crates/minds-git/src/diff.rs`, `blame.rs`, `objects.rs` (reading blobs at a revision)
- `crates/minds-core/src/lineage.rs` (`Effect`, `EffectKind`)

## Design
```rust
// minds-reader/src/reconcile.rs (new)
pub struct Reconciliation {
    pub commit: CommitId,
    pub base: Option<CommitId>,          // first parent; None for root commits
    pub files: Vec<FileRecon>,           // sorted by path
    pub explained_lines: u64,            // Explained + ExplainedFsOnly
    pub total_changed_lines: u64,        // added/modified lines in the commit
}
pub struct FileRecon {
    pub path: String,
    pub class: ReconClass,               // file-level class
    pub line_level: LineLevel,           // Available(Vec<LineRecon>) | Unavailable(Reason)
    pub committed: ContentHash,          // blake3 of the committed blob
    pub last_observed: Option<ObservedAt>, // hash + seq/time of the last matching observation
}
pub struct LineRecon { pub line: u32, pub class: ReconClass }
pub enum ReconClass { Explained, ExplainedFsOnly, ReportedOnly, Unexplained }

/// Pure: no I/O inside. Callers pass the blobs they read.
pub fn reconcile(input: &ReconInput<'_>) -> Reconciliation;
pub struct ReconInput<'a> {
    pub commit: CommitId, pub base: Option<CommitId>,
    pub changed: &'a [ChangedFile],      // path, base blob bytes, committed blob bytes
    pub sessions: &'a [&'a Session],     // sessions linked to the commit
    pub observations: &'a [FsObservation], // empty until EA-08
}
```
File-level rules (per changed path P, committed hash H):
1. `last_claim(P)` = the last write effect to P across the linked sessions (turn order),
   using `Effect.written`. `last_obs(P)` = the last `fs.observed` for P in the session
   window (empty before EA-08).
2. If `last_obs(P) == H` and some claimed `written == H` → `Explained`.
3. If `last_obs(P) == H` and no claim has H → `ExplainedFsOnly`.
4. If no observations exist for P and `last_claim(P).written == H` → `ReportedOnly`.
5. Otherwise → `Unexplained` (includes: `written` unavailable, deleted/renamed without
   evidence, human edits).
Deletes: a committed deletion is explained if a `Delete` effect or an observation of
absence exists; otherwise unexplained. Renames: treat as delete + add.

Line level (best effort):
- Reconstruct the observed content for P: for `Write` use the payload content; for `Edit`
  replay onto the base blob as in EA-01a. Accept the reconstruction only if it hashes to
  the observed/claimed hash; otherwise `Unavailable(ReconstructionMismatch)`.
- Diff reconstructed vs committed (line diff, same algorithm as `minds-git::diff`): lines
  added/modified in the commit (vs base) that are identical in the reconstruction inherit
  the file-level class; lines that differ are `Unexplained`.
- `total_changed_lines` counts added/modified lines vs base; binary files count as 1 line
  and have `Unavailable(Binary)`.

## Acceptance criteria
- [ ] Agent-only commit (fixture) → all files `ReportedOnly` at A1 (no observations), 0 unexplained.
- [ ] Same commit plus one human-edited line → exactly that line `Unexplained`, file class `Unexplained`, all other lines keep their class.
- [ ] A file written via `Bash` (no `written`) → `Unexplained` at A1 (becomes `ExplainedFsOnly` with observations — covered by a test with a synthetic `FsObservation`).
- [ ] With synthetic observations matching claims → `Explained`.
- [ ] Deterministic: same input ⇒ identical `Reconciliation` (property test over shuffled `sessions` order where turn order is defined).
- [ ] No ref moves, no store writes (asserted like `reinterpret_is_read_only_and_deterministic`).

## Tests
`reconcile_agent_only_commit_is_reported_only`, `reconcile_human_line_is_unexplained`,
`reconcile_shell_write_is_unexplained_without_observer`,
`reconcile_observed_and_claimed_is_explained`, `reconcile_fs_only`,
`reconcile_delete_and_rename`, `reconcile_binary_file_file_level_only`,
`reconcile_is_deterministic`, `reconcile_is_read_only`.

## Security review focus
Blob reads are bounded (cap line-level work at e.g. 2 MiB per file, larger → file level
only). No content from blobs is printed by this module.
