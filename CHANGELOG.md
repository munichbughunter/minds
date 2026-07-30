# Changelog

Alle nennenswerten Änderungen an Minds stehen hier.

Das Format folgt [Keep a Changelog](https://keepachangelog.com/de/1.1.0/), die
Versionierung [Semantic Versioning](https://semver.org/lang/de/).

> **Noch keine 1.0.** Solange die Führungsziffer `0` ist, gibt es keine
> Kompatibilitätszusage: Jede MINOR-Version (`0.1` → `0.2`) darf die CLI-Oberfläche
> und das Store-Layout brechen. PATCH-Versionen (`0.1.0` → `0.1.1`) enthalten nur
> Korrekturen.
>
> **Davon getrennt zu betrachten ist `schema_version`** in den abgelegten Objekten
> (Session, Review). Die Version des Binaries versioniert die *Oberfläche*, das
> Schema versioniert das *gespeicherte Objekt* — und ein Objekt lebt so lange wie das
> Repo. Es gilt: ein neueres Binary liest alle älteren Schema-Versionen; das Schema
> steigt nur bei einer brechenden Änderung an der Nutzlast, nie bei einem zusätzlichen
> Feld.

## [Unreleased]

## [0.1.0] — 2026-07-29

Die erste veröffentlichte Version — und die erste, die über einen Installer
ausgeliefert wird statt als handgebautes Archiv.

Minds schreibt den Kontext einer Agent-Session dorthin, wo er hingehört: in Git
selbst, neben den Code. Was eine Änderung veranlasst hat, wer sie geschrieben hat und
wer sie geprüft hat, liegt content-adressiert und signiert unter `refs/minds/` und
wandert mit dem Repo — ohne Datenbank, ohne Cloud, offline und im Air-Gap
verifizierbar.

> Frühere, von Hand gebaute Archive trugen bereits dieselbe Versionsnummer, liegen
> aber vor diesem Tag. `minds --version` meldet heute nur `0.1.0` und unterscheidet
> die beiden nicht — wer noch ein altes Archiv im Pfad hat, installiert bitte neu.

### Hinzugefügt

**Erfassung**

- **Hook-basiertes Capture.** `minds enable` registriert Agent- und Git-Hooks;
  idempotent und fremdschonend. Der heiße Pfad (`minds hook`) schreibt jedes Event
  ins lokale Journal und endet immer mit 0, der kalte Pfad (`minds checkpoint`)
  deutet, redigiert, speichert und hängt den Session-Id-Trailer an den Commit. Siehe
  [ADR-0003](docs/adr/0003-hooks-statt-transkript-parsing.md).
- **Fünf Agents registrierbar:** `claude-code`, `codex`, `cursor`, `gemini`,
  `opencode`. Die Deutung der Tool-Ebene ist zunächst Claude Code vorbehalten (siehe
  *Bekannte Einschränkungen*).
- **Redaction, fail-closed.** Secrets und personenbezogene Daten gehen raus, *bevor*
  ein Byte in den Store geht — im Zweifel blockiert Minds, statt zu riskieren. Regeln
  erweiterbar über `.minds/redact.json`.
- **Import bestehender Historie** mit heuristischer Zuordnung Session → Commit;
  vermutete Zuordnungen werden als *vermutet* ausgewiesen statt als Tatsache. Siehe
  [ADR-0004](docs/adr/0004-import-und-store-index.md).

**Speicherung**

- **Content-adressierter Store** (`SessionId = blake3(canonical_json)`) mit zwei
  Backends hinter einem Trait: in-repo unter `refs/minds/` und als separates
  Child-Repo.
- **Ein Ref je Session.** Kein gemeinsam beschriebener Ref, damit kein
  Serialisierungspunkt für Schreiben und Pushen: Der Ref-Name *ist* der Inhalts-Hash,
  zwei Agents fassen verschiedene Refs an, und ein Repo, das nur eincheckt, zahlt für
  den Hook 0,02 s. Siehe [ADR-0010](docs/adr/0010-ein-ref-je-session.md).
- **Browserbare Session-Branches.** Jede Session erscheint als
  `minds/session/<hash>` mit `session.json` (maßgeblich) und `session.md` (gerendert)
  — GitLab zeigt den Branch damit ohne jeden Reader-Deploy als lesbare Seite.
- **`minds forget <session> [--reason]`** — DSGVO-Löschung: Die Nutzlast wird durch
  einen Tombstone ersetzt, die Hash-Referenz bleibt auflösbar, getilgt wird an allen
  Ablageorten. `why`, `show` und `fsck` bleiben grün und degradieren ehrlich, statt zu
  brechen. Siehe [ADR-0007](docs/adr/0007-forget-redigierbare-nutzlast.md).

**Nachschlagen**

- **`minds why <datei>:<zeile>`** — die Session hinter einer einzelnen Zeile, über
  blame und Trailer aufgelöst.
- **`minds show [<commit>] [--full]`** — Absicht und Attribution hinter einem Commit.
- **`minds blame <datei>`** — Attribution je Zeile, nach Session aggregiert, mit
  Kontext-Abdeckung in Prozent.
- **`minds recap`** und **`minds search <query>`** — die jüngsten Sessions auf einen
  Blick; Absicht, Verlauf und Dateien durchsuchbar.
- **`minds render`** baut eine zustandslose HTML-Seite: Zeile anklicken, Prompt
  dahinter sehen, Gesprächsverlauf und Tool-Aufrufe aufklappbar.
- **`minds fsck`** prüft, ob jeder Trailer auflösbar ist, und meldet Journal-Lücken.

**Kontext-Rückführung**

- **`minds recall <ziel>`**, **`minds brief [<datei>…]`** und
  **`minds distill [--path] [--out]`** geben den erfassten Kontext an den nächsten
  Agenten zurück — als Brief zu einer Zeile, als größenbegrenzter Startblock oder als
  AGENTS.md-Entwurf aus der Repo-Historie. Deterministisch, ohne LLM-Aufruf, ohne
  Tokens; gleiche Eingabe ergibt byte-gleiche Ausgabe. Optional automatisch beim
  Session-Start über `minds enable --recall`. Siehe
  [ADR-0005](docs/adr/0005-kontext-rueckfuehrung.md).

**Identität und Nachweis**

- **Change-Id** als stabile Identität einer logischen Änderung, erzeugt und erhalten
  über `prepare-commit-msg` (Trailer `Minds-Change-Id`). Überlebt Rebase, Squash,
  Amend und Cherry-Pick. Siehe [ADR-0006](docs/adr/0006-change-id.md).
- **Signierte Attribution.** `minds sign <session>` signiert die kanonische
  Attribution per `ssh-sig` (kein Netz, air-gap-tauglich), `minds verify` prüft sie und
  endet bei Manipulation mit einem Rückgabewert ≠ 0. Aus „Agent X, Modell Y schrieb
  diese Zeilen" wird ein Nachweis statt einer Behauptung. Siehe
  [ADR-0008](docs/adr/0008-signierte-attribution.md).
- **`minds audit --export`** bündelt die Provenienz-Kette
  (Change → Session → Attribution → Verdict) als portable JSON-Datei mit den
  kanonischen Payloads und Signaturen — prüfbar ohne dieses Werkzeug. Was das Bundle
  beweist und was nicht, steht in
  [docs/nachweis-leitfaden.md](docs/nachweis-leitfaden.md).

**Review**

- **Reviews als Git-Objekte.** `minds review <subject> --approve|--reject|--needs-work`
  legt ein content-adressiertes, optional signiertes Verdict unter
  `refs/minds/reviews/` ab; `minds reviews <subject>` listet Verdicts und prüft
  Signaturen. Das Verdict hängt an der Change-Id und überlebt damit Rebase, Squash und
  Force-Push. Siehe [ADR-0009](docs/adr/0009-reviews-als-git-objekte.md).
- **Review-Thread.** `minds comment <subject> --on <datei:zeile|turn:n> "<text>"` —
  ein append-only Log content-adressierter Einträge. Zwei Reviewer, die offline
  kommentieren, erzeugen keinen Konflikt, sondern eine Vereinigung.
- **`minds stack`** zeigt die abhängigen Changes ab einer Basis mit ihrem jeweiligen
  Review-Stand.
- **GitLab-Brücke, einweg und idempotent.** `minds gitlab mirror <subject> --mr <nr>`
  spiegelt Verdicts als MR-Note (optional als Approval); `minds gitlab webhook` deutet
  einen MR-Kommentar (`/minds approve|reject|needs-work`) als Verdict — opt-in,
  zustandslos, kein Dienst. Das Token kommt ausschließlich aus der Umgebung.
  Betriebsmodell in [docs/betriebsmodell-gitlab.md](docs/betriebsmodell-gitlab.md).
- **Policy als Binary statt YAML.** `minds fsck --require-review` verlangt für jeden
  agent-geschriebenen Change ein gültiges Verdict und wird sonst rot. Dazu ein
  wiederverwendbarer CI-Include (`ci/minds-review-gate.gitlab-ci.yml`), der nichts tut
  als das Binary aufzurufen.

**Betrieb**

- **`minds sync [--remote]`** schickt Kontext und Reviews in einer Verbindung ans
  Remote — alle fälligen Refs auf einmal, nie mit `--force`. Ohne neue Refs kostet der
  Aufruf keine Verbindung. Führt zusammen, was ein `git fetch` an fremden Verdicts
  mitgebracht hat.
- **`minds metrics [--format prometheus|openmetrics|json]`** projiziert den Store
  on-demand ins Prometheus-Textformat — kein Daemon, kein Doppel-Speichern. Dazu ein
  importierbares Grafana-Dashboard (`dashboards/minds.json`) und ein opt-in
  CI-Include (`ci/minds-metrics.gitlab-ci.yml`).
- **`minds agent-help`** gibt die Kommando-Karte maschinenlesbar als JSON aus — für
  Agents, nicht für Menschen.

### Sicherheit

- **Die Secret-Wall auf dem heißen Pfad ist agent-agnostisch.** Der Datei-Pfad wird
  aus der Union bekannter Feldvarianten gezogen (`file_path`, `notebook_path`, `path`,
  `absolute_path`, `filepath`, …) plus einer Heuristik über den Feldnamen. Fail-closed
  gilt damit für alle Agents, nicht nur für den, dessen Feldnamen wir zuerst kannten.

### Bekannte Einschränkungen

- Die von `minds enable` eingetragenen Git-Hooks rufen `minds` **ohne Pfad** auf und
  fangen jeden Fehlschlag mit `|| true` ab — ein Rekorder darf keinen Commit scheitern
  lassen. Liegt das Binary **nicht im `PATH`**, laufen die Hooks deshalb **still** ins
  Leere: Committen funktioniert weiter, es gibt keine Fehlermeldung, aber auch keine
  Change-Id am Commit und keine erfasste Session. Dasselbe greift, wenn eine
  **veraltete** `minds` im `PATH` liegt — sie bedient die Hooks und schreibt
  gegebenenfalls ein älteres Store-Layout. `minds enable` prüft beides heute noch
  nicht; nachsehen lässt es sich mit `command -v minds` und `minds --version`.
- Die Deutung der **Tool-Ebene ist noch Claude-Code-spezifisch**. Für `gemini`,
  `codex`, `cursor` und `opencode` wird der Prompt erfasst, aber Tool-Aufrufe,
  berührte Dateien und Modell-/Token-Angaben werden noch nicht ausgewertet. Welcher
  Agent als nächstes vollständig unterstützt wird, richtet sich nach dem Bedarf der
  Testgruppe.
- Die **Review-Schicht braucht mindestens zwei Personen auf einem Repo**, um
  überhaupt beansprucht zu werden.
- Der Reader (`minds render`) zeigt Sessions, Dateien und den Gesprächsverlauf;
  **Übersichts-Kacheln und Diagramme fehlen noch**, obwohl `minds metrics` die
  Kennzahlen bereits liefert.
- Das Release enthält **Linux x86_64** (musl, statisch) und — sobald ein Mac-Runner
  registriert ist — **macOS für Apple Silicon und Intel**. **Windows und ARM-Linux
  werden derzeit nicht gebaut**; dort ist der Weg `cargo build --release --bin minds`.
