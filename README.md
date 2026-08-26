# minds

**Git knows *what* changed. It doesn't know *why*.**

As long as humans wrote the code, that was tolerable — the reason lived in
the author's head, and you could ask. Now agents write the code, and the reason
evaporates the moment the terminal window closes.

Minds writes the reason where it belongs: **into Git itself, next to the code.**
What prompted a change, who wrote it, and who reviewed it live
content-addressed and signed under `refs/minds/` — and travel with the repo.
No database, no cloud, no service. One static binary, one hard dependency: `git`.

## Install

```sh
curl -sSfL https://raw.githubusercontent.com/munichbughunter/minds/main/install.sh | sh
```

Installs `minds` to `~/.local/bin` — set `MINDS_INSTALL_DIR` for a different
target, `MINDS_VERSION` to pin a version. Prebuilt archives for macOS (Apple
Silicon and Intel), Linux (x86_64 and ARM64, static musl builds) and Windows
(x86_64) are attached to every
[release](https://github.com/munichbughunter/minds/releases); for air-gapped
installs, unpack them by hand. On Windows there is no install script: unpack
`minds-<version>-x86_64-pc-windows-msvc.zip` and put `minds.exe` on your PATH —
or use **WSL**, where the Linux install applies unchanged. Alternatively,
[build from source](#build-from-source).

## Five minutes to the first *why*

```sh
cd your-repo
minds enable --agent claude-code   # registers the hooks — once, idempotent
```

Then work with your agent and commit as you always do. Capture happens in the
hooks — there is nothing to remember day to day. When you want to know
where a change came from:

```sh
minds show                  # the session behind the last commit
minds why src/retry.rs:42   # the session behind a single line
minds recap                 # the latest sessions at a glance
minds inspect               # the same as a TUI: sessions, graph, why chain, evidence (e)
```

That is the daily loop. Two more become useful as soon as an agent starts the
*next* session:

```sh
minds recall src/retry.rs   # condensed context brief for a file — 0 tokens, no LLM
minds brief                 # size-capped context block to start an agent session
```

Everything beyond that — reviews, signatures, verification, GDPR deletion,
GitLab mirroring — is in the [command reference](docs/commands.md).
`minds --help` shows the same in the terminal; `minds agent-help` prints it as
JSON for agents, not humans.

One detail worth knowing: `minds enable` records where the binary lives in the
repo's Git config (`minds.binary`), and the hooks call it through that — so
they work even where no shell profile sets the PATH: commits from VS Code, Fork
or Tower, minimal CI shells. Keep `minds` on your PATH anyway for your own
calls, and re-run `minds enable` after moving the binary.

## What makes it different

- **Reviews are Git objects.** Verdict, comments and approval live under
  `refs/minds/reviews/`, signed and bound to a **change id** — they survive
  rebase, squash and force-push. GitLab becomes a projection, not the source of
  truth. Switch platforms and the review history comes along.
- **Redaction runs fail-closed** — *before* a byte reaches the store. A secret
  that is never stored never needs deleting.
- **`minds forget` actually deletes.** The payload is replaced by a tombstone
  while the hash reference stays resolvable — GDPR deletion without breaking
  the chain. Plain Git structurally cannot do that.
- **Evidence, not claims.** Sessions are sealed into a hash chain
  (`minds verify`), gaps are recorded as gaps, and `minds audit --export`
  produces a bundle that is verifiable without this tool — see the
  [verification guide](docs/verification-guide.md).
- **No service, no telemetry.** Nothing leaves your machine that you don't push
  yourself. Fully functional offline and air-gapped.

## Agent support

| Agent | Status |
|---|---|
| Claude Code | complete — prompt, tool calls, files, model, tokens |
| Codex, Cursor, Gemini, opencode | hooks register, prompt is captured; tool calls are stored as raw evidence, not yet interpreted |

Intent: one agent done right rather than four done halfway. Which agent gets
full support next follows what users actually run — [tell us what you
use](https://github.com/munichbughunter/minds/issues).

## Platform focus: GitLab

Capture, lookup and reviews need only Git — they work on any forge, or none.
The **platform bridge** — mirroring verdicts as MR notes (`minds gitlab
mirror`), webhook, CI gate — targets **GitLab**, self-managed and gitlab.com.
Bridges to other platforms are not currently planned.

## Build from source

```sh
cargo build --release --bin minds     # Rust 1.85+
cargo test --workspace
```

## Further reading

- [**What Minds is and why it exists**](docs/for-testers.md) — the long-form
  introduction for anyone picking it up for the first time
- [Command reference](docs/commands.md) — every command, with an example
- [Roadmap & strategy](Roadmap.md) — the thesis, the market, the technical roadmap
- [GitLab operating model](docs/gitlab-operating-model.md) — Git is the source,
  GitLab is the projection
- [Verification guide](docs/verification-guide.md) — what an audit bundle
  proves, and what it doesn't
- [Architecture decision records](docs/adr/) — why hooks instead of transcript
  parsing, why one ref per session, why reviews as Git objects
- [CHANGELOG](CHANGELOG.md)

## License

Apache-2.0
