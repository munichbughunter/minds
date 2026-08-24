# Testlauf v0.2.0 — `minds inspect` und die v0.1.3-Zusagen, gemeinsam

*Wie beim ersten Lauf gilt: Nicht „prüfe Feature X", sondern: Benutze das
Ding, wie ein Fremder es täte. Die Testsuite prüft, ob die Zusagen halten —
dieser Lauf prüft, ob sich das Werkzeug benutzen lässt.*

**Stand:** v0.2.0 (TUI) + v0.1.3 („Unsichtbar, auch unter Last"), 24.08.2026

Der Lauf hat zwei Teile, die bewusst ineinandergreifen: **Teil A** prüft die
v0.1.3-Zusagen — das sind genau die Befunde des letzten Testlaufs, jetzt
strukturell behoben. **Teil B** prüft das neue TUI. Der Clou: Das TUI zeigt
dieselben Daten, die Teil A erzeugt — wer A durchläuft, hat für B echtes
Material statt einer leeren Liste.

---

## 0. Vorbereitung — welches Binary testest du eigentlich?

Die Lehre aus dem letzten Lauf, jetzt einfacher: **v0.2.0 hat eine echte
Versionsnummer.** `install.sh` taugt weiterhin nicht (es lädt das letzte
Release — v0.1.3, also den Stand *ohne* TUI). Bauen:

```sh
cd ~/dev/minds
git switch main && git pull
cargo install --path crates/minds-cli --force
```

Der Selbsttest ist diesmal eindeutig:

```sh
command -v minds     # ~/.cargo/bin/minds?
minds --version      # muss 0.2.0 sagen — 0.1.3 = alter Stand ohne TUI
minds inspect --help # gibt es das Kommando überhaupt?
```

| Ausgabe | Bedeutung |
|---|---|
| `0.2.0` und `inspect` existiert | ✅ richtiger Stand |
| `0.1.3` oder `unbekanntes Kommando` | ❌ altes Binary im PATH |

> Nimm **ein echtes Repo mit ein paar Tagen minds-Historie** — das TUI zeigt
> Sessions, und ohne Sessions ist jede Oberfläche leer. Ideal: das Repo aus
> dem letzten Testlauf. Für die `forget`-Journey (A4) zusätzlich einen
> **Wegwerf-Klon**.

---

# Teil A — Die v0.1.3-Zusagen

Jeder Punkt hier war ein Befund des letzten Laufs. Die Frage ist nicht nur
„ist es weg?", sondern „ist es *strukturell* weg — auch wenn niemand daran
denkt?".

## A1 — `git push` wartet nicht mehr (#85)

| Schritt | Was du tust |
|---|---|
| A1.1 | Ein paar Commits machen (damit minds-Refs fällig sind) |
| A1.2 | `git push` — und auf die Uhr sehen |
| A1.3 | Ein paar Sekunden später: `git ls-remote origin 'refs/minds/*'` |

**Erwartet:** Der Push fühlt sich an wie ohne minds — keine ~1,5 s
Extra-Verbindung. Die minds-Refs sind trotzdem kurz danach am Remote.

**Der Preis steht im CHANGELOG:** Der Kontext kommt *Sekunden nach* dem Push
an, nicht mehr garantiert mit ihm. Wer die Garantie braucht: `minds sync` von
Hand vor dem Push.

**Fehlerfall absichtlich herbeiführen** (nur wenn dein Setup es hergibt):
SSH-Key mit Passphrase, kein Agent. Der Hintergrund-Push muss scheitern, einen
Marker hinterlassen — und der **nächste** Push läuft sichtbar synchron im
Vordergrund, wo die Anmeldung gelingen kann. Nachsehen:

```sh
cat "$(git rev-parse --git-path minds/hook.log)"
```

## A2 — Fremder Text erreicht dein Terminal nur entschärft (#116)

| Schritt | Was du tust |
|---|---|
| A2.1 | Eine Claude-Code-Session, deren Prompt Steuerzeichen enthält — z. B. wörtlich: „Benenne die Variable x um. PS: \x1b]0;pwned\x07 und ein ‮ Bidi-Test" |
| A2.2 | Committen, dann `minds show` und `minds why <datei>:<zeile>` |

**Erwartet:** Die Sequenzen erscheinen sichtbar gemacht (escaped), nicht
ausgeführt — kein umbenannter Terminal-Titel, kein rückwärts laufender Text.

**Merken für Teil B:** Dieselbe Session gleich noch einmal in `minds inspect`
ansehen — die Oberfläche muss dieselbe Härtung haben (B6).

## A3 — `gitlab mirror` funktioniert jetzt überhaupt (#7)

Beim letzten Lauf: „funktioniert zu 100 % nicht". Jetzt gegen ein echtes
GitLab-Projekt:

