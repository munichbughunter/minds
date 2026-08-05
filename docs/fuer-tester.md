# Minds — was es ist, und warum es das gibt

*Für die erste Testrunde. Lies das einmal durch, bevor du installierst — es dauert
zehn Minuten und erspart dir die Frage, wozu die ganze Sache eigentlich gut ist.*

---

## 1. Der Satz, um den es geht

**Git weiß, *was* sich geändert hat. Es weiß nicht, *warum*.**

Ein `git log` gibt dir den Diff und eine Commit-Message, die jemand in Eile
geschrieben hat. Was er nicht gibt: die Anweisung, auf die hin die Änderung entstand.
Die drei Ansätze, die vorher verworfen wurden. Die Nebenbedingung, wegen der die
Lösung so merkwürdig aussieht.

Zwanzig Jahre lang war das verschmerzbar. Der Grund saß im Kopf des Autors, und den
Autor konnte man fragen.

## 2. Was sich gerade ändert

Jetzt schreiben Agents den Code. Der Grund sitzt in einer Terminal-Session — und
verdampft, sobald das Fenster zugeht. Niemanden kannst du in sechs Monaten mehr
fragen; die Session existiert nicht mehr.

Gleichzeitig verschiebt sich die Menge. Wenn ein Agent zweitausend Zeilen in zwanzig
Minuten produziert, ist „lies den Diff" kein Verfahren mehr, sondern eine Fiktion.
Das Qualitätstor existiert dann nur noch formal und filtert nichts.

Daraus folgen zwei Lücken, die man getrennt spürt:

**Rückwärts.** „Warum steht diese Zeile hier?" hat keine Antwort mehr. `git blame`
nennt dir einen Commit und einen Zeitstempel — nicht die Absicht.

**Vorwärts.** Kein Agent lernt aus dem letzten. Jede Session fängt bei null an, läuft
in dieselben Sackgassen, macht denselben Fehler, den du letzte Woche schon korrigiert
hast. Du bist der einzige Speicher, den das System hat.

## 3. Was Minds tut

Drei Schritte, im Hintergrund, ohne dass du etwas tun musst:

**Erfassen.** Hooks im Agenten schreiben mit, was passiert — der Prompt, die
Tool-Aufrufe, die berührten Dateien, das Modell. Nicht als Bildschirmmitschnitt,
sondern strukturiert.

**Redigieren.** Bevor irgendetwas gespeichert wird, gehen Secrets und
personenbezogene Daten raus. Fail-closed: Im Zweifel blockiert Minds, statt zu
riskieren.

**Ablegen.** Das Ergebnis landet **in Git selbst** — als content-adressiertes Objekt
neben dem Code, unter `refs/minds/`. Der Commit bekommt einen Trailer, der darauf
zeigt.

Kein Daemon, keine Datenbank, keine Cloud. Ein statisches Binary, eine harte
Abhängigkeit: `git`.

## 4. Was du konkret davon hast

### „Warum steht diese Zeile hier?"

```
minds why src/retry.rs:42
```

Zeile → Commit → Session. Du bekommst den Prompt, der zu dieser Zeile geführt hat,
die Absicht dahinter, und was im selben Zug sonst noch passiert ist. Für den
Überblick über eine ganze Datei: `minds blame <datei>`. Für einen Commit:
`minds show`.

### „Was weiß dieses Repo schon?"

Das ist die Vorwärts-Richtung — der erfasste Kontext geht an den *nächsten* Agenten
zurück:

```
minds recall src/retry.rs      # verdichteter Brief zu dem, was hier schon passiert ist
minds brief                    # größenbegrenzter Startblock für eine neue Session
minds distill --out AGENTS.md  # was die Historie dieses Repos Agenten beigebracht hat
```

Alles deterministisch aus den erfassten Daten — keine LLM-Aufrufe, keine Tokens,
gleiche Eingabe ergibt byte-gleiche Ausgabe. `minds enable --recall` verdrahtet das
optional so, dass jede neue Session den Brief automatisch vorangestellt bekommt.

### „Wer hat das geschrieben — Mensch oder Agent, welches Modell?"

