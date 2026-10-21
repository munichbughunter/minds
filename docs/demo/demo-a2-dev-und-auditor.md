# Demo: ein Change von der Anforderung bis zum Audit — Dev-Sicht (A2) und Auditor-Sicht

Stand 2026-10-19, minds 0.4.0, macOS. Zwei Durchläufe auf einem Rechner: zuerst
die Entwicklerin mit geschütztem Witness (A2 witnessed), dann der Auditor in einem
eigenen Clone mit eigener Vertrauensdatei. Alles ohne Netz, die "Forge" ist ein
lokales Bare-Repo.

Die erwarteten Ausgaben sind aus Dokumentation und Golden-Tests übernommen. Wo
die Zahlen vom echten Agentenlauf abhängen, steht *sinngemäß*.

Pfade in dieser Demo:

| | |
|---|---|
| Demo-Projekt (Dev) | `~/dev/minds-demo-a2` |
| "Forge" (Bare-Repo) | `~/dev/minds-demo-a2-remote.git` |
| Auditor-Clone | `~/audit/minds-demo-a2` |
| Vertrauensdatei (Dev, von `minds enable`) | `~/.config/minds/allowed_signers` |
| Intent-Schlüssel (Dev, von `minds enable`) | `~/.config/minds/intent_ed25519` |
| Vertrauensdatei (Auditor) | `~/audit/trust/allowed_signers` |

---

## Teil 0 · Vorbereitung (einmalig, vor der Aufnahme)

### 0.1 Das aktuelle Binary installieren

Der geschützte Witness kopiert das Binary, mit dem `minds enable` läuft, root-eigen
nach `/usr/local/libexec/minds/minds`. Ein Binary aus `target/` lehnt er ab, also
das installierte auf dem PATH aktualisieren:

```sh
cd ~/dev/minds
cargo build --release --bin minds
cargo install --path crates/minds-cli --locked
minds --version          # minds 0.4.0
command -v minds         # /Users/pd/.cargo/bin/minds
```

Ein älterer Witness unter `/usr/local/libexec/minds/minds` wird von
`MINDS_WITNESS=protected minds enable` in Schritt 1.2 ausgetauscht.

Mehr Vorbereitung braucht es nicht: `minds enable` legt in Schritt 1.2 auch
die Vertrauensdatei (`~/.config/minds/allowed_signers`, `git config --global
minds.signers`) und den Intent-Schlüssel (`~/.config/minds/intent_ed25519`,
ohne Passphrase, `git config --global minds.intentKey`) an. Am geschützten
Witness signiert `minds intent sign` abgeschirmt (leere Umgebung, kein
ssh-agent, stdin geschlossen), eine Passphrase kann dort nicht eingegeben
werden; ein FIDO-Key geht auf macOS am geschützten Witness ebenfalls nicht
(Apples `ssh-keygen` braucht einen root-eigenen Provider). Die Freigabe durch
einen Menschen kommt in dieser Konstellation vom `sudo`-Passwort beim
Aktivieren (Schritt 1.4), nicht vom Schlüssel. Wer einen eigenen Schlüssel
(etwa einen FIDO-Key) will, setzt `minds.intentKey` vorher — `enable` legt
dann keinen an.

### 0.2 Alte Witness-Dienste aufräumen (optional, braucht sudo)

Geschützte Witnesses früherer Repos stören die Demo nicht, aber `launchctl
print system` und die Managed Settings sind damit unübersichtlich. Alles auf
einmal, mit einem `sudo`:

```sh
minds disable --all
launchctl print system | grep minds    # nichts mehr
```

Nimmt jeden LaunchDaemon `dev.minds.witness.*`, die Witness-Homes unter
`/private/var/db/minds-witness`, minds' eigene Managed-Settings-Datei (nur
Deny-Regeln) und das root-eigene Binary; `enable` legt in Schritt 1.2 alles
neu an, dann nur mit dem Demo-Repo.

### 0.3 Das Demo-Projekt anlegen

```sh
sh ~/dev/minds/docs/demo/bootstrap-demo-a2.sh
```

Legt `~/dev/minds-demo-a2` an: ein Python-Projekt (uv, pytest als Dev-Abhängigkeit,
`uv.lock` schon committet, damit der Agent später nur `src/` und `tests/`
anfasst), die Anforderung `anforderung/LOG-2417.md` mit sechs
Akzeptanzkriterien, von denen AC5 (stabil) und AC6 (nach Schweregrad) sich
widersprechen, ein Commit `chore: Projektgerüst und Anforderung LOG-2417`, das
Bare-Remote und `~/audit/trust/`.

