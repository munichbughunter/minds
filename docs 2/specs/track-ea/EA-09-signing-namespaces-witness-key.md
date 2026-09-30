# EA-09 — Signing namespaces and the witness key

- Commit: `feat(attest): signing namespaces — witness, intent and anchor keys kept apart`
- Branch: `feat/signing-namespaces`
- Depends on: — (uses `ssh_sign_ns` from EA-06a if merged, otherwise adds it) · Size: S · Demo: yes

## Goal
Distinct ssh-sig namespaces per key role, a witness key generator, and verification that
enforces "a witness-scoped seal must be signed under `minds-witness` by an allowed witness
principal".

## Read first
`crates/minds-attest/src/lib.rs`, `crates/minds-cli/src/verify_cmd.rs` (`check_seals`,
`signature_state`, `resolve_signers`), `docs/verification-guide.md`, ADR-0008.

## Design
- Constants in `minds-attest`: `NS_DEFAULT = "minds"`, `NS_WITNESS = "minds-witness"`,
  `NS_INTENT = "minds-intent"`, `NS_ANCHOR = "minds-anchor"`. `NAMESPACE` stays as an alias
  of `NS_DEFAULT`.
- `ssh_sign_ns(payload, key, ns)`, `ssh_verify_ns(payload, sig, signers, identity, ns)`,
  `ssh_find_principals(sig, signers) -> Vec<String>` (wraps `ssh-keygen -Y find-principals`).
- `minds witness keygen`: `ssh-keygen -t ed25519 -N "" -C minds-witness@<hostname> -f <home>/key/witness_ed25519`,
  refuse if the key exists, enforce 0600, print the `allowed_signers` line to stdout:
  `minds-witness@<host> namespaces="minds-witness" ssh-ed25519 AAAA…`
- Seal verification: for scope `witness/v1` or `witness-fs/v1`, find the principal via
  `find-principals` against the provided `allowed_signers`, then verify with `NS_WITNESS`.
  Outcomes: valid → signature state `witness-signed (<principal>)`; signature present but
  invalid or under the wrong namespace → **integrity finding** (`TAMPERED`, with the
  reason `witness seal not signed under minds-witness`); no `allowed_signers` given →
  `signature not checked` (no verdict change; EA-11 will cap assurance at A1).
- A witness seal **without** `seal.sig` → integrity finding (a witness always signs).

## Acceptance criteria
- [ ] Golden: a seal signed with a human key under `minds` but carrying `scope=witness/v1` → TAMPERED with the reason above.
- [ ] Valid witness signature with correct `allowed_signers` → `witness-signed (minds-witness@host)`.
- [ ] `allowed_signers` restricting the witness principal to `minds-witness`: the same key used to sign a review under `minds` fails verification (ssh-keygen enforces it; test proves it).
- [ ] `keygen` refuses to overwrite, sets 0600, prints the allowed_signers line.

## Tests
`witness_seal_wrong_namespace_is_tampered`, `witness_seal_missing_signature_is_tampered`,
`witness_seal_valid_signature`, `namespace_restriction_blocks_cross_role_use`,
`witness_keygen_is_safe`.

## Docs
Verification guide: recipe with `ssh-keygen -Y find-principals` and `-Y verify -n minds-witness`.
