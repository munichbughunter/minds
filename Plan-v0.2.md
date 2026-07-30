# Minds — Umsetzungsplan v0.2

*Begleitdokument zu `Plan.md` (v0.1) und `Roadmap.md` (die große Wette).
Kleine Schritte, kleine Commits, jeder Commit bleibt grün.*

---

## Leitentscheidung: mehr ins Repo, weniger in die Plattform

Fast jede Schwäche im heutigen Git/GitLab-Modell löst sich in **eine** Richtung auf:
Dinge, die heute in der Plattform-Datenbank liegen (Reviews, Identität, Kontext),
gehören ins Repo. Git ist nicht zu wenig — es wird zu wenig benutzt, weil die
Plattformen kein Interesse daran haben. Minds wettet genau darauf: **Kontext als
Git-Objekt.** v0.2 zieht diese Wette konsequenter durch.

**Leitlinie für jedes neue Feature:**
> Zuerst fragen „geht das als **Git-Objekt**?", erst dann „geht das als
> GitLab-Feature?". Ein Git-Objekt wandert mit dem Repo, überlebt Migration,
> funktioniert offline und im Air-Gap. Ein Plattform-Feature nicht.

---

## Der Code-Stand, der die Reihenfolge bestimmt

Zwei Befunde aus der Analyse des Ist-Stands:

1. **Ein echtes Sicherheitsloch, keine Lücke.** Die Secret-Wall auf dem heißen Pfad
   (`crates/minds-capture/src/secretwall.rs:53`) liest **Claude-Feldnamen**. Ein
   Gemini/Codex, der eine `.env` liest, rutscht daran vorbei. Das bricht das
   fail-closed-Versprechen für alle Nicht-Claude-Agents → **muss zuerst**.
2. **Capture ist an genau einer Stelle Claude-only.** `normalize::tool_facts`
   (`crates/minds-capture/src/normalize.rs:107`) interpretiert Tools nur für
   `claude-code`. Der heiße Pfad nimmt schon jeden Agenten an (Journal), aber
   Effekte/produzierte Dateien/Artefakt-Hashes gibt es nur für Claude.

Das Kern-Datenmodell (`minds-core`) ist **agent-neutral** und bleibt unverändert.
Es wird gefüllt, nicht umgebaut — die Naht ist schon da.

---

## Schichten-Übersicht

- **Schicht 0** — Sicherheit zuerst (ein Commit).
- **Schicht 1** — Das Fundament real machen: Multi-Agent-Capture + `session.md` im Branch.
- **Schicht 2** — Die These schärfen: Change-Id, signierte Attribution, `minds forget`.
- **Bereitstellung** — Selbst-Installation ohne Handverteilung (querschnittlich, früh scharf schalten).
- **CLI-Vollständigkeit** — Kontext-Rückführung (recall/distill), Parität (blame/log/search/recap/agent-help).
- **Sichtbarkeit (parallel liefern)** — Metriken + Grafana (Track M) **und** eigener UI-Reader-Ausbau (Track U), damit Nutzer zeitnah etwas sehen.
- **Sync/Mirror** — Transport-Primitiv für `refs/minds/*`, erst *nach* der CLI-Vollständigkeit.
- **Schicht 3** — North Star: Reviews als Git-Objekte (hier nur Richtung; Kommit-Zerlegung in `Roadmap.md`).

Schicht 2 hängt **nicht** an Schicht 1 — signierte Claude-Attribution ist sofort
wertvoll, ohne Gemini-Support. Nach dem Secret-Wall-Fix können Schicht 1 und 2
parallel laufen.

---

## Schicht 0 — Das Sicherheitsloch (ein Commit)

- **0.1** `fix(capture): Secret-Wall erkennt Nicht-Claude-Feldnamen (fail-closed für alle Agents)`
  - `secretwall::guard` zieht Datei-Pfade aus der Union bekannter Feldvarianten
    (oder Dispatch pro Agent): Claude `tool_input.file_path`/`notebook_path` + die
    Gemini-/Codex-Pendants.
  - **DoD:** Fixture-Test, der einen Gemini-`.env`-Read auf dem heißen Pfad
    **blockt**. Vorher rot, nachher grün. Kein Verhaltenswechsel für Claude.

---