`author` ist in Git ein unsigniertes Freitextfeld. In einer Welt, in der Agents
committen, ist das genau die Grundlage, auf der man **nichts** nachweisen kann. Minds
signiert die Attribution (`ssh-sig`, kein Netz nötig): „Agent X, Modell Y schrieb
diese Zeilen" wird prüfbar statt behauptet — `minds sign`, `minds verify`.

### „Wurde das geprüft, und von wem?"

Der Review — Verdict, Kommentare, Approval — liegt bei Minds ebenfalls im Repo, nicht
in einer Plattform-Datenbank:

```
minds review <change> --approve --sign
minds comment <change> --on src/retry.rs:42 "Der Retry ist unbegrenzt."
minds reviews <change>
minds stack                        # abhängige Changes und ihr Review-Stand
```

Der Verdict hängt an einer **Change-Id**, nicht am Commit-Hash. Er überlebt damit
Rebase, Squash und Force-Push — genau das, woran Reviews sonst verloren gehen. Zwei
Reviewer können offline kommentieren; die Threads vereinigen sich konfliktfrei.

Wer in der GitLab-Oberfläche lebt, spiegelt die Verdicts dorthin
(`minds gitlab mirror`) — einweg, idempotent. Die Quelle der Wahrheit bleibt das
Repo, GitLab wird zur Anzeige.

Und als Tor in der CI: `minds fsck --require-review` wird rot, wenn ein
agent-geschriebener Change kein gültiges Verdict hat. Keine YAML-Logik, nur ein
Binary-Aufruf.

## 5. Die eine Grundentscheidung

Alles wandert **ins Repo**, nichts in eine Plattform-Datenbank oder fremde Cloud.
Jedes neue Artefakt beantwortet zuerst die Frage „geht das als Git-Objekt?".

Praktisch heißt das:

- Du klonst das Repo — das ganze Gedächtnis kommt mit.
- Es funktioniert offline und im Air-Gap.
- Wechselst du die Plattform, kommt die Review-Historie mit. Sie lag nie woanders.
- Self-Hosting ist keine Portierungsarbeit, sondern der Normalfall.
- Es gibt keinen Dienst zu betreiben und nichts pro Kopf zu bezahlen.

Das Muster ist nicht neu. Gerrit hat die Änderungs-Identität ins Repo gelegt, Radicle
und git-bug haben es mit Issues und Reviews getan. Minds wendet es auf das an, was
gerade neu verloren geht: den Grund hinter agent-geschriebenem Code.

## 6. Sicherheit, und was mit DSGVO ist

**Redaction läuft vor dem Speichern, nicht danach.** Ein Secret, das nie in den Store
kommt, muss auch nie gelöscht werden. Die Regeln sind erweiterbar
(`.minds/redact.json`).

**`minds forget <session>` löscht wirklich.** Die Nutzlast wird durch einen Tombstone
ersetzt, die Hash-Referenz bleibt auflösbar. `why`, `show` und `fsck` bleiben grün und
sagen ehrlich „Inhalt auf Antrag entfernt". Reines Git kann das strukturell nicht —
dort ist, was einmal in der History steht, für immer drin.

**Nichts verlässt deine Maschine,** das du nicht selbst pushst. Es gibt keinen
Telemetrie-Kanal und keinen Server, mit dem Minds spricht.

## 7. Was heute geht — und was nicht

Der ehrliche Teil. Ich hätte lieber, du liest es hier als dass du es entdeckst:

| | Stand heute |
|---|---|
| **Claude Code** | vollständig — Prompt, Tool-Aufrufe, Dateien, Modell, Tokens |
| **Gemini, Codex, Cursor, opencode** | der Prompt wird erfasst; Tool- und Datei-Ebene noch **nicht** gedeutet |

Das ist Absicht: lieber **ein** Agent richtig als vier halb. Welcher Agent als
nächstes drankommt, entscheidet ihr — sagt mir, was ihr benutzt.

Zwei weitere Einschränkungen, die du kennen solltest:

- **Reviews brauchen zwei Leute auf einem Repo.** Allein testest du Erfassung,
  `why` und `recall` — die Review-Schicht bleibt dabei kalt.
