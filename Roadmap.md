# Minds — Roadmap & Strategy

*The outward-facing document: for partners, investors, contributors. It explains
the thesis, the market, the defensibility — and lays out the full technical
roadmap, including the big bet (reviews as Git objects).*

*Status: v0.3.0, August 2026. Progress per layer is marked inline; the
decisions behind it live in the [ADRs](docs/adr/), the release-by-release
record in the [CHANGELOG](CHANGELOG.md).*

---

## 1. The one-liner

**Git knows *what* changed. It doesn't know *why*.** As long as humans wrote the
code, that didn't matter — the reason lived in the author's head and in the MR
description. Now agents write the code, and the reason evaporates the moment
the terminal window closes. Minds writes the reason where it belongs: **into
Git itself, next to the code.**

## 2. The new break point

When an agent produces 2,000 lines in twenty minutes, "read the diff" is no
longer a procedure — it is a fiction. Line-by-line review does not scale with
agent fleets. The quality gate becomes a formality and filters nothing.

The answer is not to make the diff prettier. The answer is to **make the
intent a versioned artifact and check the diff against it.** The reviewer reads
what was asked for, then checks whether the code does that.

## 3. The thesis: more into the repo, less into the platform

Put the weaknesses of today's Git/GitLab model side by side and they almost all
resolve in **the same direction**. That is not a coincidence — it is an
observation you can build on.

| Today's break point | Where it belongs | Prior art |
|---|---|---|
| Commit identity is tied to the hash; rebase/squash destroy it | **Change id** in the repo | Gerrit, Jujutsu |
| Author is an unsigned free-text field | **Signed attribution** in the repo | sigstore, ssh-sig |
| The context of a change evaporates with the session | **Context as a Git object** | *Minds today* |
| Reviews/approvals/discussion live in Postgres | **Reviews as Git objects** | Radicle, git-bug |
| The MR is too coarse (per branch instead of per change) | **Review per change** | Gerrit |
| Secrets/PII in history stay forever (GDPR ⊥ Merkle) | **Redactable payload** | — |
| The line diff is the wrong unit | **Structural/AST diff** | Difftastic, Darcs/Pijul |
| YAML as a programming language for CI | **Policy as a binary** | — |

Git does not do too little — it is **used too little**, because the platforms have
no interest in a repo-native memory. Their business model *is* keeping the data
in their own database. That is exactly where the gap opens.

**Guiding rule, derived from the thesis:**
> For every new artifact, ask first "does this work as a **Git object**?" and
> only then "does this work as a platform feature?". A Git object travels with the
> repo, survives migration, works offline and air-gapped.

## 4. Why this is a defensible position

- **GitLab is where the regulated shops live** — banks, insurers, public
  sector, automotive. Self-managed, often on-prem, sometimes air-gapped.
  Exactly the customers who will soon have to prove AI involvement in their
  code.
- **These customers cannot adopt a SaaS solution.** The tooling *must* be
  self-hostable. That is not a feature — it is a structural constraint.
- **The Git-native approach fits that exactly.** If the data lives in the
  repo, self-hosting is not a porting effort but the default. The dashboard is
  a reader, not a service with its own state.
- **A platform vendor is reluctant to copy this.** Anyone coming from the
  cloud model and optimizing for their own data custody has no interest in a
  model that deliberately moves the data *out* of the platform and into the
  repo. That is the moat.

### Why not entire's path — the hosting trap

The next-bigger player in this field, **entire.io** ($60M seed), has tipped
from "Git companion" into **its own hosted, distributed Git network**
("agent-scale cloning without rate limits", "India's fastest Git hosting").
For us that is deliberately **not** a model to follow: a hosted service is
exactly what self-managed, on-prem and air-gapped regulated customers are not
allowed to adopt. entire's strength is off-limits to us — and ours (repo-native,
self-hostable, platform-fungible) is the one a well-funded SaaS-first vendor
is reluctant to build. We won't beat them at hosting and don't try; we
occupy the other shore.

Still, there is confirmation here: entire stores checkpoints **in refs**
and integrates agents via **hooks** — the same two foundational decisions as
Minds. The path is right; only the destination differs. And their ecosystem
shows the value of rich, open data: third-party tools like **Grain**
(`scan` → `AGENTS.md`, `audit` → provenance) build on the captured session
history. Exactly this kind of layer on top is what we want to enable —
repo-native instead of tied to a host.

## 5. Where Minds stands today (honestly)

A Rust workspace (one static binary, the only hard dependency is `git`) that
has long since closed the v0.1 chain — as of v0.3.0 it covers:

- **Capture**, hook-based: `minds enable` installs agent + Git hooks; the hot
  path (`minds hook`) writes every event to the local journal, the cold path
  (`minds checkpoint`) redacts → stores → appends a trailer. Agents without a
  dedicated adapter no longer lose their tool level: the generic fallback
  keeps the name and raw arguments as redacted evidence.
- **Redaction**, fail-closed: secrets/PII are removed *before* a byte reaches
  the store; a rejected session leaves an auditable block seal instead of a
  silent gap.
