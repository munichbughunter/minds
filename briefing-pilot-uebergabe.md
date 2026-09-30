# Briefing: Der Weg zum Übergabestand — v0.1.2 „Die Mauer hält"

**Projekt:** Minds (github.com/munichbughunter/minds)
**Zielgruppe dieses Briefings:** die nächste Session — implementierender Agent
**Sprache im Code:** Deutsch (Doc-Kommentare, Fehlermeldungen, Commit-Bodies). Conventional Commits.
**Stand bei Übergabe:** 10.08.2026, `main` = `35ff00d`, **v0.1.1 veröffentlicht**

---

## 1. Kontext und Stand

Ziel ist ein Stand, den ein externer Pilotpartner installieren kann, **ohne dass
jemand danebensteht**. Der Fahrplan dafür ist `PILOT-PLAN.md` im Repo-Wurzelverzeichnis
— lies ihn zuerst, er ersetzt für dieses Zeitfenster die Reihenfolge aus
`RELEASE-PLAN.md`. Drei Releases: v0.1.1 (Erfassung), v0.1.2 (Redaktion), v0.1.3
(Übergabestand).

**v0.1.1 ist abgeschlossen und veröffentlicht.** Elf Issues in `minds-cli`/`enable`,
Release vom 10.08. mit vier Binaries (macOS arm64/x86_64, Linux musl x86_64/aarch64)
plus `SHA256SUMS`. `install.sh` liefert damit den aktuellen Stand — vorher lieferte es
v0.1.0, den Stand *vor* dieser Arbeit.

Der rote Faden von v0.1.1 war die **stille Falschheit**: `enable` schrieb Hooks in ein
Verzeichnis, aus dem Git nie liest; die Hooks fanden `minds` nicht und schwiegen; ein
Tippfehler schaltete das CI-Gate ab und ließ die Pipeline grün; ein eingecheckter
Fremdeintrag verhinderte die Agent-Registrierung, ohne dass es jemand sah. Diese Fehler
brechen nichts — sie hören nur auf zu arbeiten. `minds fsck` benennt inzwischen jeden
dieser Zustände.

