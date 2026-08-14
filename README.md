# minds

**Git weiß, *was* sich geändert hat. Es weiß nicht, *warum*.**

Solange Menschen den Code schrieben, war das verschmerzbar — der Grund saß im Kopf des
Autors, und den konnte man fragen. Jetzt schreiben Agents den Code, und der Grund
verdampft, sobald das Terminal-Fenster zugeht.

Minds schreibt den Grund dorthin, wo er hingehört: **in Git selbst, neben den Code.**
Was eine Änderung veranlasst hat, wer sie geschrieben hat und wer sie geprüft hat,
liegt content-adressiert und signiert unter `refs/minds/` — und wandert mit dem Repo.
Keine Datenbank, keine Cloud, kein Dienst. Ein statisches Binary, eine harte
Abhängigkeit: `git`.

## Installation

```sh
curl -sSfL https://raw.githubusercontent.com/munichbughunter/minds/main/install.sh | sh
```

Legt `minds` nach `~/.local/bin`. Für ein anderes Ziel `MINDS_INSTALL_DIR` setzen, für
eine bestimmte Version `MINDS_VERSION`. Im Air-Gap hängen dieselben Archive an jedem
[Release](https://github.com/munichbughunter/minds/releases) und lassen sich von Hand
auspacken.

Gebaut werden macOS (Apple Silicon und Intel) und Linux (x86_64 und ARM64, jeweils
musl und statisch). Für Windows gibt es kein natives Binary — der unterstützte Weg
ist **WSL**, dort gilt die Linux-Installation unverändert. Alternativ lässt sich
minds aus dem [Quelltext bauen](#aus-dem-quelltext-bauen).

### Einmal `minds enable` — den Ort merkt sich Minds

Wo das Binary liegt, ist nach dem `enable` nicht mehr dein Problem: Es hält den Ort
in der lokalen Git-Config fest (`minds.binary`), und die Git-Hooks rufen `minds`
darüber auf. Sie funktionieren damit auch dort, wo kein Shell-Profil den PATH setzt
— beim Commit aus VS Code, Fork oder Tower, in minimalen CI-Shells. Erst wenn am
gemerkten Ort kein Binary mehr liegt (etwa nach einem Umzug), suchen die Hooks im
PATH; ein erneutes `minds enable` erneuert den Eintrag, und `minds fsck` weist
darauf hin.

Im PATH liegen sollte `minds` trotzdem — für deine eigenen Aufrufe im Terminal:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Das Installationsskript prüft das selbst und warnt, wenn sein Zielverzeichnis nicht im
PATH liegt.

## In fünf Minuten

```sh
cd dein-repo
minds enable --agent claude-code   # registriert die Hooks, idempotent

# … ganz normal mit dem Agenten arbeiten, committen …

minds show                         # die Session hinter dem letzten Commit
minds why src/retry.rs:42          # die Session hinter einer einzelnen Zeile
minds recap                        # die letzten Sessions auf einen Blick
```

`minds enable` fasst fremde Konfiguration nicht an und lässt sich beliebig oft
aufrufen.

## Die Kommandos

| | |
|---|---|
| **Nachschlagen** | `show`, `why`, `blame`, `recap`, `search`, `render` |
| **Kontext zurückgeben** | `recall`, `brief`, `distill` — deterministisch, ohne LLM, 0 Tokens |
| **Identität & Nachweis** | `sign`, `verify`, `audit --export` |
| **Review** | `review`, `reviews`, `comment`, `stack`, `gitlab mirror` |
| **Betrieb** | `enable`, `checkpoint`, `sync`, `fsck`, `metrics`, `forget` |

`minds --help` zeigt alles im Detail; `minds agent-help` gibt dieselbe Karte als JSON
aus — für Agents, nicht für Menschen.

## Was Minds besonders macht

- **Reviews sind Git-Objekte.** Verdict, Kommentare und Approval liegen unter
  `refs/minds/reviews/`, signiert und an eine **Change-Id** gebunden — sie überleben
  Rebase, Squash und Force-Push. GitLab wird zur Projektion, nicht zur Quelle der
  Wahrheit. Wer die Plattform wechselt, nimmt die Review-Historie mit.
- **Redaction läuft fail-closed**, *bevor* ein Byte in den Store geht. Ein Secret, das
  nie gespeichert wird, muss auch nie gelöscht werden.
- **`minds forget` löscht wirklich.** Die Nutzlast wird durch einen Tombstone ersetzt,
  die Hash-Referenz bleibt auflösbar — DSGVO-Löschung, ohne die Kette zu brechen. Das
  kann reines Git strukturell nicht.
- **Kein Dienst, keine Telemetrie.** Nichts verlässt deine Maschine, das du nicht
  selbst pushst. Offline und im Air-Gap voll funktionsfähig.

## Agent-Unterstützung

| Agent | Stand |
|---|---|
| Claude Code | vollständig — Prompt, Tool-Aufrufe, Dateien, Modell, Tokens |
| Codex, Cursor, Gemini, opencode | Hooks registrierbar, Prompt wird erfasst; die Tool-Ebene wird noch nicht gedeutet |

Absicht: lieber ein Agent richtig als vier halb. Welcher als nächstes vollständig
unterstützt wird, richtet sich nach dem Bedarf der Nutzer — [sag uns, was du
benutzt](https://github.com/munichbughunter/minds/issues).

## Plattform-Fokus: GitLab

Erfassung, Nachschlagen und Reviews brauchen nur Git — sie funktionieren auf jeder
Forge und ganz ohne. Die **Plattform-Brücke** dagegen — Verdicts als MR-Notiz
spiegeln (`minds gitlab mirror`), perspektivisch Webhook und CI-Gate — zielt auf
**GitLab**, self-managed wie gitlab.com. Eine Anbindung anderer Plattformen ist
derzeit nicht geplant.

## Aus dem Quelltext bauen

```sh
cargo build --release --bin minds     # Rust 1.85+
cargo test --workspace
```

## Weiterlesen

- [**Was Minds ist und warum es das gibt**](docs/fuer-tester.md) — die ausführliche
  Einführung, gedacht für alle, die es zum ersten Mal in die Hand nehmen
- [Roadmap & Strategie](Roadmap.md) — die These, der Markt, die technische Roadmap
- [Betriebsmodell GitLab](docs/betriebsmodell-gitlab.md) — Git ist Quelle, GitLab ist
  Projektion
- [Nachweis-Leitfaden](docs/nachweis-leitfaden.md) — was ein Audit-Bundle beweist und
  was nicht
- [Architekturentscheidungen](docs/adr/) — warum Hooks statt Transkript-Parsing, warum
  ein Ref je Session, warum Reviews als Git-Objekte
- [CHANGELOG](CHANGELOG.md)

## Lizenz

Apache-2.0
