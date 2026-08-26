# Minds — what it is, and why it exists

*For the first test round. Read this once before you install — it takes ten
minutes and saves you from wondering what the whole thing is actually for.*

---

## 1. The one sentence that matters

**Git knows *what* changed. It does not know *why*.**

A `git log` gives you the diff and a commit message someone wrote in a hurry.
What it does not give you: the instruction the change was written against.
The three approaches that were discarded first. The constraint that makes the
solution look so odd.

For twenty years that was bearable. The reason lived in the author's head, and
the author could be asked.

## 2. What is changing right now

Now agents write the code. The reason lives in a terminal session — and
evaporates the moment the window closes. There is nobody left to ask in six
months; the session no longer exists.

At the same time, the scale shifts. When an agent produces two thousand lines
in twenty minutes, "read the diff" is no longer a procedure — it is a fiction.
The quality gate then exists only formally and filters nothing.

Two gaps follow, and you feel them separately:

**Backwards.** "Why is this line here?" no longer has an answer. `git blame`
gives you a commit and a timestamp — not the intent.

**Forwards.** No agent learns from the last one. Every session starts from
zero, runs into the same dead ends, makes the same mistake you already
corrected last week. You are the only memory the system has.

## 3. What Minds does

Three steps, in the background, without you having to do anything:

**Capture.** Hooks in the agent record what happens — the prompt, the tool
calls, the files touched, the model. Not as a screen recording, but
structured.

**Redact.** Before anything is stored, secrets and personal data are stripped.
Fail-closed: when in doubt, Minds blocks rather than risk it.

**Store.** The result lands **in Git itself** — as a content-addressed object
next to the code, under `refs/minds/`. The commit gets a trailer pointing to
it.

No daemon, no database, no cloud. One static binary, one hard dependency:
`git`.

## 4. What you actually get out of it

### "Why is this line here?"

```
minds why src/retry.rs:42
```

Line → commit → session. You get the prompt that led to this line, the intent
behind it, and what else happened in the same pass. For an overview of a whole
file: `minds blame <file>`. For a commit: `minds show`.

### "What does this repo already know?"

That is the forward direction — the captured context goes back to the *next*
agent:

```
minds recall src/retry.rs      # condensed brief on what has already happened here
minds brief                    # size-capped starter block for a new session
minds distill --out AGENTS.md  # what this repo's history has taught agents
```

All of it deterministic from the captured data — no LLM calls, no tokens, the
same input yields byte-identical output. `minds enable --recall` optionally
wires this up so every new session gets the brief prepended automatically.

### "Who wrote this — human or agent, which model?"

`author` in Git is an unsigned free-text field. In a world where agents
commit, that is exactly the foundation on which you can prove **nothing**.
Minds signs the attribution (`ssh-sig`, no network needed): "agent X, model Y
wrote these lines" becomes verifiable instead of asserted — `minds sign`,
`minds verify`.

### "Was this reviewed, and by whom?"

With Minds, the review — verdict, comments, approval — also lives in the repo,
not in a platform database:

```
minds review <change> --approve --sign
minds comment <change> --on src/retry.rs:42 "The retry is unbounded."
minds reviews <change>
minds stack                        # dependent changes and their review state
```

The verdict hangs on a **change id**, not on the commit hash. It therefore
survives rebase, squash, and force-push — exactly where reviews are otherwise
lost. Two reviewers can comment offline; the threads merge without conflict.

If you live in the GitLab UI, mirror the verdicts there
(`minds gitlab mirror`) — one-way, idempotent. The source of truth stays the
repo; GitLab becomes a display.

And as a gate in CI: `minds fsck --require-review` turns red when an
agent-written change has no valid verdict. No YAML logic, just one binary
invocation.

## 5. The one foundational decision

Everything moves **into the repo**, nothing into a platform database or
someone else's cloud. Every new artifact first answers the question "can this
be a Git object?".

In practice that means:

- You clone the repo — the entire memory comes with it.
- It works offline and in an air gap.
- If you switch platforms, the review history comes along. It never lived
  anywhere else.
- Self-hosting is not a porting effort — it is the normal case.
- There is no service to operate and nothing to pay per seat.