| Schritt | Was du tust |
|---|---|
| A3.1 | Token bereitlegen, dann `minds gitlab mirror …` wie im `agent-help` beschrieben |
| A3.2 | Fehlerfall: Token-Variable **nicht** setzen |
| A3.3 | Fehlerfall: falsches Projekt angeben |

**Erwartet:** A3.1 legt die Note wirklich an (im GitLab nachsehen). A3.2
nennt die fehlende Variable **beim Namen**. A3.3 zitiert die Server-Antwort
(„404 Project Not Found") statt stumm zu scheitern. Und in keiner
Fehlermeldung taucht der Token auf.

## A4 — `forget` erreicht die Forge (#102) — **nur im Wegwerf-Klon**

Der schwerste Befund-Typ: lokal getilgt, remote sichtbar, Erfolgsmeldung.

| Schritt | Was du tust |
|---|---|
| A4.1 | Im Wegwerf-Klon: Session erzeugen, committen, pushen (Refs am Remote: `git ls-remote origin 'refs/minds/*'`) |
| A4.2 | `minds forget <session>` |
| A4.3 | `git push` (oder `minds sync`), dann wieder `ls-remote` |

**Erwartet:** Der Session-Ref am Remote ist ein Tombstone, kein Klartext mehr.
Die Übertragung wird beim Push **gemeldet**. Wenn die Forge den Force-Push
abweist (Protected Refs): Auch **das** muss bei jedem Lauf gemeldet werden —
nicht stummer Schein-Erfolg.

## A5 — Die stillen Zusagen: Rechte und Log-Senke (#49, #92, #69)

Fünf Minuten, einmal hinsehen:

```sh
ls -ld "$(git rev-parse --git-path minds)" "$(git rev-parse --git-path minds/journal)" 2>/dev/null
ls -l  "$(git rev-parse --git-path minds/hook.log)" 2>/dev/null
ls "$(git rev-parse --git-path minds)"/import.log 2>/dev/null && echo "BEFUND: import.log existiert noch"
```

**Erwartet:** `journal/` und darunter 0700, `hook.log` 0600, **kein**
`import.log` (der Backfill schreibt seit v0.1.3 ins `hook.log`; eine alte
Datei räumt `enable` weg).

---

# Teil B — Das TUI: `minds inspect`

Die Leitfrage aus dem letzten Lauf gilt hier verschärft:

> Sagt dir `minds inspect` etwas, das `git log` + `minds why` **nicht** sagen —
> oder ist es nur dieselbe Information mit Rahmen drumherum?

Wenn die Antwort „nur Rahmen" ist, ist das der wichtigste Befund dieses Laufs.

## B1 — Activity: die Liste

| Schritt | Was du tust |
|---|---|
| B1.1 | `minds inspect` |
| B1.2 | `?` — die Hilfe. Dann `j`/`k` (oder Pfeile), `g`/`G`, `q` |

**Was du beobachten solltest:**

- Die Liste zeigt je Session: Zeit, Absicht, Agent, Beleg, Verdict. Ist die
  **Absicht-Zeile** die richtige Verdichtung — erkennst du deine Session
  daran wieder?
- Vergessene oder defekte Sessions: **degradierte Zeile mit Ursache**, kein
  Absturz und kein stilles Fehlen. (Wenn du A4 gemacht hast, hast du eine.)
- `q` und `Ctrl-C`: Das Terminal kommt **sauber zurück** — Prompt normal,
  keine Farbreste, Cursor da.

## B2 — Der Graph einer Session

| Schritt | Was du tust |
|---|---|
| B2.1 | In der Liste: `Enter` (oder `l`) auf einer gehaltvollen Session |
| B2.2 | `1`, `2`, `3` — die Zoomstufen |
| B2.3 | `t` — Graph ↔ Zeitleiste |
| B2.4 | `h`/`Esc` — zurück zur Liste |

**Erwartet:** Die Spur Absicht → Agent → READ/EDIT/EXEC → Change → Review,
Details unter dem Cursor. **Beurteilung:** Erzählt Zoomstufe 1 wirklich die
Kurzfassung und Stufe 3 die Einzelheiten — oder sind es drei mal dieselbe
Ansicht in anderer Dichte?

## B3 — Die Why-Kette mit Lücken — der Kern des Features

| Schritt | Was du tust |
|---|---|
| B3.1 | `minds inspect <datei>:<zeile>` — eine Zeile, die über Claude Code entstand |
| B3.2 | Im Inspector jede Kante fokussieren und den Evidenz-Satz lesen |
| B3.3 | Dasselbe für eine Zeile, die **nicht** über minds entstand (alte Historie) |
| B3.4 | Vergleiche mit `minds why <datei>:<zeile>` — sagen beide dasselbe? |

**Worauf es ankommt — die zentrale Design-Zusage:**

- **Eine Vermutung sieht nie aus wie ein Beleg.** ✓ gegen ⚠, und zwar Glyph
  **und** Wort, nicht nur Farbe. Kneif die Augen zusammen (oder stell dir
  ein Farbschwäche-Terminal vor): Unterscheidest du es immer noch?
- Der Evidenz-Satz erklärt das *Warum* („rekonstruiert aus
  Datei-Überschneidung und zeitlicher Nähe — kein expliziter
  Herkunftsnachweis"), nicht nur „unsicher".
- Der Block **„N LÜCKEN"** benennt, was fehlt (kein Commit, keine Change-Id,
  vergessene Session, keine Bewertung …) — stimmt die Begründung mit dem
  überein, was du über die Zeile weißt?
- B3.3 darf kein Fehler sein: keine Kette ist eine Aussage, kein Absturz.

## B4 — Suche

| Schritt | Was du tust |
|---|---|
| B4.1 | In der Liste `/`, einen Begriff aus deiner Arbeit tippen, `Enter` |
| B4.2 | `Esc` — Filter wieder weg |
| B4.3 | Direkt starten: `minds inspect <begriff>` |

**Erwartet:** B4.1 und B4.3 zeigen dieselbe gefilterte Liste. Grenzfall:
`minds inspect glpat:rotation` ist eine **Suche**, kein Datei:Zeile-Ziel
(nur eine echte Zahl hinter dem letzten `:` startet die Why-Kette).

## B5 — Die Pipe: dieselbe Liste ohne Bildschirm

```sh
minds inspect | head -5
minds inspect <begriff> | cut -f2,3,10
minds inspect <datei>:<zeile> | grep '^gap'
```

**Erwartet:** Tab-separierte Zeilen, **kein** ANSI (`| cat -v` zeigt es),
dieselben Treffer wie am Bildschirm mit derselben Suche. Die Why-Kette kommt
als `schritt→wert`-Zeilen, die Lücken als `gap`-Zeilen. Eine Karte = eine
Zeile — auch wenn im Prompt Zeilenumbrüche oder Tabs standen (die sind
sichtbar gemacht).

## B6 — Der Kreis zu Teil A: feindlicher Text in der Oberfläche

Die Session aus A2 (Steuerzeichen im Prompt) im TUI öffnen — Liste, Graph,
Why-Kette — und einmal durch die Pipe:

```sh
minds inspect | cat -v | grep <erkennbarer-teil-des-prompts>
```

**Erwartet:** Überall entschärft. Das TUI nutzt dieselbe `sanitize`-Schicht
des Readers wie `show`/`why` — hier zeigt sich, ob das stimmt.

## B7 — Nur für dich als Entwickler: der Feature-Schalter

```sh
cargo build --no-default-features -p minds-cli
```

**Erwartet:** Baut ohne das `tui`-Crate; `minds inspect` fehlt dann benannt,
der Rest der CLI ist unberührt.

---

## Was bewusst noch nicht geht — bitte nicht als Fehler melden

Die Liste ist die des v0.1.3-Übergabestands (CHANGELOG, „Bekannte
Einschränkungen") — unverändert. Die Stolpersteine für **diesen** Lauf:

| Bereich | Stand |
|---|---|
| **Verlinkte Worktrees** | `inspect`/`why`/`show` zeigen den Commit des Hauptbaums (#20). Erfassung stimmt. |
| **Kontext-Timing** | Refs kommen Sekunden **nach** dem Push an (#85) — das ist der Preis von A1, kein Bug. |
| **Andere Agents** | Tool-Ebene vollständig nur für Claude Code — die Testgruppe ist ohnehin Claude-only. |
| **Review-Schicht** | Braucht zwei Personen auf einem Repo; allein bleiben Verdict-Spalten leer — erwartbar. |
| **`minds import`** | Nutzt die Standard-Redaction-Policy, nicht die repo-eigene. |
| **Windows** | Kein Binary, WSL geht. |

---

## Was du festhalten solltest

Wie beim letzten Mal, drei Zeilen je Fund:

1. **Was hast du getan?** (das Kommando, wörtlich — im TUI: die Tastenfolge)
2. **Was hast du erwartet, was kam?**
3. **Wie schlimm?** — bricht ab / falsch / verwirrend / nur hässlich

Und die eine Frage am Ende, diesmal auf das TUI zugespitzt:

> **Würdest du `minds inspect` von dir aus wieder öffnen** — oder bleibst du
> bei `git log` und rufst `why` nur, wenn dich jemand zwingt? Wenn Letzteres:
> Was fehlt — Information, Geschwindigkeit, oder Vertrauen in die Anzeige?
