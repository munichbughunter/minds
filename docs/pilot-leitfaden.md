# Minds — Pilot-Leitfaden

*Für den Piloten beim Partner. Stand: v0.1.3 — der Übergabestand. Was Minds
ist und warum es das gibt, steht in [`fuer-tester.md`](fuer-tester.md); dieses
Dokument regelt den Zuschnitt: was der Pilot prüft, wie du installierst, und
was ausdrücklich nicht dazugehört.*

*English version: [pilot-guide.md](pilot-guide.md)*

---

## 1. Der Zuschnitt

| | |
|---|---|
| **Umfang** | 1–2 Repositories, 3–5 Entwickler, 3–4 Wochen |
| **Agent** | Claude Code — nur dort ist die Tool-Ebene vollständig gedeutet |
| **Plattform** | macOS oder Linux; Windows nur über WSL (es gibt kein natives Windows-Binary) |
| **Version** | fest `v0.1.3` — nicht „latest", damit alle denselben Stand testen |

**Die Leitfrage des Piloten:** *Beantwortet `minds why` nach drei Wochen eine
Frage, die `git blame` nicht beantwortet?* Alles andere ist Beifang.

Vorab gehört die [Datenschutz-Übersicht](datenschutz-uebersicht.md) zur
internen Freigabe — sie ist bewusst eine Seite und nennt die bekannten
Lücken beim Namen.

## 2. Installation — feste Version

```sh
# 1. Installieren — die Version ist gepinnt
MINDS_VERSION=v0.1.3 sh -c \
  'curl -sSfL https://raw.githubusercontent.com/munichbughunter/minds/main/install.sh | sh'

# 2. Nachsehen, dass minds im PATH liegt — muss einen Pfad ausgeben
command -v minds

# 3. Im Repo scharf schalten — idempotent, fremdschonend
cd euer-repo
minds enable --agent claude-code

# Optional: jede neue Claude-Code-Session bekommt den Repo-Kontext vorangestellt
minds enable --agent claude-code --recall
```

Schritt 2 ist wichtiger, als er aussieht: Die Hooks lösen `minds` zuerst
über den bei `enable` gemerkten Ort auf (`git config minds.binary`); der
PATH ist die Rückfallebene. Fällt beides aus, laufen sie **still** ins
Leere — Committen geht weiter, aufgezeichnet wird nichts. Gibt
`command -v minds` nichts aus, ergänze in `~/.zshrc` oder `~/.bashrc`
`export PATH="$HOME/.local/bin:$PATH"` und öffne die Shell neu.

Danach: ganz normal arbeiten. Eine Agent-Session, ein Commit — mehr braucht
es nicht, der Rest passiert im Hintergrund.

## 3. Die Kommandos des Piloten

**Rückwärts — „warum steht das hier?":**

```sh
minds show                    # die Session hinter dem letzten Commit
minds why <datei>:<zeile>     # die Session hinter einer Zeile
minds blame <datei>           # welche Session hinter welchen Zeilen steht
```

**Überblick und Suche:**

```sh
minds recap                   # die letzten Sessions auf einen Blick
minds search <begriff>        # Volltextsuche über Prompts und Sessions
minds render                  # statische HTML-Ansicht nach ./site
```

**Betrieb und Löschung:**

```sh
minds fsck                    # benennt jeden Zustand, der die Erfassung stört
minds forget <session>        # DSGVO-Löschung — Grenzen: Datenschutz-Übersicht, Abschnitt 6
```

**Wenn das Pilot-Repo auf GitLab liegt**, zusätzlich die Review-Schicht:

```sh
minds review <change> --approve --summary "…"   # Verdict als Git-Objekt
minds reviews <change>                          # Review-Stand eines Change
minds gitlab mirror <change> --mr <nr>          # Verdicts als MR-Notiz spiegeln (einweg, idempotent)
```

Für `gitlab mirror` gehört der Token in eine Umgebungsvariable
(`MINDS_GITLAB_TOKEN`), nie auf die Kommandozeile; URL und Projekt kommen
aus `--url`/`--project` oder aus `git config minds.gitlabUrl` /
`minds.gitlabProject`.

