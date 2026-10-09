# Minds — Privacy Overview

*For the pilot partner's internal approval process. As of v0.1.3; the parts
on the witness, observations, intents and CI anchors (ADR-0012) describe the
unreleased state after v0.4.0. Every claim in this document can be traced to
the code; the known gaps are listed at the end — with issue numbers, not in a
footnote.*

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
- **Tool results are not stored**: whatever a tool returned — say, the content of a
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
is no database, no service, no cloud component. The one exception is the
optional **witness** (ADR-0012, `minds enable --witness <profile>`). It keeps its working
state in its own directory on the host, outside the agent's reach (last
rows of the table).

| Location | Content | Protection |
|---|---|---|
| `.git/minds/journal/…` | **raw data before redaction**, including tool results in plain text | files 0600; deleted after a successful checkpoint (gap: section 6) |
| `.git/minds/hook.log` | diagnostic lines from the hooks and from the backfill started by `minds enable` | 0600, rotated at 1 MiB; control characters escaped, lines capped; URL credentials are stripped |
| `refs/minds/store/<hash>` | the **redacted** session (`session.json`) plus its edges to the commit | reaches the store only after redaction (enforced by the type system) |
| `refs/minds/sessions/<hex>` | browsable copy (including rendered `session.md`) | appears on push as a regular branch `minds/session/<hex>` |
| `refs/minds/context` | index over the sessions | same as store |
| `refs/minds/reviews` | review verdicts: decision, reviewer e-mail, free-text summary | **not redacted** — whatever the reviewer writes in `--summary` sits verbatim in the ref and in the mirrored MR note; responsibility lies with the reviewer |
| commit trailers | `Minds-Session-Id` / `Minds-Change-Id` | hashes only, never content |
| `refs/minds/observations/<hash>` | witness only: the file observer's record — repo-relative paths and blake3 hashes, never file content (section 5) | paths pass the same redaction pipeline, fail-closed; bound by a witness-signed seal |
| `refs/minds/intents/<hash>` | the intent anchor plus the **redacted** requirement snapshot, and the approver's signature | snapshot redacted fail-closed before it is hashed (section 5) |
| `refs/minds/anchors/…` | CI countersignatures and replay records: seal id, project path, pipeline id, CI timestamp; replay records add the decisive commands (argv from the already-redacted session) with exit codes, expected and observed test counts and benchmark values (only under names the session already carries), plus the CI environment (CI name, image, policy blob id) — never text from the replay's output | written by CI, signed under `minds-anchor`; `minds anchor --mirror` also posts the countersignature as a merge request note (section 4) |
| witness home on the host (`--home`, else `MINDS_WITNESS_HOME`, else `$XDG_STATE_HOME/minds-witness/<repo-id>/`, else `~/.local/state/minds-witness/<repo-id>/`) | the witness journal: **raw hook events before redaction** (prompts, tool calls, tool results, and the event fields `cwd`/`transcript_path`, which are absolute host paths that usually contain the user name), unredacted `fs.observed` paths and `witness.start` facts; salts and epoch state, the witness key, the ledger of produced seals, `witness.json` (including the pinned redaction policy and therefore its `deny_pii`/`deny_secrets` terms), `log/witness.log` | directories 0700 with an owner check (0710 only for an explicit socket group), files and key 0600; the agent's domain sees only the socket — **if** the witness runs under its own account (`container`, `user` profile). A witness started as the developer's own user (`managed`, or by hand) shares the agent's UID, and the agent can read this directory. Only the credential-file wall applies before a raw event reaches the journal. The journal is deleted after a successful checkpoint, as on the agent side. If that deletion fails, it is reported and the raw data stays. A session the witness **defers** (`session(s) deferred` in its log) stays in the journal until a later checkpoint succeeds, so there is no fixed upper bound |

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
  `hook.log` applies a stricter rule there). Extensible per repository via
  `.minds/redact.json` (denylist/allowlist).
- **A broken configuration stops the write.** A typo in `redact.json` aborts
  with a line number — never a silent fallback to weaker protection. The
  error message quotes no values.
- The stored object records only **counts** about redaction (how many
  findings), never the found values themselves.

## 4. What leaves the machine? Nothing on its own.

The binary contains **no HTTP stack, no telemetry, no update check**.
Exactly five network paths exist, all triggered by the user or the
user's CI. The GitLab paths run through `curl`. Only `minds anchor
--mirror` and the countersignature read-back of `minds verify --online`
take it from the first absolute `PATH` entry outside the checkout. The
other GitLab paths (2, 3 and the issue re-read in 5) use a plain `PATH`
lookup (section 6):

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
3. **`minds intent bind --issue <group/project#iid>`**: only on explicit
   invocation. It **reads** the issue's text from GitLab to bind it as an
   intent (redacted before it is stored, section 5).
4. **`minds anchor --mirror`** (in CI): looks up the merge requests of the
   commit and posts the first-sight countersignature on them as a note.
   That is the seal id, project, pipeline id, CI time and signature, with
   no session content.
5. **`minds verify --online`**: **reads** those notes back to detect a
   deleted countersignature. For an intent bound to an issue, it also
   re-reads the issue to check its version.

The token for 3–5 comes from an environment variable, as for 2.