- **Store**, content-addressed (`SessionId = blake3(canonical_json)`), two
  backends behind one trait: in-repo (`refs/minds/context`) and child repo.
  One ref per session ([ADR-0010](docs/adr/0010-one-ref-per-session.md));
  `minds sync` ships all due refs in one connection.
- **Context return**: `recall`, `brief` and `distill` hand the captured
  context back to the next agent — deterministic, 0 tokens.
- **Reviews as Git objects** (layer 3, R1–R6 complete): verdicts, threads,
  stacks, the GitLab one-way mirror, `fsck --require-review` as a CI gate,
  and `minds audit --export` as a portable provenance bundle.
- **The Evidence Chain** ([ADR-0011](docs/adr/0011-evidence-chain.md)):
  journal events are folded into sealed hash chains — gaps included — and
  `minds verify` renders a verdict on the integrity × coverage matrix.
  Optionally ssh-signed; verifiable without Minds
  ([verification guide](docs/verification-guide.md)).
- **Surfaces**: `minds inspect` (TUI with session list, graph, why chain and
  evidence report), `minds render` (a stateless HTML page), `minds metrics`
  (Prometheus/OpenMetrics for the customer's Grafana).
- **Releases** for macOS (Apple Silicon and Intel), Linux (x86_64/ARM64,
  static musl) and, since v0.3.0, native Windows (x86_64).

**Known gaps:** tool-call *interpretation* is still Claude-Code-only (other
agents capture raw evidence via the generic fallback); the reader shows
sessions and history but no overview tiles or charts yet, although
`minds metrics` already supplies the numbers; the CLI output is
German today — English output is on the list.

## 6. The roadmap in layers

### Layer 1 — Make the foundation real ✅ *(capture)* / ◐ *(interpretation)*
Multi-agent capture beyond Claude: done for prompts, and since v0.3.0 no agent
loses its tool level — the generic fallback stores uninterpreted calls as
redacted evidence. Still open: *interpreting* the tool level for Codex,
Cursor, Gemini and opencode; which one comes first follows user demand. The
secret wall applies fail-closed to all agents.

### Layer 2 — Sharpen the thesis ✅
The three building blocks that lift Minds from "context tool" to "repo-native
trust layer" — and that laid the foundation for layer 3:

- **Change id** — stable change identity, survives force-push/rebase/squash.
- **Signed attribution** — "agent X, model Y wrote these lines", verifiable
  instead of claimed.
- **`minds forget`** — redactable payload: GDPR deletion of the content while
  the hash reference stays resolvable. The thing plain Git structurally
  cannot do.

### Layer 2b — CLI completeness & context return ✅ *(before any UI)*
The CLI had to be complete before any UI. The core is **context return**:
`minds recall`/`distill` hands the captured context back to the next agent as
an AGENTS.md-style brief, closing vision problem #3 ("no agent learns from the
last one") that v0.1 left open. Alongside it, parity with what entire/Grain
demonstrate: `blame`, `search`, `recap`, `agent-help`.

### Layer 2c — Metrics & observability ✅ *(opt-in)*
`minds metrics` projects the data already captured (tokens, steps, session
length, agent share, redaction hits, context coverage) into a standard format
(Prometheus/OpenMetrics) — for **the customer's Grafana**. No duplicated
state, no service we operate: we emit into infrastructure that regulated teams
run anyway. It is also the cheapest visible surface, long before a UI of our own
exists.

### Layer 3 — Reviews as Git objects ✅ *(the big bet — spelled out below)*
See section 7. Implemented R1–R6; since then extended with the Evidence Chain
([ADR-0011](docs/adr/0011-evidence-chain.md)), which turns the stored record
into a sealed, verifiable one.

### Layer 4 — Structural diff & AST attribution *(later)*
The line diff is the wrong unit. Difftastic-style structural diff in the
reader; attribution refined from the line to symbol/AST node. Dissolves a
large share of the "conflicts" that are in fact artifacts of the line-based
model.

### Layer 5 — Sync & mirror ◐ *(transport, not a place)*
The foundation is in place: one ref per session
([ADR-0010](docs/adr/0010-one-ref-per-session.md)), `minds sync` as the
transport primitive, and the child-repo backend for keeping context out of
the code remote. Still open: mirroring the `refs/minds/*` namespace between
*multiple* remotes so context travels across an agent fleet/team and across
air gaps — **without us hosting anything**. An optional, self-hostable
aggregation/reader surface across many repos is conceivable, but it would run
**on the customer's infrastructure**.

---

## 7. Layer 3 in detail — reviews as Git objects

**The goal:** the review of a change — verdict, comments, approval — lives
content-addressed and signed under `refs/minds/reviews/`, travels with the
repo and survives every platform migration. GitLab becomes a *cache* of the
truth, not its source. This is where the thesis becomes a product.

Builds directly on layer 2: signed identity (who reviews) and stable change
ids (what is reviewed) are the prerequisites.

> **Status 2026-07-29: R1–R6 implemented.** The breakdown below stands as a
> record of what each phase meant; the decisions live in
> [ADR-0009](docs/adr/0009-reviews-as-git-objects.md), the transport in
> [ADR-0010](docs/adr/0010-one-ref-per-session.md).

### Phase R1 — The review object ✅
- `docs: ADR — review as a content-addressed, signed Git object`
- `feat(core): review envelope (schema_version, subject: change id|session id, reviewer: signed identity, decision: approve|reject|needs-work, summary, at)`
- `feat(store): put_review/get_review under refs/minds/reviews/ (same layout as sessions, dedup by hash)`
- `feat(cli): minds review <change> --approve|--reject|--needs-work [--summary]`
- `feat(cli): minds reviews <change|commit> — list verdicts, check signatures`
- `test: review roundtrip + signature check + rebase survival (subject = change id)`

### Phase R2 — The review thread (git-bug pattern) ✅
Discussion must be mergeable — two reviewers offline, both comment, no
conflict.
- `feat(core): comment as an append-only operation (content-addressed), anchored to file:line OR turn`
- `feat(store): thread as an operation log; deterministic merge of two logs (commutative, conflict-free)`
- `feat(cli): minds comment <change> --on <file:line|turn> "<text>"`
- `test: two divergent threads merge conflict-free to the same state`

### Phase R3 — Review per change, not per branch (the Gerrit lesson) ✅
- `feat: verdicts hang on the change id → stacked changes reviewable individually, continuity across force-push`
- `feat(cli): minds stack — show dependent changes and their review state`
- `test: force-pushing a stack preserves verdicts per change`

### Phase R4 — The platform becomes a cache ✅
One-way bridge: mirror Git-native verdicts into GitLab MR notes/approvals, for
teams that live in the GitLab UI. The source of truth stays Git. Migrate away
and the review history comes along.
- `feat(minds-gitlab): verdict → MR note/approval (one-way, idempotent)`
- `feat(minds-gitlab): webhook receiver (stateless) — MR comment → review object (optional, opt-in)`
- `docs: operating model — Git is the source, GitLab is the projection`

### Phase R5 — Policy as a binary, not as YAML ✅
- `feat(cli): minds fsck --require-review — no agent-authored change without a signed verdict`
- `feat(ci): reusable .gitlab-ci.yml include that only calls the binary (no YAML logic)`
- `test: gate red on missing/invalid verdict, green otherwise`

### Phase R6 — Audit export for regulated environments ✅
- `feat(cli): minds audit --export — signed provenance chain (change → session → attribution → verdict) as a portable bundle`
- `docs: verification guide (what the bundle proves, what it doesn't)`

**The result of layer 3:** a repo carries its own, cryptographically provable
answer to "who wrote this, on whose instruction, who checked it, and why was
it merged?" — without a platform, without a database, verifiable offline.

**Postscript from the implementation (2026-07-29).** Testing revealed that
`git push` with Minds enabled took ~1.9 s longer — on *every* push, even
without new context. The cause was not the hook but the data structure: the
whole store hung off one ref, which made that ref the serialization point for
writing *and* pushing. Resolved in
[ADR-0010](docs/adr/0010-one-ref-per-session.md): one ref per session, edges
stored with their session, one push for all refs. Afterwards a repo that only commits
writes to **no shared ref** at all; without new context the hook costs
0.02 s instead of 1.86 s. The same lesson `entireio/cli` drew when moving from
`entire/checkpoints/v1` to `refs/entire/`.

Fixed along the way: `refs/minds/reviews` was never pushed before — layer 3
was not team-ready until then.

---

## 8. The risks, honestly

- **The agent adapters are a treadmill.** Every agent has its own format,
  and they change constantly. Antidote: an adapter trait with golden fixtures
  per agent, a versioned schema, a tolerant reader — in place since v0.3.0;
  the treadmill itself remains.
- **GitLab could build this itself.** The counter: Git-native and
  self-hostable is structurally hard for a vendor that optimizes for its own
  data custody.
- **Adoption takes a team, not an individual.** The value emerges in
  review. A solo dev doesn't need layer 3. Antidote: layers 1+2 already
  deliver individual value (a safer, signed, searchable record).
- **The thesis could be early.** Today the pain is felt by teams that work
  aggressively with agent fleets. There are not many of them yet — but they
  are multiplying fast,
  and the regulation wave is coming regardless.

## 9. The pitch in three sentences

> For twenty years software development had one shape: diff, review, merge.
> That shape is breaking right now, because agents produce more code than a
> human can read — and because the reason for every change disappears the
> moment the session ends.
>
> Minds writes the context, the identity and ultimately the review itself
> where they belong: into Git, next to the code — as signed, redactable
> objects that travel with the repo. Not into someone else's cloud, not into
> a platform database.
>
> And GitLab is the right entry point, because that is where you find the
> people who must prove AI involvement in their code — and who cannot adopt
> anything that runs in someone else's cloud.
