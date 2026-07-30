# ADR-0005 — Kontext-Rückführung: deterministisch, ehrlich, 0 Tokens

- Status: angenommen
- Datum: 2026-07-28
- Betrifft: `minds-core`, `minds-reader`, `minds-capture`, `minds-cli`
- Ergänzt: ADR-0003 (Hooks statt Transkript-Parsing), ADR-0004 (Import & Store-Index)

## Kontext

Die Vision nennt vier Probleme. `minds show`/`why`/`render` lösen #1 (der MR als
Ratespiel) und #4 (die Audit-Lücke). **Problem #3 — „kein Agent lernt aus dem, was
der letzte gemacht hat" — war offen.** Das Wissen einer Session (funktionierende
Befehle, Sackgassen, berührte Dateien) liegt längst redigiert im Store; es las nur
niemand zurück.

Dritt-Werkzeuge wie *Grain* (auf entire.io) zeigen den Weg: aus der Session-Historie
einen Brief bzw. eine `AGENTS.md` destillieren, die der nächste Agent liest.

## Entscheidung 1: deterministisch statt generiert

Die Rückführung extrahiert **beobachtete Fakten**, sie erfindet keine Prosa. Kein
Modell im Pfad — gleiche Sessions ⇒ byte-gleicher Brief (golden-getestet). Der
optionale LLM-Summary-Pfad bleibt wie geplant für M8 zurückgestellt, dann mit
Content-Hash-Caching über die `SessionId`.

Der reine Kern ist `minds_core::extract` (`Extract::from_sessions`); die
Markdown-Fläche ist `minds_reader::brief`. Beide ohne I/O, beide golden-getestet.

## Entscheidung 2: stark vs. heuristisch wird sichtbar getrennt

Nicht jedes Signal ist gleich verlässlich, und der Brief sagt es:

- **stark**, weil aus dem normalisierten `Effect` gelesen: funktionierende Befehle
  (Exec), Hot-Files, Co-Change-Cluster.
- **heuristisch**, weil aus Mustern bzw. Freitext geraten: Rework (Churn) und
  Korrekturen (Korrektur-Sprache in einem User-Turn) — im Brief ausdrücklich als
  „(heuristisch)" beschriftet.

„Konventionen" als Stilregeln entstehen bewusst **nicht**: die bräuchten den Code
selbst oder ein Modell.

## Entscheidung 3: drei Kommandos nach Blickrichtung

- `minds recall <ziel>` — rückblickend/gezielt: der Brief hinter einer Datei, Zeile
  oder einem Commit. Die Agent-Schwester von `why`.
- `minds distill [--path|--out]` — kumulativ/repo-weit: ein `AGENTS.md`-**Entwurf**.
  Der Merge in eine bestehende Datei bleibt bewusst dem Menschen überlassen (v0.3).
- `minds brief [<datei>...] [--hook]` — vorausschauend/Session-Start:
  größenbegrenzt, damit der Agent-Input klein bleibt (Headroom-Rücksicht).

## Entscheidung 4: die Rückführung an den Agenten ist opt-in

`minds enable --recall` registriert für Claude Code einen SessionStart-Hook, der
`minds brief --hook` ausgibt; dessen `hookSpecificOutput.additionalContext` stellt
Claude der neuen Session voran. **Opt-in**, weil es Agent-Tokens kostet. Der
Envelope-Vertrag ist agent-spezifisch — andere Agents folgen, sobald ihr Format
verifiziert ist.

## Ehrlich zu den Grenzen

- **`intent.discarded`** wird beim Checkpoint deterministisch befüllt — aus dem
  Muster „Datei geschrieben und wieder entfernt". Da der Claude-Adapter **keinen**
  `Delete`-Effekt kennt (Löschen läuft über `Bash rm`), wird die Entfernung auch aus
  `rm`/`git rm`-Kommandos gelesen. Ein künftiger Adapter mit echtem `Delete`-Effekt
  fällt automatisch mit hinein.
- **`intent.constraints`** bleibt leer: dafür gibt es kein verlässliches
  deterministisches Signal. Ein geratener Constraint wäre schlechter als keiner.
- Die Befehls-Entrauschung (`cd …` weg, Pipe-Kopf, Kürzung) ist eine benannte
  Heuristik; sie gruppiert `cargo clippy … | grep x/y` zu **einem** Fakt.

## Konsequenzen

Problem #3 ist adressiert, ohne ein Modell, ohne Netz, ohne neuen Zustand — die
Daten lagen schon im Store. Die Qualität der Rückführung steigt automatisch mit der
Qualität der Erfassung (Track A: mehr Agents mit echten Effekten → reichere Fakten).