## Schicht 1 — Das Fundament real machen

### Track A — Multi-Agent-Capture

- **A.1** `test(capture): echte Hook-Payloads + Transkripte für Gemini/Codex als Fixtures`
  - In einem Test-Repo `minds enable --agent gemini`/`--agent codex`, kurze echte
    Session, Journal unter `<git-dir>/minds/journal/<agent>/…` inspizieren,
    anonymisieren, als Golden-Fixtures ablegen. Dazu je ein `NOTES.md` mit dem
    Feld-Mapping (Prompt, Tool-Name, Tool-Input, Datei-Pfad, Event-Namen,
    Transkript-Ort/-Format).
  - **DoD:** Für Gemini und Codex je ≥1 realer Payload + Transkript-Snippet + Mapping.
  - *Ohne echte Samples ist dieser Commit „Samples beschaffen" — der Rest wäre sonst Raten.*
- **A.2** `refactor(capture): ToolAdapter-Trait, claude-code als Referenz-Impl (kein Verhaltenswechsel)`
  - Das `match agent { "claude-code" => … }` hinter einen Trait + Registry legen.
    **DoD:** alle Tests grün, kein Byte anders im Output.
- **A.3** `feat(capture): Gemini ToolAdapter — tool→effect, produced files, Artefakt-Hashes`
- **A.4** `feat(capture): Codex/OpenAI ToolAdapter — dito` (auch `is_git_commit` im
  Backfill agent-agnostisch machen, heute hart auf Tool-Name `"Bash"`)
- **A.5** `feat(capture): Gemini-Transkript-Reader (model, tokens, agent_version, Assistant-Text)`
  - Eigenes Modul analog `transcript.rs` (Claude-JSONL-spezifisch). `provider_of()`
    kennt `google`/`openai`/`anthropic` schon. *Graceful degradation:* fehlt der
    Reader, kommt die Session trotzdem aus dem Journal (Struktur + Tools).
- **A.6** `feat(capture): Codex-Transkript-Reader`
- **A.7** `feat(capture): echte Importer für Gemini/Codex statt Stubs` (`import::import_agent`)
- **A.8** `test(capture): End-to-End je Agent — enable → hook → checkpoint → Session korrekt`
  - Sicherheitsnetz gegen Format-Drift (das Hauptrisiko der Vision).

### Track C — Der Session-Branch als GitLab-natives Review-Artefakt

Der Branch aus `put_session_branch` trägt heute nur `session.json`. Mit einer
gerenderten `session.md` **zeigt GitLab den Branch nativ als lesbare Seite** — kein
Reader-Deploy nötig. Billig, hoher Demo-Effekt, direkt „mehr ins Repo".

- **C.1** `feat(reader): einzelne Session als Markdown rendern (session.md, deterministisch, 0 Tokens)`
  - Reine Funktion: `Session` → Markdown (Intent, Herkunft, Verlauf, Tool-Calls,
    berührte Dateien, Redaction-Zähler). Kein I/O, golden-testbar.
- **C.2** `feat(store): put_session_branch schreibt session.json + session.md`
  - Baum in `git_store::write_session_branch` erweitern. Idempotenz bleibt
    (gleicher Baum → `Unchanged`). **DoD:** Klick auf `minds/session/<hash>` in
    GitLab zeigt eine lesbare Seite.
- **C.3** `feat(store): README.md/Index im Session-Branch für Navigation` *(optional)*

---

## Schicht 2 — Die These schärfen (additiv, differenzierend)

Fast alles additive Trailer/Objekte — kleiner Aufwand, großer Positionsgewinn.

### Change-Id — Identität, die Rebase/Squash überlebt

Heute überlebt der Trailer, weil er in der Message steht. Change-Ids machen daraus
ein *Prinzip*: „diese Änderung" wird von „diese Version dieser Änderung" getrennt
(wie Gerrit/Jujutsu). Folge: stabile Verweise, stacked changes, Review-Kontinuität
über Force-Push hinweg.

