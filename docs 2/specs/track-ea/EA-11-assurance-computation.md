# EA-11 — Assurance computation (read model)

- Commit: `feat(reader): assurance levels computed per sealed range, never stored`
- Branch: `feat/assurance`
- Depends on: EA-01, EA-08, EA-09 (EA-14/EA-15/EA-18b/EA-19 extend it later) · Size: M · Demo: yes

## Goal
A pure function that states, per sealed range and for the session overall, which
assurance level the material supports and why — computed from facts at read time (W2).

## Read first
ADR-0012 "Assurance levels", `00-conventions.md`, `crates/minds-reader/src/model.rs`
(`EvidenceReport`, `EpochReport`), `index.rs::evidence_report`, EA-01/EA-08 reader APIs.

## Design
```rust
pub enum Assurance { A0Claimed, A1Observed, A2Witnessed, A3Reproduced } // Ord: A0 < … < A3
pub struct RangeAssurance { pub seal: ContentHash, pub level: Assurance, pub reasons: Vec<Reason> }
pub struct AssuranceReport {
    pub overall: Assurance,                 // min over ranges; A0 if no ranges
    pub ranges: Vec<RangeAssurance>,
    pub witness: Option<WitnessFacts>,      // principal, profile (from witness.start), key fingerprint
    pub intent: IntentState,                // Unbound | Bound{signed: Option<SignerKind>} (EA-14/15; Unbound until then)
    pub replay: Option<ReplaySummary>,      // EA-18b
    pub anchored: Option<AnchorSummary>,    // EA-19
}
pub struct AssuranceInput<'a> { /* seals with signature states, observation coverage,
    reconciliation, intent, replay, anchors, ledger (Option), trusted_signers: bool */ }
pub fn assess(input: &AssuranceInput<'_>) -> AssuranceReport; // pure, deterministic
```
Rules per range (first failing condition sets the level and adds the reason):
- **A0** — no seal, legacy, or provenance `inferred`.
- **A1** — seal scope `agent-hooks/v1`; or a witness scope whose signature could not be
  checked (`no trusted allowed_signers` → reason).
- **A2** requires all of: scope `witness/v1`; signature valid under `minds-witness` from a
  trusted `allowed_signers` (passed explicitly, never read from the repo); a
  `witness-fs/v1` range covering the session window without gaps; profile ≠ `managed`
  unless EA-S1 enabled it (constant in code, flipped by the spike follow-up); intent
  bound **and** signed under `minds-intent` (until EA-15 is merged: reason
  `intent not bound` and cap at A1 — i.e. A2 becomes reachable only after EA-15).
- **A3** requires A2 plus: replay present with no `claim not reproduced` and at least one
  decisive command (or the reason `no decisive commands to reproduce`), plus a valid
  `minds-anchor` countersignature for every seal of the session.
- Ledger (when `--witness-home` given): a ledger seal id missing from the repo is **not** an
  assurance reason but an integrity finding reported by EA-12.
- Uncorroborated claims and unexplained lines do **not** lower the level — they are
  coverage facts (shown by EA-12). The level says who observed, not how clean the session was.

## Acceptance criteria
- [ ] Table-driven test covering every rule and its reason text.
- [ ] Overall = weakest range (mixed A1/A2 fixture → A1 with the A1 range's reason).
- [ ] Determinism and read-only asserted.
- [ ] No serde `Serialize` on `Assurance` types that could end up in stored objects (compile-time: keep them out of `minds-core` stored envelopes; add a doc test or a grep test).

## Tests
`assurance_rules_table`, `assurance_overall_is_weakest_range`,
`assurance_untrusted_signers_caps_at_a1`, `assurance_requires_fs_coverage_for_a2`,
`assurance_is_never_stored`.
