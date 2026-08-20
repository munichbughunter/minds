# Minds — Pilot Guide

*For the pilot at the partner. As of v0.1.3 — the handover state. What Minds
is and why it exists is covered in [`fuer-tester.md`](fuer-tester.md) (in
German); this document defines the scope: what the pilot tests, how to
install, and what explicitly is not part of it.*

*Deutsche Fassung: [pilot-leitfaden.md](pilot-leitfaden.md)*

---

## 1. The scope

| | |
|---|---|
| **Size** | 1–2 repositories, 3–5 developers, 3–4 weeks |
| **Agent** | Claude Code — only there is the tool level fully interpreted |
| **Platform** | macOS or Linux; Windows only via WSL (there is no native Windows binary) |
| **Version** | pinned to `v0.1.3` — not "latest", so everyone tests the same state |

**The pilot's guiding question:** *After three weeks, does `minds why`
answer a question that `git blame` cannot?* Everything else is a bonus.

Before starting, the [privacy overview](privacy-overview.md) belongs to the
internal approval process — it is deliberately one page and names the known
gaps outright.

## 2. Installation — pinned version

```sh
# 1. Install — the version is pinned
MINDS_VERSION=v0.1.3 sh -c \
  'curl -sSfL https://raw.githubusercontent.com/munichbughunter/minds/main/install.sh | sh'

# 2. Verify that minds is on the PATH — must print a path
command -v minds

# 3. Arm the repository — idempotent, leaves foreign config alone
cd your-repo
minds enable --agent claude-code

# Optional: every new Claude Code session gets the repo context prepended
minds enable --agent claude-code --recall
```

Step 2 matters more than it looks: the hooks resolve `minds` first through
the location remembered at `enable` time (`git config minds.binary`); the
PATH is the fallback. If both fail, they run into the void **silently** —
committing keeps working, nothing gets recorded. If `command -v minds`
prints nothing, add `export PATH="$HOME/.local/bin:$PATH"` to `~/.zshrc` or
`~/.bashrc` and open a new shell.

After that: just work normally. One agent session, one commit — that is all
it takes; the rest happens in the background.

## 3. The pilot's commands

**Backwards — "why is this here?":**

```sh
minds show                    # the session behind the last commit
minds why <file>:<line>       # the session behind a single line
minds blame <file>            # which session is behind which lines
```

**Overview and search:**

```sh
minds recap                   # the latest sessions at a glance
minds search <term>           # full-text search across prompts and sessions
minds render                  # static HTML view into ./site
```

**Operations and deletion:**

```sh
minds fsck                    # names every condition that disturbs capture
minds forget <session>        # GDPR deletion — limits: privacy overview, section 6
```

**If the pilot repository is on GitLab**, additionally the review layer:

```sh
minds review <change> --approve --summary "…"   # verdict as a Git object
minds reviews <change>                          # review state of a change
minds gitlab mirror <change> --mr <nr>          # mirror verdicts as MR notes (one-way, idempotent)
```

For `gitlab mirror`, the token belongs in an environment variable
(`MINDS_GITLAB_TOKEN`), never on the command line; URL and project come from
`--url`/`--project` or from `git config minds.gitlabUrl` /
`minds.gitlabProject`.

## 4. What is not part of the pilot

Deliberate decisions, not gaps in the test plan:

- **The reverse direction GitLab → repo** (`minds gitlab webhook`). The
  command ships in the binary (default: dry run) but has no token
  verification yet — with `--write`, an arbitrary payload could create a
  review object. Do not use it in the pilot; no service invokes it.
- **The CI review gate** (`fsck --require-review` as a pipeline gate). The
  flag exists, but as a pipeline gate it will only be recommended once exit
  codes and error chains are dependable.
- **`minds sync` across multiple machines** as a scenario of its own.
- **Other agents** (Gemini, Codex, Cursor, opencode): the prompt is
  captured, the tool and file level is not yet interpreted. The pilot runs
  on Claude Code.
- **Multi-agent scenarios.**

## 5. When nothing shows up

The most important principle: a recorder must never make a commit fail.
Outages are therefore silent — but not invisible:

1. `minds fsck` — tells you whether the hooks are in the right place,
   whether they stem from an older version, and whether the log has entries.
2. `.git/minds/hook.log` — everything the hooks had to report lands there
   (e.g. a broken `.minds/redact.json`, which stops capture fail-closed).
3. `command -v minds` — the classic, see section 2.

## 6. Known limitations of the handover state

The honest list — read it as "applies today"; it is part of the handover:

- **Linked Git worktrees:** capture works there, but `minds show` and
  `minds why` display the main tree's commit
  ([#20](https://github.com/munichbughunter/minds/issues/20)). Work in the
  main checkout until this is fixed.
- **No native Windows binary** — Windows means WSL.
- **Tool level only for Claude Code** (see section 4).
- **The review layer needs two people on one repository** — alone, you can
  test capture, `why`, and `recall`, but not reviews.
- **`forget` and already-pushed sessions:** the next push catches up the
  deletion via a targeted force-push
  ([#102](https://github.com/munichbughunter/minds/issues/102)); what the
  forge retains as unreachable objects is covered in the privacy overview.
- **A push with new sessions opens two connections**
  ([#85](https://github.com/munichbughunter/minds/issues/85)) — noticeable
  against a distant remote; without new sessions the hook costs nothing.
- **No self-update:** version changes run through `install.sh` with
  `MINDS_VERSION`.

## 7. Feedback channel

- **Reproducible findings without confidential content:** as an issue in the
  public tracker — as raw as possible; "feels odd" is a valid finding.
- **Anything containing session content, repository names, or customer
  references:** to the named contact from the invitation, never into a
  public issue.
- The three questions whose answers evaluate the pilot (from
  [`fuer-tester.md`](fuer-tester.md)): Did the installation work without
  asking anyone? When did the first unprompted `minds why` happen? What did
  you look for and not find?
