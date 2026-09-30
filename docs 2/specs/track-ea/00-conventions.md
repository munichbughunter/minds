# 00 — Conventions shared by all Track EA specs

## Invariants (in addition to `.claude/skills/feature/SKILL.md`)

The five project invariants (evidence vs. derivation, fail-closed redaction, Git
invisibility, static musl binary, tolerant read / canonical write) apply unchanged.
Track EA adds these; violating one is a hard blocker:

- **W1 — One writer.** Witnessed evidence (journal, salt, epoch state, witness key) lives
  only in the witness state dir. No code path running on the agent side may read or write
  it. The agent side knows the socket path, nothing else.
- **W2 — Assurance is computed, never stored.** No field in any stored object claims an
  assurance level, a reconciliation class, `corroborated`, or `reproduced`. These are
  derived in `minds-reader` at read time.
- **W3 — The hook stays a hook.** `minds hook`: exit 0 always, `catch_unwind`, no byte on
  stdout, no repo open, no config read. Witness discovery only via the environment
  variable `MINDS_WITNESS_SOCKET`. The existing latency budget holds.
- **W4 — The chain is not reinvented.** Chain fold, hash domains `minds/evidence/v1/*`,
  salt rules and `minds-seal-v1` fields stay as defined in ADR-0011. New hash domains are
  added only where a spec names them.
- **W5 — Derivations live in the reader.** Reconciliation, corroboration, scope findings,
  replay matching are `minds-reader` functions over stored material. Nothing of this
  enters `minds-capture`'s write path.
- **W6 — The verdict contract is frozen.** Three axes (Integrity / Coverage /
  Interpretation), exit codes 0 VERIFIED · 1 TAMPERED · 2 VERIFIED, INCOMPLETE · 3 NOT
  VERIFIABLE · 4 operational failure. Assurance and reconciliation never change them,
  except the explicit gates (`--require-*`), which map a failed gate to exit 2.
- **W7 — Observation, not surveillance.** The file observer records repo-relative paths
  and blake3 hashes only; secret-file wall applies; `.gitignore`d and untracked-outside-repo
  paths are never fingerprinted; nothing is ever attributed to a person.

## Vocabulary (use exactly these identifiers)

| Concept | Rust | CLI text |
|---|---|---|
| Assurance levels | `Assurance::{A0Claimed, A1Observed, A2Witnessed, A3Reproduced}` | `A0 claimed`, `A1 observed`, `A2 witnessed`, `A3 reproduced` |
| Reconciliation classes | `ReconClass::{Explained, ExplainedFsOnly, ReportedOnly, Unexplained}` | `explained`, `explained (fs only)`, `reported only`, `unexplained` |
| Corroboration of a write claim | `Corroboration::{Corroborated, Uncorroborated, NotApplicable}` | `uncorroborated` |
| Scope finding | `Finding::OutOfScope` | `out of scope` |
| Replay finding | `Finding::ClaimNotReproduced` | `claim not reproduced` |
| Isolation profiles | `WitnessProfile::{Container, User, Managed}` | `container`, `user`, `managed` |

## Seal scopes

| Scope | Written by | Meaning |
|---|---|---|
| `agent-hooks/v1` | local checkpoint (today) | A1 range, journal in `<git-dir>` |
| `witness/v1` | witness | agent hook events received and chained by the witness |
| `witness-fs/v1` | witness | file-system observations chained by the witness |

## ssh-sig namespaces

| Namespace | Signs | Signer |
|---|---|---|
| `minds` | reviews, attributions, legacy seals (unchanged) | humans |
| `minds-witness` | seals with scope `witness/v1` and `witness-fs/v1` | witness key |
| `minds-intent` | intent anchors | humans (sk key recommended) |
| `minds-anchor` | replay records, first-sight countersignatures | CI key |

`allowed_signers` restricts each principal with `namespaces="…"`. A witness-scoped seal
signed under another namespace is an **integrity** finding.

## Witness state dir (host side)

```text
$MINDS_WITNESS_HOME            default: $XDG_STATE_HOME/minds-witness/<repo-id>  (0700)
├─ witness.json                config: repo root (host path), path map, profile
├─ journal/                    same layout as <git-dir>/minds/journal/
├─ evidence/state/             salts and epoch state, same format as today
├─ key/witness_ed25519[.pub]   witness key (0600)
├─ ledger                      append-only list of produced seal ids
├─ log/witness.log             diagnostics, redacted like hook.log
└─ run/witness.sock            the only thing mounted into the agent's domain
```

`<repo-id>` = first 16 hex of `blake3(canonical host repo root path)`.

## Configuration locations

- Agent side: environment only (`MINDS_WITNESS_SOCKET`). Nothing in `.git/config` may
  influence trust decisions, because the agent can edit it.
- Witness side: `witness.json` in the state dir.
- Team policy that goes through code review: `.minds/replay.json` (replay tolerances),
  `.minds/redact.json` (unchanged).

## Test and code style

- German doc comments, English CLI output (one verdict vocabulary in `minds-core`).
- Golden tests for every text format (frame, intent anchor, seal lines, CLI blocks).
- Unix-only features behind `#[cfg(unix)]` with an explicit "not supported on this
  platform" error elsewhere; Windows must still build.
- No new dependency that breaks `x86_64-unknown-linux-musl`. Allowed new crates:
  `notify` (file observer). Anything else needs a note in the commit body.