- **S2.1** `docs: ADR-0005 — Change-Id (stabile Änderungs-Identität, überlebt Force-Push)`
- **S2.2** `feat(core): Change-Id-Typ + parse/format (Trailer Minds-Change-Id)`
- **S2.3** `feat(git): Change-Id sicherstellen (prepare-commit-msg: erzeugen falls fehlt, erhalten bei amend/rebase)`
- **S2.4** `feat(store): Session an Change-Id binden; Session-Branches nach Change-Id gruppieren`
- **S2.5** `test(store): Rebase/Squash-Simulation — Change-Id + Verknüpfung überleben`

### Signierte Attribution — Author wird beweisbar statt behauptet

`author` ist heute ein unsigniertes Freitextfeld. In einer Welt, in der Agents
committen, ist das genau die Grundlage, auf der man **nichts** nachweisen kann. Eine
detached Signatur über die kanonische Attribution macht „Agent X, Modell Y schrieb
diese Zeilen" verifizierbar — das Kernargument für die regulierte Zielgruppe.

- **S2.6** `docs: ADR-0006 — signierte Attribution (ssh-sig, verifizierbar, air-gap-tauglich)`
- **S2.7** `feat(core): Signatur-Feld über kanonische Attribution (detached)`
- **S2.8** `feat(cli): Signatur beim checkpoint (ssh-key), Trailer Minds-Attribution-Sig`
- **S2.9** `feat(cli): minds verify — Attribution-Signatur prüfen`
- **S2.10** `test(cli): verify schlägt bei Manipulation fehl, grün bei Original`

### `minds forget` — redigierbare Nutzlast (das, was Git strukturell nicht kann)

Ein Secret oder personenbezogene Daten in der History sind in reinem Git für immer
drin — DSGVO-Löschung und Merkle-Kette schließen sich aus. Minds trennt aber
**Referenz** (Trailer im Commit) von **Nutzlast** (Session-JSON im Store) und ist
content-adressiert mit graceful degradation. Also: Nutzlast durch einen Tombstone
ersetzen, Hash-Referenz bleibt auflösbar. Fast geschenkt — und ein echter
Differentiator.

- **S2.11** `docs: ADR-0007 — redigierbare Nutzlast (Tombstone, Referenz überlebt)`
- **S2.12** `feat(store): forget(session) — Payload durch signierten Tombstone ersetzen`
- **S2.13** `feat(cli): minds forget <session> [--reason] (DSGVO-Löschung, auditierbar)`
- **S2.14** `feat(reader): Tombstone graceful anzeigen („Inhalt auf Antrag entfernt, Referenz erhalten")`
- **S2.15** `test: forget entfernt Inhalt; why/show/fsck bleiben grün (degradiert, kein Fehler)`

---

## Bereitstellung — Selbst-Installation ohne Handverteilung

**Das Problem:** Bisher wird für jeden Empfänger ein `tar.xz`-Archiv von Hand gebaut
und verschickt. Das skaliert nicht und ist der falsche Weg — ein User soll `minds`
**selbst** mit einem Befehl installieren und künftige Versionen selbst nachziehen.

**Das Gerüst steht schon** (`dist-workspace.toml`: cargo-dist 0.32, Targets für
macOS/Linux/Windows, `installers=["shell"]`, `ci=["gitlab"]`, `install-updater=true`).
Es muss verdrahtet und einmal scharf geschaltet werden.

- **D.1** `chore(dist): dist-workspace.toml finalisieren, cargo dist plan grün`
  - Targets/Installer prüfen; `cargo dist plan` läuft fehlerfrei.
- **D.2** `ci(dist): GitLab-Release-Pipeline generieren (cargo dist generate)`
  - Tag `v*` → Build-Matrix über alle Targets → Release mit Assets + Installer-Skript.
- **D.3** `feat(dist): gehosteter Shell-Installer — curl … | sh nach ~/.local/bin`
  - Ein Befehl lädt das passende Target aus der GitLab-Release und legt es in den PATH.
- **D.4** `fix(dist): macOS-Gatekeeper — Installer entfernt Quarantäne automatisch`
  - `xattr -d com.apple.quarantine` im Installer; **Notarisierung** (Apple Developer
    ID, kostenpflichtig) als Option dokumentieren für einen ganz nahtlosen Start.
- **D.5** `chore(release): SemVer + CHANGELOG; minds --version an den Git-Tag koppeln`
- **D.6** `docs: README ersetzen — aktuell GitLab-Boilerplate → echte Install-Zeile + Kurzanleitung`
  - Die eine `curl … | sh`-Zeile ganz oben; `INSTALL.md` (Handarchiv) bleibt als
    **Air-Gap-Fallback** für Kunden ohne Netz erhalten.