Der Pfad ist neu: unter `~/.claude/projects/` liegen noch Transkripte der alten
Demo-Repos (`-Users-pd-dev-minds-demo` usw.). `minds enable` importiert
Transkripte, die zum Repo-Pfad passen, im Hintergrund. Ein neuer Pfad verhindert
fremde `imported`-Sessions in der Overview.

### 0.4 Bildschirm

Zwei Terminals, Schrift groß, mindestens 160 Spalten für die TUI (dann hat
Changes drei Spalten: FILES, Diff, WHY THIS LINE).

- **Links "Dev":** `cd ~/dev/minds-demo-a2`, hier laufen `minds enable`, `claude`,
  `git commit`.
- **Rechts "Blick":** dasselbe Verzeichnis, hier läuft `minds inspect` (lädt live
  nach, wenn sich HEAD oder `refs/minds/*` ändern) und später `minds verify`.

---

## Teil 1 · Dev-Sicht: Witness (A2)

### 1.1 Ausgangslage zeigen

Links:

```sh
cd ~/dev/minds-demo-a2
git log --oneline            # e93b1db chore: Projektgerüst und Anforderung LOG-2417
cat anforderung/LOG-2417.md  # AC1–AC6, AC5 und AC6 kollidieren
uv run pytest                # no tests ran
minds doctor
```

Erwartet von `minds doctor` (noch nichts eingerichtet):

```text
fail  agent hooks: ".claude/settings.json" carries no minds registration — no event is captured — `minds enable`
fail  git hooks: post-commit, prepare-commit-msg are missing in ".git/hooks"
ok    store: in-repo, refs/minds/context
warn  witness: not configured — sessions are recorded as A1 (`minds enable` sets up a local witness)
```

**Was man sieht:** ein gewöhnliches Repo, nichts aufgezeichnet.

### 1.2 Repo scharf schalten, Witness geschützt einrichten

Links:

```sh
MINDS_WITNESS=protected minds enable --agent claude-code
```

(Ohne die Variable fragt `enable` auf dem Terminal `Set up the witness protected
(administrator rights, once)? [Y/n]`, Enter heißt ja. Mit der Variable läuft es
ohne Rückfrage und tauscht auch das alte Witness-Binary aus.)

Ablauf und erwartete Zeilen (sinngemäß):

```text
minds: intent key ~/.config/minds/intent_ed25519 created (software key, no passphrase) — minds.intentKey set, its minds-intent line recorded in ~/.config/minds/allowed_signers
minds: setting up the protected witness — it runs as _minds from /usr/local/libexec/minds/minds; you are asked for your password once
  minds.intentKey (~/.config/minds/intent_ed25519) is a software key (or has no security key's .pub next to it) — nothing pinned; intents are activated with your password
Password: ********
minds: protected witness running for this repository (user _minds, socket "/private/var/db/minds-witness/<repo-id>/run/witness.sock")
minds: witness signers line — it vouches that this witness runs separated (an auditor keeps it in a file of their own, outside the repository):
  minds-witness-user@patrick namespaces="minds-witness" ssh-ed25519 AAAA…
minds: Claude Code managed settings installed (/Library/Application Support/ClaudeCode/managed-settings.json) — deny rules for the witness only
minds: witness signers line recorded in ~/.config/minds/allowed_signers
minds: minds.signers set to ~/.config/minds/allowed_signers — minds verify and minds inspect check signatures against it
```

Was das eine `sudo` tut: Binary root-eigen kopieren, Nutzer und Gruppe `_minds`
anlegen (Mitglieder: `_minds` und `pd`), `.git/objects`, `refs/`, `logs/` dieser
Gruppe geben, Witness-Home unter `/private/var/db/minds-witness/<repo-id>`
anlegen, Schlüssel erzeugen, LaunchDaemon `dev.minds.witness.<repo-id>` starten,
Managed Settings mit Deny-Regeln schreiben.

Die Vertrauensdatei des Entwicklers ist damit vollständig — nichts kopieren:

```sh
cat ~/.config/minds/allowed_signers
```

```text
patrick@doering-it namespaces="minds-intent" ssh-ed25519 AAAA…
minds-witness-user@patrick namespaces="minds-witness" ssh-ed25519 AAAA…
```

`minds verify` und `minds inspect` prüfen gegen diese Datei (`minds.signers`),
ohne `--signers`. Der Prinzipal `minds-witness-user@…` ist der Vertrauensanker
für den Auditor (nur so zählt der Witness als getrennt); der Auditor übernimmt
die beiden Zeilen in **seine** Datei (Teil 2), die außerhalb jeder
Agenten-Reichweite liegt — die des Entwicklers kann ein Agent unter dessen
Kennung schreiben.

