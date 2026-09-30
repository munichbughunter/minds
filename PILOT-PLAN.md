# Minds — Pilot-Plan

*Der Fahrplan zu einem Stand, den ein externer Partner installieren kann, ohne dass
jemand danebensteht. Ergänzt `RELEASE-PLAN.md` und **ersetzt dessen Reihenfolge für
das Zeitfenster bis zur Übergabe**.*

**Stand: 21.08.2026** · Übergabestand: **v0.1.3** · Basis: 46 offene
Issues (seit dem 19.08. 7 geschlossen — #69, die komplette Welle 2
(#102/#100), #8, #23, #71 und #26 —, keine neuen aus den Reviews)

> **Alle sechs v0.1.3-Punkte sind gemerged (14.08.)** — der Fix an
> `gitlab mirror` (#7), der Test-Zuschnitt aus #51, die Repo-Hygiene (#60)
> und das komplette Doku-Paket samt englischer Fassungen
> (`pilot-leitfaden.md`/`pilot-guide.md`,
> `datenschutz-uebersicht.md`/`privacy-overview.md`, README mit WSL- und
> GitLab-Fokus, eigene „Bekannte Einschränkungen" im CHANGELOG). **Vor dem
> Tag kommt jetzt noch das Security-Paket** (entschieden am 14.08.,
> Abschnitt 3): alle elf `security`-Issues plus #4 und #85 — der
> Übergabestand soll bei `label:security` keine offenen P1 zeigen. Der
> Parallelstrang (Abschnitt 5) startet unabhängig davon sofort; beide
> Pilot-Dokumente liegen dafür auf Deutsch und Englisch vor.
>
> **Welle 1 und 2 sind durch, Welle 3 läuft (21.08.):** #69 ist gemerged
> (PR #118), das DSGVO-Versprechen aus Welle 2 steht — #102 (PR #119) und
> #100 (PR #120) —, aus Welle 3 sind #8 (PR #121), #23 (PR #122),
> #71 (PR #123) und #26 (PR #124) gemerged, und #72 ist umgesetzt:
> Branch `fix/72-unsichtbare-fueller`, wartet auf Merge (Entscheidung aus
> dem Issue-Kommentar übernommen: alle fünf Zeichen plus U+034F und
> U+1D159 entschärft, Kriterium Default-Ignorable/leerer Glyph steht im
> Code, ein Test misst es gegen die komplette DICP-Property; Code- und
> Security-Review CLEAN). Als Nächstes #92, dann #85.

---

## 1. Warum die Reihenfolge hier eine andere ist

`RELEASE-PLAN.md` ordnet nach Nutzen für die eigene Testgruppe. Bei einem externen
Partner gilt ein anderer Maßstab, weil zwei Dinge wegfallen:

- **Du bist nicht im Raum.** Ein stiller Ausfall wird nicht als Bug gemeldet, sondern
  als „macht nichts" abgehakt — und die Gelegenheit ist weg. Sichtbares Scheitern
  schlägt hier jedes Feature.
- **Es ist Kundencode unter NDA.** Ein durchgelassenes Secret ist kein Ärgernis,
  sondern ein Vorfall. Die Redaktion ist damit nicht Qualität, sondern
  Zutrittsvoraussetzung.

Daraus folgt der Zuschnitt: **alles, was still scheitern oder Daten beschädigen kann,
ist im Paket. Alles andere ist es nicht** — auch dann nicht, wenn es beeindruckender
wäre.

---

## 2. Der Pilot-Zuschnitt

Der Zuschnitt ist der stärkste Hebel auf die Geschwindigkeit: Er entscheidet, welche
Issues überhaupt auf den kritischen Pfad kommen.

| | |
|---|---|
| **Umfang** | 1–2 Repositories, 3–5 Entwickler, 3–4 Wochen |
| **Agent** | Claude Code (die Tool-Ebene ist nur dort vollständig gedeutet) |
| **Im Piloten** | `enable`, Erfassung, `show`, `why`, `blame`, `recap`, `search`, `render`, `fsck`, `forget` |
| **Im Piloten, wenn GitLab-Repo** | `review`, `reviews`, `gitlab mirror` (Push-Richtung) |
| **Nicht im Piloten** | Webhook-Empfänger, CI-Review-Gate, `sync` zwischen Maschinen, Multi-Agent |
| **Leitfrage** | *Beantwortet `minds why` nach drei Wochen eine Frage, die `git blame` nicht beantwortet?* |

**Warum der Webhook-Empfänger draußen bleibt:** Er hatte keine Token-Verifikation
([#8](https://github.com/munichbughunter/minds/issues/8)) — jeder konnte Reviews mit
beliebigem Autor einschleusen. Ihn nicht auszuliefern war eine Entscheidung von einer
Minute und nahm drei Issues vom kritischen Pfad (#8, #23, #37); #8 und #23 sind mit
dem Security-Paket inzwischen doch gefixt, offen bleibt #37. Am Zuschnitt ändert das
nichts: Der Webhook kommt in v0.1.4, während der Pilot schon läuft.

**Warum das CI-Gate draußen bleibt:** Ein Gate, das bei Fehlbedienung grün meldet
([#11](https://github.com/munichbughunter/minds/issues/11), #24), ist schlimmer als
keins. #11 wird trotzdem gefixt — es trifft auch `review --sign` —, aber das Gate
selbst wird erst angeboten, wenn Exit-Codes und Fehlerketten stimmen.

---

## 3. Die drei Releases

### v0.1.1 — „Der Hook feuert wirklich und sagt es"

*Der Bereich, den jeder Tester in Minute eins anfasst. Ein Fehler hier kostet den
gesamten Piloten, alle anderen kosten nur eine Meldung.*

**Fertig und veröffentlicht (10.08.).**

| Status | Issue | Was es war |
|---|---|---|
| ✅ | [#9](https://github.com/munichbughunter/minds/issues/9) | `enable` folgt `core.hooksPath` — sonst feuert der Hook in husky-Repos nie. |
| ✅ | [#10](https://github.com/munichbughunter/minds/issues/10) | Fehler aus dem Hook-Pfad landen in `hook.log` statt in `/dev/null`. |
| ✅ | [#25](https://github.com/munichbughunter/minds/issues/25) | **Das größte Einzelrisiko.** Hooks lösen den Binary-Ort über `minds.binary` auf — der Commit aus VS Code, Fork oder Tower erzeugt jetzt einen Checkpoint. |
| ✅ | [#11](https://github.com/munichbughunter/minds/issues/11) | Strikter Parser: Ein Tippfehler schaltet das CI-Gate nicht mehr lautlos ab, `--summary --sign` legt kein unsigniertes Review an. |
| ✅ | [#66](https://github.com/munichbughunter/minds/issues/66) + [#64](https://github.com/munichbughunter/minds/issues/64) | Ortsregel: Ein eingecheckter Symlink lenkt `enable` nicht mehr um, und außerhalb des Repos wird nur mit Zustimmung geschrieben. |
| ✅ | [#65](https://github.com/munichbughunter/minds/issues/65) | Agent-Konfigurationen werden nicht durch Symlinks geschrieben — auch nicht über ein verlinktes `.claude`-Verzeichnis. |
| ✅ | [#52](https://github.com/munichbughunter/minds/issues/52) | Verlorene Execute-Bits, fremde Shebangs, exakter `codex_hooks`-Schalter. |
| ✅ | [#54](https://github.com/munichbughunter/minds/issues/54) | Kein Panic-Backtrace in der Agent-Sitzung; dazu: Nicht-UTF-8-Argumente stürzen nicht mehr ab. |
| ✅ | [#21](https://github.com/munichbughunter/minds/issues/21) | `enable` in verlinkten Worktrees **und Submodulen** — Erfassung funktioniert dort jetzt. Der Lese-Weg (`show`/`why`) bleibt offen, siehe unten. |
| ✅ | [#68](https://github.com/munichbughunter/minds/issues/68) + [#78](https://github.com/munichbughunter/minds/issues/78) | `brief --hook` schrieb seine Fehler ins Nichts; die Idempotenz-Prüfung war substring-naiv, sodass ein eingecheckter Fremdeintrag die Registrierung lautlos verhinderte und ein geänderter Aufruf bestehende Installationen nie erreichte. Die Agent-Registrierungen haben jetzt eine Soll-Quelle, und `fsck` benennt ihren Zustand. |
| ⏸ | [#63](https://github.com/munichbughunter/minds/issues/63) | **Zurückgestellt (06.08.):** husky-spezifisch; klärt die Vorab-Frage im Pilot-Vorschlag. `fsck` macht den Ausfall inzwischen sichtbar. Wird vorgezogen, sobald ein Pilot-Repo konkret einen Hook-Manager nutzt. |

**Abnahme:** `env -i PATH=/usr/bin:/bin git commit` erzeugt einen Checkpoint ✅.
`enable` verhält sich korrekt und sagt, was es tut — im Linked Worktree ✅, im
Repo mit symlinktem `.githooks` ✅, bei einem Hook-Verzeichnis außerhalb des
Repos ✅ (Rückfrage), bei einem fremden Interpreter ✅ (Abbruch mit Grund), bei
einem eingecheckten Fremdeintrag in der Agent-Konfiguration ✅ (der echte
Eintrag entsteht daneben, der fremde bleibt). Und `minds fsck` benennt jeden
dieser Zustände ✅.

> **Eine Einschränkung, die mit in die Übergabe geht:** Im verlinkten Worktree
> zeigen `minds show` und `minds why` den Commit des **Hauptbaums**. Erfassung
> und `fsck` stimmen dort, das Nachschlagen nicht. Ursache ist eine
> Wurzelberechnung, die an elf Stellen dupliziert ist
> ([#20](https://github.com/munichbughunter/minds/issues/20)) — zu groß für
> #21 und zu klein, um den Piloten aufzuhalten, solange sie benannt ist. Sie
> steht im CHANGELOG beim #21-Eintrag. Der Testlauf hat das Argument fürs
> Vorziehen verstärkt — siehe Abschnitt 9.

---

### v0.1.2 — „Die Mauer hält"

*Das Release, das über die Freigabe entscheidet — nicht über die Begeisterung. Ein
Fehler in der Redaktion ist der einzige im Projekt, der nicht ärgerlich, sondern
schädlich ist.*

**Fertig und veröffentlicht (12.08.).**

**Zuschnitt erweitert am 10.08.:** #93 (neu, aus dem Security-Review zu #34) und
#83 (vorher Abschnitt 8) sind aufgenommen — Begründungen in den Tabellen.

**Vorne (Redaktion):**

| Status | Issue | Was es ist |
|---|---|---|
| ✅ | [#1](https://github.com/munichbughunter/minds/issues/1) | Multibyte-Panic in `is_filesystem_path` — `PASSWORD=hunter€2` stürzte ab. |
| ✅ | [#2](https://github.com/munichbughunter/minds/issues/2) | `curl -u user:pass` — neuer Short-Flag-Detektor, in die Standard-Pipeline eingehängt. |
| ✅ | [#3](https://github.com/munichbughunter/minds/issues/3) | JSON-escapte Werte und PEM mit literalem `\n`. Die Reviews fanden vier Folge-Befunde derselben Klasse — drei davon wären erst durch den Fix entstanden (u. a. kippte die Pfad-Ausnahme von Teil- auf Total-Leck). |
| ✅ | [#33](https://github.com/munichbughunter/minds/issues/33) | Token-Regeln: `sk-ant`/`sk-proj`, SendGrid, die **GitLab-Token-Familie** (`glcbt-`, `glptt-`, …) und realistische Caps. Dazu eine Vorfilter-Lücke: Die nicht-überlappende Suche ließ ausgerechnet den Anthropic-Key komplett durchrutschen. |
| ✅ | [#73](https://github.com/munichbughunter/minds/issues/73) | `private_token` in URL-Queries — Wiederverwendung der Redaction-Policy statt einer zweiten Parameterliste, plus formbasiertes Auffangnetz für Pfad und Fragment. |
| ✅ | [#34](https://github.com/munichbughunter/minds/issues/34) | secretfile-Mauer erweitert (GCP-Service-Accounts, FIDO-SSH, Ansible-Vault, `.dockercfg`, …). Der Review fand eine selbst gerissene Lücke — Backups wie `credentials.bak` fielen durch — behoben im selben Commit. |
| ✅ | [#35](https://github.com/munichbughunter/minds/issues/35) | Envelope-Felder `at`/`lineage`/`edges` werden gescannt. Beide Reviews CLEAN — zum ersten Mal in der Serie; der Leak über den Import-Pfad wurde end-zu-ende nachgewiesen. |
| ✅ | [#93](https://github.com/munichbughunter/minds/issues/93) | Die Mauer gilt auf **beiden** Eingangswegen, mit byte-gleicher Envelope-Form. Drei Zusatzbefunde im selben Commit: Der Hook-Weg verlor Marker und Grund im Envelope schon immer; die Pipeline redigierte den eigenen Auslass-Grund (`secret` im Feldnamen); doppelt serialisierter Input schlüpfte an der Mauer vorbei, während die Verbatim-Kopie den Inhalt mitnahm. |
| ✅ | [#36](https://github.com/munichbughunter/minds/issues/36) | Envelope-realistischer Korpus + Property-Test — der Abschluss der Redaktions-Hälfte. Kann seit #93 beide Eingangswege gegen dieselben Fixtures prüfen. |

**Hinten (Tilgung):**

| Status | Issue | Was es ist |
|---|---|---|
| ✅ | [#5](https://github.com/munichbughunter/minds/issues/5) | `forget` tilgt auch `refs/minds/sessions/*`. |
| ✅ | [#6](https://github.com/munichbughunter/minds/issues/6) | `put` lehnt Tombstones ab statt sie zu überschreiben. |
| ✅ | [#14](https://github.com/munichbughunter/minds/issues/14) | Tombstone als elternloser Wurzel-Commit. |
| ✅ | [#83](https://github.com/munichbughunter/minds/issues/83) | **Neu im Zuschnitt (10.08., vorher Abschnitt 8):** `all_sessions` bricht nach einem einzigen `forget` dauerhaft ab — `recall`, `distill`, `brief` liefern nichts mehr. Lösungsrichtung: skip-and-continue. Begründung fürs Vorziehen: #5/#6/#14 machen `forget` erst belastbar und damit attraktiver — solange #83 offen ist, ist die DSGVO-Löschung von der ersten README-Seite eine Falle. |

> **#14 ist gegenüber `RELEASE-PLAN.md` aus v0.3.0 vorgezogen.** Nach `forget` bleibt
> der Klartext heute als Parent-Commit regulär erreichbar
> (`git show refs/minds/store/<hash>~1:session.json`) und repliziert bei jedem Push
> auf jede Maschine. Diesen Satz will man nicht von der Datenschutz-Abteilung des
> Partners hören, nachdem „DSGVO-Löschung" auf der ersten README-Seite steht. Der Fix
> ist laut Issue billig, weil der Session-Ref eine private Orphan-Kette ist.

**Abnahme:** Der Korpus aus #36 läuft grün — **auf beiden Eingangswegen**, Hook
und Import (#93). Nach `forget` enthält `git rev-list --objects --all` den
Payload-Blob nicht mehr. Ein zweiter `put` derselben Session reanimiert sie
nicht. Und nach einem `forget` liefern `recall`/`distill`/`brief` weiterhin
Ergebnisse (#83).

---

### v0.1.3 — „Pilotreife" · **der Übergabestand**

*Fast kein Produktionscode. Trotzdem das Release, an dem der Pilot hängt.*

1. ✅ `fix(gitlab): Header und Body trennen — mirror sendet den Body wieder` — [#7](https://github.com/munichbughunter/minds/issues/7)
   *Gemerged am 13.08. Der Body geht jetzt über eine kurzlebige 0600-Tempdatei, stdin gehört allein dem Token; die Fehlermeldung zitiert neu GitLabs `message` — vorher war die eigentliche Ursache unsichtbar. Vier Stub-Tests gegen echtes `curl` sichern den Netz-Pfad erstmals ab; beide Reviews CLEAN. Paginierung (#38) und Change-Id-Suche (#39) treffen einen 3–5-Personen-Piloten nicht und bleiben in v0.1.4.*
2. ✅ `test(cli): Integrationstests für die Kommandos des Pilot-Zuschnitts` — Teil von [#51](https://github.com/munichbughunter/minds/issues/51)
   *Gemerged am 13.08. Neun Tests gegen das echte Binary — genau der Pfad, der beim Partner nicht selbst debuggbar ist: `prepare-commit-msg` über echten `git commit` (inkl. Amend-Idempotenz), `blame`/`recap`/`search` mit Happy-Path und Fehlerfall, das `brief --hook`-Envelope (Schema und Inhalt) und `gitlab mirror` über die ganze CLI-Strecke gegen einen lokalen Stub. Der Rest von #51 bleibt offen und im Issue dokumentiert.*
3. ✅ `chore: generierte site/ aus dem Repo entfernen` — [#60](https://github.com/munichbughunter/minds/issues/60) — **gemerged (14.08., PR #108)**
   *Befund bei der Umsetzung: Die Streu-Dateien (`hello.txt`, `test.txt`, `retest_szenario_1.txt`, `test-szenario-3`) waren bereits untracked und ignoriert — getrackt war nur noch die `site/` (58 Dateien, −29 728 Zeilen).*
4. ✅ `docs: Pilot-Leitfaden, Datenschutz-Übersicht, bekannte Einschränkungen` — **gemerged (14.08., PR #109)**
   *`docs/datenschutz-uebersicht.md` und `docs/pilot-leitfaden.md`, dazu englische Fassungen (`privacy-overview.md`, `pilot-guide.md`); die Behauptungen der Datenschutz-Übersicht sind gegen den Code verifiziert — drei zu starke Zusagen des ersten Entwurfs wurden dabei zurückgenommen (Reviews unredigiert, Journal-Klartextfenster, Webhook im Binary). Zwei Funde aus der Verifikation: `minds import` nutzt die eingebaute Default-Policy statt `.minds/redact.json` (Issue-Kandidat), und die Modul-Doku von `tombstone.rs` widerspricht seit #14 dem Code.*
5. ✅ `docs: README benennt Windows (WSL) und den GitLab-Fokus` — **gemerged (14.08., PR #109)**
6. ✅ `docs(changelog): eigene „Bekannte Einschränkungen" für den Übergabestand` — **gemerged (14.08., PR #109; repariert nebenbei den beim #108-Merge verlorenen `Hinzugefügt`/`Entfernt`-Block unter `[Unreleased]`)**
7. *Dazu, außerhalb des ursprünglichen Zuschnitts:* ✅ englische Fassungen beider Pilot-Dokumente (`privacy-overview.md`, `pilot-guide.md`), querverlinkt — gemerged im selben PR.
   *Aufgefallen bei #21: Der Abschnitt im CHANGELOG steht unter **v0.1.0** und
   nennt dort unter anderem die PATH-Abhängigkeit — die seit #25 nicht mehr
   besteht. Historisch ist das richtig (alte Releases werden nicht
   umgeschrieben), für einen Tester aber irreführend: Er liest die Liste als
   „gilt heute". Der Übergabestand braucht seine eigene, mit dem, was
   tatsächlich offen ist — darunter der Lese-Weg im Worktree
   ([#20](https://github.com/munichbughunter/minds/issues/20)), fehlendes
   Windows-Binary und die Claude-Code-only-Tool-Ebene.*

---

### Das Security-Paket — vor dem Tag (entschieden am 14.08.)

*Begründung: Wer bei einem Datenschutz-Werkzeug `label:security` filtert und
drei offene P1 sieht, liest keine Begründungen mehr. Der v0.1.3-Tag wartet
auf dieses Paket; der Parallelstrang (Abschnitt 5) startet trotzdem sofort —
die interne Freigabe beim Partner dauert länger als diese Codearbeit, damit
kostet das Paket null Kalenderzeit auf dem kritischen Pfad. Geschätzt: 5–7
Sitzungen inkl. beider Reviews.*

**Welle 1 — billig und auf dem Pilot-Pfad** — *durch (19.08.)*:

| Status | Issue | Was es ist |
|---|---|---|
| ✅ | [#12](https://github.com/munichbughunter/minds/issues/12) | P1: Newline-Injection in signierbare Payloads — Typ-Invarianten, fixierte Zeilenzahl. **Startpunkt.** Gemerged 14.08. (PR #110). |
| ✅ | [#4](https://github.com/munichbughunter/minds/issues/4) | P1: Lost-Update-Race in `GitStore::link` — verlorene Kanten heißen: `why`/`show` finden die Session nicht. Gemerged 14.08. (PR #114). |
| ✅ | [#49](https://github.com/munichbughunter/minds/issues/49) | Journal-Elternverzeichnisse 0700 + Verzeichnis-fsync. Steht in der Datenschutz-Übersicht. Gemerged 14.08. (PR #115). |
| ✅ | [#95](https://github.com/munichbughunter/minds/issues/95) | `local_id` entschärfen in Verzeichnisnamen und Ausgaben. Gemerged 19.08. (PR #117). |
| ✅ | [#69](https://github.com/munichbughunter/minds/issues/69) | **Umgesetzt (19.08.), gemerged (21.08., PR #118).** Statt `import.log` einzeln zu härten, ist der Backfill jetzt ein Hook-Pfad wie `checkpoint` (`Source::Import`): Fehler gehen entschärft, gedeckelt, rotiert und mit 0600 ins `hook.log`, `fsck` verweist darauf, `import.log` entsteht nicht mehr und eine vorhandene wird weggeräumt. Security-Review CLEAN. Nebenbefund aus dem Code-Review gleich mitgefixt: Ein Transkript ohne Leserechte war nur eine *Notiz* neben „kein Importer" und wäre auch im neuen Log stumm geblieben — jetzt ein Befund. Bewusst still bleibt der Gutfall, sonst meldete `fsck` nach jedem `enable` einen Hinweis. |

**Welle 2 — das DSGVO-Versprechen:**

| Issue | Was es ist |
|---|---|
| ✅ [#102](https://github.com/munichbughunter/minds/issues/102) | P1: Tombstones erreichen die Forge — gezielter Force-Push **nur** für Tombstone-Refs. Der größte Vorbehalt der Datenschutz-Übersicht wird zur Stärke. |
| ✅ [#100](https://github.com/munichbughunter/minds/issues/100) | Gleiche Baustelle: voller Hash statt 16-Hex-Präfix bzw. Inhaltsprüfung vor der Tilgung des Browse-Branches. |

**Welle 3 — Webhook-Paar und Rest:**

| Issue | Was es ist |
|---|---|
| ✅[#8](https://github.com/munichbughunter/minds/issues/8) | P1: Token-Verifikation für Webhook-Payloads — macht den Webhook für v0.1.4 überhaupt auslieferbar. |
| ✅ [#23](https://github.com/munichbughunter/minds/issues/23) | **Gemerged (21.08., PR #122).** Die Webhook-Commit-Id wird erst als volle Hex-`CommitId` gelesen (Vorbild `audit.rs`) und kanonisch hinter `--end-of-options` an git gereicht; dazu `--end-of-options` vor allen Revs im CLI-Crate. Security-Review CLEAN. Der Code-Review fand einen Blocker der bekannten Klasse „beim Reparieren entstanden": `rev-parse --abbrev-ref` ohne `--verify` reicht `--end-of-options` wörtlich auf stdout durch — `minds stack` wäre in genau dem Normalfall mit konfiguriertem Upstream gebrochen. Vor dem Commit gefixt; der bis dahin ungetestete Upstream-Zweig hat jetzt seinen Integrationstest. |
| ✅ [#71](https://github.com/munichbughunter/minds/issues/71) | **Umgesetzt (21.08., Branch `fix/71-push-abweisung-porcelain`, wartet auf Merge).** Ob ein gescheiterter Push eine Divergenz war — und `sync` in den Reconcile-Zweig geht, der fremde Verdicts zieht —, entscheidet nicht mehr eine Substring-Suche im vermischten stdout+stderr, sondern die `--porcelain`-Struktur auf stdout: Nur ein von git selbst festgestelltes `[rejected]` (non-fast-forward/fetch first/stale info) zählt; das wörtliche Server-Zitat `[remote rejected]` nicht, auch nicht mit dem „richtigen" Wortlaut. Beide Akzeptanzkriterien sind mit Integrationstests gegen echtes git belegt (echter non-ff öffnet reconcile weiterhin, ein „rejected" schreibender pre-receive-Hook nicht). Beide Reviews CLEAN; die Minor-Befunde — die Meldung verlor die strukturierten `!`-Zeilen, der io-Fehlerpfad lief an der Credential-Redaktion vorbei — wurden vor dem Commit eingearbeitet. |
| [#26](https://github.com/munichbughunter/minds/issues/26) | **Eng geschnitten:** nur der tempfile-Teil (vorhersagbare /tmp-Dateien); die Library-Extraktion bleibt Ausblick. |
| [#72](https://github.com/munichbughunter/minds/issues/72) | **Entschieden am 14.08.: entschärfen.** Alle fünf Füllzeichen in `INVISIBLE_CARRIERS`; MUST_SURVIVE mit echtem Koreanisch und Braille gegen Über-Entschärfung. |
| [#92](https://github.com/munichbughunter/minds/issues/92) | Nach #69 neu bewerten — vermutlich schrumpft es auf die dann noch offenen Senken. *Stand 19.08.: Mit #69 ist `hook.log` die einzige Log-Senke der Hook-Pfade; was bleibt, ist stderr beim Hand-Aufruf (sanitized, aber ohne Deckel) und die `fsck`-Ausgabe.* |

**Dazu, kein Security-Thema, aber erste spürbare Reibung:**

| Issue | Was es ist |
|---|---|
| [#85](https://github.com/munichbughunter/minds/issues/85) | **Entschieden am 14.08.: entkoppeln (Richtung 3).** Der pre-push-Hook stößt den Sync losgelöst an; Fehler bleiben über `hook.log`/`fsck` sichtbar, `sync.lock` fängt Parallelläufe. Messung vorher/nachher + Regressionstest für den Early-Return. **Richtung 2 (ein Transport) bleibt als v0.2-Ziel im Hinterkopf** — dort passt sie zum Read-Model-/Store-Umbau. |

---

## 4. Das Nicht-Code-Paket

Der Code ist die kleinere Hälfte. Das hier entscheidet, ob der Partner überhaupt
starten **darf**:

- **Datenschutz-Übersicht (eine Seite).** Was wird gespeichert, wo genau, was verlässt
  die Maschine (nichts), wie wirkt `forget`, welche Lücken bleiben bekannt. **Das
  Dokument mit dem höchsten Hebel im ganzen Paket** — ohne das kommt der Pilot an der
  internen Freigabe nicht vorbei, unabhängig von der Codequalität.
- **Pilot-Leitfaden** auf Basis von `docs/fuer-tester.md` (238 Zeilen, steht): Zuschnitt,
  Installation, die fünf Kommandos, was *nicht* Teil des Piloten ist, Rückkanal.
- **Feste Version.** Installation über `MINDS_VERSION=v0.1.3`, nicht über „latest" —
  sonst testen fünf Leute drei verschiedene Stände.
- **Benannter Ansprechpartner + Rückkanal.** Das Repo ist öffentlich, Issues
  funktionieren. Ein Kanal für Vertrauliches (Session-Inhalte!) muss daneben stehen.
- **Ehrliche Einschränkungsliste.** Kein Windows-Binary (WSL), Tool-Ebene nur für
  Claude Code, kein Self-Update, Review-Schicht braucht zwei Personen auf einem Repo.

---

## 5. Der Parallelstrang — startet sofort, nicht nach dem Code

**Das ist der eigentliche Geschwindigkeitshebel.** Die interne Freigabe beim Partner
(Datenschutz, Freigabe eines Kundenrepos, Teamauswahl) dauert länger als die
Codearbeit und läuft unabhängig davon.

Diese Woche, ohne auf ein Release zu warten:

1. Pilot-Vorschlag senden: Zuschnitt aus Abschnitt 2, Leitfrage, Zeitraum, Termin für
   die Übergabe.
2. Datenschutz-Übersicht mitschicken — damit die Prüfung anläuft, während gebaut wird.
3. Vier Fragen stellen, die den Umfang noch verschieben:
   - Liegt das Pilot-Repo auf **GitLab**? (Entscheidet über Punkt 1 in v0.1.3.)
   - Benutzen die Pilot-Teams **Claude Code**? Wenn überwiegend etwas anderes, ist das
     die erste echte Nachfrage nach Track A — und du erfährst es, bevor du neun
     Commits investierst.
   - **macOS, Linux oder Windows?** Bei Windows: WSL, oder der Pilot bekommt andere
     Teams.
   - Nutzt das Pilot-Repo einen **Hook-Manager** (husky, lefthook, pre-commit)?
     Wenn ja, wird [#63](https://github.com/munichbughunter/minds/issues/63)
     vorgezogen; wenn nein, bleibt es Ausblick.

Wer erst lieferbereit ist und *dann* fragt, verliert drei Wochen an fremder
Bürokratie statt an eigenem Code.

---

## 6. Abnahmekriterien — was offen bleiben darf

Ohne diese Schwelle gewinnt ein unbegrenztes Audit immer gegen endliche Arbeit. Für
die Übergabe gilt:

**Muss null sein:**

- offene **P1**-Issues
- offene **Security**-Issues auf dem Pfad `minds-redact`, `minds-store`, `minds-capture`
- bekannte **Stillausfälle** — jeder Pfad, der scheitern kann, ohne dass der Nutzer
  oder das Log es erfährt
- **Regressionen** aus der Arbeit dieses Plans

> **Fünf Issues kollidieren derzeit mit dieser Schwelle, ohne in einem
> Release-Topf zu liegen** — jede braucht eine ausdrückliche Entscheidung
> (Abschnitt 9), denn eine Schwelle, die stillschweigend unterlaufen wird, ist
> keine:
>
> - [#12](https://github.com/munichbughunter/minds/issues/12) (P1, security,
>   `minds-core`) — Newline-Injection in signierbare Payloads. **Im Plan bisher
>   nirgends erwähnt**, aber `review --sign` ist Teil des Pilot-Zuschnitts,
>   sobald das Pilot-Repo auf GitLab liegt.
> - [#4](https://github.com/munichbughunter/minds/issues/4) (P1, `minds-store`)
>   — bewusst nach v0.3.0 verschoben, aber P1-gelabelt.
> - [#8](https://github.com/munichbughunter/minds/issues/8) (P1, security,
>   `minds-gitlab`) — bewusst nicht ausgeliefert, aber P1-gelabelt.
> - [#49](https://github.com/munichbughunter/minds/issues/49) (security,
>   `minds-capture`) — Journal-Verzeichnisse ohne 0700. Brisant, weil das
>   Journal die **unredigierten** Rohdaten hält; die Redaktion läuft erst
>   danach.
> - [#95](https://github.com/munichbughunter/minds/issues/95) (security P3,
>   `minds-capture`) — nur lokale Senken, aber auf dem Muss-null-Pfad.
>
> Präzisierungs-Kandidat für die Schwelle selbst: „P1 **im ausgelieferten
> Pfad**" — das löste #4 und #8 formal, ohne die Substanz zu ändern.

**Darf offen bleiben** — sichtbar, im CHANGELOG unter *Bekannte Einschränkungen*:
alles andere. Das sind nach heutigem Stand rund 40 Issues, überwiegend P2/P3,
Architektur und Aufräumen. Ein Projekt, das seine Lücken kennt und benennt, ist für
einen Partner das stärkere Signal, nicht das schwächere.

**Zur Einordnung der Issue-Zahl:** Von den ursprünglich 72 offenen entstanden 62 an
einem einzigen Tag durch **einen Audit-Durchlauf** über unveränderten Code. Der Zähler
misst Prüftiefe, nicht Qualitätsverfall.

Der Verlauf seit dem 05.08. bestätigt das: **12 Issues geschlossen, 2 neu** —
[#78](https://github.com/munichbughunter/minds/issues/78) (substring-naive
Idempotenz-Prüfung, aus dem Security-Review zu #65; inzwischen mit #68
erledigt) und [#83](https://github.com/munichbughunter/minds/issues/83)
(`all_sessions` bricht bei einer getilgten Session ab, aus der Planung zu #68).
Beides *Funde*, keine Regressionen.

**Seit dem v0.1.1-Release (10.08.):** 8 geschlossen (#1, #2, #3, #33, #73,
#34, #35, #93), **3 neu aus den v0.1.2-Reviews** —
[#92](https://github.com/munichbughunter/minds/issues/92) (Log-Redaktion an der
Senke statt an jeder Quelle), [#93](https://github.com/munichbughunter/minds/issues/93)
(Import umgeht die Mauer; in den Zuschnitt aufgenommen und noch am selben Tag
geschlossen) und [#95](https://github.com/munichbughunter/minds/issues/95)
(`local_id` roh in lokalen Ausgaben). Wieder Funde, keine Regressionen im
ausgelieferten Stand.

**Das ist die eigentliche Erkenntnis dieser Woche:** Nicht die Issue-Zahl misst
die Qualität, sondern das Verhältnis von Befunden im Review zu Befunden beim
Nutzer. Bisher wurde **jeder** Blocker vor dem Commit gefunden — und es waren
nicht wenige:

| Issue | Im Review gefunden |
|---|---|
| #25 | 2 Majors (`--local` fehlte, Clone-Fall) |
| #52 | 1 Blocker (TOML-Heuristik zerstörte Konfigurationen), 3 Majors |
| #54 | 2 Majors (globaler Panic-Handler verschluckte alle Assert-Meldungen) |
| #21 | 1 Blocker (Sicherheitswarnung stillgelegt), 2 Majors |
| #68 | 1 Blocker (doppelte Erfassung), 9 Majors — darunter: `fsck` hing an einer eingecheckten FIFO, im CI-Gate bis zum Pipeline-Abbruch |
| #3 (v0.1.2) | 4 Majors über zwei Review-Runden — drei davon Regressionen des eigenen Fixes (die Pfad-Ausnahme kippte von Teil- auf Total-Leck) |
| #33 (v0.1.2) | 3 Majors (GitLab-Token-Familie fehlte komplett, `xoxd-`, Caps unter der Realität) plus 2 Fehlalarm-Funde exakt in der Zieldomäne |
| #34 (v0.1.2) | 1 Blocker: der eigene Segmentgrenzen-Fix ließ `credentials.bak` durchfallen — gefunden und im selben Commit behoben |
| #93 (v0.1.2) | 3 Befunde über Reviews **und** Binary-Probe: die Pipeline redigierte den eigenen Auslass-Grund, der Hook-Weg verlor die Marker im Envelope, doppelt serialisierter Input umging die Mauer |

Auffällig ist das Muster: Die schwersten Befunde entstanden nicht beim
Übersehen eines Falls, sondern **beim Reparieren** — eine Heuristik, die an
gewöhnlichem TOML scheitert; ein Handler, der zu breit stillstellt; ein
Aufräumen, das fremde Daten mitnimmt. Wer eine Fläche anfasst, macht sie
zunächst größer. Das ist der Grund, warum jeder dieser Commits durch beide
Reviews ging — v0.1.2 hat die Prognose bestätigt: Die Redaktions-Reviews haben
in **jedem** Durchgang echte Befunde geliefert, dreimal davon Regressionen der
eigenen Fixes.

**Die eine Regel, die Neuentstehung senkt:** Erweitert ein Fix die Fläche, die ein
Kommando anfassen darf, gehört die Absicherung dieser Fläche in **denselben** Commit.
#64 und #66 sind genau daran entstanden — #9 ließ `enable` erstmals einem
konfigurierbaren Pfad folgen, ohne die Prüfungen im selben Zug mitzuziehen. Die Regel
gehört in eine `CLAUDE.md` und als Prüffrage in `.claude/agents/security-reviewer.md`.

**Eine zweite Regel, die sich seither herausgestellt hat:** Prüfungen, die einen Lauf
abbrechen können, gehören in den **Vorlauf** — vor die erste Änderung am Dateisystem.
Bei #52 und #65 saß je eine Prüfung zu spät und hinterließ ein halb eingerichtetes
Repo: Agent-Konfiguration auf der Platte, keine Hooks, keine Store-Config. Der Agent
journaliert dann, und nichts checkt je ein. Beide Male fand es erst der Review.

---

## 7. Zeitgerüst

| Release | Umfang | Aufwand | Stand |
|---|---|---|---|
| v0.1.1 | 11 Issues in `minds-cli`/`enable` | 3–4 Sitzungen geschätzt | **veröffentlicht (10.08.)** — es wurden deutlich mehr |
| — | eigener Testlauf von Hand | 1 Sitzung | **erledigt (10.08.)** |
| v0.1.2 | 13 Commits in `minds-redact`/`minds-store`/`minds-capture` (11 + #93/#83) | 4–5 Sitzungen geschätzt | **veröffentlicht (12.08.)** |
| v0.1.3 | 6 Punkte, davon einer Produktionscode | 1–2 Sitzungen | **alle 6 Punkte gemerged (14.08.)** |
| v0.1.3 Security-Paket | 13 Issues in drei Wellen + #85 | 5–7 Sitzungen geschätzt | **läuft** — Welle 1 und 2 durch, #8 und #23 gemerged, #71 wartet auf Merge (21.08.); der Tag wartet auf den Rest von Welle 3 (#26, #72, #92) und #85 |

Bei konzentrierter Arbeit realistisch **zwei Wochen**. Das Risiko ist klein, weil kein
Punkt neue Funktionalität enthält und jedes Issue eine verifizierte Repro plus
Lösungsvorschlag mitbringt — der Aufwand ist vorab bekannt.

> **Eine Korrektur an der ersten Schätzung:** v0.1.1 hat länger gedauert als „3–4
> Sitzungen", weil jeder Fix durch Code- **und** Security-Review ging und diese
> Reviews substanzielle Befunde lieferten — bei #52 einen Blocker, bei #25, #65 und
> #54 je zwei Majors, darunter mehrere Regressionen der eigenen Arbeit. Das ist kein
> Verzug, sondern der Preis dafür, dass der Übergabestand hält. v0.1.2 bestätigt
> das Muster (siehe Tabelle in Abschnitt 6); mit den zwei aufgenommenen Issues
> ist eher mit 6 Sitzungen zu rechnen als mit 5.

Die hohe Lokalität ist beabsichtigt: v0.1.1 fasst fast nur `enable` an, v0.1.2 fast
nur `minds-redact`. Ein Bereich, einmal geöffnet, wird fertig — statt neunmal
Kontext zu wechseln.

**Arbeitsweise unverändert:** roter Test zuerst, jeder Commit einzeln grün
(`cargo fmt` → `cargo clippy -- -D warnings` → `cargo test --workspace`), geprüft in
sauberer Umgebung — eine globale `minds` in `~/.cargo/bin` verfälscht lokale Läufe.
Dazu hat sich in v0.1.2 die **empirische Probe gegen das gebaute Binary** als
eigenständige Fehlerquelle-Finderin bewährt (Envelope-Pfad, Journal-Pfad,
hook.log) — zweimal war die erste Messmethode selbst kaputt und der eingebaute
Kontrollfall hat es gemeldet.

---

## 8. Was bewusst wartet

- **`minds ui` (v0.2.0).** AP0 ist die einzige echte Unbekannte im ganzen Backlog; das
  kostet Wochen. `show`, `why`, `recap` und `render` liefern den sichtbaren Nutzen
  bereits. Die TUI erhöht Freude, nicht Vertrauen — und Vertrauen ist bei diesem
  Empfänger der Engpass. Sie kommt **während** des Piloten als erstes sichtbares
  Update; das ist strategisch sogar besser, weil es Bewegung zeigt.
- **Rest der GitLab-Brücke** (#38, #39, #37) → v0.1.4, parallel zum Piloten;
  #8 und #23 sind inzwischen über das Security-Paket gefixt.
- **`--json` für Lese-Kommandos** (#55) — höchster Hebel im Backlog für Agent-Flotten,
  aber erst nach dem Read-Model aus v0.2.0.
- **Track A (Multi-Agent).** Neun Commits auf Verdacht. Erst nach der Antwort auf
  Frage 2 in Abschnitt 5.
- **Store-Integrität jenseits von #5/#6/#14** (#4, #13, #15, #16, #17) → v0.3.0.
  *Aber:* #4 ist P1 und kollidiert mit der Abnahmeschwelle — Entscheidung in
  Abschnitt 9.
- **P3-Aufräumpakete** (#56–#62) — sammeln, bis ein Release sie mitnimmt.
- ~~[#83](https://github.com/munichbughunter/minds/issues/83)~~ — **entschieden
  am 10.08.: vorgezogen in v0.1.2** (Abschnitt 3, Tilgungs-Hälfte).
- **Neu aus den v0.1.2-Reviews:**
  [#92](https://github.com/munichbughunter/minds/issues/92) (Log-Redaktion an
  der Senke statt an jeder Quelle — Architekturänderung an allen
  Log-Schreibern) → v0.1.4 oder später;
  [#95](https://github.com/munichbughunter/minds/issues/95) → Entscheidung in
  Abschnitt 9.
- **Neu im Store-Umfeld (seit 10.08.):**
  [#100](https://github.com/munichbughunter/minds/issues/100) (16-Hex-Präfix
  im Session-Branch — `forget` kann bei Kollision den falschen Branch tilgen)
  und [#102](https://github.com/munichbughunter/minds/issues/102)
  (DSGVO-Löschung eines gepushten Refs erreicht die Forge nicht). Beide
  berühren das `forget`-Versprechen von der ersten README-Seite — sie
  brauchen dieselbe Art ausdrücklicher Entscheidung wie die Abschnitt-9-Fälle,
  spätestens für die Datenschutz-Übersicht: benennen oder beheben.

---

## 9. Nächster Schritt

1. ~~**Eigener Testlauf von Hand.**~~ **Erledigt am 10.08.** Sieben User Journeys
   (`TESTLAUF.md`), Ergebnis: kein Blocker. Was der Lauf gebracht hat, das die
   Testsuite nicht gefunden hätte:

   - [#85](https://github.com/munichbughunter/minds/issues/85) — ein `git push`
     mit neuen Sessions öffnet **zwei** Netzwerkverbindungen. Gegen einen entfernten
     Remote spürbar. Ohne neue Sessions kostet der Hook nichts (gemessen: 0,02 s);
     der zunächst vermutete „unnötige Push" existiert nicht.
   - [#86](https://github.com/munichbughunter/minds/issues/86) — `fsck` nennt
     Index-Kanten „vermutet". Fachlich richtig, liest sich aber wie ein Defekt.
     Genau die Art Befund, die nur auffällt, wenn jemand den Bericht liest, ohne
     den Code zu kennen.
   - Bestätigt: [#20](https://github.com/munichbughunter/minds/issues/20) fiel im
     Worktree sofort auf — dem Autor selbst. Das ist das Argument, es vor die
     Übergabe zu ziehen (Kommentar am Issue).

2. ~~**v0.1.1 taggen und veröffentlichen.**~~ **Erledigt am 10.08.** Release mit
   vier Binaries (macOS arm64/x86_64, Linux musl x86_64/aarch64) plus
   `SHA256SUMS`; `install.sh` liefert jetzt den aktuellen Stand. Der Selbsttest
   `minds fsck --gibtsnicht` in `TESTLAUF.md` ist damit hinfällig.

3. ~~**Entscheiden:** Kommt #83 noch in den Übergabestand?~~ **Entschieden am
   10.08.: ja, vorgezogen in v0.1.2** — Begründung in Abschnitt 3.

4. ~~**v0.1.2 fertigstellen**, in dieser Reihenfolge: #36 → #5 → #6 → #14 →
   #83.~~ **Erledigt am 12.08.** — Release v0.1.2 veröffentlicht; der letzte
   Bereichswechsel vor dem Übergabestand ist durch.

4b. **Das Security-Paket abarbeiten, dann v0.1.3 taggen** (entschieden am
   14.08., Abschnitt 3): drei Wellen plus #85, Reihenfolge ~~#12 → #4 → #49 →
   #95 → #69 → #102/#100 → #8 → #23~~ (gemerged) → ~~#71~~ (Branch wartet
   auf Merge) → **#26/#72/#92** (nächster Schritt) → #85. Der Tag wartet auf
   das Paket — der Übergabestand soll `label:security` ohne offene P1
   zeigen. Danach: Release mit vier Binaries + `SHA256SUMS` wie gehabt; die
   Installationszeile im Pilot-Leitfaden (`MINDS_VERSION=v0.1.3`) wird mit
   dem Tag scharf. Damit sind die Abschnitt-9-Entscheidungen zu #12, #49
   und #95 gefallen; offen bleiben #4/#8-Labels nur noch formal (beide
   werden gefixt statt umetikettiert) und [#20](https://github.com/munichbughunter/minds/issues/20)
   als Kandidat.

5. **Entscheiden — die fünf Konflikte mit der Abnahmeschwelle** (Abschnitt 6):

   - [#12](https://github.com/munichbughunter/minds/issues/12): einziges
     unverplantes P1. Vorziehen (v0.1.2 hinten oder v0.1.3) oder begründet
     heruntertriagieren. Pilotrelevant, sobald das Pilot-Repo auf GitLab liegt.
   - [#4](https://github.com/munichbughunter/minds/issues/4) /
     [#8](https://github.com/munichbughunter/minds/issues/8): Label senken
     (mit Begründung am Issue) oder Schwelle auf „P1 im ausgelieferten Pfad"
     präzisieren. Bei #4 spricht die Lokalität fürs Prüfen, solange #5/#6/#14
     den Store ohnehin öffnen.
   - [#49](https://github.com/munichbughunter/minds/issues/49): billige
     Härtung (0700 + fsync) — das Journal hält unredigierte Rohdaten. Kandidat
     zum Mitnehmen in v0.1.2/v0.1.3, auch fürs Datenschutz-Dokument.
   - [#95](https://github.com/munichbughunter/minds/issues/95): Ausgaben-Fix
     (billig, fällt teils mit #92 zusammen) oder Re-Triage.
   - [#20](https://github.com/munichbughunter/minds/issues/20): Kandidat für
     v0.1.3 — der Testlauf hat gezeigt, dass der Autor selbst sofort darüber
     stolperte. Fachlich Übergabestand, nicht Redaktion.

6. **Parallel und unabhängig davon:** Pilot-Vorschlag und Datenschutz-Übersicht an
   den Partner, mit den vier Fragen aus Abschnitt 5. Das ist weiterhin der Punkt mit
   dem höchsten Hebel — die interne Freigabe dauert länger als die restliche
   Codearbeit, und sie hat noch nicht angefangen.
