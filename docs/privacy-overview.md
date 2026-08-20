# Minds — Privacy Overview

*For the pilot partner's internal approval process. As of v0.1.3. Every claim
in this document can be traced to the code; the known gaps are listed at the
end — with issue numbers, not in a footnote.*

*Deutsche Fassung: [datenschutz-uebersicht.md](datenschutz-uebersicht.md)*

---

## 1. What is captured?

Minds records **agent sessions** (in the pilot: Claude Code) and stores each
one as a structured object. That object contains:

- **Prompts** in full text and the agent's **text responses**. The model's
  internal "thinking" blocks are not carried over.
- **Tool calls with their arguments** — including file paths, shell commands
  in plain text, and, for writing tools (`Write`/`Edit`), the content being
  written. All of it passes through redaction before anything is stored
  (section 3).
- **Tool results do not**: whatever a tool returned — say, the content of a
  file that was read — never reaches the stored object. The written file
  artifact itself is additionally referenced only as a BLAKE3 hash — and for
  credential files, not even that.
- **Metadata**: agent and version, model, token counts, timestamps, and the
  working directory. The directory path passes through the PII check, but
  that check only recognizes e-mail shapes and denylist terms — an ordinary
  username in the path (`/Users/<name>/…`) is left in place. If that is not
  acceptable, add the name to the policy's denylist (`deny_pii` in
  `.minds/redact.json`).

Not part of the Minds object, but present in any Git repository: the Git
identity (`user.name`/`user.email`) on commits. Reviews carry the reviewer's
e-mail address from the Git configuration.

## 2. Where does the data live?

Everything stays **inside the repository and its `.git` directory** — there
is no database, no service, no cloud component.

| Location | Content | Protection |
|---|---|---|
| `.git/minds/journal/…` | **raw data before redaction**, including tool results in plain text | files 0600; deleted after a successful checkpoint (gap: section 6) |
| `.git/minds/hook.log` | diagnostic lines from the hooks and from the backfill started by `minds enable` | 0600, rotated at 1 MiB; control characters escaped, lines capped; URL credentials are stripped |
| `refs/minds/store/<hash>` | the **redacted** session (`session.json`) plus its edges to the commit | reaches the store only after redaction (enforced by the type system) |
| `refs/minds/sessions/<hex>` | browsable copy (including rendered `session.md`) | appears on push as a regular branch `minds/session/<hex>` |
| `refs/minds/context` | index over the sessions | same as store |
| `refs/minds/reviews` | review verdicts: decision, reviewer e-mail, free-text summary | **not redacted** — whatever the reviewer writes in `--summary` sits verbatim in the ref and in the mirrored MR note; responsibility lies with the reviewer |
| commit trailers | `Minds-Session-Id` / `Minds-Change-Id` | hashes only, never content |

Beyond that there are only **user-driven exports**: `minds render`,
`minds distill --out`, and `minds audit --out` write only on explicit
invocation, to the location given — and from already-redacted data.

## 3. Redaction runs before storing

A secret that never enters the store never needs to be deleted. The data
path is built on that:

- **Fail-closed, enforced by the type system:** the store only accepts
  objects that have passed the redaction pipeline — the type system
  guarantees this, not a convention. Both intake paths (live capture and
  `minds import`) pass through the same wall.
- **Credential files never reach the stored object.** Anyone touching
  `.env`, `id_rsa`, `credentials.json`, keystores or the like leaves only
  `[omitted:secret-file]` plus the rule name in the object. The limit of
  this wall — it is path-based — is described in section 6.
- **Detectors** (all active by default): known token shapes — including the
  GitLab family (`glpat-`, `glcbt-`, …), Anthropic, OpenAI, AWS key IDs,
  Slack, JWT, PEM blocks —, an entropy safety net, assignments
  (`PASSWORD=…`), URL credentials in the userinfo (`https://user:pw@…`),
  auth flags (`curl -u`), and e-mail addresses (PII). Query parameters such
  as `?private_token=…` are caught only when the value looks like a
  credential — a purely alphabetic value may survive (the diagnostic sink
  `hook.log` applies a stricter rule there). Extendable per repository via
  `.minds/redact.json` (denylist/allowlist).
- **A broken configuration stops the write.** A typo in `redact.json` aborts
  with a line number — never a silent fallback to weaker protection. The
  error message quotes no values.
- The stored object records only **counts** about redaction (how many
  findings), never the found values themselves.

## 4. What leaves the machine? Nothing on its own.

The binary contains **no HTTP stack, no telemetry, no update check**.
Exactly two network paths exist, both triggered by the user:

1. **`git push`** — the `pre-push` hook transfers `refs/minds/*` to exactly
   the remote the user is pushing to anyway. If there is nothing new, no
   connection is opened. It never pushes with `--force` — with exactly one,
   narrowly scoped exception: a session ref erased via `minds forget`
   (verifiably a tombstone, never plain text) is force-pushed deliberately
   so the deletion reaches the forge as well
   ([#102](https://github.com/munichbughunter/minds/issues/102)); the
   transfer is reported during the push and recorded in `hook.log`. Can be
   disabled via `git config minds.sync false`.
2. **`minds gitlab mirror`** — only on explicit invocation. What is
   transferred is a review verdict as a merge request note (decision,
   reviewer, summary, hash) — **no session content**, but the reviewer's
   summary verbatim and unredacted (see the table in section 2). The API
   token comes exclusively from an environment variable and appears neither
   in the process list nor on disk nor in error messages.

The `pre-push` hook transfers only Minds' own refs; the browsable session
refs appear on the remote as regular branches `minds/session/<hex>` (see the
table in section 2).

The data therefore lives exclusively in the partner's repository and on the
partner's forge — nothing reaches the maker of Minds.

## 5. Deletion: `minds forget`

`minds forget <session>` replaces the payload at all three storage locations
with a **parentless tombstone commit** — the plain text is no longer
reachable even through the ref history (`~1`), and `git rev-list --objects
--all` no longer finds the payload blob (covered by tests). Re-importing the
same session is rejected; `show`/`why`/`fsck` keep working and name the
session as forgotten instead of failing. Physical removal happens with the
next `git gc`; until then the object is unreachable but present. In its
default configuration Git keeps no reflog for `refs/minds/*`. A session ref
already pushed to the forge is caught up by the next `git push` (or
`minds sync`) via a targeted force-push — the deletion thereby reaches the
ref tip on the forge as well (#102).

## 6. Known gaps — as of v0.1.3

The list an approval decision needs. None of this is hidden; all of it is
public as issues:

- **The forge retains erased objects by its own rules.** Since
  [#102](https://github.com/munichbughunter/minds/issues/102), `sync`
  transfers the deletion of an already-pushed session ref automatically (a
  targeted force-push of the tombstone on the next push). The plain text
  thereby leaves the forge's **ref tip**; the old objects, however, remain
  subject to the platform's object retention (unreachable objects until
  housekeeping, backups, mirrors), which is outside Minds' control. The
  shared context ref of a legacy repository also stays out of scope: it
  carries the other sessions too and is never force-pushed; its remote
  history has to be caught up by hand if needed. **Recommendation for the
  pilot:** `forget` before the first push is fully effective; after a push,
  the housekeeping question to the forge is part of the deletion process.
- **The raw-data journal is the one plain-text window.** Between capture and
  checkpoint, the unredacted raw data — including tool results such as the
  output of `cat .env` — sits under `.git/minds/journal/` (files 0600, local
  only). In normal operation the window closes with the next commit; if the
  checkpoint fails (e.g. a broken `redact.json`), it stays open until the
  checkpoint is retried. `forget` does not reach the journal — a session
  that was never checked in has no identifier. Related,
  [#49](https://github.com/munichbughunter/minds/issues/49): the directories
  above the event files are created with umask permissions — on multi-user
  machines, agent names and session identifiers (not the contents) are
  visible to other local users.
- **The backfill started by `minds enable` uses the built-in default policy,
  not the repository's own `.minds/redact.json`.** When backfilling old transcripts, the standard
  detectors and the credential-file wall apply, but no project-specific
  denylist (such as customer names). For the pilot: backfill only after
  consultation.
- **The credential wall is path-based.** It triggers on path fields of tool
  calls. `cat .env` in a shell command names the file only by name: the
  *output* never reaches the stored object (tool results are never stored
  there), but it does sit in the raw-data journal until checkpoint (see
  above). Secrets that appear in the command itself are caught by the
  redaction pipeline. Events that cannot be parsed, or that were truncated
  at the size limit, go into the journal unchanged — there too, the pipeline
  applies before anything is stored.
- **Collision edge case of the browse branch**
  ([#100](https://github.com/munichbughunter/minds/issues/100)). The
  browsable branch carries only the first 16 hex characters of the
  identifier; on a collision, `forget` would also erase the wrong browse
  branch. The direction of the failure is over-deletion, never a leak — the
  authoritative storage location is addressed by the full hash.
- **`hook.log`** does not pass through the full redaction pipeline. It is
  limited to diagnostic lines (0600, truncated, URL credentials stripped)
  and payload-free by construction; a dedicated test ensures transcript
  content cannot reach it.

## 7. Summary for the approval decision

What is captured are prompts, agent responses, and tool calls — redacted
before anything reaches the store, and stored exclusively locally in the
repository. There is no outbound channel except the user's own `git push`
and the explicitly invoked GitLab mirror (review verdicts only — their
free-text summary is the reviewer's own responsibility; it is not redacted).
GDPR deletion exists, is fully effective locally, and since #102 also
reaches the ref tip of already-pushed session refs; its known limit is the
forge's object retention. Confidential questions and
findings containing session content go to the named contact, not to the
public issue tracker.
