# Minds — Release-Plan

*Der operative Fahrplan: kleine Commits, schnelle Releases, jedes Release mit
erkennbarem Nutzen. Ergänzt `Plan-v0.2.md` (die Feature-Zerlegung) und `Roadmap.md`
(die Strategie) um die Frage: **was wird als Nächstes ausgeliefert, und warum das?***

**Stand: 04.08.2026** · Basis: 62 offene Issues (12× P1, 40× P2, 10× P3) gegen den
Code-Stand nach v0.1.0.

---

## Die Ausgangslage, die den Plan bestimmt

`Plan-v0.2.md` sagt „Multi-Agent-Capture, dann Sync". Die Issue-Analyse sagt etwas
anderes: **Die Kernversprechen halten empirisch nicht.** Das schlägt jede
Feature-Arbeit.

| Versprechen | Realität |
|---|---|
| „Kontext bei jedem Commit" | [#9](https://github.com/munichbughunter/minds/issues/9): In Repos mit `core.hooksPath` (husky, lefthook) feuern die Hooks **nie**. `enable` meldet Erfolg. |
| „Fehler landen in `hook.log`" | [#10](https://github.com/munichbughunter/minds/issues/10): Der Checkpoint-Pfad hat kein Log. Ein Tippfehler in `redact.json` stoppt jede Erfassung — dauerhaft, lautlos. |
| „fail-closed, Secrets raus vor dem Store" | [#1](https://github.com/munichbughunter/minds/issues/1) Panic bei `PASSWORD=a€10` · [#2](https://github.com/munichbughunter/minds/issues/2) `curl -u user:pass` durchgelassen · [#3](https://github.com/munichbughunter/minds/issues/3) JSON-escapte Secrets und PEM-Keys leaken |
| „DSGVO-Löschung" | [#5](https://github.com/munichbughunter/minds/issues/5): `forget` tilgt den Session-Branch nicht — Klartext bleibt als `session.md` **auf GitLab**, mit Erfolgsmeldung. [#6](https://github.com/munichbughunter/minds/issues/6): Der nächste `put` reanimiert ihn. |
| „Policy als Binary, CI-Gate" | [#11](https://github.com/munichbughunter/minds/issues/11): `minds fsck --require-reviews` (Typo) → Exit 0, Gate wirkungslos, Pipeline grün. |
| „Plattform wird zum Cache" (R4) | [#7](https://github.com/munichbughunter/minds/issues/7): `mirror` sendet einen leeren Body — die Spiegelung nach GitLab funktioniert schlicht nicht. |

**Fokus-Entscheidung:** Die Testgruppe arbeitet ausschließlich mit GitLab, und das
Produkt zielt bewusst nur auf GitLab. Das ist die zusätzliche Abgrenzung zu
entire.io (gehostetes Git-Netzwerk, plattformagnostisch) — wir sind repo-nativ *und*
GitLab-nativ. Damit sind die Brücken-Issues Pflichtprogramm, nicht Kür.

> *Nebenbemerkung:* Das Projekt selbst wird auf GitHub entwickelt und released
> (Actions, `install.sh`). Das ist kein Widerspruch zum GitLab-Fokus, aber README und
> `INSTALL.md` sollten es benennen, damit ein GitLab-Kunde nicht stutzt.

---

## Arbeitsprinzipien

1. **Ein Thema je Release.** 3–10 Commits, in ein bis zwei Sitzungen lieferbar.
2. **Rot zuerst.** Jeder Fix beginnt mit dem Test, der den Fehler zeigt — die Issues
   liefern die verifizierten Repro-Fälle mit. Kein Fix ohne Regressionstest.
3. **Jeder Commit einzeln grün.** `cargo fmt` → `cargo clippy -- -D warnings` →
   `cargo test --workspace`. Geprüft wird in sauberer Umgebung; eine globale `minds`
   in `~/.cargo/bin` verfälscht lokale Läufe.
4. **Kein Release ohne CHANGELOG-Eintrag.** Tag `v*` → `release.yml`.
5. **SemVer nach der eigenen Regel aus dem CHANGELOG:** PATCH = nur Korrekturen,
   MINOR = neue Oberfläche oder geänderte Store-Semantik.

---

## Die Reihenfolge — und ihre Begründung

Zwei Abwägungen bestimmen die ersten drei Releases:

- **Redaktion vor GitLab-Brücke.** Die Spiegelung ist eine idempotente
  Einweg-Projektion (R4): repariert man sie später, projiziert sie alles nach. Ein
  durchgelassenes Secret dagegen wird einmal geschrieben und steht für immer in der
  History — und `forget` kann es wegen #5/#6 heute nicht einmal entfernen. Die
  Verzögerungskosten sind hier irreversibel, dort nicht.
- **Erfassung vor allem.** Eine TUI über einem leeren Store ist eine leere Liste;
  eine Spiegelung ohne erfasste Sessions spiegelt nichts. #9/#10/#25 sind die
  Vorbedingung für jeden sichtbaren Nutzen.

| Release | Thema | Art |
|---|---|---|
| **v0.1.1** | Der Hook feuert wirklich | Fix |
| **v0.1.2** | Die Mauer hält — vorne und hinten | Fix |
| **v0.1.3** | Die GitLab-Brücke trägt | Fix |
| **v0.2.0** | `minds ui` — der nutzbare Kern | Feature |
| **v0.2.1** | `minds ui` — Filter, Fokus, Sparkline (+ Öffentlichkeit) | Feature |
| **v0.2.2** | Kein falsches Grün | Fix |
| **v0.3.0** | Store-Integrität | Semantik |
| **v0.3.1** | Signatur bindet, was sie behauptet | Fix |
| **v0.4.0** | Neuer Nutzen für Agent-Flotten | Feature |

---

## v0.1.1 — „Der Hook feuert wirklich"

*Der Stillausfall. Trifft die Testgruppe heute, kostet am wenigsten Arbeit.*