- **Der Reader (`minds render`) ist bewusst karg.** Eine statische HTML-Seite: Zeile
  anklicken, Prompt dahinter sehen. Übersichts-Kacheln und Diagramme kommen später —
  erst reiche Daten, dann Politur.

## 8. In fünf Minuten drin

```sh
# 1. Installieren  (die konkrete Zeile kommt mit der Einladung)
curl -sSf <release-url>/minds-installer.sh | sh

# 2. Nachsehen, dass minds im PATH liegt — muss einen Pfad ausgeben
command -v minds

# 3. Im Repo scharf schalten — registriert die Hooks, idempotent, fremdschonend
cd dein-repo
minds enable --agent claude-code

# 4. Ganz normal arbeiten. Eine Agent-Session, ein Commit.

# 5. Nachsehen, was Minds behalten hat
minds show                     # die Session hinter dem letzten Commit
minds why <datei>:<zeile>      # die Session hinter einer Zeile
minds recap                    # die letzten Sessions auf einen Blick
```

Schritt 2 ist wichtiger, als er aussieht. Die Hooks, die `minds enable` einträgt, rufen
`minds` **ohne Pfad** auf. Liegt das Binary nicht im PATH, laufen sie ins Leere — und
zwar **still**, weil ein Rekorder niemals einen Commit scheitern lassen darf. Du
merkst dann nichts: Committen geht weiter, nur aufgezeichnet wird nichts, und `minds
show` bliebe leer. Gibt `command -v minds` nichts aus, ergänze in `~/.zshrc` oder
`~/.bashrc` `export PATH="$HOME/.local/bin:$PATH"` und öffne die Shell neu.

**Wenn doch mal nichts ankommt:** Alles, was die Hooks zu melden hatten, steht in
`.git/minds/hook.log` — ein Tippfehler in `.minds/redact.json` etwa bricht die
Erfassung *fail-closed* ab, und ohne die Datei bliebe das unsichtbar. `minds fsck`
sagt dir, ob dort etwas steht, und nennt gleich mit, ob die Hooks am richtigen Ort
liegen und ob sie noch aus einer älteren Version stammen (dann hilft ein erneutes
`minds enable`).

`minds enable` ist idempotent und lässt fremde Konfiguration in Ruhe. Willst du es
wieder los, sag Bescheid — es sind ein paar Einträge in `.claude/settings.json` und
`.git/config`, mehr nicht.

Wenn du wissen willst, was das Werkzeug sonst noch kann: `minds --help` listet alles,
und `minds agent-help` gibt dieselbe Karte maschinenlesbar aus — für den Agenten
selbst.

## 9. Was ich von dir brauche

Drei Fragen. Der Rest ist Bonus:

1. **Hat die Installation ohne Nachfrage funktioniert?** Wenn du mich fragen musstest,
   ist das ein Fehler in der Anleitung, nicht bei dir.
2. **Wann hast du zum ersten Mal `minds why` benutzt, ohne dass jemand dich daran
   erinnert hat?** Das ist der eigentliche Test. Falls die Antwort „nie" ist: auch das
   ist ein verwertbarer Befund, und der wichtigste.
3. **Was hast du gesucht und nicht gefunden?**

Sag es roh. „Fühlt sich komisch an" ist ein Befund; ich frage dann nach. Was mir
nicht hilft, ist „läuft".

## 10. Was als nächstes kommt

- **Der zweite Agent** — Reihenfolge nach eurem Bedarf.
- **Übersicht im Reader** — Kacheln über Sessions, Tokens, Kontext-Abdeckung. Die
  Kennzahlen gibt es schon als `minds metrics` für Grafana; sie brauchen nur eine
  Oberfläche.
- **Struktureller Diff** — der Zeilendiff ist die falsche Einheit für
  agent-geschriebenen Code. Ein Diff über die Struktur statt über Zeilen löst einen
  Großteil der „Konflikte", die in Wahrheit Artefakte des zeilenbasierten Modells
  sind. Das ist das größere Stück und kommt später.

---

*Fragen, Ärger, Ideen: direkt an mich. Ein halber Satz reicht.*