- **D.7** `feat(dist): minds update verifizieren (Self-Update, install-updater ist aktiv)`
- **D.8** `chore(dist): Homebrew-Tap + cargo install als Zweitkanäle` *(optional)*

**DoD:** Der Kollege installiert mit **einem** `curl … | sh` (oder `brew install`),
ohne dass jemand ein Archiv baut. `minds update` zieht künftige Versionen selbst.
Das Handarchiv bleibt nur noch der dokumentierte Air-Gap-Weg.

---

## CLI-Vollständigkeit — bevor irgendeine UI kommt

> Dein Prinzip: erst muss die CLI sauber laufen. entire's Release-Verlauf und Grain
> zeigen, welche Kommandos dazugehören. Drei Tracks — Kontext-Rückführung (R),
> Parität (E), Metriken (M) — plus Sync (S) bewusst *nach* der CLI-Vollständigkeit.

### Track R — Kontext-Rückführung an den nächsten Agenten (Vision-Problem #3)

**Das Problem:** Die Vision nennt vier Probleme; v0.1 löst #1 (MR-Ratespiel) und #4
(Audit). **#3 — „kein Agent lernt aus dem letzten" — ist offen.** Das Wissen einer
Session (Korrekturen, Sackgassen, die Befehle, die wirklich funktioniert haben) liegt
schon redigiert im Store; heute liest es nur niemand zurück. Grain (`scan` →
`AGENTS.md`) und entire (Skills / „continue") zielen genau hierauf.

**Drei Kommandos, klar getrennt nach Blickrichtung:**

| Kommando | Blickrichtung | Frage | Ausgabe |
|---|---|---|---|
| `minds recall <ziel>` | rückblickend, gezielt | „Was ist der Kontext hinter *diesem* Ding?" | knapper Brief zu 1–n Sessions |
| `minds distill [--path]` | kumulativ, repo-weit | „Was hat die Historie dieses Repos Agenten beigebracht?" | AGENTS.md-Entwurf |
| `minds brief --for <pfade>` | vorausschauend, Session-Start | „Ich fange gleich an X an — was muss ein Agent wissen?" | kompakter, größenbegrenzter Block |

`recall` ist die Agent-freundliche Schwester von `minds why`: `why` ist der
menschliche Deep-Dive (voller Transcript), `recall` der verdichtete, handlungs-
orientierte Brief — und aggregiert über mehrere Sessions.

**Woraus wir deterministisch extrahieren (0 Tokens, kein LLM):**

| Signal | Quelle im Modell | Güte |
|---|---|---|
| Absicht je Session | `intent.request` | stark |
| Funktionierende Befehle | `ToolCall{effect.kind = Exec}` (Bash/Shell) + `arguments` | stark (bereits redigiert) |
| Berührte/erzeugte Dateien, Hot-Spots | `produced.files`, Häufung über Sessions | stark |
| Rework / Sackgassen | Effekt-Muster: `Write`→`Delete`, `Edit` revidiert früheren `Edit` | mittel (Heuristik) |
| Korrekturen | User-Turn nach Assistant-Turn mit Korrektur-Sprache („nein/stattdessen/revert/war falsch") | mittel (Heuristik) |
| Co-Change-Cluster | Dateien, die in denselben Sessions/Commits zusammen auftauchen | mittel |
| Verworfene Ansätze | `intent.discarded` | schwach heute (Feld noch leer) → Enabler R.2 |

> **Ehrlich:** „Konventionen" als Stilregeln brauchen den Code selbst oder ein LLM.
> v0.2-`distill` liefert **beobachtete Fakten** (Befehle, Korrekturen, Hot-Spots,
> Co-Changes), keine erfundene Prosa. Heuristische Signale sind als solche markiert.

**Garantien (testbar):**
- **Deterministisch:** gleiche Eingabe → byte-gleiche Ausgabe (Sortierung nach
  Zeit/Hash). Golden-Tests.
- **Redigiert:** Ausgabe stammt ausschließlich aus dem Store, der nur
  `RedactedSession` hält. Zusätzlich ein Test, dass ein gepflanztes Secret **nie** im
  Brief erscheint.
- **0 Tokens** im Default. Optionaler `--llm`-Pfad (später, M8) verdichtet den
  deterministischen Extrakt zu Prosa — **content-hash-gecacht**, gleiche Session nie
  zweimal bezahlt.

**Ausgabe & Auswahl:**
- Default Markdown (liest sich wie AGENTS.md); `--format json` für Tooling.
- `recall <ziel>`: Ziel = `datei` | `datei:zeile` | `change-id` | `commit` |
  `session-id`, aufgelöst über blame→Trailer→Session (wie `why`).
- `distill`: `--path <dir>` / `--since <ref>` grenzt die Sessions ein; `--out AGENTS.md`
  schreibt statt stdout. Merge in bestehende AGENTS.md ist v0.3 (erst Entwurf, User merged).
- `brief --for <pfade>`: leitet den Kontext aus den genannten (oder gestagten)
  Dateien ab; **größenbegrenzt** (Top-N nach Relevanz), damit der Agent-Input klein
  bleibt (Headroom-Rücksicht).

**Optionaler Auto-Loop (opt-in):** `minds enable --recall` verdrahtet einen
SessionStart-Hook, der `minds brief` für den aktuellen Arbeitsstand erzeugt und dem
Agenten voranstellt — so lernt jede neue Session automatisch aus den letzten. Aus,
solange nicht ausdrücklich aktiviert (kostet Agent-Tokens).

**Kommit-Zerlegung:**
- **R.1** `feat(core): Extraktoren — Befehle (Exec), Hot-Spots, Co-Change, Rework-Muster (rein, testbar, kein I/O)`
- **R.2** `feat(capture): intent.discarded/constraints beim Checkpoint befüllen (Enabler für Sackgassen)` *(Cross-Link zu Track A)*
- **R.3** `feat(cli): minds recall <ziel> — Brief zu 1–n Sessions (Markdown/JSON)`
- **R.4** `feat(cli): minds distill [--path|--since|--out] — AGENTS.md-Entwurf aus beobachteten Fakten`
- **R.5** `feat(cli): minds brief --for <pfade> — größenbegrenzter Kontext-Block, --format agent`
- **R.6** `feat(cli): minds enable --recall — optionaler SessionStart-Hook (Auto-Rückführung)`
- **R.7** `test(cli): Golden-Fixtures — Determinismus + „Secret erscheint nie" + Heuristik-Grenzfälle`
- **R.8** `docs: ADR — deterministische Rückführung, Heuristiken benannt, optionaler LLM-Pfad`

**Offene Entscheidungen für diesen Track:**
1. **`brief`-Relevanz:** rein strukturell (Sessions, die genau diese Dateien
   berührten) für v0.2 — semantische Relevanz später? *(Empfehlung: ja, strukturell.)*
2. **`distill` LLM-Default:** deterministisch als Default, `--llm` opt-in. *(Empfehlung: ja.)*
3. **AGENTS.md-Merge:** v0.2 nur Entwurf ausgeben, Merge dem User überlassen. *(Empfehlung: ja.)*

### Track E — CLI-Parität (die Kommandos, die entire/Grain aufzeigen)

- **E.1** `feat(cli): minds blame <datei> — pro-Zeile-Attribution im Überblick (git-/entire-vertraut)`
  - **Entscheidung `why` vs. `blame`:** *beide behalten*, unterschiedliche
    Granularität. `minds blame <datei>` = wer/welcher Agent pro Zeile (Überblick).
    `minds why <datei>:<zeile>` = *warum* genau diese Zeile → volle Session/Intent.
    `why` ist unser Marken-Verb (Intent, nicht nur Autorschaft) — das geben wir nicht
    auf. `minds blame <datei>:<zeile>` darf als Alias auf `why` zeigen.
- **E.2** `feat(cli): minds log — Session-/Checkpoint-Übersicht (Zeit, Agent, Dateien, Diff, Tokens)`
- **E.3** `feat(cli): minds search <query> — Prompts/Sessions durchsuchen (echte Lücke heute)`
- **E.4** `feat(cli): minds recap [--since] — die letzten Sessions als Klartext-Zusammenfassung`
- **E.5** `feat(cli): minds agent-help — maschinenlesbare Karte der eigenen CLI für Agents (billig, hoher Hebel)`
- **E.6** `feat(redact): user-defined Redaction-Regeln aus Config, zusätzlich zu den Detektoren`

### Track M — Metriken & Observability (opt-in, kein Doppel-Speichern)

**Prinzip:** `minds metrics` liest den Store on-demand und **projiziert** — kein
zweiter Zustand, kein laufender Dienst. Ausgabe im Prometheus-/OpenMetrics-Textformat;
Grafana läuft **beim Kunden** (haben regulierte Läden ohnehin). Die
Kennzahl-Ableitungen liegen in einer **reinen, geteilten `minds-metrics`-Crate**
(hängt nur an `core`, kein I/O), die sowohl das CLI-Kommando **als auch** die
Reader-Kacheln (Track U) nutzen — **eine Definition, zwei Oberflächen.**

**Die vier Kern-Kacheln (identisch zu entire's Overview, aus unseren Daten):**

| Kachel | Definition | Quelle |
|---|---|---|
| **Throughput** | Ø Tokens je Checkpoint | `usage.input+output` / #Checkpoints |
| **Iteration** | Ø Schritte je Checkpoint (Schritt = Tool-Call) | #`tool_calls` / #Checkpoints |
| **Continuity** | längste Session (h) | max(`lineage.ended_at − started_at`) |
| **Streak** | aufeinanderfolgende Tage mit ≥1 Checkpoint | aus `lineage`-Tagen |

**Audit-/Team-Kennzahlen (der eigentliche Mehrwert für Regulierte):** Agent-vs-Human-
Anteil, Redaction-Treffer, **Kontext-Abdeckung** (Anteil agent-authored Commits mit
auflösbarem Trailer — aus `fsck`), Sessions/Repo, Tool-Calls nach Effekt.

**Metriknamen (stabil, niedrige Kardinalität — nie nach Session-Id labeln):**
```
minds_sessions_total{repo,agent,model}
minds_tokens_total{repo,agent,kind="input|output"}
minds_tool_calls_total{repo,agent,effect="read|write|delete|exec|other"}
minds_session_duration_seconds{repo,agent}         # Summary/Histogram
minds_attribution_agent_ratio{repo}                # Gauge 0..1
minds_redaction_hits_total{repo,kind="secret|pii"}
minds_context_coverage_ratio{repo}                 # Gauge 0..1 (aus fsck)
minds_checkpoints_total{repo,agent}
```

**Kommit-Zerlegung:**
- **M.1** `feat(metrics): reine minds-metrics-Crate — Ableitungen, kein I/O, golden-getestet` (von CLI und Reader geteilt)
- **M.2** `feat(cli): minds metrics --format prometheus|openmetrics|json (Projektion aus dem Store)`
- **M.3** `feat(ci): opt-in Job — minds metrics in Textfile/Pushgateway emittieren (bricht nichts)`
- **M.4** `chore(dashboards): dashboards/minds.json — importierbares Grafana-Dashboard`

**Grafana-Panels (im mitgelieferten JSON):** (1) vier Stat-Panels Throughput/
Iteration/Continuity/Streak, (2) Agent-vs-Human-Anteil als Gauge, (3) Tokens über
Zeit, (4) Tool-Calls nach Effekt, (5) **Kontext-Abdeckung** als Audit-Gauge, (6)
Redaction-Treffer + Sessions/Repo.

**Bewusst später:** historischer Backfill über Zeit-Buckets (`--since`) und ein
`--serve`-Endpoint. v0.2 = Ein-Schuss-Emit im Scrape-Modell.

### Track S — Sync/Mirror (erst *nach* der CLI-Vollständigkeit)

Verallgemeinerung des Child-Repo-Backends zu einem Transport-Primitiv (git-sync-Stil,
SSH-Creds wiederverwenden): spiegelt `refs/minds/*` zwischen Remotes. **Transport,
nicht Ort** — kein Hosting. Vollständige Ausarbeitung in `Roadmap.md`.

- **S.1** `feat(cli): minds sync — refs/minds/* zwischen Remotes spiegeln (push/pull, idempotent)`
- **S.2** `feat(cli): SSH-Remote-Mirroring (vorhandene Creds), Air-Gap-Export/-Import`

---

## Track U — Eigene UI (Reader-Ausbau nach entire-Vorbild)

**Entscheidung:** Die „eigene UI" ist der **Ausbau des statischen Readers**
(`minds render` → `minds-reader`), **kein** gehosteter Web-Dienst. Damit bleibt sie
zustandslos, air-gap-tauglich, ein Binary — thesenkonform — und wir bauen auf dem
vorhandenen Renderer auf statt bei null. Alle Interaktion (Tabs, Tool-Drawer,
Aufklappen) läuft in eingebettetem Vanilla-JS (der Reader bringt das Muster schon
mit). Vorlage: die neun Screenshots in `./screenshots`.

**Was die Screenshots zeigen → was wir bauen:**

| entire-Screen | Reader-Baustein | Datenquelle |
|---|---|---|
| Overview: 4 KPI-Kacheln | Index-Kopf mit Kacheln (Track-M-Crate!) | `minds-metrics` |
| Overview: Aktivitäts-Chart (Bubble, Farbe=Agent) | Inline-SVG-Chart über Zeit | `lineage`, `produced`, `agent` |
| Overview/Repo: Checkpoint-Liste nach Tag | gruppierte Liste (Commits/Files/Diff) | Index |
| Repo-View: Branch + Suche | Filter/Suche im Index | Index |
| Checkpoint-Detail: Tabs Sessions/Files | Session-Seite mit Tabs | Session |
| **Session-Transcript** (Prompt→Antwort, Rollen) | **turns-Timeline** | `turns` |
| **Tool-Calls-Drawer** (Liste + args/Diff) | **Tool-Panel** (Liste + Detail) | `tool_calls`, `effect` |
| Attribution „100% AI" | Attributions-Leiste | Attribution |
| Commit-View mit Trailer + Side-by-Side-Diff | Trailer sichtbar machen (Diff haben wir) | Git + Trailer |

**Kommit-Zerlegung (in Liefer-Reihenfolge — jeder Schritt ist sofort sichtbar):**
- **U.1** `feat(reader): turns-Timeline im Session-Panel (User/Assistant/Tool, Rollen, Reihenfolge)` — die größte Content-Lücke.
- **U.2** `feat(reader): Tool-Calls-Panel — Liste (Name/Icon) + Detail (arguments ODER Effekt-Diff)` — das Drawer-Muster.
- **U.3** `feat(reader): Attributions-Leiste (X% Agent) pro Session/Checkpoint`
- **U.4** `feat(reader): Index-Kopf mit KPI-Kacheln (Throughput/Iteration/Continuity/Streak) aus minds-metrics`
- **U.5** `feat(reader): Aktivitäts-Chart (Inline-SVG, Bubble = Diff-Größe, Farbe = Agent)`
- **U.6** `feat(reader): Checkpoint-Liste nach Tag gruppiert + Suche/Filter im Index`
- **U.7** `feat(reader): Zero-Dep-Syntax-Highlighting für Diffs/Code (Rust/JS/TS/Python)`
- **U.8** `polish(reader): Styling nach Screenshots — Typo, Agent-Badges, Light/Dark, Empty-States`

**Schnellster sichtbarer Slice (Deliver-First):** **U.1 + U.2 + U.3** auf einer echten
Session — die Transcript-Ansicht mit Tool-Drawer und Attribution ist der „Magic
Moment" und genau das, was heute fehlt. Danach **U.4/U.5** (Overview) für den
Dashboard-Effekt. Läuft **parallel** zu Track M (Grafana), der noch schneller sichtbar ist.

---

## Schicht 3 — North Star: Reviews als Git-Objekte (Richtung, kein Zeitplan hier)

Das Projektgedächtnis von GitLab — Reviews, Diskussionen, Approvals — liegt in
Postgres, nicht im Repo. Migriert man weg, verliert man die Hälfte der Geschichte.
Radicle und git-bug zeigen den anderen Weg: **Issues und Reviews als Git-Objekte.**
Minds' nächste große Wette ist, dasselbe für Agent-Reviews zu tun — der Verdict zu
einer Session/Change liegt content-adressiert unter `refs/minds/reviews/`, signiert,
wandert mit dem Repo. GitLab wird zum Cache, nicht zur Quelle der Wahrheit.

**Warum es zwingend hierhin gehört:** Schicht 2 liefert die Bausteine schon —
signierte Identität (S2.6–S2.10) und stabile Change-Ids (S2.1–S2.5) sind genau das,
worauf ein Git-natives Review aufsetzt. Reviews sind die logische Fortsetzung, kein
Themenwechsel.

→ **Die vollständige Zerlegung in Phasen R1–R6 steht in `Roadmap.md`** (das
vorzeigbare Dokument). Hier bewusst nur die Richtung.

---

## Empfohlene Reihenfolge für die nächste Session

1. **0.1** Secret-Wall-Fix (Sicherheit, klein, sofort).
2. **D.1 → D.6** Selbst-Installation scharf schalten — beendet das Archiv-Verschicken;
   parallel, blockiert nichts.
3. **A.1 → A.4** Multi-Agent-Capture (Formate pinnen → Effekte Gemini/Codex).
4. **C.1 → C.2** `session.md` in den Branch (GitLab-nativ lesbar).
5. **R.1 → R.8** Kontext-Rückführung (recall/distill) — **priorisiert**; löst
   Vision-Problem #3.
6. **Parallel & schnellstmöglich sichtbar:** **M.1 → M.4** (Metriken + Grafana) **und**
   **U.1 → U.3** (Transcript-Ansicht + Tool-Drawer + Attribution im Reader — der Magic Moment).
7. **U.4 → U.8** Overview-Kacheln, Aktivitäts-Chart, Liste/Suche, Highlighting, Politur.
8. **E.1 → E.6** CLI-Parität (blame/log/search/recap/agent-help + user-Redaction).
9. Parallel: **S2.1 → S2.5** (Change-Id), **S2.11 → S2.15** (`forget`).
10. **A.5 → A.6** Transkript-Anreicherung; **S2.6 → S2.10** signierte Attribution.
11. **S.1 → S.2** Sync/Mirror (nach CLI-Vollständigkeit).

**Minimaler wertstiftender Durchstich:** `0.1 → A.1 → A.3 (nur Gemini) → C.1 → C.2`.
Ergebnis: Gemini-Sessions werden sicher und korrekt erfasst und erscheinen als
lesbare Seite direkt in GitLab.

---

## Datei-Landmarken

| Zweck | Ort |
|---|---|
| Dispatch-Punkt (Tool→Effekt), Claude-only | `crates/minds-capture/src/normalize.rs:107` |
| Claude-Effekt-Map (Vorlage) | `crates/minds-capture/src/normalize.rs:146` |
| Secret-Wall (Sicherheits-Fix 0.1) | `crates/minds-capture/src/secretwall.rs:53` |
| Transkript-Reader (Claude-JSONL) | `crates/minds-capture/src/transcript.rs` |
| Backfill-Importer (honest stubs) | `crates/minds-capture/src/import.rs:59` |
| Hook-Registrierung je Agent | `crates/minds-cli/src/enable.rs` |
| Session-Branch schreiben (Track C) | `crates/minds-store/src/git_store.rs` (`write_session_branch`) |
| Trailer schreiben/lesen (Change-Id, Sig) | `crates/minds-git` + `crates/minds-core` |
| HTML/CSS-Generierung | `crates/minds-reader/src/html.rs` |
| Kern-Datenmodell (bleibt unverändert) | `crates/minds-core/src/session.rs:44` |

---

## Offene Entscheidungen

- **Signatur-Verfahren (S2.7):** `ssh-sig` (überall vorhanden, air-gap-tauglich) vs.
  sigstore/gitsign (moderner, aber Netz/OIDC). Empfehlung: `ssh-sig` als Default,
  sigstore optional später.
- **Syntax-Highlighting im Reader:** Zero-Dep-Prinzip halten (eigener Mini-Highlighter)
  vs. `syntect` hinter Feature-Flag. Empfehlung: Zero-Dep halten. *(Reader-Politur ist
  bewusst niedrig priorisiert — erst reiche Daten, dann Politur.)*
- **Change-Id-Format:** eigenes `b3-`-Schema vs. Gerrit-kompatibles `I<40hex>`.
  Empfehlung: Gerrit-kompatibel, damit vorhandene Tooling-Erwartungen greifen.