## 4. Was nicht Teil des Piloten ist

Bewusste Entscheidungen, keine Lücken im Testplan:

- **Die Gegenrichtung GitLab → Repo** (`minds gitlab webhook`). Das Kommando
  ist im Binary enthalten (Default: Dry-Run), hat aber noch keine
  Token-Verifikation — mit `--write` könnte eine beliebige Nutzlast ein
  Review-Objekt erzeugen. Im Piloten nicht verwenden; es gibt keinen
  Dienst, der es aufruft.
- **Das CI-Review-Gate** (`fsck --require-review` als Pipeline-Tor). Das
  Flag existiert, aber als Pipeline-Tor wird es erst empfohlen, wenn
  Exit-Codes und Fehlerketten belastbar sind.
- **`minds sync` zwischen mehreren Maschinen** als eigenes Szenario.
- **Andere Agents** (Gemini, Codex, Cursor, opencode): Der Prompt wird
  erfasst, die Tool- und Datei-Ebene noch nicht gedeutet. Der Pilot läuft
  mit Claude Code.
- **Multi-Agent-Szenarien.**

## 5. Wenn etwas nicht ankommt

Der wichtigste Grundsatz: Ein Rekorder darf nie einen Commit scheitern
lassen. Ausfälle sind deshalb still — aber nicht unsichtbar:

1. `minds fsck` — sagt, ob Hooks am richtigen Ort liegen, ob sie aus einer
   älteren Version stammen und ob im Log etwas steht.
2. `.git/minds/hook.log` — dort landet alles, was die Hooks zu melden
   hatten (z. B. eine kaputte `.minds/redact.json`, die die Erfassung
   fail-closed stoppt).
3. `command -v minds` — der Klassiker, siehe Abschnitt 2.

## 6. Bekannte Einschränkungen des Übergabestands

Die ehrliche Liste — lies sie als „gilt heute", sie ist Teil der Übergabe:

- **Verlinkte Git-Worktrees:** Die Erfassung stimmt dort, aber `minds show`
  und `minds why` zeigen den Commit des Hauptbaums
  ([#20](https://github.com/munichbughunter/minds/issues/20)). Im
  Haupt-Checkout arbeiten, bis das behoben ist.
- **Kein natives Windows-Binary** — Windows heißt WSL.
- **Tool-Ebene nur für Claude Code** (siehe Abschnitt 4).
- **Die Review-Schicht braucht zwei Personen auf einem Repo** — allein
  bleiben Erfassung, `why` und `recall` testbar, Reviews nicht.
- **`forget` und bereits gepushte Sessions:** Die Löschung zieht der
  nächste Push per gezieltem Force-Push nach
  ([#102](https://github.com/munichbughunter/minds/issues/102)); was die
  Forge an unerreichbaren Objekten behält, steht in der
  Datenschutz-Übersicht.
- **Ein Push mit neuen Sessions öffnet zwei Verbindungen**
  ([#85](https://github.com/munichbughunter/minds/issues/85)) — gegen ein
  entferntes Remote spürbar, ohne neue Sessions kostet der Hook nichts.
- **Kein Self-Update:** Versionswechsel laufen über `install.sh` mit
  `MINDS_VERSION`.

## 7. Rückkanal

- **Reproduzierbare Befunde ohne vertrauliche Inhalte:** als Issue im
  öffentlichen Tracker — so roh wie möglich, „fühlt sich komisch an" ist ein
  gültiger Befund.
- **Alles mit Session-Inhalten, Repo-Namen oder Kundenbezug:** an den
  benannten Ansprechpartner aus der Einladung, nie in ein öffentliches
  Issue.
- Die drei Fragen, deren Antworten den Piloten auswerten (aus
  [`fuer-tester.md`](fuer-tester.md)): Lief die Installation ohne Nachfrage?
  Wann kam das erste unaufgeforderte `minds why`? Was hast du gesucht und
  nicht gefunden?