1. `fix(cli): enable respektiert core.hooksPath` — [#9](https://github.com/munichbughunter/minds/issues/9)
2. `fix(cli): checkpoint- und prepare-commit-msg-Fehler nach hook.log` — [#10](https://github.com/munichbughunter/minds/issues/10)
3. `fix(cli): enable schreibt den absoluten Binary-Pfad in die Hooks` — [#25](https://github.com/munichbughunter/minds/issues/25)
4. `fix(cli): enable funktioniert in Linked Worktrees` — [#21](https://github.com/munichbughunter/minds/issues/21)
5. `fix(cli): enable-Härtung — Execute-Bits, fremde Shebangs, codex_hooks-Präfix` — [#52](https://github.com/munichbughunter/minds/issues/52)
6. `feat(cli): fsck prüft den Hook vom effektiven Hook-Verzeichnis aus` — [#9](https://github.com/munichbughunter/minds/issues/9)
7. `fix(cli): unbekannte Flags brechen ab; Wert-Flags nicht als Positional lesen` — [#11](https://github.com/munichbughunter/minds/issues/11), Teil (a)+(b)

**Warum #11 vorgezogen ist:** Ein Commit, der eine ganze Klasse stiller Falschheit
schließt — unter anderem, dass `minds review I… --summary --sign` das Review
**unsigniert** anlegt und dass ein Tippfehler das GitLab-CI-Gate abschaltet. Bei
GitLab-Fokus ist `ci/minds-review-gate.gitlab-ci.yml` die Auslieferungsfläche; ein
Gate, das bei Fehlbedienung grün meldet, ist schlimmer als keins.

**Nutzen:** „Bei mir kommt nichts an" hört auf. Ohne dieses Release ist jedes weitere
Feature unsichtbar.

---

## v0.1.2 — „Die Mauer hält — vorne und hinten"

*Das wichtigste Release im Plan und die Vorbedingung für jede Öffentlichkeit. Die
Redaktion ist das eine Versprechen, bei dem ein Fehlschlag nicht ärgerlich, sondern
schädlich ist — und was doch durchrutscht, muss entfernbar bleiben.*

**Vorne (Redaktion):**

1. `fix(redact): kein Panic bei Multibyte — is_filesystem_path auf char-Grenzen` — [#1](https://github.com/munichbughunter/minds/issues/1)
2. `fix(redact): curl -u user:pass und mysql -pSecret redigieren` — [#2](https://github.com/munichbughunter/minds/issues/2)
3. `fix(redact): JSON-escapte Werte und PEM mit literalem \n` — [#3](https://github.com/munichbughunter/minds/issues/3)
4. `fix(redact): Token-Regeln aktualisieren — sk-ant/sk-proj, Caps` — [#33](https://github.com/munichbughunter/minds/issues/33)
5. `fix(redact): secretfile-Mauer um gängige Credential-Dateien erweitern` — [#34](https://github.com/munichbughunter/minds/issues/34)
6. `fix(redact): Envelope-Felder at/lineage/edges scannen` — [#35](https://github.com/munichbughunter/minds/issues/35)
7. `test(redact): envelope-realistischer Korpus + Property-Test` — [#36](https://github.com/munichbughunter/minds/issues/36)

**Hinten (Tilgung):**

8. `fix(store): forget tilgt auch refs/minds/sessions/*` — [#5](https://github.com/munichbughunter/minds/issues/5)
9. `fix(store): put lehnt Tombstones ab statt sie zu überschreiben` — [#6](https://github.com/munichbughunter/minds/issues/6)

**Warum #5/#6 hier und nicht in v0.3.0:** Sie gehören zum selben Versprechen wie die
Redaktion, und bei GitLab-Fokus wiegt #5 besonders schwer — der nicht getilgte
Session-Branch liegt sichtbar auf dem Server, den das ganze Team liest. Die tiefere
Store-Integrität (Races, Merge-Invarianten) bleibt in v0.3.0.

---

## v0.1.3 — „Die GitLab-Brücke trägt"

*Bei GitLab-Fokus ist das kein Randfeature: `mirror` funktioniert derzeit zu 100 %
nicht, und damit fällt R4 („Die Plattform wird zum Cache") komplett aus.*

1. `fix(gitlab): Header und Body trennen — mirror sendet den Body wieder` — [#7](https://github.com/munichbughunter/minds/issues/7)
2. `fix(gitlab): has_note paginiert — kein doppelter Mirror` — [#38](https://github.com/munichbughunter/minds/issues/38)
3. `fix(gitlab): Change-Id nur in der Kommandozeile suchen` — [#39](https://github.com/munichbughunter/minds/issues/39)
4. `fix(gitlab): [REDACTED]-E-Mail aus Webhooks wird nicht Review-Autor` — [#37](https://github.com/munichbughunter/minds/issues/37)
5. `feat(gitlab): X-Gitlab-Token timing-sicher prüfen` — [#8](https://github.com/munichbughunter/minds/issues/8)
6. `fix(cli): --end-of-options vor Webhook-Commit-Ids` — [#23](https://github.com/munichbughunter/minds/issues/23)

**Nutzen:** Der Kreis schließt sich für die Testgruppe — ein Verdict im Repo
erscheint als MR-Note in GitLab, ohne dass jemand die Oberfläche wechselt. Weil die
Spiegelung idempotent ist, holt sie alle bisherigen Verdicts nach.

---

## v0.2.0 — `minds ui`, der nutzbare Kern

*Der stärkste Sichtbarkeits-Posten im Backlog. `minds render` erzeugt HTML, das
jemand bauen und öffnen muss; `minds ui` ist ein Befehl im Terminal, in dem der
Entwickler ohnehin steht. Vollständige Spezifikation in `briefing-minds-tui.md`.*

**AP0 ist echte Arbeit — hier liegt das Terminrisiko.** `Index`
(`crates/minds-reader/src/index.rs:45`) liefert heute Sessions und Commits je
Session. Für die Spalten aus AP2 fehlen drei Dinge:

| Gebraucht | Stand |
|---|---|
| Commit-SHAs je Session (AP3) | vorhanden (`Index::commits_of`) |
| **Change-Id je Session** (AP2-Spalte, **AP5 Ctrl-F**) | fehlt — nur im Commit-Trailer, muss über `commits_of` aufgelöst werden |
| **Review-Verdict je Session** (AP2-Spalte) | fehlt — der Reader kennt `ReviewStore` nicht |
| **Degradierte Zeilen** (DoD #4: `⌦`/`?`) | nur als Zähler (`Index::unreadable`), nicht als Zeilen |

Commits:

1. `feat(reader): Change-Id je Session über Commit-Trailer auflösen` — AP0
2. `feat(reader): Review-Verdict je Session bereitstellen` — AP0
3. `feat(reader): unlesbare Einträge und Tombstones als Zeilen statt als Zähler` — AP0
4. `feat(tui): Crate-Gerüst minds-tui + minds ui hinter Feature tui` — AP1
5. `ci: Build von minds-cli ohne Default-Features bleibt grün`
6. `feat(tui): Listenansicht — Spalten, Navigation, Leerzustand` — AP2
7. `feat(tui): Drill-down — Dateien, Tool-Aufrufe, Checkpoints` — AP3
8. `feat(tui): Piped-Fallback — tab-separiert, keine ANSI-Codes` — AP7
9. `feat(tui): Panic-Hook stellt das Terminal wieder her` — DoD #5
10. `test(tui): TestBackend-Snapshots, Filterlogik-Units, Pipe-Integrationstest` — AP8

**Der Piped-Fallback ist mehr wert, als das Briefing ihm zugesteht:** Er macht
`minds ui` zur ersten Read-Schnittstelle, die ein Agent direkt konsumieren kann —
genau das, was [#55](https://github.com/munichbughunter/minds/issues/55) (`--json`)
später breit ausrollt. Mensch bekommt TUI, Agent bekommt Zeilen, eine Datenquelle.

**Leitplanken aus dem Briefing, die nicht verhandelbar sind:** kein zweiter
Datenpfad (nur `minds-reader`), strikt lesend, nur redigierte Store-Daten,
`ratatui` + `crossterm` als einzige neue Dependencies (pure Rust — das statische
Binary bleibt statisch), fail-soft bei kaputten Einträgen.

---

## v0.2.1 — `minds ui`: Filter, Fokus, Sparkline — und die Öffentlichkeit

1. `feat(tui): Spaltenfilter mit Tab-Wechsel und UND-Termen` — AP4
2. `feat(tui): Ctrl-F — Fokus auf die Change-Id der gewählten Zeile` — AP5
3. `feat(tui): Tages-Sparkline über die gefilterte Historie` — AP6
4. `chore: generierte site/ und Streu-Dateien aus dem Repo entfernen` — [#60](https://github.com/munichbughunter/minds/issues/60)
5. `docs: README-Screencast + Tastenkürzel-Tabelle`

Punkt 4+5 sind der eigentliche öffentliche Liefergegenstand: Ein 20-Sekunden-Cast
erklärt das Produkt in einem Bild, und `site/`, `hello.txt`, `test.txt`,
`test-szenario-3`, `retest_szenario_1.txt` im Root sind das Erste, was ein Besucher
stattdessen sieht.

> **Reihenfolge-Hinweis:** Der erste öffentliche Screenshot kommt erst nach v0.1.2.
> Die TUI zeigt gespeicherte Session-Inhalte prominent — ein durchgelassenes
> `curl -u admin:hunter2` (#2) stünde sonst im Screencast.

---

## v0.2.2 — „Kein falsches Grün"

*Ein Werkzeug, das bei Fehlbedienung Exit 0 liefert, ist schlimmer als keins.*

1. `fix(cli): agent-help aus einer Quelle generieren statt handgepflegt` — [#11](https://github.com/munichbughunter/minds/issues/11)
2. `feat(cli): Exit-Codes trennen „Prüfung negativ" von „Prüfung nicht möglich"` — [#24](https://github.com/munichbughunter/minds/issues/24)
3. `fix(cli): kein Backtrace aus minds hook` — [#54](https://github.com/munichbughunter/minds/issues/54)
4. `refactor(cli): zentrales Kommando-Gerüst — Fehlerketten erreichen den Nutzer` — [#22](https://github.com/munichbughunter/minds/issues/22)
5. `refactor(gitlab): thiserror-Enum statt Result<_, String>` — [#40](https://github.com/munichbughunter/minds/issues/40)

---

## v0.3.0 — Store-Integrität

*MINOR, weil sich Store-Semantik ändert.*

1. `fix(store): link — Read-Merge-Serialize in die CAS-Retry-Schleife` — [#4](https://github.com/munichbughunter/minds/issues/4)
2. `fix(store): forget — Parent-Commits unerreichbar, Mehr-Ort-Tilgung atomar` — [#14](https://github.com/munichbughunter/minds/issues/14)
3. `fix(store): write_session löscht links.json beim Reparatur-Put nicht mehr` — [#13](https://github.com/munichbughunter/minds/issues/13)
4. `fix(store): .sig-Blobs verletzen die Merge-Invariante nicht mehr` — [#15](https://github.com/munichbughunter/minds/issues/15)
5. `fix(store): Fail-open-Lesepfade an fsck melden` — [#16](https://github.com/munichbughunter/minds/issues/16)
6. `fix(store): Lock-Contention retry-fähig klassifizieren, CAS mit Backoff` — [#17](https://github.com/munichbughunter/minds/issues/17)
7. `perf(store): ReviewStore::entries löst Ref und Baum einmal auf` — [#18](https://github.com/munichbughunter/minds/issues/18)

---

## v0.3.1 — „Signatur bindet, was sie behauptet"

1. `fix(core): Freitextfelder in signierbaren Payloads validieren` — [#12](https://github.com/munichbughunter/minds/issues/12)
2. `refactor(core): ssh-Signing als Library, tempfile statt /tmp` — [#26](https://github.com/munichbughunter/minds/issues/26)
3. `fix(core): Comment::order_key sortiert Zeitstempel typisiert` — [#29](https://github.com/munichbughunter/minds/issues/29)
4. `fix(core): append_all bei eingerücktem Schlussabsatz` — [#31](https://github.com/munichbughunter/minds/issues/31)
5. `fix(metrics/reader): Zeitstempel validieren, Tages-Buckets bei Offsets` — [#41](https://github.com/munichbughunter/minds/issues/41)

---

## v0.4.0 — Neuer Nutzen für Agent-Flotten

*Erst wenn das Fundament trägt. Nach Hebel sortiert.*

1. `feat(cli): --json für alle Lese-Kommandos` — [#55](https://github.com/munichbughunter/minds/issues/55).
   Höchster Hebel im Backlog: macht Minds für Agent-Flotten konsumierbar, also für
   genau die Zielgruppe. Baut auf dem Read-Model aus v0.2.0 auf.
2. `feat(cli): minds log — Session-/Checkpoint-Übersicht` — Plan-v0.2 E.2, die letzte
   Lücke der CLI-Parität.
3. `feat(dist): minds update — Self-Update` — Plan-v0.2 D.7. Die Testgruppe versorgt
   sich selbst; du hörst auf, Bindeglied zu sein.

---

## Parallel: Voraussetzungen fürs Öffentlichmachen

Blockieren keine Release-Kette, laufen nebenher:

- [#44](https://github.com/munichbughunter/minds/issues/44) publish-Entscheidung + Versionen zentralisieren
- [#59](https://github.com/munichbughunter/minds/issues/59) tote Dependencies (clap, anyhow), xtask-Stub
- [#43](https://github.com/munichbughunter/minds/issues/43) `[workspace.lints]` mit `missing_docs`/`unsafe_code`
- [#45](https://github.com/munichbughunter/minds/issues/45) TempRepo hinter `testing`-Feature konsolidieren
- [#51](https://github.com/munichbughunter/minds/issues/51) Integrationstests für die 12 nicht abgedeckten Kommandos
- [#46](https://github.com/munichbughunter/minds/issues/46) Store-Testlücken (Konkurrenz, Korruption, Shallow Clones)
- Auslieferung: macOS-Runner registrieren; Windows/ARM-Linux werden derzeit nicht
  gebaut (siehe CHANGELOG, *Bekannte Einschränkungen*)

---

## Was bewusst wartet

- **Track A — Multi-Agent-Capture** (Gemini/Codex, Plan-v0.2 A.1–A.8). Neun Commits
  für aktuell null Nutzer: Die Testgruppe ist Claude-only. Wird gezogen, sobald
  jemand danach fragt — die Naht (`normalize.rs:108`) ist vorbereitet.
- **Track S.2 — Air-Gap-Export/-Import.** `minds sync` spiegelt Refs; Bundle-Transport
  kommt, wenn ein Kunde ohne Netz konkret wird.
- **Roadmap-Schicht 4 — struktureller/AST-Diff.** Vorbereitet in
  `briefing-semantic-provider.md`, aber kein Nutzerdruck.
- **P3-Aufräumpakete** ([#56](https://github.com/munichbughunter/minds/issues/56),
  [#57](https://github.com/munichbughunter/minds/issues/57),
  [#58](https://github.com/munichbughunter/minds/issues/58),
  [#61](https://github.com/munichbughunter/minds/issues/61),
  [#62](https://github.com/munichbughunter/minds/issues/62)) — sammeln, bis ein
  Release sie mitnehmen kann.

---

## Warum dieser Plan verlässlich ist

Jedes Element hat ein Issue mit empirisch verifizierter Repro und einem
Lösungsvorschlag im Text. Jeder Commit beginnt damit, diesen Fall als roten Test zu
schreiben — der Aufwand ist dadurch vorab bekannt. Die ersten drei Releases enthalten
**keine neue Funktionalität**, nur Reparatur; das Risiko, dass eines rutscht, ist
entsprechend klein. Das einzige Release mit echter Unbekannten ist v0.2.0, und die
Unbekannte ist benannt: AP0, die drei Reader-Erweiterungen.

Die Testgruppe bekommt in dieser Reihenfolge: *meine Sessions kommen an* (v0.1.1) →
*meine Secrets sind wirklich draußen* (v0.1.2) → *ich sehe die Verdicts in meinem MR*
(v0.1.3) → *ich sehe, was Minds über mein Repo weiß* (v0.2.0). Ab v0.2.0 hat sie zum
ersten Mal etwas, das sie **benutzt** statt nur installiert.