Dann:

```sh
minds doctor
```

Erwartet:

```text
ok    agent hooks: registered for claude-code
warn  agent config: ".claude/settings.json" untracked — commit it before binding a scope
ok    git hooks: installed in ".git/hooks"
ok    store: in-repo, refs/minds/context
ok    witness: protected, runs as _minds, answers ping
ok    witness home: not readable for you (permission denied)
```

`permission denied` ist hier der Beleg, nicht der Fehler: Der Witness ist vom
Entwickler getrennt.

Die Warnung `agent config` ist die Scope-Falle: `.claude/settings.json` hat
`enable` eben geschrieben; bliebe sie uncommittet, läge sie im Commit des
Agenten — außerhalb des Bereichs `src/**,tests/**`. Also jetzt committen,
vor dem Binden:

```sh
git add .claude/settings.json && git commit -qm "chore: minds hooks"
minds doctor      # ohne die Warnung
```

(Vergisst man es, zeigt `verify` die Datei später als `tooling … (written by
minds enable; commit, claim)` statt `out of scope`, und `--require-in-scope`
scheitert nicht daran — solange der Agent nichts hineinschreibt.)

Beleg, dass der Agent nicht an den Witness kommt (die Zeile aus dem Loom-Drehbuch):

```sh
ls /private/var/db/minds-witness/<repo-id>/        # Permission denied
cat "/Library/Application Support/ClaudeCode/managed-settings.json"   # Deny-Regeln für minds witness, refs/minds, Witness-Home
```

**Was man sieht:** Hooks in `.claude/settings.json` (PreToolUse, PostToolUse,
UserPromptSubmit, Stop, SubagentStop, SessionStart, SessionEnd) und
`.git/hooks`; der Witness läuft unter einem anderen Nutzer; sein Journal und
sein Schlüssel sind für den Entwickler (und damit für den Agenten) unlesbar.

### 1.3 Anforderung binden

Links:

```sh
minds intent bind --file anforderung/LOG-2417.md --scope 'src/**,tests/**'
```

Erwartet (Ids anders):

```text
intent  file:anforderung/LOG-2417.md@<blob>  content b3-…
scope   src/**, tests/**
anchor  b3-<64 hex>
```

Die Datei ist in HEAD, es kommt keine Warnung `requirement not committed`. Der
Anker liegt unter `refs/minds/intents/<id>`, mit dem redigierten Snapshot des
Anforderungstexts und dem erwarteten Scope.

### 1.4 Anforderung freigeben und am Witness aktivieren

Links:

```sh
minds intent sign
```

(Ohne `--key`: der Schlüssel aus `minds.intentKey`, den `enable` angelegt hat.)

Erwartet:

```text
intent  file:anforderung/LOG-2417.md@<blob>  content b3-…
scope   src/**, tests/**
review  anchor b3-<64 hex>
review  content b3-<64 hex>
review  file anforderung/LOG-2417.md blob <40 hex>
signed  patrick@doering-it (minds intent) (ssh-ed25519, software key)
minds: activating the intent at the protected witness as _minds — your password approves it
minds: your password approves activating this intent at the protected witness: ********
active  witness (protected, approved with your password)
```

Vor dem Passwort die `review anchor`-Zeile mit der `anchor`-Zeile aus 1.3
vergleichen: Das ist die Stelle, an der ein Mensch prüft, was er freigibt.

