# Testlauf vor der Übergabe

*Nicht „prüfe Feature X", sondern: Benutze das Ding, wie ein Fremder es täte.
Die Testsuite prüft, ob die Zusagen halten. Dieser Lauf prüft, ob sich das
Werkzeug benutzen lässt — das kann sie nicht.*

**Stand:** v0.1.1 vollständig (07.08.2026)

---

## 0. Vorbereitung — der wichtigste Schritt

**Prüfe zuerst, welches Binary du eigentlich testest.** Du hast eine globale
`minds` in `~/.cargo/bin`. Liegt die im PATH, testest du womöglich einen alten
Stand und hältst dessen Fehler für neue.

```sh
command -v minds        # welches wird gefunden?
minds --version
```

**`install.sh` taugt hier nicht** — es lädt fertige Release-Binaries, und das
letzte Release ist v0.1.0, also der Stand *vor* allem, was zu testen ist. Bauen:

```sh
cd ~/dev/minds
git switch main && git pull
cargo install --path crates/minds-cli --force
```

`cargo install` statt `cargo build` + PATH, aus einem konkreten Grund: Das
Binary landet unter `~/.cargo/bin/minds`, also an einem **stabilen Pfad**.
`minds enable` merkt sich genau diesen Ort in `.git/config` (`minds.binary`,
#25) — baust du ins `target/` und räumst es später auf, zeigt der Eintrag ins
Leere. `--force` überschreibt die bestehende globale `minds`; danach ist
`command -v minds` eindeutig.

### Der Selbsttest: habe ich wirklich den neuen Stand?

**`minds --version` hilft nicht** — die Crate-Version steht seit dem Release
unverändert auf `0.1.0`. Verlässlich ist nur ein Verhalten, das es vorher nicht
gab:

```sh
minds fsck --gibtsnicht
```

| Ausgabe | Bedeutung |
|---|---|
| `unbekanntes Flag …`, Exit ≠ 0 | ✅ neuer Stand |
| läuft durch, Exit 0 | ❌ noch der alte — das *ist* der Bug aus #11 |

Zweiter Beleg: `minds agent-help` listet **25** Kommandos (alt: 17).

### In Repos, die minds schon kannten

Die Hook-**Rümpfe** stehen in den Hook-Dateien, nicht im Binary — ein Update
allein ersetzt sie nicht:

```sh
minds enable --agent claude-code --recall
```

`minds fsck` sagt von sich aus, wenn ein Block aus einer älteren Version stammt.

> Nimm **ein echtes Repo**, nicht `/tmp/test`. Ein Projekt, an dem du
> tatsächlich arbeitest — nur so merkst du, ob die Ausgaben etwas sagen.
> Am besten eins, das du zur Not wegwerfen kannst (frischer Clone).

---

## Journey 1 — Ich installiere mir minds und richte ein Repo ein

| Schritt | Was du tust |
|---|---|
| 1.1 | `cd <dein-repo>` |
| 1.2 | `minds enable --agent claude-code --recall` |

**Was du beobachten solltest:**

- **Fast nichts.** Der Normalfall ist still — eine einzige Zeile oder gar keine.
  Kommen mehrere `Hinweis:`-Zeilen, ist das ein Befund: Entweder sagt dir minds
  etwas Wichtiges (dann prüfe, ob es stimmt), oder es ist zu geschwätzig.
- Falls dein Repo **husky/lefthook** benutzt: Es sollte einen Hinweis geben,
  wohin die Hooks gehen. Ohne diesen Hinweis wüsstest du nicht, dass sie
  woanders liegen.

**Wie du prüfst, dass es wirklich passiert ist:**

```sh
minds fsck
```

Erwartet: `Hooks: installiert in …`, `Agents: registriert für claude-code`,
Schlusszeile `fsck: in Ordnung`.

```sh
git config --local --get-regexp '^minds\.'   # backend, contextRef, binary
ls -l "$(git rev-parse --git-path hooks)"    # post-commit, prepare-commit-msg, pre-push, alle mit x
```

**Die entscheidende Frage an dich selbst:** Hättest du nach dieser Ausgabe
gewusst, dass es funktioniert hat? Oder musstest du `fsck` aufrufen, um es zu
glauben?

---

## Journey 2 — Ich arbeite ganz normal und committe

Das ist der Kern. Wenn hier etwas nicht ankommt, ist alles andere egal.

| Schritt | Was du tust |
|---|---|
| 2.1 | Mit Claude Code eine echte Änderung machen (kein „hello world" — etwas, wo der *Grund* interessant ist) |
| 2.2 | `git add …` und `git commit` — **im Terminal** |
| 2.3 | `git log -1` |

**Was du beobachten solltest:**

- Der Commit läuft normal durch. **Keine** zusätzliche Ausgabe von minds, keine
  Verzögerung, die auffällt.
- In der Commit-Message stehen jetzt zwei Trailer: `Minds-Change-Id:` und
  `Minds-Session-Id:`.

**Wie du prüfst:**

```sh
minds show                    # zeigt es den Prompt, der die Änderung ausgelöst hat?
minds why <datei>:<zeile>      # der Magic Moment — nimm eine Zeile, die du gerade geschrieben hast
minds recap                    # die letzten Sessions
```

**Worauf es hier wirklich ankommt** — und das ist eine Beurteilung, keine
Prüfung:

> Sagt `minds why` dir etwas, das `git blame` **nicht** sagt?

Wenn die Antwort „nicht wirklich" ist, ist das der wichtigste Befund des ganzen
Testlaufs — wichtiger als jeder Bug. Notiere, woran es lag: zu wenig Kontext im
Prompt? Falsche Session verknüpft? Zu viel Rauschen?

```sh
minds fsck    # nach dem Commit: keine Waisen, keine Journal-Lücken?
```

---

## Journey 3 — Ich committe aus der GUI

**Das ist der Test für #25** und war das größte Einzelrisiko der Übergabe. Vor
dem Fix bekam ein GUI-Commit *nie* einen Checkpoint — lautlos.

| Schritt | Was du tust |
|---|---|
| 3.1 | Änderung machen, aber **nicht** im Terminal committen |
| 3.2 | Commit aus **VS Code** (Source Control), Fork, Tower — was du benutzt |
| 3.3 | `git log -1 --format=%B` |

**Erwartet:** Dieselben Trailer wie in Journey 2.

**Wenn sie fehlen:** Sieh sofort ins Log — dort steht der Grund:

```sh
cat "$(git rev-parse --git-path minds/hook.log)"
```

Das ist selbst schon ein Test: Vor #10 stand dort *nichts*, egal was schiefging.

---

## Journey 4 — Bei mir kommt nichts an

Der Fall, den du als Erstes gemeldet bekommst. Mach ihn absichtlich kaputt und
sieh nach, ob minds es dir sagt.

### 4a — Der Hook verliert sein Execute-Bit (#52)

```sh
chmod -x "$(git rev-parse --git-path hooks)/post-commit"
minds fsck
```

**Erwartet:** `… ist nicht ausführbar` + `Git überspringt ihn stillschweigend`.
Vor dem Fix meldete `fsck` hier „installiert".

```sh
minds enable --agent claude-code
```

**Erwartet:** Eine sichtbare Zeile — nicht stillschweigend repariert. `chmod -x`
ist auch der Weg, einen Hook *absichtlich* stillzulegen.

### 4b — Ein Kollege hat etwas in die Konfiguration eingecheckt (#78)

Der Fall, der in einem Team-Repo bei *jedem* auftritt, der klont:

```sh
# Simuliere einen fremden Eintrag:
python3 - <<'EOF'
import json, pathlib
p = pathlib.Path(".claude/settings.json")
d = json.loads(p.read_text())
d.setdefault("hooks", {}).setdefault("Stop", []).insert(0,
    {"hooks": [{"type": "command", "command": 'echo "minds hook ist nett"'}]})
p.write_text(json.dumps(d, indent=2))
EOF

minds fsck
```

**Erwartet vor `enable`:** Wenn du minds vorher schon eingerichtet hattest,
bleibt alles grün (der echte Eintrag ist ja da). Interessanter ist der Fall
*ohne* vorherige Registrierung — lösche dafür deine eigenen Einträge und lass
nur den fremden stehen. Dann muss `fsck` sagen:
`trägt keine minds-Registrierung — kein Event wird erfasst`.

```sh
minds enable --agent claude-code
```

**Erwartet:** Der fremde Eintrag bleibt **wortgleich** stehen, der echte
entsteht daneben. Prüfe das wirklich nach — hier ging es früher schief.

### 4c — Der Hook ist weg

```sh
rm "$(git rev-parse --git-path hooks)/post-commit"
minds fsck        # muss ihn vermissen
minds enable --agent claude-code
minds fsck        # wieder grün
```

---

## Journey 5 — Ich arbeite in einem Worktree (#21)

```sh
git worktree add ../mein-zweig
cd ../mein-zweig
minds enable --agent claude-code
```

**Erwartet:** Ein Hinweis, dass die Hooks für **alle** Arbeitsbäume gelten. Dann
normal arbeiten und committen:

```sh
git log -1 --format=%B     # Trailer da?
minds fsck                 # in Ordnung?
```

> **Bekannte Grenze:** `minds show` und `minds why` zeigen hier den Commit des
> **Hauptbaums**, nicht deinen. Das ist [#20](https://github.com/munichbughunter/minds/issues/20)
> und steht so im CHANGELOG. Erfassung stimmt, Nachschlagen nicht. **Kein neuer
> Befund** — aber sieh es dir an und entscheide, ob das einem Tester zumutbar
> ist oder ob #20 vor die Übergabe muss.

---

## Journey 6 — Ich sehe mir an, was minds über mein Repo weiß

Nach ein paar Tagen echter Arbeit — das ist der Test, der Zeit braucht.

```sh
minds recap --limit 20
minds search "<ein Begriff aus deiner Arbeit>"
minds blame <eine Datei, an der du länger gearbeitet hast>
minds brief                # was ein Agent beim Start bekäme
minds render --out /tmp/site && open /tmp/site/index.html
```

**Worauf du achten solltest:**

- Ist `minds brief` **nützlich** oder nur lang? Es kostet den Agenten Tokens.
- Zeigt `minds blame` eine plausible Abdeckung, oder sind große Teile ohne
  Kontext?
- Ist die HTML-Seite etwas, das du jemandem zeigen würdest?

---

## Journey 7 — Der Kollege klont das Repo

Wenn dein Repo `.claude/settings.json` eincheckt (viele tun das):

```sh
cd /tmp && git clone <dein-repo> klon && cd klon
minds fsck
```

**Erwartet:** Die Agent-Registrierung ist da (sie ist eingecheckt), aber
`minds.binary` fehlt und die Hooks fehlen — die liegen in `.git`, das reist
nicht mit. `fsck` sollte beides benennen.

```sh
minds enable --agent claude-code    # ein Aufruf, dann läuft es
```

**Die Frage:** Steht in der `fsck`-Ausgabe klar genug, was der Kollege tun muss?

---

## Was bewusst noch **nicht** geht — bitte nicht als Fehler melden

Diese Dinge sind bekannt und stehen für v0.1.2/v0.1.3 an. Wenn du hier etwas
findest, ist es *erwartet* — verliere keine Zeit damit:

| Bereich | Stand |
|---|---|
| **Redaction** | Hat bekannte Löcher: `curl -u user:pass` geht durch (#2), JSON-escapte Secrets und PEM-Keys (#3), `sk-ant`-Token fehlen in den Regeln (#33), Panic bei Umlauten/€ in Werten (#1). **Das ist v0.1.2** — das Release, an dem die Freigabe beim Partner hängt. Teste hier nicht auf Vollständigkeit. |
| **`minds forget`** | Tilgt den Session-Branch nicht (#5), ein erneuter `put` reanimiert (#6) — und danach liefert `minds brief`/`recall`/`distill` **gar nichts** mehr (#83). Wenn du `forget` ausprobierst, tu es im Wegwerf-Klon. |
| **`minds gitlab mirror`** | Funktioniert zu 100 % nicht (#7, leerer Body). Kommt in v0.1.3. |
| **`show`/`why` im Worktree** | Zeigt den Hauptbaum-Commit (#20). |
| **Windows** | Kein Binary. WSL geht. |
| **Andere Agents** | Für Codex/Cursor/Gemini/opencode wird der Prompt erfasst, die Tool-Ebene nicht gedeutet. |

---

## Was du festhalten solltest

Für jeden Fund drei Zeilen, mehr nicht:

1. **Was hast du getan?** (das Kommando, wörtlich)
2. **Was hast du erwartet, was kam?**
3. **Wie schlimm?** — bricht ab / falsch / verwirrend / nur hässlich

Und einmal am Ende, unabhängig von Bugs:

> **Würdest du das einem Kollegen geben?** Und wenn nein: Was fehlt dazu —
> ein Fehler, eine Erklärung, oder ein Stück Nutzen?

Das ist die Frage, für die dieser Testlauf eigentlich da ist.