The pattern is not new. Gerrit put change identity into the repo; Radicle and
git-bug did it with issues and reviews. Minds applies it to what is being
lost right now: the reason behind agent-written code.

## 6. Security, and what about GDPR

**Redaction runs before storing, not after.** A secret that never enters the
store never needs to be deleted. The rules are extensible
(`.minds/redact.json`).

**`minds forget <session>` really deletes.** The payload is replaced by a
tombstone; the hash reference stays resolvable. `why`, `show`, and `fsck`
stay green and honestly say "content removed on request". Plain Git
structurally cannot do this — there, whatever once entered the history is in
it forever.

**Nothing leaves your machine** that you do not push yourself. There is no
telemetry channel and no server Minds talks to.

## 7. What works today — and what does not

The honest part. I would rather you read it here than discover it yourself:

| | State today |
|---|---|
| **Claude Code** | complete — prompt, tool calls, files, model, tokens |
| **Gemini, Codex, Cursor, opencode** | the prompt is captured; the tool and file level is **not** yet interpreted |

That is deliberate: better **one** agent done right than four done halfway.
Which agent comes next is your call — tell me what you use.

Two more limitations you should know about:

- **Reviews need two people on one repository.** Alone, you test capture,
  `why`, and `recall` — the review layer stays cold.
- **The reader (`minds render`) is deliberately spartan.** A static HTML
  page: click a line, see the prompt behind it. Overview tiles and charts
  come later — rich data first, then polish.

## 8. In five minutes, you're in

```sh
# 1. Install  (the concrete line comes with the invitation)
curl -sSf <release-url>/minds-installer.sh | sh

# 2. Verify that minds is on the PATH — must print a path
command -v minds

# 3. Arm the repository — registers the hooks, idempotent, leaves foreign config alone
cd your-repo
minds enable --agent claude-code

# 4. Work completely normally. One agent session, one commit.

# 5. See what Minds kept
minds show                     # the session behind the last commit
minds why <file>:<line>        # the session behind a line
minds recap                    # the latest sessions at a glance
```

Step 2 matters more than it looks. The hooks that `minds enable` registers
invoke `minds` **without a path**. If the binary is not on the PATH, the calls
go nowhere — **silently**, because a recorder must never make a commit fail.
You notice nothing: committing keeps working, nothing gets recorded, and
`minds show` stays empty. If `command -v minds` prints
nothing, add `export PATH="$HOME/.local/bin:$PATH"` to `~/.zshrc` or
`~/.bashrc` and open a new shell.

**If nothing shows up anyway:** everything the hooks had to report is in
`.git/minds/hook.log` — a typo in `.minds/redact.json`, for instance, stops
capture *fail-closed*, and without that file this would stay invisible.
`minds fsck` tells you whether there is anything in there, and also whether
the hooks are in the right place and whether they date from an older version
(in which case a fresh `minds enable` helps).

`minds enable` is idempotent and leaves other tools' configuration alone. If you
want it gone again, say so — it is a few entries in `.claude/settings.json`
and `.git/config`, nothing more.

If you want to know what else the tool can do: `minds --help` lists
everything, and `minds agent-help` prints the same map in machine-readable
form — for the agent itself.

## 9. What I need from you

Three questions. The rest is a bonus:

1. **Did the installation work without asking anyone?** If you had to ask me,
   that is a bug in the instructions, not in you.
2. **When did you first use `minds why` without anyone reminding you?** That
   is the real test. If the answer is "never": that too is a usable finding,
   and the most important one.
3. **What did you look for and not find?**

Say it raw. "Feels odd" is a finding; I will follow up. What does not help me
is "works fine".

## 10. What comes next

- **The second agent** — order determined by your needs.
- **Overview in the reader** — tiles over sessions, tokens, context coverage.
  The metrics already exist as `minds metrics` for Grafana; they only need a
  surface.
- **Structural diff** — the line diff is the wrong unit for agent-written
  code. A diff over structure instead of lines resolves a large share of the
  "conflicts" that are really artifacts of the line-based model. That is
  the bigger piece and comes later.

---

*Questions, complaints, ideas: straight to me. Half a sentence is enough.*
