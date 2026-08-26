# Operating model — Git is the source, GitLab is the projection

*Layer 3, R4. Belongs to ADR-0009.*

## The one sentence that matters

The verdict on a change lives content-addressed and signable under
`refs/minds/reviews`. It travels with the repo, is verifiable offline, and
survives every platform migration. **GitLab displays it — it does not hold
it.**

That is not an implementation nicety; it is the project's thesis. Move away
from GitLab today and you lose reviews, approvals, and discussion, because
they live in Postgres, not in the repo. With Minds, this half of the story
comes along.

## The two directions are not equal

```
   Repository  ──── minds gitlab mirror ───▶  GitLab MR      (default, one-way)
   Repository  ◀─── minds gitlab webhook ───  GitLab MR      (opt-in, manual)
```

**Outbound** is the normal case and cannot break anything: what is in the repo
becomes visible as an MR note. The note carries an invisible marker
(`<!-- minds:review:<hash> -->`), and the note is read before it is written.
If the marker is already there, nothing happens. Because the hash
content-addresses the verdict, "same marker" also means "same content" — so
the job can safely run on every push.

**Inbound** exists, but only deliberately. `minds gitlab webhook` reads a
payload from stdin and interprets a comment of the form

```
/minds approve      backoff is correct now
/minds reject       not like this
/minds needs-work   please update the test
```

as a verdict. Without `--write`, it only shows what would be created.
Everything else — every ordinary comment, every other event — creates nothing
and reports nothing.

Why not automate both directions? Because then there would be two sources,
and someone would have to decide which one wins. That is exactly the state this
project avoids.

## There is no service

`minds gitlab webhook` is a command, not a receiver. If you need an HTTP
endpoint, put any one you like in front of it (a CI job, a `socat`, a
function endpoint on the customer's side); if you want none, feed it stored
payloads. We operate nothing, and there is nothing to operate.

The command runs in a checkout. That is deliberate: GitLab knows commit
hashes; a verdict hangs on a **change id**. The bridge between the two is the
trailer in the commit message, and that lives in the repo.

## What a verdict hangs on

On the change id — never on the commit. A force-push rewrites every hash; a
verdict on the hash would be orphaned afterwards. `minds stack` shows the
stack with the state of each entry, and the test
`a_force_push_of_the_stack_keeps_every_verdict` pins this down.

If the comment contains a change id (`I` + 40 hex), it wins. Otherwise the
MR's latest commit is resolved locally to its change id. If none is found,
**no** review is created — better nothing than a verdict that hangs on
nothing.

## Setup

```sh
git config minds.gitlabUrl     https://gitlab.example
git config minds.gitlabProject group%2Fproject      # id or URL-encoded path
```

The token comes **only** from an environment variable (`MINDS_GITLAB_TOKEN`,
or whatever `--token-env` names). Never from an argument: that would show up
in `ps` and in the shell history. Even to `curl` it goes via stdin, not via
the argument list.

Required permissions: `api` for the note, plus approval permissions for
`--approve`.

## In CI

```yaml
minds:mirror:
  rules:
    - if: '$CI_MERGE_REQUEST_IID'
  script:
    - git fetch origin '+refs/minds/*:refs/minds/*' || true
    - minds gitlab mirror "$(minds stack --base "$CI_MERGE_REQUEST_TARGET_BRANCH_NAME" | …)" \
        --mr "$CI_MERGE_REQUEST_IID"
```

The mirroring is idempotent, so a repeated run has no effect. The policy gate
(`minds fsck --require-review`, see `ci/minds-review-gate.gitlab-ci.yml`)
stays untouched by this: it checks the **repo**, not GitLab. A verdict that
exists only in the UI does not open the gate.

## What happens when you move away

Nothing is lost. The notes stay behind in the old instance — they were never
the source. Verdicts, threads, and signatures live in `refs/minds/reviews`
and are cloned with the repo. `minds audit --export` additionally bundles
them into a portable file (see the
[verification guide](verification-guide.md)).