**Was man sieht:** Der Anfang der Kette ist nicht der erste Prompt, sondern diese
Anforderung in genau dieser Version, signiert unter `minds-intent`, und am
Witness aktiv, bevor der Agent startet. (A2 verlangt "intent bound from the
session start".) Die Reihenfolge 1.3 → 1.4 → 1.5 ist zwingend.

Rechts (ab jetzt offen lassen):

```sh
cd ~/dev/minds-demo-a2 && minds inspect
```

Tab `4` Requirements: eine Zeile `✓ <kurz-id> · anforderung/LOG-2417.md (HEAD) ·
src/**,tests/** · 0 sessions`; rechts oben der Anforderungstext, darunter STATUS
`✓ proven — the stored snapshot matches the file at <blob>`, `✓ signed — software
key, checked against your allowed_signers (as minds verify does)`. Space klappt
TECHNICAL auf (Anker, Content-Hash, Quelle in voller Länge).

### 1.5 Der Agent arbeitet, der Witness schaut zu

Links:

```sh
claude
```

Prompt (so oder ähnlich; die zwei Einschränkungen sind wichtig, siehe unten):

```text
Setze LOG-2417 aus anforderung/LOG-2417.md um: sort_lines() in src/logsort/sort.py,
__main__.py für `python -m logsort`, Tests mit pytest unter tests/. Ändere nur
Dateien unter src/ und tests/. Nicht committen — ich committe selbst.
```

- *Nur src/ und tests/*: der gebundene Scope. Eine Änderung an `README.md` oder
  `pyproject.toml` wäre `out of scope` und ließe `--require-in-scope` mit Exit 2
  scheitern. Das ist kein Fehler der Demo, aber ein anderer Film.
- *Nicht committen*: A2 verlangt, dass der Witness die Session beim Checkpoint
  **ruhend** gesehen hat — letztes Event ein `Stop` (der Agent hat geantwortet),
  kein Tool-Aufruf offen. Versiegelt wird heute nur beim Commit. Committet der
  Agent selbst, mitten im Tool-Aufruf, bleibt dieser Commit bei A1
  (`session active at checkpoint (PreToolUse #n open)`), bis ein späterer Commit
  einen ruhenden Bereich versiegelt. Der Mensch committet, wenn der Agent fertig
  ist — die Session darf dabei offen bleiben.

Der Agent liest die Anforderung, stößt auf AC5 gegen AC6, entscheidet (und
begründet im Code), schreibt `src/logsort/sort.py`, `src/logsort/__main__.py`,
`tests/test_sort.py`, lässt `uv run pytest` laufen, vielleicht mehrmals.

Während er arbeitet, laufen die Hook-Events (`UserPromptSubmit`, `PreToolUse`,
`PostToolUse Write …`, `Stop`) über den Socket zum Witness, der sie beim
Eintreffen verkettet, und der Dateibeobachter des Witness sieht jede geschriebene
Datei im Worktree (Pfad und Hash, nie Inhalt), unabhängig davon, was der Agent
meldet. Die Rohdaten liegen im Witness-Home, nicht im Repo, deshalb zeigt die
TUI rechts jetzt noch nichts Neues. Das ist Absicht: Bis zum Seal gibt es keinen
Stand im Store, den irgendwer umschreiben könnte.

Wenn der Agent fertig ist (die Antwort steht, der Prompt wartet), **committen**
— Schritt 1.6, in diesem Terminal oder einem dritten. Die Claude-Code-Session
bleibt offen; `/exit` ist nicht nötig. Das `Stop` am Ende der Antwort ist der
letzte Frame, den der Witness für A2 braucht: Die Session ruht. Was der Agent
später noch tut, läuft in die nächste Epoche, die der nächste Commit versiegelt
(`chain closed (2 epochs)`).

### 1.6 Commit = Checkpoint = Seal

Links:

```sh
git status --short        # nur src/ und tests/
uv run pytest             # grün, zur eigenen Vergewisserung
git add -A
git commit -m "feat(logsort): Logzeilen nach Zeitstempel sortieren (LOG-2417)"
```

Der `prepare-commit-msg`-Hook hängt den Trailer `Minds-Session-Id: b3-…` an, der
`post-commit`-Hook delegiert den Checkpoint an den Witness. Erwartet (sinngemäß):

```text
  SESSION SEALED
    Seal       b3-<64 hex>
    Root       <chain root>
    Events     <n> event(s) · seq 1–<n>
    Scope      witness/v1
    Signature  signed (validity is checked by `minds verify`)
```

Rechts lädt die TUI von selbst nach (`updated HH:MM:SSZ` in der Fußzeile).

**Was man sieht:** `git log -1` zeigt den Trailer; `git for-each-ref refs/minds/`
zeigt `refs/minds/context`, `refs/minds/evidence/<seal>`, `refs/minds/intents/<id>`.
Nichts davon liegt außerhalb von Git.

### 1.7 Der tägliche Blick zurück (CLI)

Links:

```sh
minds show                              # die Session hinter HEAD: Prompt, Agent, Modell, Dateien
minds why src/logsort/sort.py:20        # die Session hinter einer Zeile (git blame → Trailer)
minds blame --lines src/logsort/sort.py # eine Zeile je Quellzeile mit Session-Id
minds recap                             # die letzten Sessions
minds recall src/logsort/sort.py        # der Kontext-Brief für die nächste Agenten-Session, 0 Tokens
```

**Was man sieht:** `why` antwortet auf "warum steht diese Zeile hier" mit Prompt
und Session, nicht nur mit Autor und Datum. `show` nennt das Modell
(`claude-…`), weil der Hook die Modell-Id beim `Stop` an den Witness meldet.

### 1.8 Das Urteil: `minds verify`

Rechts (TUI mit `q` verlassen oder drittes Terminal):

```sh
minds verify --require-assurance A2 --require-explained 100 --require-in-scope
echo "exit $?"
```

Erwartet (sinngemäß, Zahlen aus dem echten Lauf):

```text
Signers        ~/.config/minds/allowed_signers (minds.signers)
Session        b3-…  (commit <sha>)
Seal           b3-…  witness/v1  signed  events 1–<n>
Integrity      intact
Coverage       complete within the boundary (boundary: witness/v1 — activity outside it is not captured · 0 gaps · artifact <n>/<n> lines explained · 0 out of scope)
Interpretation complete
Assurance      A2 witnessed   (minds-witness-user@patrick, profile user; intent signed, software key)
Intent         intent signed, software key  b3-…  chained
Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)
Overall        VERIFIED
exit 0
```

Die drei Achsen getrennt lesen: *Integrity* (wurde am Beleg etwas verändert),
*Coverage* (fehlt etwas, erklärt die Session jede hinzugefügte Zeile des
Commits, blieb alles im Scope), *Interpretation* (konnte jeder Tool-Aufruf
gedeutet werden). *Assurance* ist keine Achse, sondern die Antwort auf "wer
stand dahinter": Witness unter eigenem Nutzer, Profil `user`, Intent von einem
Menschen signiert. Und `Not proven` steht immer dabei; `minds verify --limits`
listet alles.

Wenn stattdessen `A1 observed (…)` erscheint, nennt die Klammer den ersten
Grund; die häufigsten in der Demo:

| Grund | Ursache | Abhilfe |
|---|---|---|
| `session active at checkpoint (PreToolUse #n open)` / `(last event #n is not a stop)` | Commit, während der Agent noch arbeitet (etwa der Agent committet selbst aus einem Tool-Aufruf) | Warten, bis die Antwort steht, nochmal committen (ein zweiter Checkpoint versiegelt den Rest als Epoche 2) |
| `the session's end` unter `Not proven` (`--limits`: `Not that the session has ended …`) | Commit ohne `/exit` — kein Fehler | Nichts; die Session darf offen bleiben, A2 steht |
| `intent not bound` / `intent not proven from session start` | `intent sign` nach dem Start von `claude` | Reihenfolge 1.3 → 1.4 → 1.5 |
| `witness signature not checked — no trusted allowed_signers` | keine Vertrauensdatei (`Signers`-Zeile fehlt) | `minds enable` erneut, oder `--signers` |
| `witness isolation not proven` | Signer-Zeile trägt `minds-witness@` statt `minds-witness-user@` | die Zeile aus der `enable`-Ausgabe nehmen, nicht die eines lokalen Witness |
| Exit 2, `out of scope` | Agent hat README/pyproject angefasst | ohne `--require-in-scope` zeigen und den Befund erklären |

### 1.9 Dasselbe in der TUI

Rechts:

```sh
minds inspect
```

**Tab 1 Overview.** Titel `OVERVIEW · 2 commits · 1 with an agent session`. Oberste
Zeile: `◉CC  feat(logsort): … · ◈ sealed · ⚑ · @main ▲ HEAD · now`, darunter gedimmt
der Prompt. Rechts das Detail:

- REQUIREMENT: Anker, Quelle `anforderung/LOG-2417.md`, Scope; `Space` zeigt den Text.
- PROMPT `◌ as recorded by the agent` (ein Claim, keine Evidenz, deshalb `◌`).
- SESSIONS: `claude-code · <modell>`, Id, Zeit, Dateien, Events, `0 gaps`, `◈ sealed`.
- EVIDENCE: INTEGRITY zunächst `… checking` oder `sealed, not yet verified`; `v`
  drücken → `✓ VERIFIED · A2 witnessed`. COVERAGE `● <n> of <n> added lines
  observed` (vom Witness im Dateisystem gesehen, nicht nur vom Agenten
  gemeldet). INTERPRETATION `✓ all tool calls interpreted`. REQUIREMENT `✓ bound ·
  ✓ proven · ✓ signed`. SCOPE `✓ all changes inside src/**, tests/**`. REVIEW
  `◦ not reviewed yet`. NEXT `→ Enter: how it was made — nothing is open`.

**Enter → Tab 3 Sessions, die Story.** Kopf mit Session, Agent, Modell,
Zeitspanne, Trust Strip `◈ ● ✓`. PROMPT. Dann nummerierte Schritte: `read
anforderung/LOG-2417.md`, `wrote src/logsort/sort.py ● observed`, `tests uv run
pytest ✗ 2 failed`, `edited src/logsort/sort.py`, `tests … ✓ 8 passed`, rechts
jeweils `+m:ss`. Wenn der Agent AC5/AC6 abgewogen hat, erscheinen CONSTRAINTS
oder DISCARDED. `z` faltet Wiederholungen, `3` zeigt den Agententext je Turn als
`◌`-Claim. Unten RESULT: Dateien, Commit.

**`e` → Evidence.** Der Satz für Menschen: *This session's record is intact and
signed, and all <n> events lie inside the witness boundary. The <n> added lines
were observed by a witness. Every tool call was interpreted.* Darunter `Verdict
of minds verify: ✓ VERIFIED (signatures checked …)`, die drei Achsen, LIMITS als
Satz für A2, `TECHNICAL (for auditors) ▸ Space` mit Seal-Id, Chain-Root,
Algorithmus.

**Tab 2 Changes.** FILES links (`● src/logsort/sort.py +… · ● tests/test_sort.py
+…`), Diff in der Mitte, jede hinzugefügte Zeile mit `●` observed am Rand.
Cursor auf eine Zeile, `c` → WHY THIS LINE: `src/logsort/sort.py:20 ● OBSERVED`,
WRITTEN BY `◉CC claude-code · turn 2 · <zeit>` mit Modell, SESSION `b3-… · ◈
record sealed`, REQUIREMENT `⚑ <id> · witness event · in scope src/**`, ASKED FOR
und AGENT SAID als `◌`-Zitate, REVIEW `◦ not reviewed yet`. `n` springt zur
nächsten nicht beobachteten Zeile; die Taste fehlt in der Fußzeile, wenn es keine
gibt. Das ist der Kernsatz der Demo: **Jede Zeile des Commits wurde in der Session
beobachtet, vom Agenten gemeldet und vom Witness im Dateisystem gesehen.**

**Tab 4 Requirements.** Jetzt `✓ <id> · anforderung/LOG-2417.md (HEAD) · src/**,tests/**
· 1 session`; STATUS dritte Zeile `· binding … — the agent named this anchor in a
witness event, not a local file`. SESSIONS (1) mit Prompt; Enter öffnet die Story.

### 1.10 Nach draußen

Links:

```sh
git push
```

Der `pre-push`-Hook ruft `minds sync --detach`: `minds: N ref(s) → origin in the
background`. Alles unter `refs/minds/*` (Context, Evidence, Intents) geht mit,
ohne `--force`. Prüfen:

```sh
git ls-remote ~/dev/minds-demo-a2-remote.git 'refs/minds/*'
```

### 1.11 Optional: ein Mensch ändert nachträglich eine Zeile

Nur wenn Zeit ist; zeigt die Coverage-Achse. Links:

```sh
sed -i '' '20s/$/  # nachträglich/' src/logsort/sort.py
git commit --amend --no-edit -a
minds verify --require-explained 100
echo "exit $?"
```

Erwartet (sinngemäß):

```text
Coverage       incomplete (… · artifact <n-1>/<n> lines explained)
  why            1 unexplained line(s): 1 with no tool claim or shell mention found
  unexplained    src/logsort/sort.py:20  not observed in the session
…
Overall        VERIFIED, INCOMPLETE
exit 2
```

Kein Vorwurf, eine Tatsache: Diese Zeile hat die aufgezeichnete Session nicht
erzeugt. In der TUI: Overview NEXT `→ d: inspect the 1 line not observed`, in
Changes die Zeile mit `◦` in voller Farbe, `n` springt hin. Zurück:

```sh
git reset --hard HEAD@{1}
```

Die Angriffs-Skripte aus dem Loom-Drehbuch (`forge-write-claim.sh`,
`tamper-seal.sh`, EA-22) existieren noch nicht; Fälschungsversuche 5a und 5c
daher nicht zeigen.

---

## Teil 2 · Auditor-Sicht

Rolle: jemand, der dem Entwickler-Rechner nicht traut, nur dem Git-Remote und
einer Vertrauensdatei, die er außerhalb des Repos bekommen hat. Der
Verification Guide sagt: *Run the check where no agent has ever run.* Auf dem
Demo-Rechner ist das nicht gegeben, das gehört gesagt; die Befehle sind
dieselben wie auf einem Auditor-Rechner oder in einem CI-Job.

Der Auditor braucht **kein** `minds enable`, keine Hooks, keinen Witness. Nur das
Binary, `git` und die Vertrauensdatei. Er schreibt nie ins Repo.

### 2.1 Eigener Clone mit den Evidence-Refs

```sh
mkdir -p ~/audit && cd ~/audit
git clone ~/dev/minds-demo-a2-remote.git minds-demo-a2
cd minds-demo-a2
git fetch origin '+refs/minds/*:refs/minds/*'
git for-each-ref refs/minds/
```

`git clone` holt `refs/minds/*` nicht von selbst, deshalb der `fetch`. Erwartet:
`refs/minds/context`, `refs/minds/evidence/<seal>`, `refs/minds/intents/<id>`.

(Der Guide empfiehlt für harte Audits `git clone --mirror --no-local <url>`; für
die TUI und die Reconciliation braucht `verify` aber einen Worktree, deshalb hier
ein normaler Clone plus Fetch.)

### 2.2 Die Vertrauensdatei

Der Auditor führt seine eigene; die zwei Zeilen stammen aus der des
Entwicklers (Schritt 1.2) und kamen außerhalb des Repos zu ihm:

```sh
cp ~/.config/minds/allowed_signers ~/audit/trust/allowed_signers   # in der Demo: dieselbe Maschine
cat ~/audit/trust/allowed_signers
```

Zwei Zeilen, je Rolle genau ein Namespace:

```text
patrick@doering-it namespaces="minds-intent" ssh-ed25519 AAAA…
minds-witness-user@patrick namespaces="minds-witness" ssh-ed25519 AAAA…
```

Erklären: Ein Schlüssel ohne `namespaces=` oder mit mehreren zählt nie. Ein
Entwickler-Commit-Key, den ein Agent über ssh-agent erreicht, kann so nie
Witness oder Freigeber werden. Die Datei kommt nie aus dem Repo.

### 2.3 Das Urteil, mit allen Gates

```sh
minds verify --signers ~/audit/trust/allowed_signers \
  --require-assurance A2 --require-explained 100 --require-in-scope
echo "exit $?"
```

Erwartet: derselbe Block wie in 1.8, `Overall VERIFIED`, `exit 0`. Die Exit-Codes
sind die Schnittstelle zur Pipeline: 0 VERIFIED, 1 TAMPERED, 2 VERIFIED
INCOMPLETE (ein Gate), 3 NOT VERIFIABLE, 4 operativer Fehler.

Dann die Grenzen vollständig:

```sh
minds verify --signers ~/audit/trust/allowed_signers --limits
```

Zeigt alles, was A2 **nicht** beweist: Modellidentität, fachliche Richtigkeit,
Handlungen außerhalb der Beobachtungsgrenze. Das ist der Satz, mit dem die Demo
endet.

Gegenprobe ohne Vertrauensdatei (um zu zeigen, dass die Stufe nicht im Repo
steht — auf dem Auditor-Rechner gibt es kein `minds.signers`; auf der
Demo-Maschine die Datei nennen, die es nicht gibt):

```sh
minds verify --signers /nonexistent
```

Erwartet: `Assurance A1 observed (range 1: witness signature not checked — no
trusted allowed_signers)`. Kein gespeichertes Objekt behauptet eine Stufe;
`verify` rechnet sie jedes Mal aus dem Material und der Vertrauensdatei.

### 2.4 Das Policy-Gate über die ganze Historie

```sh
minds fsck --require-assurance A2 --signers ~/audit/trust/allowed_signers
echo "exit $?"
```

Prüft, dass jeder Trailer auflösbar ist, und verlangt A2 für jeden
agenten-geschriebenen Commit. `--require-review` würde zusätzlich eine
Freigabe verlangen (hier Exit 2, da noch niemand reviewt hat; als Ausblick
nennen).

### 2.5 Die TUI als Lesewerkzeug

```sh
minds inspect
```

(Die TUI prüft gegen `minds.signers`; auf dem Auditor-Rechner zeigt man das
vorher auf die eigene Datei: `git config --global minds.signers
~/audit/trust/allowed_signers`.)

- **Overview:** `v` → EVIDENCE `✓ VERIFIED · A2 witnessed`, SCOPE `✓ all changes
  inside src/**, tests/**`, REQUIREMENT `✓ bound · ✓ proven · ✓ signed`. Die TUI
  ist strikt lesend; sie schreibt nichts in den Clone.
- **Requirements (4):** Der Anforderungstext **zuerst**, dann STATUS mit drei
  Sätzen. Der Auditor liest: Die Anforderung stand fest, bevor der Agent
  anfing; der Agent hat sich auf sie bezogen (witness event); hier ist der
  Beleg. `Space` → TECHNICAL: Anker-Id, Content-Hash, Signatur.
- **Changes (2):** Zeile für Zeile `●` observed; `c` → WHY THIS LINE mit
  REQUIREMENT `in scope`. Die Frage "hat der Agent das geschrieben?" ist je Zeile
  beantwortbar.
- **Sessions (3) → `e`:** TECHNICAL aufklappen: *externally verifiable: seal
  identity and signature*, *chain root reproducible only locally*. Das ist die
  ehrliche Grenze dessen, was der Auditor ohne Witness-Journal nachrechnen kann.

### 2.6 Das Audit-Bündel

```sh
minds audit --export --signers ~/audit/trust/allowed_signers --mode proof --out ~/audit/LOG-2417-audit.json
python3 -c "import json;b=json.load(open('$HOME/audit/LOG-2417-audit.json'));print(b['assurance']);print(*[p['id'] for p in b['proves']],sep='\n');print('--');print(*[p['id'] for p in b['does_not_prove']],sep='\n')"
```

Das Bündel trägt die kanonischen Payloads und Signaturen (Change → Session →
Attribution → Verdict) und nennt unter `proves` / `does_not_prove` die Sätze,
die auf seiner Stufe gelten, mit stabilen Ids. Es ist ohne minds prüfbar: Der
Verification Guide (`docs/verification-guide.md`, Abschnitt *Checking witnessed
evidence by hand*) hat die Rezepte mit nur `git`, `b3sum` und `ssh-keygen -Y
verify`. Für die Demo reicht der Hinweis, dass jede Prüfung dort eine `ok:`-Zeile
druckt und man sie zählt.

### 2.7 Der Schlusssatz

`Not proven` groß zeigen. *Was bleibt unbewiesen: welches Modell wirklich
geantwortet hat, ob die Entscheidung fachlich richtig war, und alles außerhalb
der Beobachtungsgrenze. Der Punkt ist: Der Agent schreibt seine Evidence nicht
selbst. Er wird beobachtet, von etwas, das er nicht anfassen kann. Und was er
behauptet, wird gegengeprüft.*

---

## Anhang

### A · Reihenfolge auf einen Blick

```text
Dev      bootstrap → enable (protected) → doctor → intent bind → intent sign → claude … (Antwort steht)
         → git commit (SESSION SEALED) → show/why/recall → verify (A2) → inspect → git push
Auditor  clone + fetch refs/minds/* → trust file → verify --require-… → fsck → inspect → audit --export
```

### B · Was wo liegt

| Was | Wo | Für pd lesbar |
|---|---|---|
| Hooks des Agenten | `~/dev/minds-demo-a2/.claude/settings.json` | ja (Agent auch) |
| Git-Hooks | `.git/hooks/{prepare-commit-msg,post-commit,pre-push}` | ja |
| Store | `refs/minds/context`, `refs/minds/evidence/*`, `refs/minds/intents/*` | ja, lesend; schreibt der Witness |
| Witness-Home (Journal, Schlüssel, Socket) | `/private/var/db/minds-witness/<repo-id>` | nein (`_minds`) |
| Witness-Binary | `/usr/local/libexec/minds/minds` | lesen ja, schreiben nein (root) |
| Dienst | `/Library/LaunchDaemons/dev.minds.witness.<repo-id>.plist` | lesen |
| Managed Settings | `/Library/Application Support/ClaudeCode/managed-settings.json` | lesen |
| Vertrauensdatei (Dev) | `~/.config/minds/allowed_signers` (`minds.signers`) | ja (Agent auch) |
| Intent-Schlüssel (Dev) | `~/.config/minds/intent_ed25519` (`minds.intentKey`) | ja (Agent auch) |
| Vertrauensdatei (Auditor) | `~/audit/trust/allowed_signers` | ja |

### C · Aufräumen nach der Demo

```sh
cd ~/dev/minds-demo-a2 && minds disable      # Hooks, Dienst, Witness-Home, Managed-Settings-Eintrag; ein sudo
launchctl print system | grep minds          # nichts mehr
rm -rf ~/dev/minds-demo-a2 ~/dev/minds-demo-a2-remote.git ~/audit
```

(`minds disable --all` räumt jeden geschützten Witness des Rechners ab.
Vertrauensdatei und Intent-Schlüssel unter `~/.config/minds` bleiben — sie
gelten für alle Repos.)

Nutzer und Gruppe `_minds` dürfen bleiben; die nächste Demo nutzt sie wieder.