Not Minds' own traffic, but worth knowing: **`minds replay`** (in CI)
re-runs a session's decisive test and benchmark commands. Minds does not
restrict their network access. Whatever those commands reach, they reach
from the CI job.

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
default configuration Git keeps no reflog for `refs/minds/*`. For a session
ref already pushed to the forge, the next `git push` (or `minds sync`)
propagates the deletion via a targeted force-push — it thereby reaches the
ref tip on the forge as well (#102).

**Witness file observations are not subject to `forget`.** When a witness
runs (ADR-0012), it records observation objects under
`refs/minds/observations/<hash>`. They contain only repo-relative paths
(passed through the same redaction pipeline, fail-closed) and blake3
hashes — never file content. Secret files (`.env`, keys …) appear without a
hash, ignored paths (`.gitignore`, `.git/info/exclude`) and `.git/` do not
appear at all, and no symlink target is ever read — only the link text, as Git stores it.
A file whose content the redaction pipeline would change (a token, a
password assignment) and any non-text file get no hash either. No content
can be erased from such an object because none is in it. Two honest
caveats: an untracked file that is *not* ignored and is written during a
session appears with its path and hash even if it is never committed —
for a very short file (a PIN, a single word) such a hash can be guessed
offline; and
only the repository's `.gitignore` and `.git/info/exclude` count — your
global excludes (`core.excludesFile`, `~/.config/git/ignore`) are **not**
consulted by the witness. Keep private scratch files in `.gitignore` or
`.git/info/exclude`.

*Why observations are not erasable:* `forget` exists for the content of a
session — prompts, responses and tool calls a person wrote or caused. An
observation object carries no such content. It belongs to the witness's
signed chain, not to a session: a `witness-fs/v1` seal names it by hash and
the next epoch's seal names that seal. Deleting it would leave a
witness-signed seal pointing at nothing. `minds verify` would then mark the
observations as incomplete (scope not assessed), and the deleted
observations would no longer corroborate or explain any line. If it was
the first epoch of the chain, there would be no observation window at all,
and the session could not reach A2. Verify cannot tell a privacy deletion
from an agent removing an inconvenient observation.

*No attribution to persons.* An observation has exactly these fields:
sequence number, the witness's timestamp, repo-relative path, content hash or
the reason for leaving it out (`secret_file`, `redacted_content`,
`too_large`, `deleted`, `outside_repo`, `unscannable`). The object as a
whole adds its schema and three witness timestamps (epoch start, first
and last observation). There is no user
name, uid, process, or keystroke. The observer watches the agent's worktree,
not a person: it cannot tell a write by the agent from a write by a human in
the same worktree, and it does not try. Two things around it do carry
identity, and an approval decision should know them. First, like every ref
under `refs/minds/`, each observation object is stored in a Git commit that
records the Git identity configured for the repository and the commit
time. Second, the timestamps of observed writes, read together with the
session and commit they belong to, show *when* work happened in that
worktree. Read [`betrvg-note.md`](betrvg-note.md) (German) before using
this in a works-council setting.

**Intent anchors are not subject to `forget` either.** A session can be
bound to a requirement (ADR-0012, decision 5). The anchor and a
**redacted** snapshot of the requirement text are stored under
`refs/minds/intents/<hash>`: the snapshot passes the same redaction
pipeline fail-closed, and its hash is computed over the redacted bytes —
never over removed secrets. An anchor whose source path, issue reference
or scope globs contain something the policy would redact is refused, not
rewritten. A requirement **file** whose text needs redaction gets no anchor
at all: its Git blob id is a hash over the raw bytes and, next to the
redacted snapshot, would let anyone test guesses for the removed value —
clean the file first. Credential files (`.env`, `.pgpass`, `.netrc`, …, the
same secret-file wall as capture) and files ignored by `.gitignore` are
refused before they are read — a plain password in them is not always
something a detector recognizes. Unlike observations, a snapshot **does** carry content (the
requirement as written), and `minds forget <session>` does not remove it:
the anchor belongs to the requirement, not to one session. Do not bind
requirements whose text must later be erasable.

**Replay records and countersignatures are not subject to `forget`
either.** They live under `refs/minds/anchors/`. A replay record carries
the session id, the commit and the argv of the session's decisive commands.
The argv is taken from the already-redacted session, so it holds no text
from the replay's output. Even after `minds forget <session>`, those
command lines stay in the replay record. A countersignature carries only
the seal id, project path, pipeline id and CI time.

## 6. Known gaps — as of v0.1.3

The list an approval decision needs. None of this is hidden; all of it is
public as issues:

- **`curl` is not resolved the same way on every GitLab path.** `minds
  gitlab mirror`, `minds intent bind --issue` and the issue re-read of
  `minds verify --online` run the first `curl` in `PATH`, which can be a
  relative entry or a program inside the checkout. Run them only with a
  `PATH` you control. Only `minds anchor --mirror` and the countersignature
  read-back resolve `curl` outside the checkout.

- **The forge retains erased objects by its own rules.** Since
  [#102](https://github.com/munichbughunter/minds/issues/102), `sync`
  transfers the deletion of an already-pushed session ref automatically (a
  targeted force-push of the tombstone on the next push). The plain text
  thereby leaves the forge's **ref tip**; the old objects, however, remain
  subject to the platform's object retention (unreachable objects until
  housekeeping, backups, mirrors), which is outside Minds' control. The
  shared context ref of a legacy repository also stays out of scope: it
  carries the other sessions too and is never force-pushed; its remote
  history has to be cleaned up by hand if needed. **Recommendation for the
  pilot:** `forget` before the first push is fully effective; after a push,
  the housekeeping question to the forge is part of the deletion process.
- **With a witness, a second plain-text window opens on the host.** The
  local `.git/minds/journal/` still catches what the witness cannot take
  (witness unreachable, oversized or unparsable events). The
  witness journal holds raw hook events (prompts, tool calls, tool results)
  until its checkpoint. It lies outside the agent's reach (0700/0600, owner
  check) as long as the witness runs under its own account, but it is readable by whoever controls the witness account on
  that host, and `forget` does not reach it. Its retention follows the
  same rule as the agent-side journal described next.
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