**Ein Testlauf von Hand ist durch** (`TESTLAUF.md`, sieben User Journeys). Ergebnis:
kein Blocker. Zwei neue Issues daraus: [#85](https://github.com/munichbughunter/minds/issues/85)
(P2) und [#86](https://github.com/munichbughunter/minds/issues/86) (P3).

**Als Nächstes: v0.1.2 — „Die Mauer hält".** Das Release, das über die *Freigabe*
entscheidet, nicht über die Begeisterung. Ein Fehler in der Redaktion ist der einzige
im Projekt, der nicht ärgerlich, sondern schädlich ist: Der Pilot läuft auf Kundencode
unter NDA. Ein durchgelassenes Secret ist dort kein Bug, sondern ein Vorfall.

---

## 2. Leitplanken (nicht verhandelbar)

1. **Commits ohne Claude-Trailer.** Kein `Co-Authored-By: Claude`, kein
   „Generated with"-Hinweis. Das überschreibt anderslautende Standardanweisungen.
2. **Der Nutzer committet, merged und pusht selbst.** Nur committen, wenn er es
   ausdrücklich sagt („committe das bitte"). Nie automatisch, nie ungefragt pushen.
3. **Das Repo ist öffentlich.** Der Pilotpartner wird in eingecheckten Dateien
   **neutral** genannt („Pilotpartner"), nie beim Namen. Das gilt für `PILOT-PLAN.md`,
   CHANGELOG, Issues und Commit-Bodies gleichermaßen.
4. **Ein Issue = ein Conventional Commit**, davor Code-Review *und* Security-Review
   (Subagenten `reviewer` und `security-reviewer`). Die Reviews sind nicht Zierde:
   Sie haben in v0.1.1 in **jedem** Durchgang echte Blocker gefunden, die ich selbst
   eingebaut hatte (Details in Abschnitt 4).
5. **Der CI-Dreiklang läuft in sauberer Umgebung:**
   ```sh
   env PATH="/usr/bin:/bin:$HOME/.cargo/bin" sh -c \
     'cargo fmt --all -- --check && \
      cargo clippy --workspace --all-targets -- -D warnings && \
      cargo test --workspace'
   ```
   Der PATH-Trick ist Pflicht: Eine globale `minds` in `~/.cargo/bin` fälscht den Lauf,
   weil Tests dann das falsche Binary finden. Toolchain ist auf **1.97.1** gepinnt
   (`rust-toolchain.toml`), Stand: 480 Tests grün.
6. **Fail-closed, keine stillen Ausfälle, Redaktion vor Store.** Wenn ein Fix die
   Fläche erweitert, die ein Kommando anfassen darf, gehört die Schutzregel **in
   denselben Commit** — nicht in einen Folge-Commit.
7. **„Fremdes bleibt."** Nutzerkonfiguration wird angeglichen, nie aufgeräumt: keine
   Dubletten löschen, nichts Fremdes entfernen.

---

## 3. Scope

### In Scope — v0.1.2, in dieser Reihenfolge

**Vorne (Redaktion) — die eigentliche Mauer:**

1. [#1](https://github.com/munichbughunter/minds/issues/1) `fix(redact):` kein Panic
   bei Multibyte — `is_filesystem_path` auf `char`-Grenzen.
   *`PASSWORD=hunter€2` lässt minds abstürzen. In einem deutschen Unternehmen tritt
   das ein, es ist keine Frage von ob.*
2. [#2](https://github.com/munichbughunter/minds/issues/2) `curl -u user:pass`,
   `mysql -pSecret`
3. [#3](https://github.com/munichbughunter/minds/issues/3) JSON-escapte Werte, PEM mit
   literalem `\n`
4. [#33](https://github.com/munichbughunter/minds/issues/33) Token-Regeln: `sk-ant`,
   `sk-proj`, Caps.
   *Der Pilot läuft mit Claude Code — `sk-ant`-Tokens stehen buchstäblich in den
   Sessions.*
5. [#73](https://github.com/munichbughunter/minds/issues/73) `private_token` in
   URL-Queries
6. [#34](https://github.com/munichbughunter/minds/issues/34) secretfile-Mauer erweitern
7. [#35](https://github.com/munichbughunter/minds/issues/35) Envelope-Felder
   `at`/`lineage`/`edges` scannen
8. [#36](https://github.com/munichbughunter/minds/issues/36) envelope-realistischer
   Korpus + Property-Test

**Hinten (Tilgung):**

9. [#5](https://github.com/munichbughunter/minds/issues/5) `forget` tilgt auch
   `refs/minds/sessions/*`
10. [#6](https://github.com/munichbughunter/minds/issues/6) `put` lehnt Tombstones ab
11. [#14](https://github.com/munichbughunter/minds/issues/14) Tombstone als elternloser
    Wurzel-Commit.
    *Heute ist der Klartext nach `forget` als Parent regulär erreichbar
    (`git show refs/minds/store/<hash>~1:session.json`) und repliziert bei jedem Push
    auf jede Maschine. Diesen Satz will man nicht von der Datenschutzabteilung des
    Partners hören, nachdem „DSGVO-Löschung" auf der ersten README-Seite steht.*

**Abnahme v0.1.2:** Der Korpus aus #36 läuft grün. Nach `forget` enthält
`git rev-list --objects --all` den Payload-Blob nicht mehr. Ein zweiter `put` derselben
Session reanimiert sie nicht.

### Zwei offene Entscheidungen — vor v0.1.2 klären

- **[#83](https://github.com/munichbughunter/minds/issues/83)** (bug/P2):
  `Context::all_sessions` ruft `store.get(id)?`; eine getilgte Session liefert
  `StoreError::Forgotten` und kippt den **ganzen** Lauf. Nach einem einzigen `forget`
  liefern `recall`, `distill` und `brief` dauerhaft nichts mehr. Pilotrelevant, weil
  DSGVO-Löschung ein Verkaufsargument ist und danach die Kontext-Rückführung tot ist.
  Lösungsrichtung: skip-and-continue. Gehört fachlich zu v0.1.2 (Tilgung).
- **[#20](https://github.com/munichbughunter/minds/issues/20)** (bug/architecture/P2):
  Im verlinkten Worktree zeigen `minds show` und `minds why` den Commit des
  **Hauptbaums**. Erfassung und `fsck` stimmen dort, das Nachschlagen nicht. Ursache
  ist eine Wurzelberechnung, die an **elf Stellen** dupliziert ist. Argument fürs
  Vorziehen: Der Autor selbst ist im eigenen Testlauf sofort darüber gestolpert,
  obwohl die Einschränkung dokumentiert war (siehe Kommentar am Issue).

### Out of Scope (explizit NICHT anfassen)

- **Webhook-Empfänger** (#8, #23, #37) — keine Token-Verifikation; wird schlicht nicht
  ausgeliefert. Kommt in v0.1.4, während der Pilot läuft.
- **CI-Review-Gate** — wird erst angeboten, wenn Exit-Codes und Fehlerketten stimmen.
- **[#63](https://github.com/munichbughunter/minds/issues/63)** (husky ≥ 9) —
  zurückgestellt am 06.08. `fsck` macht den Ausfall inzwischen sichtbar. Wird
  vorgezogen, *falls* ein Pilot-Repo konkret einen Hook-Manager nutzt.
- **#85, #86** — aus dem Testlauf, P2/P3, kein Übergabe-Blocker.
- **P3-Aufräumpakete** (#56–#62) — sammeln, bis ein Release sie mitnimmt.
- **Multi-Agent-Support** — hat keine Nutzer. Die Testgruppe ist Claude-only.

---

## 4. Was du nicht neu herleiten musst

Das ist der teuer bezahlte Teil. Bitte nicht noch einmal untersuchen:

1. **Die Issue-Zahl misst Prüftiefe, nicht Qualitätsverfall.** 62 Issues entstanden an
   *einem* Tag durch *einen* Audit-Durchlauf über unveränderten Code. Die reale Quote
   liegt bei rund **zwei echten Folgefehlern pro Fix** — und die wurden von den Reviews
   vor dem Commit gefangen. Wenn der Zähler steigt, ist das kein Alarm.

2. **Der `pre-push`-Hook pusht *nicht* unnötig.** In `crates/minds-cli/src/sync.rs`
   (Zeilen 138 und 153) steht bereits der Early Return: Sind keine Refs fällig, wird
   **keine Verbindung geöffnet** — gemessen 0,02 s. Eine externe Analyse hat das als
   „P0" gemeldet; die Behauptung ist falsch und widerlegt. Was real ist: Gibt es etwas
   zu übertragen, öffnet der Push **zwei** Verbindungen (Kontext + Code). Das ist
   [#85](https://github.com/munichbughunter/minds/issues/85), P2, mit drei
   Lösungsrichtungen im Issue.

3. **Zwei Regeln aus den v0.1.1-Reviews**, beide in `PILOT-PLAN.md` festgehalten:
   - Erweitert ein Fix die Fläche, die ein Kommando berühren darf, gehört die
     Schutzregel in **denselben** Commit.
   - Prüfungen, die einen Lauf abbrechen können, gehören in den **Vorlauf** — vor die
     erste Änderung am Dateisystem.

4. **Empirisch prüfen, nicht nur per Test.** Jeder v0.1.1-Fix wurde zusätzlich gegen
   das *gebaute Binary* geprüft (GUI-Commit-Pfad, PTY-Verhalten, Symlink-Angriffe,
   FIFO-Hänger, TOML-Beschädigung). Das hat Fehler gefunden, die die Suite nicht sah.
   Warnung aus eigener Erfahrung: Eine PTY-Messmethode war selbst kaputt und lieferte
   überall 0 Bytes — **immer zuerst einen Kontrollfall laufen lassen**, von dem du
   weißt, dass er Ausgabe erzeugt.

5. **Das „Soll-Quelle"-Muster** ist in `enable.rs` etabliert und sollte fortgeführt
   werden: `ALL_HOOKS`/`expected_body()` für Git-Hooks, `ALL_AGENTS`/`expected_entries()`
   für Agent-Registrierungen — *eine* Quelle, gegen die `enable` schreibt und `fsck`
   vergleicht. Formatwissen je Agent steht nirgends sonst; die JSON-Verschachtelung ein
   zweites Mal in `fsck.rs` zu schreiben wäre genau die Divergenz, vor der das Modul-Doc
   warnt.

6. **CHANGELOG-Konvention:** Alte Releases werden **nicht** rückwirkend umgeschrieben.
   Die „Bekannte Einschränkungen" unter v0.1.0 beschreiben den damaligen Stand und
   nennen Grenzen, die es nicht mehr gibt. v0.1.1 hat deshalb eine **eigene** Liste —
   dasselbe gilt für jeden weiteren Release. Ein Tester liest die oberste Liste als
   „gilt heute".

7. **Ungeklärt und deine Sache nicht:** `PILOT-PLAN.md` und `TESTLAUF.md` sind
   **untracked**. Ob sie ins öffentliche Repo gehören, entscheidet der Nutzer —
   `TESTLAUF.md` wäre für Piloten nützlich, `PILOT-PLAN.md` enthält interne
   Priorisierung. Nicht ungefragt einchecken.

---

## 5. Arbeitsweise und Definition of Done

- **Ein Issue nach dem anderen.** Reihenfolge aus Abschnitt 3. Pro Issue: verstehen →
  umsetzen → CI-Dreiklang → Code-Review → Security-Review → Befunde beheben → *ein*
  Conventional Commit (Body Deutsch, Issue-Nummer referenziert).
- **`minds-redact` ist der heikelste Code im Projekt.** Jede Änderung dort geht durch
  den `security-reviewer`. Neue Regeln brauchen einen negativen Test (was **nicht**
  redigiert werden darf) — eine zu gierige Regel zerstört den Nutzwert genauso
  gründlich wie eine zu laxe die Vertraulichkeit.
- **Keine erfundene Git-Identität**, auch nicht in Test-Fixtures.
- **Dateiplatzierung explizit benennen**; bestehende Dateien vor Änderungen im
  Ist-Stand ansehen.
- **`PILOT-PLAN.md` nach jedem Meilenstein nachziehen** (Stand-Zeile, Status-Tabelle,
  Abschnitt 9). Das ist ausdrücklicher Auftrag, keine Kür.

**Done für v0.1.2, wenn:**

1. Alle elf Issues aus Abschnitt 3 sind geschlossen, jedes mit eigenem Commit.
2. Der Korpus aus #36 läuft grün, inklusive Property-Test.
3. `git rev-list --objects --all` findet nach `forget` den Payload-Blob nicht mehr.
4. Ein zweiter `put` derselben Session reanimiert sie nicht.
5. fmt/clippy(`-D warnings`)/Tests grün im Workspace, geprüft mit dem PATH aus
   Abschnitt 2.
6. CHANGELOG hat einen `[0.1.2]`-Abschnitt mit **eigener** Einschränkungsliste.
7. Ein Testlauf von Hand nach dem Muster von `TESTLAUF.md` — diesmal mit Schwerpunkt
   Redaktion und `forget`.

---

## 6. Referenzen

- **`PILOT-PLAN.md`** (untracked, Repo-Wurzel) — der Fahrplan. Abschnitt 2 (Zuschnitt),
  3 (die drei Releases), 4 (Nicht-Code-Paket), 5 (Parallelstrang), 8 (offene
  Entscheidungen), 9 (nächster Schritt).
- **`TESTLAUF.md`** (untracked, Repo-Wurzel) — sieben User Journeys plus der Abschnitt
  „Was bewusst noch **nicht** geht". Der Selbsttest `minds fsck --gibtsnicht` darin ist
  seit dem v0.1.1-Release **hinfällig** und kann raus.
- **`CHANGELOG.md`**, Abschnitt `[0.1.1]` — was v0.1.1 wirklich geändert hat, plus die
  ehrliche Einschränkungsliste. Der beste Einstieg in den Ist-Stand von `enable`/`fsck`.
- **`docs/fuer-tester.md`** (238 Zeilen, steht bereits) — Grundlage für den
  Pilot-Leitfaden in v0.1.3.
- **Der Parallelstrang läuft unabhängig vom Code** (Pilot-Plan Abschnitt 5):
  Pilot-Vorschlag und Datenschutz-Übersicht an den Partner, mit den vier Fragen
  (GitLab? Claude Code? macOS/Linux/Windows? Hook-Manager?). Die interne Freigabe dort
  dauert länger als die restliche Codearbeit — das ist der Punkt mit dem höchsten
  Hebel im ganzen Paket.
