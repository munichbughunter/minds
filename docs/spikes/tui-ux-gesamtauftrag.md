# minds TUI — UX-Überarbeitung: Auftrag und Spezifikation

Ein Dokument, zwei Teile:

- **Teil A** ist der nächste ausführbare Auftrag (Batch 2): sieben Pakete in fester Reihenfolge, drei Stopp-Punkte.
  Damit beginnst du.
- **Teil B** ist die Spezifikation aller Pakete (A1–A6, B1–B7, C1–C11, D, K1–K2) mit Invarianten, Design-Regeln
  und der Klassenregel für bestehende Tests. Teil A verweist auf sie; nach Batch 2 folgt Batch 3 (C10, C11) aus Teil B.

Stand: 17.10.2026, `main` @ `c6f7c19`. Reihenfolge der Umsetzung: **Teil A → C12.2 → C10 → C11 + C12.1 → C12.3 → Rest von Teil B nach Bedarf.**

---

# Teil A · Nächster Auftrag (Batch 2): Transkript-Lesefehler, Breadcrumb, Changes v2

Basis: `main` @ `c6f7c19`. Die Pakete K1, K2 und C9 (inkl. Nachtrag C9.3) stehen in Teil B.
Teil A ist die ausführbare Reihenfolge dazu. Wo Teil A von Teil B abweicht, gilt Teil A.

## Rahmen

- Branch `feat/batch-2-transcript-changes`, ein Paket = ein Conventional Commit, Skill `.claude/skills/feature/SKILL.md`.
- Vor jedem Paket: Plan in 5 Zeilen (Dateien, Ansatz, Tests). Nach jedem Paket: CI-Triade grün, Bericht mit
  TestBackend-Render vorher/nachher (160×40, Demo-Fixture), Eintrag in `CHANGELOG.md`.
- Tests nach der Klassenregel aus Teil B („Umgang mit bestehenden Tests“) (Klasse 1 nie ändern, Klasse 2 nur mit Begründung und negativer Assertion).
- **Stopp-Punkte**: nach Paket 0 (Diagnose), nach Paket 1 (K1-Fix), nach Paket 5 (Why-Box). Dort warten.
- Doc-Comments Deutsch, UI Englisch. Nichts außerhalb des Auftrags „mitreparieren“ — auffällige Funde in den Bericht.
- Das Demo-Repo liegt beim Nutzer unter `<DEMO>` (Pfad erfragen, falls nicht bekannt). Dort nur **lesen**,
  nie committen, nie `minds forget` ohne Freigabe.

---

## Paket 0 · Diagnose im Demo-Repo (kein Code, nur Befund)

Ziel: Zwei Fragen mit Belegen beantworten, bevor irgendetwas gefixt wird.

### 0.1 · Warum ist das Modell nicht aufgezeichnet? (K1)

Mechanik: `minds-capture/src/adapter.rs::read_transcript` nimmt den `transcript_path` des letzten Journal-Events,
das einen trägt, liest die Datei; jeder Fehler wird still zu `Transcript::default()` (Modell, Antworttext,
Token fehlen dann). Der Pfad kommt aus dem Hook-Payload (`hook_event.rs::scan(…, "transcript_path")`), das
Journal liegt unter `<DEMO>/.git/minds/journal/`.

Schritte, jeweils Ausgabe in den Bericht:
1. `cd <DEMO> && minds doctor` — alle Zeilen, die Hooks, Witness oder Transkript betreffen.
2. Session finden: `minds show HEAD --full` → Session-Id `b3-75f030fa…`, `lineage.local_id` (Claude-Session-Id)
   und `lineage.cwd`.
3. Journal prüfen: Events der Session unter `.git/minds/journal/` lesen (Format siehe `journal.rs`).
   Trägt **irgendein** Event `transcript_path`? Welchen Wert? Welche `raw_kind`s kommen vor (erwartet:
   `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`, `SessionEnd`)?
4. Transkript prüfen: existiert `~/.claude/projects/<slug>/<local_id>.jsonl`? Slug nach
   `import.rs::claude_slug(repo_root)`. `ls -la` (Rechte, Größe, mtime), erste `assistant`-Zeile mit `message.model`.
5. Wer baut die Session? `minds checkpoint` (`checkpoint.rs`) läuft im Git-Hook beim Commit **oder** im
   Hook-Prozess bei `SessionEnd`/`Stop` — welcher Pfad hat hier gebaut? Zeitstempel von Journal-Ende, Transkript-mtime
   und Session (`lineage.ended_at`) vergleichen: lag das Transkript zum Bauzeitpunkt vollständig vor?
6. Reproduktion ohne Demo-Repo: ein Unit-Test in `adapter.rs`, der `build` mit einem Event ohne
   `transcript_path` und einem mit nicht existierendem Pfad aufruft und zeigt, dass beide stumm `unknown` ergeben.

**Erwartetes Ergebnis:** genau eine der Ursachen (a) kein Event trägt den Pfad, (b) Datei fehlt / nicht lesbar,
(c) Datei existiert, enthält aber kein `message.model` (z. B. noch leer beim Bauzeitpunkt), (d) etwas anderes —
mit Beleg.

### 0.2 · Woher kommen die zwei fremden `open`-Sessions? (K2)

Die Overview listet Sessions aus `refs/minds/…` dieses Repos ohne Commit. Prüfen:
1. `git for-each-ref refs/minds/` — Anzahl, Datum der Refs.
2. Für die beiden Sessions mit Prompt „Setze LOG-2417 …“: `lineage.cwd`, `lineage.started_at`, Herkunft
   (`capture`-Status der Tool-Calls: Import oder Hook?).
3. `~/.claude/projects/<slug>/` — liegen dort Transkripte älter als das Demo-Repo? Wurde `minds import` ausgeführt
   (Shell-History, `git reflog refs/minds/…`)?
4. `.git/config` — ist ein Child-Repo-Store konfiguriert?

**Ergebnis:** Ursache + der genaue `minds forget`-Befehl je Session (nicht ausführen).

**STOPP.** Bericht abgeben, auf Freigabe warten.

---

## Paket 1 · K1: Der Grund für ein ungelesenes Transkript wird aufgezeichnet und angezeigt

`feat(capture): record why the transcript was not read` + `feat(tui): show the transcript status instead of unknown`

### Capture (`minds-capture`, `minds-core`)
- Neues additives Feld `Lineage::transcript: Option<TranscriptStatus>` (`minds-core/src/lineage.rs`),
  `#[serde(skip_serializing_if = "Option::is_none")]`, Doc-Comment auf Deutsch, Regel wie bei `closed`:
  `None` wird nicht geschrieben, alte Sessions behalten Bytes und Id.
- `TranscriptStatus` (Enum, kein Freitext): `Read`, `NoPathInEvents`, `NotFound`, `NotReadable`,
  `Empty` (gelesen, aber kein `message.model`/kein Assistant-Eintrag), `ParseFailed`.
  **Nie den Pfad speichern** (er enthält den Benutzernamen, PII — siehe `Lineage::cwd`).
- `adapter.rs::read_transcript` liefert `(Transcript, TranscriptStatus)`; `build_with_transcript` schreibt den
  Status in die Lineage. Importpfad (`import.rs`) setzt `Read`.
- Fixture-Tests: alle vier Fehlerfälle erzeugen den passenden Status; `Read` bei lesbarem Transkript mit Modell.
  Determinismus-Tests (gleiche Events → gleiche Id) bleiben grün.
- Wenn Paket 0 Ursache (c) ergab: zusätzlich im Checkpoint beim `Stop`/`SessionEnd`-Pfad das Transkript **nach**
  dem Event erneut lesen, falls beim ersten Lesen `Empty` (eine Wiederholung, kein Polling); Begründung im Commit.

### Reader / TUI
- `minds-reader`: `SessionCard` (bzw. das Modell, das Agent/Modell liefert) bekommt `transcript: Option<TranscriptStatus>`
  und liefert das Modell als `Option<Model>` statt des Strings `unknown`. Der Platzhalter `unknown` wird **im Reader**
  erkannt (`minds_capture`-Konvention), nicht in der View.
- TUI: überall, wo heute `unknown` oder `model not recorded` steht (Overview-Detail, Changes-Why-Box
  `Agent  claude-code · unknown`, Graph-Kopf, Activity-Liste): `model not recorded — <Grund>` mit Glossar-Satz je
  Status, z. B. `NoPathInEvents` → „the hook events carried no transcript path“, `NotFound` → „the transcript file
  was not found at checkpoint time“. Bei `Read` das Modell vollständig: `claude-opus-4-1 (Anthropic)`.
- Test (Klasse 1, neu): ein Render **jeder** View mit einer Session, deren Modell der Platzhalter ist, enthält das
  Wort `unknown` nirgends.
- `docs/commands.md` (Abschnitt `show`/`inspect`) und `docs/verification-guide.md`: den Status in einem Absatz erklären.

**STOPP.** Render der Overview und der Why-Box mit einer Session aus der Demo-Fixture, die den neuen Status trägt.

---

## Paket 2 · Breadcrumb wächst bei jeder Aktion (Blocker)

`fix(tui): navigation history never stacks the same origin`

Befund: `app.rs` (~Z. 1555) legt bei jedem tabüberschreitenden Sprung einen `NavEntry` ab; ein Tab-Wechsel per
F-Taste/`1–5` räumt nichts auf. `d`, F1, `d`, F1 … ergibt `Overview › 609aca8 › Overview › 609aca8 › …`.

Regeln:
1. Wechselt der Nutzer per Tab-Taste auf einen Tab, der im Verlauf als Ursprung liegt, werden alle Einträge
   **ab diesem** entfernt (er ist zurück am Ursprung, der Sprung ist beendet).
2. Vor jedem Push: ist der letzte Eintrag gleicher Tab + gleicher Ort (`place(...)`), wird nicht gepusht.
3. Der Breadcrumb zeigt nie zweimal denselben Ort hintereinander.
4. Max. 8 Einträge; älteste fallen weg.

Tests: die Sequenz `d, F1, d, F1, d` ergibt `Overview › 609aca8 › Changes › 609aca8`; `d, Esc` leert den Verlauf;
bestehender Test `verify → enter → w → esc → esc` (B2) bleibt grün.

---

## Paket 3 · C9.1: Changes landet auf der ersten Zeile, Cursor sichtbar

`feat(tui): changes lands on the first line with the why panel filled`

- `App::open_changes` (bzw. der Pfad hinter `Action::Diff` und Overview-NEXT): Fokus `Focus::Diff`, Cursor auf der
  ersten **Code**zeile des ersten Hunks (nie Header). Kommt der Sprung aus NEXT mit Zustand „reported/not observed“,
  auf der ersten Zeile mit diesem Zustand.
- Cursor: `theme::cursor()` über die ganze Diff-Zeile (Gutter bis Zeilenende). Der fokussierte Pane bekommt den
  hellen Rahmen aus B7, der andere gedimmt. `Esc` im Diff-Fokus → Fokus FILES (wie heute, „Esc files“ bleibt).
- `n`/`N`: nächste/vorige Zeile, die nicht `● observed` ist. Fußzeile: `n next not-observed (29)`; bei 0 fällt
  die Taste aus der Fußzeile. Interne Bezeichnung weiterhin „unexplained“, wenn der Reader es so nennt — nur das Wort
  in der Fußzeile folgt dem Zustand.
- Hunk-Header-Zeile: Why-Box zeigt die Datei-Zusammenfassung (`src/sort.py · 29 added lines · 29 reported by one
  Write (turn 2) · 0 observed`), nie nur den Tastenhinweis.
- Klasse 2: Tests, die die Landung auf FILES/Header oder den Satz „Hunk header — ] / [ …“ festschreiben
  (negative Assertion ergänzen).

---

## Paket 4 · C9.2: Layout nach Breite, kein Zeilen-Hintergrund

`feat(tui): changes layout by width, diff without line background`

- Breakpoints: ≥ 160 → FILES 28 · Diff Rest · WHY 56; 120–159 → FILES als Zeile über dem Diff
  (`src/sort.py +29 · src/test_sort.py +66`, `Tab` wechselt), WHY 48; < 120 → FILES-Zeile + Diff, WHY als Ebene
  per `Enter` auf der Zeile (Esc zurück).
- `+`/`-` nur als Vordergrundfarbe auf Marker und Zeilennummer; kein Zeilen-Hintergrund (reserviert für den Cursor).
- Gutter-Glyph gedimmt, solange er dem Dateizustand entspricht; Abweichungen in voller Farbe. Prüfen: Monochrom-Test
  (C6) bleibt grün, da Glyph + Wort erhalten sind.
- FILES-Titel: `FILES · 2 files · +95 -0` statt `FILES · <commit> · 1/1`.
- Tests 80/120/160: WHY nie unter Minimum, Diff bekommt Rest, keine `…` in Zeilen ≤ 80 Zeichen bei 160 Spalten.

---

## Paket 5 · C9.3: Why-Box in fester Reihenfolge, mit Satz und Brücke

`feat(tui): why-this-line in fixed order with the witness bridge`

Reihenfolge und Inhalt (leere Abschnitte entfallen), `view/changes.rs::draw_why`:

```
src/sort.py:28  ◇ REPORTED
The agent's Write tool reported this content. No hash was
recorded, so minds cannot prove the file on disk matched.
With a witness this line would be ● observed.

WRITTEN BY   ◉CC claude-code · turn 2 · 10:39:09Z
             Write src/sort.py (29 lines)
SESSION      b3-75f030fa · ◈ record sealed
REQUIREMENT  ⚑ ac8f1bb7 · in scope src/**
ASKED FOR    ◌ "Implementiere die Anforderung aus …"
AGENT SAID   ◌ "…"                      (fehlt → "— (transcript not read: <Grund>)")
REVIEW       ◦ not reviewed yet
```

- **Jeder Zustand hat seinen Satz**: `observed`, `fs only`, `reported`, `not observed` (die bestehenden Sätze für die
  Unexplained-Gründe bleiben wörtlich; neu sind die Sätze für `reported` und `fs only`). Die Brücke „With a witness
  this line would be ● observed“ nur bei `reported`/`fs only`.
- Keine Tastenhinweise im Panel; Commit/Change-Id nur als Kurzform in einer gedimmten Schlusszeile
  (`commit 609aca8 · change I8d166b6d5e7…`).
- Zeit: `10:39:09Z` + relativ, nicht RFC 3339 mit Millisekunden (Helfer aus C4/`view/mod.rs::when` verwenden oder dort ergänzen).
- Anker-Id: **eine** Kurzform überall (die der Overview, `ac8f1bb7`); Herkunft („witness event“/„local file“) als Zusatz.
- `Enter` auf einer Codezeile öffnet den Graph der Session mit Cursor auf diesem Tool-Aufruf (Rückweg per Esc über
  den Verlauf aus Paket 2). Dafür braucht der Reader die Zuordnung Zeile → (Session, Tool-Call-Index); sie steht in
  `LineRecon`/`reason` bereits bereit — sonst stoppen.
- Review ohne Gelb (`◦ not reviewed yet`), auch in Fußzeile Zeile 1 des Changes-Tabs.
- Klasse 1 (bleiben): „heuristic, not evidence“, „unverified“, „Removals are not attributed line by line“,
  „Minds cannot attest this line“.
- Klasse 2: Snapshots auf Reihenfolge/Labels `Session`, `Agent`, `Step`, `At`, `Intent`, auf `REPORTED ONLY` ohne Satz.

**STOPP.** Render bei 160×40 und 120×30: Cursor auf Zeile 5 und auf dem Hunk-Header.

---

## Paket 6 · C9.4: Ein Wort je Evidenz-Zustand, tabübergreifend

`fix(tui): one word per evidence state across tabs`

- Zustandswörter: `● observed` · `◍ fs only` · `◇ reported` · `◦ not observed`. Im Theme/Glyph-Register führen,
  Hilfe-Legende daraus erzeugt.
- Overview EVIDENCE/COVERAGE (C3.4/C3.6): `◇ 95 of 95 added lines reported, not observed` statt „stated“; die Zeile
  `· 95 added lines` entfällt (`·` ist „trifft nicht zu“). Grund-Satz unverändert („claimed without a hash“ bleibt Satz).
- Die Zahlen in Overview-Detail, FILES-Titel und Diff-Legende kommen aus **einer** Reader-Funktion; Test: dieselbe
  Fixture liefert an allen drei Stellen dieselben Werte.
- Klasse 1: `◇ reported only`-Assertions bleiben.

---

## Abschluss

- `docs/commands.md`, Abschnitt **Changes** und **Overview**, an das neue Verhalten anpassen.
- Abschlussbericht: je Paket Commit-Hash, geänderte Klasse-2-Tests (alt → neu · Grund), offene Funde außerhalb
  des Auftrags, und die drei Renders (Overview-Detail, Changes-Landung, Why-Box) aus der Demo-Fixture.

---

# Teil B · Spezifikation: UX-Überarbeitung der TUI (`minds inspect`, Crate `minds-tui`)

Stand der Analyse: `main` @ `993f794` (14.10.2026). Alle Fundstellen beziehen sich auf diesen Stand —
prüfe sie vor jeder Änderung gegen den aktuellen Code.

## So arbeitest du mit diesem Dokument

1. Lies dieses Dokument vollständig, dann `crates/minds-tui/src/` (zuerst `lib.rs`, `theme.rs`, `app.rs`, `view/mod.rs`).
2. Arbeite **Paket für Paket in der angegebenen Reihenfolge**. Ein Paket = ein Conventional Commit
   (`feat(tui): …`, `fix(tui): …`, `refactor(tui): …`). Nutze `.claude/skills/feature/SKILL.md`.
3. Vor jedem Paket: kurzer Plan (Dateien, Ansatz, betroffene Tests). Nach jedem Paket: CI-Triade
   (`cargo fmt`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`) und ein
   kurzer Bericht inkl. eines gerenderten Ausschnitts (TestBackend) vorher/nachher.
4. **Stopp und frag nach**, wenn ein Paket eine der Invarianten unten verletzen würde, wenn Daten im
   `minds-reader` fehlen, oder wenn ein Vorschlag hier dem Code widerspricht.
5. Doc-Comments auf Deutsch, UI-Texte auf Englisch (wie bisher).
6. Kein Paket darf Funktionalität entfernen, ohne dass sie anders erreichbar bleibt.

## Umgang mit bestehenden Tests (verbindlich)

Bestehende Tests **im gesamten Crate `minds-tui`** (`view/tests.rs` und die Unit-Tests in `input.rs`,
`layout.rs`, `theme.rs`, `view/activity.rs` …) sind **nicht pauschal unveränderlich**. Sie fallen in zwei Klassen:

**Klasse 1 — Versprechens-Tests: nie ändern, nie abschwächen.** Sie schützen eine Invariante.
Kollidiert ein Paket mit ihnen, hat das Paket unrecht — passe das Paket an und melde es. Beispiele:
- `without_a_checked_verdict_neither_verified_nor_incomplete` (kein VERIFIED ohne Signaturprüfung)
- `the_verdict_is_the_one_of_minds_verify` (die TUI benutzt **die Urteilswörter der CLI**)
- `an_inferred_edge_never_looks_like_an_observed_one`
- `every_epistemic_state_differs_in_glyph_or_word_never_only_in_color`
- alle Tests mit „kein ✓ … neben …“, „nie VERIFIED …“, „never only in color“ im Namen oder Kommentar

**Klasse 2 — Darstellungs-Snapshots: anpassen, wenn das Paket genau diese Darstellung bewusst ändert.**
Sie frieren einen Glyph oder String ein, den die Spec ändert (z. B. `"✓ INTENT"`, `"◇ READ …"`,
`"◆ CHANGE I"`, `" ◆CC "`, `"● HEAD"`, `"◇ sealed · unsigned"`). Regeln:
1. Nur im selben Commit wie die Darstellungsänderung, nie „vorab“.
2. Die **Absicht** des Tests bleibt erhalten oder wird stärker — der Kommentar über der Assertion ist maßgeblich
   (Beispiel: `why_shows_the_chain…` kommentiert „Aussage != Beweis“; `"✓ INTENT"` → `"◌ INTENT"` erfüllt
   diese Absicht besser als vorher).
3. Für jede geänderte Assertion zusätzlich eine **negative** Assertion auf den alten Zustand, wo er falsch wäre
   (z. B. `!out.contains("✓ INTENT")`).
4. Im Commit-Body: Liste „Test · alt → neu · Begründung“.
5. Der Reviewer-Subagent prüft ausdrücklich, dass keine Klasse-1-Assertion berührt wurde.

Im Zweifel, welcher Klasse ein Test angehört: Klasse 1 annehmen und nachfragen.

**Entschiedene Einzelfälle:**
- `a_fully_backed_chain_says_so…` (`view/tests.rs`): `" NO GAP "` ist **Klasse 2** (A6 entfernt den Block bei 0 Gaps).
  Das Versprechen des Tests — „eine lückenlose Kette **sagt** das“ — trägt `"✓ no gap"` (Fußzeile); diese Assertion
  ist **Klasse 1** und bleibt wörtlich. Daraus folgt: A2 und C2 müssen den Fußzeilentext der Why-Ansicht mit
  `✓ no gap` beginnen lassen, und er darf in 80/120/160 Spalten nicht abgeschnitten werden.
  Ersatz für `" NO GAP "`: `!out.contains(" NO GAP ")`; der Gap-Block bei Gaps > 0 bleibt über `"⚠ 2 gaps in the chain"` abgedeckt.
- `input.rs`:
  - **Klasse 1:** `ctrl_c_always_quits`, `while_searching_letters_go_into_the_query`, `vim_and_arrow_keys_agree`.
    Für B4 wird der Such-Test **erweitert**: Ziffern (`'1'`–`'5'`), `z`/`Z`, `v`, `d`, `i`, `c` landen im Suchmodus in der Suche.
  - **Klasse 2:** die Zuordnungstabelle in `every_command_key_has_its_action` (`'1'`–`'3'` → Zoom wird zu Tabs,
    `z`/`Z` → Zoom, neue Verben). Jede bisherige Taste bleibt in der Tabelle — mit neuer oder unveränderter Aktion —, keine verschwindet stillschweigend.
- `layout.rs`: `zoom_digits_map_and_unknown_digits_keep_the_level` ist **Klasse 2** (B4); die übrigen Layout-Tests sind **Klasse 1**.
- `theme.rs`: `agent_color_is_deterministic_and_total` ist **Klasse 1** (Farbe über Versionen stabil).
- `view/activity.rs`: `every_tier_fits_at_its_threshold` ist **Klasse 1** im Versprechen („jede Stufe passt an ihrer Schwelle“);
  ändern sich in A6 die Konstanten, bleibt der Test unverändert und muss mit den neuen Werten grün sein.

## Ziel

Die TUI soll für **jeden Menschen** verständlich sein — nicht nur für jemanden, der ADR-0011 kennt.
Messlatte ist der **10-Sekunden-Test**: Jemand ohne Vorwissen öffnet `minds inspect` und versteht
innerhalb von 10 Sekunden

- was er gerade sieht (Ort),
- was davon wichtig ist (Zustand),
- was er als Nächstes tun kann (Aktion).

Jede Ansicht beantwortet genau **eine** Frage:

| Ansicht   | Frage                                                         |
|-----------|---------------------------------------------------------------|
| Overview  | Was ist in diesem Repository passiert?                        |
| Sessions  | Wie ist eine Änderung entstanden?                             |
| Why       | Warum existiert genau diese Zeile / dieser Commit?            |
| Evidence  | Wie belastbar ist der Nachweis einer Session — und wo endet er?|
| Verify    | Was lässt sich für diesen Commit tatsächlich beweisen?        |
| Changes   | Welche Zeilen sind erklärt, welche nicht?                     |
| Intent    | Welche Absicht wurde festgehalten, und ist ihre Bindung belegt? |

## Unverletzbare Invarianten (bestehen bereits — nicht aufweichen)

- **Strikt lesend**, nur über `minds-reader`. Aggregationslogik gehört in `minds-reader`, nicht in die TUI.
- **Nie mehr behaupten als `minds verify`**. VERIFIED nur nach gelaufener Signaturprüfung.
- **Die drei Achsen Integrität / Coverage / Deutung werden nie zu einem Wert, Score oder einer
  Ampel zusammengefasst.** Auch keine abgeleitete Kennzahl wie „zu prüfen“.
- **Farbe trägt nie allein**: jede Bedeutung hat Glyph **und** Wort.
- **Claim ≠ Evidence**: Was der Agent aufgezeichnet hat (Intent, Begründung, verworfene Ansätze),
  darf nie wie ein Beleg aussehen.
- **Unerklärt ist kein Fehler** (kein Rot) — **Manipulation ist einer** (immer Rot, immer laut).
- **Fail-soft** und **Pipe-Modus** (`pipe.rs`) bleiben unverändert funktionsfähig.

## Design-Regeln für alle Pakete

1. **Ergebnis zuerst, Details danach**: Identität → Ergebnis → Zusammenhang → technische Details.
2. **Ein Zustand, eine Darstellung**: Derselbe Zustand sieht überall gleich aus und heißt überall gleich.
3. **Wenn alles hervorgehoben ist, ist nichts hervorgehoben**: Standardzustände (z. B. Review „open“)
   sind leise; nur Abweichungen sind laut.
4. **Fußzeile = zwei Zeilen mit festen Rollen**: Zeile 1 *Kontext* (wo bin ich, was ist hier wichtig),
   Zeile 2 *Aktionen* (was kann ich jetzt tun). Keine statischen Erklärsätze, keine Legenden.
5. **Begriffe werden im Kontext erklärt**, nicht nur in der Hilfe (z. B. „sealed“ ≠ „verified“).
6. **Responsiv mit Prioritäten**: Bei wenig Platz fällt zuerst Sekundäres weg — nie der Vertrauenszustand,
   nie `? help`, nie der Ausgang.
7. **Gleiche Taste, gleiche Bedeutung** — in jedem Tab.

## Test-Konvention (für jedes Paket)

- Render-Tests über `TestBackend` (siehe Helfer `render_at` in `view/tests.rs`) in **drei Größen**:
  80×24, 120×30, 160×40.
- Für jede Größe und jeden Tab prüfen: Tab-Leiste vollständig sichtbar, `? help` sichtbar,
  Vertrauenszustand des fokussierten Objekts sichtbar, kein Text ohne `…` hart abgeschnitten.
- Bestehende Tests: siehe „Umgang mit bestehenden Tests“ (Klasse 1 nie ändern, Klasse 2 nur mit Begründung).

---

# Phase A — Ehrlichkeit und Lesbarkeit (vor der Loom-Demo)

## A1 · Glyph-Register: eine Bedeutung pro Glyph

**Problem.** `theme.rs` verspricht „eine Farbe und ein Glyph je Bedeutung“, hält es aber nicht:

| Glyph | Bedeutungen heute |
|---|---|
| `◆` | content-Evidence · CHANGE · COMMIT · Agent-Badge (Overview, ohne Nerd Font) |
| `◇` | declared-Evidence · READ · reported-only (Recon) · Seal unsigned/partial/incomplete (Overview) |
| `●` | observed-Evidence · YOU/Intent-Knoten · explained (Recon) · Bullet jeder Listenzeile · HEAD-Pille · SESSION-Glied in Why |
| `✓` | recomputed · approved · VERIFIED · „kein Gap“ (Why-Kette) |
| `·` | unlinked · legacy · TURN · TOOL · unsealed · hash-valid |
| `✕` | DELETE · rejected |

**Änderung.**
- Führe in `theme.rs` ein zentrales **Glyph-Register** ein (eine Tabelle/Enum aller Glyphen mit Kategorie).
- Trenne zwei Klassen: **epistemische** Glyphen (Evidence-Quelle, Seal-/Provenance-Zustand, CLAIM,
  Recon-Klasse) und **strukturelle** Glyphen (Knotenarten, Effekte, Refs, Badges).
  Epistemische Glyphen sind **exklusiv**: Kein strukturelles Element darf eines davon verwenden.
- Vorschlag für die Neuzuordnung (darfst du begründet anpassen):
  - Evidence-Quellen behalten `● ◆ ◇ ○ ·`.
  - Intent/YOU-Knoten → `◌` (er *ist* ein Claim — gleiche Bedeutung, gleiches Glyph wie `theme::claim`).
  - CHANGE/COMMIT → eigenes Glyph (z. B. `■`), nicht `◆`.
  - READ → eigenes Glyph (z. B. `→`), nicht `◇`.
  - Review rejected → `⊘` (DELETE behält `✕`).
  - Agent-Badge in der Overview → `◉` (wie AGENT im Graph), nicht `◆`.
  - Listen-Bullet `● ` vor jeder Session in `view/activity.rs::row_of` **ersatzlos streichen** (trägt keine Information).
  - Overview-Seal-Pillen (`view/overview.rs::pills`) verwenden die Glyphen aus `theme::provenance`
    statt eigener (`◇ incomplete` vs. `! incomplete`, `✗ tampered` vs. `✗ TAMPERED`). Wird in A4 ohnehin ersetzt.
- Nur Unicode, das in gängigen Monospace-Fonts sicher ist; keine Emoji; keine Doppelbreite.

**Akzeptanz.**
- Neuer Test: Über alle Theme-Funktionen hinweg kommt jedes epistemische Glyph in genau einer Bedeutung vor;
  kein strukturelles Glyph ist epistemisch. (Erweitert `every_epistemic_state_differs_in_glyph_or_word_never_only_in_color`.)
- Die Legende der Hilfe wird aus dem Register erzeugt (siehe A6), nicht mehr von Hand gepflegt.

**Betroffene Snapshot-Tests (Klasse 2):** `enter_opens_the_graph_and_the_zoom_levels_fold_the_lane`
(`"◇ READ …"`, `"◆ CHANGE I"`), `the_list_and_the_graph_preview_share_the_screen` (`"◇ READ …"`),
Overview-Tests mit `" ◆CC "`, `"● HEAD"`, `"◇ sealed · unsigned"`. Nicht anfassen: alle Assertions auf
Evidence-Glyphen (`"● ?"`, `"● ✓"`, `"◇ reported only"`) — die Evidence-Glyphen bleiben ja gleich.

**Schnitt:** A1 in zwei Commits teilen — (a) Register + Eindeutigkeits-Test, nur Strukturglyphen ändern;
(b) Overview-Badges/-Pillen. So bleibt jeder Diff an den Tests klein und prüfbar.

## A2 · Claim und Evidence in der Why-Kette trennen

**Problem.** `view/why.rs::draw` setzt vor **jedes** Glied ohne Gap ein grünes `✓` — auch vor INTENT,
direkt über dem CLAIM-Label. Eine Aussage des Agenten liest sich damit als geprüft. Das ist genau der
Einwand „signiert, aber nicht bewiesen“.

**Änderung.**
- Der Glied-Marker unterscheidet drei Fälle: **belegtes Glied** (`✓`), **Gap** (`⚠`), **Claim-Glied**
  (`◌` im Claim-Stil aus `theme::claim`, kein Grün). INTENT ist immer ein Claim-Glied.
- Prüfe auch REVIEW-Notizen und das SESSION-Glied: Was nur aufgezeichnet ist, bekommt den Claim-Marker.
- Fußzeile und Gap-Block formulieren „every link is attested“ so, dass Claims ausdrücklich nicht
  mitgemeint sind (z. B. „no gap in the evidence links · intent is a recorded claim“).

**Akzeptanz.** Render-Test: Das INTENT-Glied zeigt nie `✓` und nie die OK-Farbe; ein belegtes
EVIDENCE-Glied zeigt `✓`.

**Betroffene Snapshot-Tests (Klasse 2):** `why_shows_the_chain_and_a_missing_link_is_named_not_hidden`
(`"✓ INTENT"` → `"◌ INTENT"` plus `!contains("✓ INTENT")`). `"✓ AGENT"`, `"✓ EVIDENCE"`, `"⚠ SESSION"` bleiben.
`a_fully_backed_chain_says_so…` (`"✓ no gap"`) nur anpassen, wenn sich der Fußzeilen-Text ändert.

## A3 · Verworfene Ansätze und Constraints sichtbar machen

**Problem.** Das Kernversprechen („welche Alternativen wurden verworfen?“) ist praktisch unsichtbar:
- `view/why.rs::describe` zeigt nur `Discarded: N` und `Constraints: N`.
- `view/graph.rs::graph_detail` zeigt von jedem Detailwert nur `v.lines().next()` — vom Prompt, den
  Constraints und den verworfenen Ansätzen also nur die **erste Zeile** (der Reader liefert sie
  mit `\n` verbunden, `minds-reader/src/graph.rs`).
- Die YOU-Box im Graph zeigt den Auftrag einzeilig und abgeschnitten.

**Änderung.**
- Why, INTENT-Glied: Prompt (bis 4 Zeilen), dann Abschnitte **Constraints** und
  **Considered & discarded** als Liste (je Eintrag eine Zeile, geclippt mit `…`; bei mehr als 5:
  `… N more`). Alles im Claim-Stil, eingeleitet mit „◌ CLAIM — as recorded by the agent“.
- Graph-Detail: mehrzeilige Werte vollständig (umgebrochen, gekappt mit Hinweis), nicht nur die erste Zeile.
  Die Detailbox darf für den Intent-Knoten höher werden als die heutigen 12 Zeilen, wenn Platz ist.
- YOU-Box: bis zu 3 Zeilen Auftrag statt einer; darunter, falls vorhanden, eine gedimmte Zeile
  „◌ 3 discarded · 2 constraints — Enter on YOU for details“.
- Leer ist leer: Ohne verworfene Ansätze erscheint der Abschnitt nicht (kein „Discarded: 0“).

**Akzeptanz.** Render-Tests mit einer Session mit 3 verworfenen Ansätzen: alle drei sind in Why und
im Graph-Detail lesbar; Stil ist Claim, nie OK-Grün.

## A4 · Eine Komponente für den Vertrauenszustand („Trust Strip“)

**Problem.** Derselbe Zustand erscheint heute an sechs Stellen mit vier Vokabularen:
SEAL-Spalte (`activity.rs`), Footer-Badge (`view/mod.rs::footer`), Graph-Kopf (`graph.rs`),
Overview-Pillen (`overview.rs::pills`), Verify-Kopf (`verify.rs::headline`), Evidence-Kopf (`evidence.rs`).
Beispiele: `! incomplete` / `◇ incomplete`, `✗ TAMPERED` / `✗ tampered`, `hash-valid`,
`! VERIFIED, INCOMPLETE` (widersprüchlich).

**Änderung.**
- Neues Modul `view/trust.rs` mit **einer** Darstellung, drei festen Slots in fester Reihenfolge:
  **Integrity · Coverage · Interpretation**. Nie verschmolzen, nie als Score.
- Drei Dichten derselben Komponente:
  - `Glyphs` (Listen, Overview): drei Glyphen, z. B. `◈ ● ◐` — je Slot genau ein Zeichen.
  - `Short` (Köpfe, Footer): `◈ intact · ● complete · ◐ 2 uninterpreted`.
  - `Full` (Verify, Evidence): eine Zeile je Achse mit Satz und Grenze.
- Die Eingaben kommen aus dem Reader (`SessionCard::provenance`, `evidence_state()`,
  `uninterpreted_calls`; für Commits `VerifyState`). Fehlt eine Achse als fertiger Wert im Reader,
  ergänze sie **dort**, nicht in der TUI.
- Ersetze alle sechs Stellen durch die Komponente. **Review bleibt eine eigene, getrennte Angabe**
  (menschliches Urteil, keine Vertrauensachse).
- Einheitliches Vokabular (in Paket C1 als Glossar festgehalten). **Quelle der Wahrheit für Urteilswörter
  ist `minds verify`** (`minds-cli/src/verify_cmd.rs`): `VERIFIED`, `VERIFIED, INCOMPLETE`, `TAMPERED`,
  `NOT VERIFIABLE` und `hash-valid` bleiben wörtlich so. Vereinheitlicht werden nur die **abweichenden
  TUI-eigenen** Varianten (z. B. `◇ incomplete` vs. `! incomplete`, `✗ tampered` vs. `✗ TAMPERED`).
  Wirkt ein CLI-Wort missverständlich, erklärt es der Glossar-Satz (C1) — umbenannt wird es nur in der CLI
  und TUI zugleich, als eigenes Paket außerhalb dieses Auftrags.
- Die `Full`-Dichte in Verify behält Labels und Reihenfolge der bestehenden Zeilen (`Integrity`, `Coverage`, …),
  weil Klasse-1-Tests sie wörtlich prüfen.
- Review „open“ ist der Normalzustand: in Listen und Overview **nicht** als Pille/Spalte zeigen,
  nur abweichende Zustände (approved, needs work, rejected).

**Akzeptanz.**
- Ein Test rendert jede Kombination von Achsenzuständen in allen drei Dichten; jede Kombination ist
  in Glyph und Wort unterscheidbar.
- `grep` über `view/` findet keine eigenen Seal-/Verdict-Strings mehr außerhalb von `trust.rs` und `theme.rs`.

## A5 · Manipulation sichtbar machen (Tamper-Banner)

**Problem.** Kippt eine Session durch Live-Reload auf TAMPERED, ändert sich nur ein Badge unten links.
Der wichtigste Moment des Produkts ist leise.

**Änderung.**
- `App::reload` vergleicht den Provenance-Zustand vorher/nachher. Wird eine Session oder ein Commit
  neu TAMPERED, erscheint unter dem Kopf ein **Banner über die volle Breite** (Rot, invertiert, Glyph + Wort):
  `✗ TAMPERED — seal material changed since 12:34:56Z · session b3-…  ·  Enter: open  Esc: dismiss`.
- Enter springt zur betroffenen Session (Evidence-Ansicht); Esc schließt das Banner.
- Bei mehreren: „✗ 3 sessions TAMPERED“ mit Sprung zur ersten.
- Auch beim Start: Ist beim ersten Laden etwas TAMPERED, erscheint das Banner ebenfalls.

**Akzeptanz.** Test über `App::reload` mit einer manipulierten Fixture (`a_tampered_session_shows_the_red_badge_and_card` als Vorlage).

## A6 · Überläufe beheben: Hilfe, Fußzeile, Kopf, Liste

Alle Werte sind aus dem Code nachgerechnet:

- **Hilfe** (`view/help.rs`): 26 Zeilen Text, Overlay innen max. 24 Zeilen → der Intent-Abschnitt fehlt.
  Eine Zeile hat 86 Zeichen bei 82 Innenbreite. Die Legende ist veraltet (es fehlen `✓ ~ ? ✗`, `◈`,
  `◌ CLAIM`, `◐ OBSERVED`, `◍`).
  → Hilfe **scrollbar** (↑↓/PgUp/PgDn) oder zweispaltig ab ausreichender Breite; Inhalt **kontextabhängig**
  (aktueller Tab oben, Rest darunter); Legende **aus dem Glyph-Register erzeugt** (A1).
- **Fußzeile** (`view/mod.rs::footer`): Sessions-Liste bis 131 Zeichen, Graph bis 135 → unter ~130 Spalten
  verschwindet `? help  q quit`.
  → Tasten-Hinweise als priorisierte Liste; `? help` ist **rechtsbündig fixiert** und fällt nie weg;
  der Rest wird nach Priorität gekürzt (wie die `Columns`-Logik in `activity.rs`).
- **Kopf** (`view/mod.rs::header`): Titelzeile ~86 Zeichen → bei 80 Spalten fehlt der Intent-Tab.
  → Unter 100 Spalten Kurzlabels (`1 Ovw 2 Ses 3 Ver 4 Chg 5 Int`); Repo/Branch werden zuerst gekürzt, nie die Tabs.
- **Liste neben der Vorschau** (`view/mod.rs::split`, `LIST_SHARE_PERCENT`): `clamp(40 %, 64, 120)` →
  die Liste bleibt fast immer `Compact`; die VERDICT-Spalte erscheint erst ab ~243 Spalten Terminalbreite,
  die Überschrift bekommt bei 120–160 Spalten nur ~14 Zeichen.
  → Mit Trust Strip (3 Zeichen) und Agent als Initialen-Pill (wie Overview) bekommt die Überschrift den
  gewonnenen Platz. Ziel: ≥ 30 Zeichen Überschrift bei 120 Spalten im Split.
- **Graph-Kopf** (`graph.rs`): ~10 Fakten in einer Zeile, Verdikte am Ende werden zuerst abgeschnitten.
  → Reihenfolge: Trust Strip (Short) und Review zuerst, dann Akteur, Zeit, Umfang.
- **Why-Höhe** (`why.rs::draw`): zwei Zeilen `│ ▼` zwischen jedem Glied; NO-GAP-Block belegt 4 Zeilen;
  mit Inspector bleibt der Kette bei 30 Zeilen wenig Platz.
  → Einzeiliger Verbinder; Gap-Block **nur bei Gaps > 0** (die Fußzeile sagt „no gap“ bereits).

**Akzeptanz.** Test-Konvention oben (80/120/160) für alle Tabs grün.

---

# Phase B — Flow und Navigation

## B1 · Ein Esc-/q-Modell für alle Tabs

**Problem.** Esc beendet in der Sessions-Liste die App (`reduce_activity`), tut in der Overview im
Graph-Fokus nichts (`reduce_overview`), springt in Verify/Changes/Intent zu Sessions — nicht zum Ursprung.

**Änderung.** Feste Reihenfolge für Esc, in jedem Tab gleich:
1. Overlay/Banner schließen → 2. Suche leeren → 3. oberste Ebene schließen → 4. zurück zum Ursprung
   (B2) → 5. sonst nichts (Fußzeile zeigt `q quit`).
**Esc beendet nie.** Nur `q` und `Ctrl-C` beenden.

**Akzeptanz.** Tabelle-getriebener Test: für jeden Tab und jeden Zustand die Wirkung von Esc.

## B2 · Rückweg über Tab-Grenzen und Breadcrumb

**Problem.** Sprünge zwischen Tabs (`goto_session`, `why_of_row`, Enter in Verify/Intent/Evidence→ARTIFACT,
Why→Intent) verlieren den Ursprung. Es gibt keinen sichtbaren Pfad.

**Änderung.**
- Ein **globaler Navigationsverlauf**: Jeder tabübergreifende Sprung legt einen Eintrag (Tab + Fokus +
  Ebenen-Stapel des Ursprungs) ab. Esc (Schritt 4 aus B1) kehrt genau dorthin zurück, inkl. Cursor.
- **Breadcrumb** in der zweiten Kopfzeile, ersetzt die heutigen globalen Statistiken (die wandern in die
  Overview, siehe C3): `Overview › a1b2c3d › Session b3-4f… › Why`. Von links gekürzt mit `…`.

**Akzeptanz.** Test: Verify → Enter (Session) → `w` → Esc → Esc landet wieder in Verify auf derselben Zeile.

## B3 · Ein aktiver Commit für Overview, Verify und Changes

**Problem.** Overview-Cursor, Verify (`[ ]`) und Changes (`[ ]`) halten je einen eigenen Commit-Zustand
und laufen auseinander.

**Änderung.** Ein `active_commit` im `App`-Zustand. Overview-Auswahl setzt ihn; F3/F4 öffnen ihn;
`[ ]` verschiebt ihn tabübergreifend. Der Kopf zeigt ihn im Breadcrumb.

## B4 · Konsistente Tasten

**Problem.** `w` öffnet sonst die Why-Kette, schaltet in Changes aber eine Spalte um; `1/2/3` sind Zoom,
Tabs nur über F-Tasten (auf dem Mac nur mit `fn`); `/` funktioniert nur in der Sessions-Liste;
in Verify ist PgUp/PgDn = Home/End.

**Änderung.**
- **Tabs: `1`–`5`** (zusätzlich F1–F5 und Tab/Shift-Tab). **Zoom: `z` / `Z`** (feiner / gröber) — passe
  Hilfe, Fußzeilen und Tests an.
- **Objekt-Verben überall gleich**, auf jedem Element mit Session oder Commit:
  `w` why · `e` evidence · `v` verify · `d` diff · `i` intent. Was nicht passt, zeigt einen kurzen Hinweis
  statt still nichts zu tun.
- Changes: die Spalte „Why this line?“ bekommt eine eigene Taste (z. B. `c` für context); `w` öffnet die Why-Kette.
- **`/` global**: filtert in Overview Commits (Betreff, Autor, Agent, Ref), in Changes Dateien, in Intent Anker.
- PgUp/PgDn blättern überall seitenweise.

## B5 · Fokus beim Zoom- und Ansichtswechsel erhalten

**Problem.** `App::relayout` klemmt nur den Cursor-**Index**; nach einem Zoomwechsel steht der Cursor auf
einem anderen Knoten. Gleiches beim Umschalten Baum ↔ Timeline.

**Änderung.** Cursor über die Knoten-Id (`Row::node`) wiederherstellen; bei zusammengefassten Zeilen auf
die Zeile, die den Knoten enthält.

## B6 · Prüfungen nicht im Event-Loop

**Problem.** `fill_verify`, `fill_intents`, `fill_conformance` laufen synchron in `App::run`
(verify-Binary, ssh-keygen). Die UI friert, ein Fortschrittshinweis kann nicht animieren.

**Änderung (entschieden — additiv, kein Bruch von `Source`).**
`Source` bekommt **kein** `Send + Sync`: Das würde bis in die CLI durchschlagen (`Live` leiht `&Context`,
`IntentCache` nutzt `OnceCell`/`RefCell`, `Box<dyn ContextStore>`), und die Test-Fakes müssten umgebaut werden.
Stattdessen eine **optionale, additive** Erweiterung:

```rust
/// Eine Prüfung, die abseits des Event-Loops laufen darf. Hält nur eigene,
/// besessene Daten — öffnet, was sie braucht, im Worker selbst.
pub trait Background: Send {
    fn verify(&mut self, commit: minds_git::CommitId) -> Result<CommitVerify, String>;
}

pub trait Source {
    // … unverändert …
    /// `None` (Vorgabe): Prüfungen laufen wie bisher synchron.
    fn background(&self) -> Option<Box<dyn Background>> { None }
}
```

- **CLI** (`minds-cli/src/inspect.rs`): `Live::background` liefert einen Job, der nur besessene Daten hält
  (`root: PathBuf`, `PinnedExe` geklont) und im Worker-Thread seinen **eigenen** `Context` öffnet;
  dann `verify_commit` wie bisher. Kein geteilter Zustand, kein `Sync`.
- **TUI**: Ist `background()` `Some`, läuft `verify` im Worker (Thread + `mpsc`); sonst der heutige synchrone Pfad.
  Alle bestehenden Fakes liefern `None` → **bestehende Tests und Fakes bleiben unverändert**.
- **Ergebnisse sind gebunden**: Jede Antwort trägt Commit-Id und eine Generation (erhöht bei jedem `reload`).
  Passt sie nicht mehr zum aktiven Commit oder zur aktuellen Generation, wird sie verworfen — nie ein altes
  Urteil übernehmen (vgl. `reload_always_checks_the_signers_again`). Solange geprüft wird: „checking…“, nie VERIFIED.
- **Umfang**: zunächst nur `verify` (Subprozess + ssh-keygen = der teure Teil). `intents` und `conformance`
  bleiben synchron; erst wenn eine Messung > 100 ms zeigt, im Folgepaket auf dasselbe Muster heben.
- Das bestehende Drosseln (`VERIFY_EVERY`) bleibt. `reduce` bleibt rein und ohne Terminal testbar.
- **Neue** Tests mit einem neuen `Send`-Fake: Ergebnis kommt an; veraltetes Ergebnis wird verworfen;
  während der Prüfung kein VERIFIED; ohne `background()` identisches Verhalten wie bisher.

## B7 · Ein Auswahl- und Fokusstil

**Problem.** Vier Cursor-Stile: REVERSED (Liste, Graph, Why), `▌` + bg 236 + bold (Overview), `▸` (Verify, Intent),
dazu keine Fokusmarkierung für Panes (Changes: Files/Diff; Overview: Graph/Sessions).

**Änderung.** `theme::cursor()` als einziger Auswahlstil; der fokussierte Pane hat einen hervorgehobenen
Rahmen, unfokussierte Panes gedimmte Rahmen. Funktioniert ohne Farbe (Rahmenart bzw. Titel-Marker).

---

# Phase C — Verständlich für alle

## C1 · Glossar an einer Stelle, Erklärung im Kontext

- Neues Modul (z. B. `copy.rs`): jedes Statuswort mit **einem Satz** Bedeutung in einfacher Sprache.
  Pflicht-Unterscheidungen: *sealed ≠ verified*, *observed ≠ checked*, *claim ≠ evidence*,
  *not observed ≠ wrong*, *incomplete ≠ tampered*.
- Fußzeile, Zeile 1 zeigt für den fokussierten Zustand diesen Satz (heute schon so für Evidence
  über `evidence_sentence` — auf alle Zustände ausweiten). Die Hilfe nutzt dasselbe Glossar.
- Fachjargon in UI-Texten vermeiden oder beim ersten Auftreten erklären (`hash-valid`, `epoch`,
  `witness`, `conformance`, `A0…A3`).

## C2 · Fußzeilen nach der Zwei-Zeilen-Regel

- Overview, Verify, Intent: die statischen Erklärsätze in Zeile 1 (`view/mod.rs::footer`) entfernen.
  Die Overview-Legende („◆CC = agent session …“) wandert in die Hilfe.
- Zeile 1 = Kontext des fokussierten Objekts (Commit/Session + Trust Strip Short + der Glossar-Satz).
- Zeile 2 = die 4–6 häufigsten Aktionen, priorisiert (A6), `? help` rechts fixiert.
- Im Suchmodus haben Suchaktionen Vorrang.

## C3 · Overview v2 — der Einstieg beantwortet drei Fragen

*Ersetzt die frühere Fassung von C3. Grundlage: Screenshot der Overview auf `aeed801` mit der Demo-Fixture
(4 Commits, 1 Agent-Session).*

Die Overview muss in 10 Sekunden beantworten:
1. **Welcher Commit stammt von einem Agenten?** (und nur diese anzeigen können)
2. **Was war der konkrete Auftrag?** (Anforderung, Sessions)
3. **Wie weit ist das belegt — und was fehlt konkret?**

Und: **Jede Zahl ist konsistent.** Was oben steht, lässt sich unten nachzählen und widerspricht sich nirgends.

### Befund (Ist-Stand)

- Kopf und Boxtitel zeigen `1 Sessions · 0 Changes · 0 % of commits with a session` — direkt über einem
  Commit mit Session. Ursache vermutlich: Die Prozentzahl (`minds_metrics::Coverage`, `view/mod.rs:257`,
  `view/overview.rs:347`) zählt die HEAD-Historie (`main`), die Liste zeigt alle Branches.
  Verschiedene Bezugsmengen → scheinbarer Widerspruch. „Changes“ meint Change-Ids, gelesen wird „Änderungen“.
- Die Statistik steht doppelt (Kopfzeile 2 und Boxtitel); „Overview“ steht dreimal (Tab, Breadcrumb, Titel).
- Der Auswahlbalken ist zerstückelt (Pillen mit eigenem Hintergrund, `▌`-Fragmente) — wirkt wie ein Renderfehler.
- Der Auftrag des Menschen ist grau, hinten und abgeschnitten — obwohl darunter 30 leere Zeilen stehen.
- `■` ohne Verbindungslinien liest sich wie eine Checkbox-Liste, nicht wie eine Historie.
- Rechts: zwei Enter-Hinweise mit verschiedener Bedeutung (die Fußzeile wiederholt sie), Metadaten statt Antwort,
  kein Anforderungsinhalt, obwohl links `⚑ intent` steht.
- `◈ sealed` (Session-Protokoll) und `⚑ intent` (Anforderung) stehen nebeneinander und werden verwechselt.
- `⚠ open` in Gelb für den Normalzustand jedes neuen Commits.

### Begriffe (verbindlich für dieses Paket)

| Sache | Wort in der UI | Verben |
|---|---|---|
| Intent-Anker (`refs/minds/intents/`) | **requirement**, ID = **anchor** | bound · proven · signed |
| Session-Protokoll (Seal) | **session record** | sealed · intact · tampered |
| Commit mit verknüpfter Session | **commit with an agent session** | — |

„Seal“ bezeichnet **nur** das Session-Protokoll. Eine Anforderung wird nie „sealed“ genannt.
Ein Agent-Commit ohne minds-Capture ist für minds unsichtbar — deshalb heißt der Filter ehrlich
„agent sessions“, nicht „agent commits“.

### C3.1 · Eine Quelle für alle Zahlen (zuerst umsetzen)

**Änderung.**
- `minds-reader` liefert zur `Overview` eine Struktur `OverviewCounts`, berechnet **aus genau den gelesenen
  Zeilen** (`Overview::rows`, `wip`): Commits gesamt, Commits mit Agent-Session, Sessions ohne Commit,
  Commits ohne Session. Aggregation gehört in den Reader, nicht in die TUI.
- Die TUI zeigt **jede Zahl genau einmal**: im Boxtitel der Overview. Kopfzeile 2 ist nur noch der Breadcrumb.
- **Keine Prozente** (sie verstecken den Nenner): `4 commits · 1 with an agent session` statt `0 %`.
- **„Changes“ fliegt aus der Statistik.** Die Change-Id erscheint im Detail, wenn es eine gibt.
- Ist die Liste begrenzt (`truncated`, `budget_hit`, `incomplete`), sagt der Titel das bei der Zahl:
  `500 commits read (limit)` — die Zahlen beziehen sich dann ausdrücklich auf das Gelesene.
- `minds_metrics::Coverage` bleibt für `minds metrics` unverändert; die TUI benutzt sie in der Overview nicht mehr.
- **Tab-übergreifend gleich**: Jede Zahl, die auch in einem anderen Tab erscheint (z. B. unerklärte Zeilen im
  Changes-Tab, `◦n`), kommt aus derselben Funktion. Zwei Berechnungen derselben Größe sind ein Fehler.

**Akzeptanz / Tests.**
- Titelzahl „with an agent session“ == Anzahl der Zeilen mit Agent-Badge (Property-artiger Test über Fixtures
  mit 0, 1, n Agent-Commits, mehreren Branches, WIP-Sessions).
- Mit Filter (C3.3): angezeigte + ausgeblendete Commits == Gesamtzahl.
- Negativ: `!contains("% of commits")`, `!contains("Changes")` in Kopf und Overview-Titel.

### C3.2 · Die Liste: Betreff vorn, Auftrag sichtbar, Status leise

```
OVERVIEW · 4 commits · 1 with an agent session      a: agent sessions only
■  PD   feat: stable_sort              feat/stable-sort       1d
│
■  ◉CC  feat: stable_sort mit Tests    ◈ ● ◐  ⚑               1d
│       Implementiere die Anforderung aus docs/anforderung…
■  PD   docs: anforderung              main · HEAD            1d
■  PD   chore: init                                           1d
```
*(Illustrativ. Die Trust-Strip-Glyphen kommen aus `view/trust.rs`, nicht aus diesem Mock.)*

**Änderung.**
- Spalten: Graph-Lanes · Badge · **Betreff** · Status · Refs · Zeit. Status und Refs sind feste,
  rechtsbündige Spalten; der Betreff bekommt den Rest.
- **Status** = Trust Strip (`Density::Glyphs`) + `⚑` wenn eine Anforderung gebunden ist + `◉N` bei mehreren
  Sessions + Review **nur wenn abweichend** (approved, needs work, rejected). „open“ wird nicht angezeigt.
- **Refs** als leise Textspalte (`feat/stable-sort`, `main · HEAD`), keine farbigen Pillen. Das bestehende
  Versprechen gilt weiter: Refs nie in Evidence-Farben (Klasse 1).
- **Agent-Commits bekommen eine zweite Zeile** mit dem Auftrag (gedimmt, kursiv falls verfügbar, mit `…` geclippt).
  Menschliche Commits bleiben einzeilig.
- **Graph-Spalte**: zwischen Commits `│`, damit die Spalte als Historie gelesen wird (bei echten Lanes die
  bestehenden Linien). Strukturglyphen aus dem Register (A1), keine Evidence-Glyphen.
- **Ein Auswahlbalken** über die volle Zeilenbreite (beide Zeilen bei Agent-Commits), `theme::cursor()`;
  keine Teil-Hintergründe innerhalb der Zeile, keine `▌`-Fragmente.
- **Zeit** relativ (`1d`, `3h`), absolut im Detail (C4).
- Startauswahl: HEAD.

**Akzeptanz / Tests.**
- Render 80/120/160: Betreff ≥ 30 Zeichen bei 120 Spalten (Split), Status-Spalte vollständig sichtbar.
- Auswahl: Kein Span der ausgewählten Zeile hat einen anderen Hintergrund als der Cursor.
- `!contains("⚠ open")` in der Liste; `contains("✓ approved")` bei freigegebenem Commit.

### C3.3 · Filter `a`: nur Commits mit Agent-Session

**Änderung.**
- **`a`** schaltet um: alle ↔ nur Commits mit Agent-Session (inkl. Sessions ohne Commit oben).
  **Esc** setzt jeden Filter zurück (Esc-Reihenfolge aus B1: Overlay → Suche → **Filter** → Ebene → Ursprung).
  `/`-Suche und `a` lassen sich kombinieren.
- **Ausgeblendete Commits werden zusammengefasst**, nie spurlos entfernt: eine gedimmte Zeile je Lauf,
  mit den Refs, die darin liegen, damit HEAD und Branches auffindbar bleiben:
  ```
  OVERVIEW · agent sessions · 1 of 4 commits                     a: all
  ┆ 1 commit hidden · feat/stable-sort
  ■  ◉CC  feat: stable_sort mit Tests    ◈ ● ◐  ⚑               1d
  │       Implementiere die Anforderung aus docs/anforderung…
  ┆ 2 commits hidden · main · HEAD
  ```
- Der Titel nennt den Filter und `N of M`; der Hinweis rechts im Titel sagt, was `a` jetzt bewirkt.
- Der Filter bleibt beim Tab-Wechsel und Reload erhalten; die Auswahl springt auf den nächsten sichtbaren Commit.
- Keine Agent-Session in der Historie: Leerzustand „No commit with an agent session in the 4 commits read.
  `a` shows all.“ — nicht einfach eine leere Liste.
- `input.rs`: `a` → neue Action (z. B. `Action::AgentFilter`). Im Suchmodus landet `a` in der Suche
  (Klasse-1-Test `while_searching_letters_go_into_the_query` entsprechend erweitern).

**Akzeptanz / Tests.** Toggle hin und zurück; Esc setzt zurück; Separatoren tragen Zahl und Refs; Summe stimmt
(C3.1); Auswahl bleibt auf sichtbarem Commit; Leerzustand.

### C3.4 · Detail: Auftrag, Sessions, Beleg

```
b89bba7 · feat: stable_sort mit Tests
on feat/stable-sort · Patrick Döring · 2026-10-14 13:18 UTC

REQUIREMENT
⚑ anchor 3f9a2c1e · file:docs/anforderung.md (HEAD version)
  scope  src/sort.rs, tests/**
  ▸ Space: show text (12 lines)

SESSIONS (1)
◉CC claude-code · opus · b3-cf28e399 · 13:02–13:17
    3 files · 87 events · 0 gaps · ◈ record sealed
    Implementiere die Anforderung aus docs/anforderung.md
    und schreibe Tests für stable_sort …

EVIDENCE                                     checked 12:35Z
INTEGRITY       ✓ record intact · signed
COVERAGE        ◦ 2 of 48 added lines unexplained
                → d, then n
INTERPRETATION  ✓ every tool call interpreted
REQUIREMENT     ✓ bound · ✓ proven · ◦ not signed
                → minds intent sign 3f9a2c1e
REVIEW          ◦ not reviewed yet
```
*(Werte illustrativ.)*

**Änderung.**
- **Keine Tastenhinweise im Detail** (heute „Enter: how it was made · v: verify · d: diff“ und
  „Enter: the session's graph · w: …“). Tasten stehen nur in der Fußzeile.
- **Kopf**: Kurz-Id + Betreff; darunter leise: Branches, die den Commit enthalten (`on feat/stable-sort`,
  max. 3 + `+N`, aus dem bereits gelesenen Graphen im Reader berechnet, kein zusätzlicher Git-Zugriff),
  Autor, Datum absolut UTC, Change-Id falls vorhanden.
- **REQUIREMENT** (nur wenn eine Session einen Anker nennt):
  - Anker-Id, Quelle (`file:…`, `issue:…`, `prompt`), bei `file:` ob es die HEAD-Fassung ist, Scope.
  - **`Space` klappt den Anforderungstext auf/zu** — **nur wenn der Beleg gilt** (`IntentDetail::proof` ist `Ok`
    und `snapshot` ist `Some`). Sonst: „text hidden — snapshot does not match the anchor“ bzw. der Grund aus `proof`.
    Ein unbelegter Snapshot wird nie als Text gezeigt (bestehende Invariante).
  - Aufgeklappt scrollt das Detail mit PgUp/PgDn; gekürzter Snapshot sagt das und verweist auf `minds intent show`.
  - Datenquelle: `Source::intents` / `IntentList` wie der Intent-Tab (gleicher Cache, gleiche Prüfung).
    Liefert `CommitRow` nur `intent: bool`, ergänzt der **Reader** die Anker-Id(s) je Commit.
  - Mehrere Anker (mehrere Sessions): je Anker ein Block.
- **SESSIONS (n)**, je Session: Agent-Badge, Agent, Modell, Session-Id (gekürzt), Zeitraum, Dateien, Events,
  Gaps, Session-Record-Zustand (Trust Strip `Short` oder Provenance-Wort), dann der Auftrag **bis 3 Zeilen**
  (umgebrochen, nicht einzeilig geclippt). Enter auf einer Session öffnet ihren Graphen.
- **EVIDENCE** — eine Zeile je Achse, **nie zusammengefasst, kein Gesamtwert**:
  INTEGRITY · COVERAGE · INTERPRETATION (die drei Trust-Achsen) · REQUIREMENT · REVIEW.
  - Zeichen mit fester Bedeutung in diesem Block: `✓` geprüft und vorhanden · `◦` offen (fehlt noch, **kein Fehler**)
    · `✗` gescheitert / ungültig / manipuliert · `… checking` während Verify läuft · `·` trifft nicht zu.
    `◦` ist im Glyph-Register (A1) als „offen/unerklärt“ zu führen; meldet der Eindeutigkeits-Test einen Konflikt:
    **stoppen und berichten** (Klasse 1).
  - **Was fehlt, steht mit nächster Aktion direkt darunter** (`→ minds intent sign <anchor>`, `→ d, then n`).
    Nur Aktionen, die es wirklich gibt.
  - Daten: `CommitVerify` über den Worker aus B6 (nur der ausgewählte Commit, gedrosselt) + Trust-Achsen
    der Sessions + die Zahl unerklärter Zeilen aus **derselben** Funktion wie der Changes-Tab (C3.1).
  - Solange nicht geprüft: `… checking` — **nie `✓` vor dem Urteil**; bei `Err`: „not checked — <Grund>“.
  - Mehrere Sessions mit verschiedenen Zuständen: je Session eine Unterzeile — **nie** zu einem `✓` mischen.
  - Wörter wie `minds verify` sie benutzt (Klasse-1-Tests zur CLI-Parität gelten).
- **Commit ohne Agent-Session**: Kopf + „No agent session linked to this commit.“; REQUIREMENT, SESSIONS und
  EVIDENCE entfallen (kein Block voller `·`).
- **Breite**: Das Detail bekommt mindestens 60 Spalten; die Liste den Rest, höchstens so breit wie ihr Inhalt braucht.
  Unter 100 Spalten: nur Liste, Detail per Enter als eigene Ebene (Esc zurück).

**Akzeptanz / Tests.**
- Negativ: `!contains("Enter: how it was made")`, `!contains("Enter: the session's graph")` im Detail.
- REQUIREMENT: belegter Anker → `Space` zeigt Snapshot-Zeilen; unbelegter → Text nie sichtbar, Grund sichtbar.
- EVIDENCE: vor Verify `… checking` und kein `✓` in INTEGRITY; nach Verify die erwarteten Zeichen;
  „not signed“ ist `◦`, nie `✗` und nie Rot; TAMPERED ist `✗` und Rot.
- Die Zahl unerklärter Zeilen im Detail == `◦n` im Changes-Tab für denselben Commit.
- Render 80/120/160.

### C3.5 · Kopf und Fußzeile der Overview

- **Kopf**: Zeile 1 Repo · **Branch-Kurzname** (`main`, nicht `refs/heads/main`) · Tabs; Zeile 2 nur Breadcrumb.
- **Boxtitel**: `OVERVIEW · <Zahlen aus C3.1>` links, Filterhinweis rechts. Kein zweites „Overview“.
- **Fußzeile Zeile 1** (Kontext): Kurz-Id des ausgewählten Commits + Filterzustand
  (`b89bba7 · showing agent sessions only`). Kein Erklärsatz, keine Backticks als Rohtext.
  Die Erklärung „sealed is not verified“ gehört ins Glossar/Hilfe und in die INTEGRITY-Zeile des Details.
- **Fußzeile Zeile 2** (Aktionen, priorisiert): `↑↓ commit · Enter how it was made · a agent only/all ·
  Space requirement` (nur wenn verfügbar) `· w why · v verify · d diff · / filter`, rechts fest `? help  q quit`.

### C3.6 · Prompt als eigener Abschnitt, Gründe für „unexplained“, kein „unknown“

*Nachtrag nach dem Screenshot vom 15.10. (Overview v2 auf `0de8751`).*

**Befund.**
- Der Prompt des Menschen steht als kursive Zeile unter den Session-Zahlen — das Wichtigste, als Fußnote.
- `COVERAGE ◦ 46 of 46 added lines unexplained` steht ohne Grund da. Der Grund ist aus denselben Daten
  ablesbar: die Session trägt `0 files` — kein Write/Edit mit Dateipfad wurde aufgezeichnet. Die Zeile ist
  korrekt (Prompt und Anforderung erklären **nie** eine Zeile — `a7f3e2d` „coverage never counts a claim as
  explained“), aber die Oberfläche lässt den Betrachter raten.
- `claude-code · unknown · …`: `unknown` ist ein Platzhalter aus `minds-capture` (`adapter.rs::unknown()`,
  wenn das Transkript nicht gelesen wurde) und wird als Modellname gedruckt.

**Änderung.**
1. **PROMPT als eigener Abschnitt** im Detail, zwischen REQUIREMENT und SESSIONS (bei einer Session) bzw. je
   Session als Unterabschnitt (bei mehreren). Volltext, umgebrochen, bis 12 Zeilen, dann `… N more lines`
   (`Space` auf dem Abschnitt klappt auf). Kopfzeile: `PROMPT  ◌ as recorded by the agent · session b3-…`.
   Die kursive Zeile im SESSIONS-Block entfällt (nichts steht doppelt).
2. **Jede `◦`-Zeile in EVIDENCE nennt ihren Grund.** Für COVERAGE kommt er aus den Gründen, die der
   Changes-Tab je Zeile kennt (`Why this line?`: *no tool claim or shell mention found*, *claimed without a
   hash*, *another version at the last write*, …): die TUI zeigt den **häufigsten Grund** mit Anzahl, bei
   mehreren `+ N other reasons → d`. Beispiel:
   `◦ 46 of 46 added lines unexplained — no tool claim for any of them (session records 0 files)`.
   Die Aggregation (Grund → Anzahl je Commit) gehört in `minds-reader`, dieselbe Funktion liefert dem
   Changes-Tab die Zählung (C3.1: eine Quelle).
3. **Widersprüche zwischen Session und Commit werden benannt**, nicht nur gezählt: Ändert der Commit Dateien,
   die Session trägt aber `0 files`, steht in SESSIONS `0 files claimed · commit changes 2 files` (gedimmt,
   kein Rot — es ist eine Lücke, kein Manipulationsbefund).
4. **Nie `unknown` als Wert drucken.** Fehlt ein Wert, wird das Feld weggelassen oder als `model not recorded`
   (gedimmt) gezeigt. Gilt für Modell, Agent-Version, Provider und jeden anderen Platzhalter aus `minds-capture`;
   die TUI prüft im Reader auf den Platzhalter, nicht per String-Vergleich in der View.
   Modell wird gezeigt als `claude-code · claude-opus-4-1 (Anthropic)` — Provider nur, wenn bekannt.

**Akzeptanz / Tests.**
- `contains("PROMPT")`, Prompt-Volltext sichtbar; `!`-Assertion: der Prompt steht nicht zusätzlich im SESSIONS-Block.
- COVERAGE mit 0 Dateiclaims: Zeile enthält den Grund und `0 files`; die Zahl stimmt mit dem Changes-Tab überein.
- Session mit `model = "unknown"`: `!contains("unknown")`, `contains("model not recorded")`.
- Mehrere Sessions: PROMPT je Session, in Session-Reihenfolge.

### C3.7 · Die nächste Aktion steht im Detail (Nachtrag 16.10.)

**Befund (Screenshot `c6f7c19`).** EVIDENCE zeigt sauber, was fehlt — aber der Betrachter muss selbst
ableiten, was er jetzt tun soll. Die Fußzeile bietet sechs gleichwertige Tasten an.

**Regel.** Die **oberste Zeile in EVIDENCE, die nicht `✓` ist, bestimmt die nächste Aktion.** Sie steht als
letzte Zeile des Blocks und in Fußzeile Zeile 1:

```
NEXT  → d: inspect the 95 lines the agent claims without a hash
```

Zuordnung (Zeile → Aktion):
- INTEGRITY `✗` → `e`: evidence of the session (was ist manipuliert / wo endet das Protokoll)
- INTEGRITY `… checking` / not checked → `v`: run verify
- COVERAGE `◦`/`◇` → `d`: Changes-Tab auf dem ersten betroffenen Hunk (`n` springt weiter)
- INTERPRETATION `◐` → `e`: evidence, Abschnitt uninterpreted calls
- REQUIREMENT `◦ not bound` → `i`: Intent-Tab · `◦ not signed` → `minds intent sign <anchor>` (Kommandozeile, als Text)
- REVIEW `◦` → `minds review …` (als Text; die TUI schreibt nicht)
- alles `✓` → `Enter`: how it was made (nur noch Nachvollziehen, keine offene Entscheidung)

Nie mehr als **eine** NEXT-Zeile. `Enter` (die Entstehung) bleibt immer verfügbar, ist aber nur dann die
empfohlene Aktion, wenn nichts mehr offen ist.

**Weitere Befunde aus demselben Screenshot (Klasse 2, mit C3.7 beheben):**
- Breadcrumb `Overview › 609aca8 › Overview › 609aca8`: der Verlauf aus B2 legt beim Tab-Wechsel auf
  denselben Tab einen zweiten Eintrag ab. Regel: gleicher Tab + gleicher Fokus = kein neuer Eintrag.
- `COVERAGE  · 95 added lines / ◇ 95 stated`: zwei Zeilen, das erste Zeichen `·` (trifft nicht zu) ist hier
  falsch, „stated“ sagt nicht, von wem. Eine Zeile: `◇ 95 of 95 added lines claimed by the agent without a hash`.
- `change I8d166b6d5e7b6ebfd3c17fb18bef8fe699b78659`: Change-Id auf 12 Zeichen kürzen (`I8d166b6d5e7…`).
- Sessions ohne Commit (`open · no commit linked yet`) tragen keine Zeit: Startzeit relativ anzeigen (`3d ago`),
  damit verwaiste Sessions als solche erkennbar sind; der Leerzustand-Hinweis nennt `minds forget`.
- In der Auswahlzeile tragen `◈ sealed` und `⚑` noch eigene Hintergründe (C3.2 fordert einen durchgehenden Balken).

### Schnitt in Commits

1. `feat(reader): OverviewCounts from the rows read` + TUI-Titel (C3.1)
2. `feat(tui): overview rows — subject first, request line, quiet status` (C3.2)
3. `feat(tui): a toggles commits with an agent session` (C3.3)
4. `feat(reader): requirement anchors and containing branches per commit` (Datenteil von C3.4)
5. `feat(tui): overview detail — requirement, sessions, evidence` (C3.4)
6. `fix(tui): overview header and footer` (C3.5)
7. `feat(reader): unexplained-line reasons per commit` + `feat(tui): prompt section, reasons, no unknown` (C3.6)
8. `feat(tui): next action from the first open evidence row` + Breadcrumb/Change-Id/WIP-Zeit (C3.7)

Nach Commit 1 und nach Commit 5: **stoppen**, Render bei 160×40 mit der Demo-Fixture zeigen.

### Betroffene Tests

- **Klasse 2** (anpassen, mit Begründung): Overview-Snapshots auf Pillen (`" ◆CC "`, `"● HEAD"`,
  `"◇ sealed · unsigned"`, `"⚠ open"`), auf `"% of commits with a session"`, auf die Tastenhinweise im Detail.
- **Klasse 1** (nicht ändern): Refs nie in Evidence-Farben; `◈ sealed` nur, wenn jedes Seal eine Signatur trägt;
  CLI-Parität der Urteilswörter; kein VERIFIED ohne Signaturprüfung; Farbe nie allein.

### Doku

`docs/commands.md`, Abschnitt **Overview**, an das neue Verhalten anpassen (Filter `a`, `Space`,
Zählweise, Detailblöcke, Wortwahl requirement vs. session record). `CHANGELOG.md` ergänzen.

## C4 · Zeiten

`view/mod.rs::when` erzeugt `14.10. 09:00Z` (deutsches Datumsformat in englischer UI, nur UTC).
→ In Listen relative Zeit (`3h ago`, `2d ago`), im Detail `2026-10-14 09:00 UTC`. Ohne neue Datums-Crate,
falls möglich (`minds_metrics::epoch_seconds` existiert).

## C5 · Begriffe, die kollidieren

- Kopf: „X % context coverage“ ist der Anteil der Commits mit Session-Trailer
  (`minds_metrics::Coverage::commits_with_context`) — nicht die Coverage-**Achse**. Umbenennen,
  z. B. `62 % of commits with a session`.
- Größe in der Liste: `3 D · 1200/4500 T` → `3 files · 5.7k tok`.
- Verify: `NOT CHECKED — run minds verify` → Taste `v` (oder `r`) startet die Prüfung direkt.

## C6 · Ohne Farbe und mit einfachen Fonts benutzbar

- Die TUI respektiert **`NO_COLOR`** (heute nur die CLI, `minds-cli/src/text.rs`): alle fg/bg-Farben
  entfallen, Modifier (fett, invertiert, unterstrichen) bleiben.
- Render-Test im Monochrom-Modus: Jeder Zustand ist allein über Glyph + Wort unterscheidbar;
  Cursor und Pane-Fokus bleiben erkennbar.
- Nerd-Font bleibt opt-in (`--nerd-font` / `MINDS_NERD_FONT`); der Standard funktioniert in jedem Monospace-Font.

## C7 · Graph-Lesbarkeit

- Bei großen Graphen (Schwelle festlegen, z. B. > 40 Zeilen in Zoom Normal) startet die Ansicht in
  Zoom Summary; die Fußzeile sagt, dass mehr da ist.
- Die Wörter `READ/EDIT/EXEC` in jeder Zeile prüfen: Glyph + Pfad reichen in Zoom 1/2, wenn die Hilfe
  und die Detailbox das Wort tragen — nur umsetzen, wenn der Monochrom-Test (C6) grün bleibt.
- Label-Breite in `graph.rs`: Abzug von pauschal 20 Spalten durch tatsächliche Breite ersetzen.

## C8 · Tool-, Bash- und Subagent-Aufrufe mit Inhalt sichtbar machen

**Ausgangslage (Datenmodell, nicht ändern).** `minds-capture` zeichnet bereits auf: jeden Tool-Aufruf
(`ToolCall::name`, `arguments` = vollständiger Tool-Input als JSON, `effect`, `capture`-Status), Bash-Kommandos
(als `Exec`-Effekt mit Kommando, Test-/Bench-Ergebnis als `ExecOutcome`), Subagenten (eigene Session, verknüpft
über `Session::edges`, Nodes `Subagent`/`Handover`), Prompts und den Antworttext des Modells je Turn
(`Turn::text`, aus dem Transkript). **Bewusst nicht** aufgezeichnet: stdout/stderr und Tool-Antworten
(EA-18a) — nur deren Deutung (Hashes, Testzahlen). Diese Grenze bleibt; sie ist eine Entscheidung,
keine Lücke.

**Problem.** Die TUI zeigt davon zu wenig: Im Graph-Detail werden `arguments` auf `DETAIL_MAX = 120` Zeichen
gekürzt (`minds-reader/src/graph.rs::truncate`); ein Bash-Kommando oder der Prompt an einen Subagenten ist
damit oft abgeschnitten. Der Antworttext des Modells erscheint nur in Zoom *Verbose*. Subagent-Knoten sind
Blätter ohne Inhalt.

**Änderung.**
1. **Graph-Detail vollständig und scrollbar**: `arguments` nicht mehr auf 120 Zeichen kürzen, sondern
   strukturiert zeigen (JSON-Felder als `key: value`, lange Werte umgebrochen; Grenze 4 000 Zeichen mit
   `… N more`). `sanitize` bleibt. Bash: `command` als erste Zeile, vollständig.
2. **Subagent-Knoten** zeigen den Prompt an den Subagenten (aus `arguments.prompt`/`description` des Task-Aufrufs)
   und die Kennzahlen der Kind-Session (Events, Dateien, Record-Zustand). **Enter** öffnet die Kind-Session als
   eigene Ebene (Esc zurück, Breadcrumb `… › subagent b3-…`).
3. **Turn-Knoten** zeigen im Detail den Antworttext des Modells (`Turn::text`, umgebrochen, bis 40 Zeilen) in
   jedem Zoom, nicht nur in *Verbose*; Kennzeichnung `◌ as recorded`.
4. **Exec-Knoten** zeigen Kommando, Exit-Deutung und `ExecOutcome` (Runner, bestanden/fehlgeschlagen/Benchmarks)
   — und sagen ausdrücklich: „stdout/stderr not recorded (by design)“.
5. **Timeline** (`t`) listet dieselben Knoten mit Zeitstempel; Bash-Kommandos dort einzeilig, gekürzt mit `…`.

**Nicht Teil dieses Pakets.** Aufzeichnung von Tool-Antworten/stdout — das wäre eine Änderung an
`minds-capture` und `minds-redact` (Secrets, Größe, Envelope) und braucht eine eigene Entscheidung.

**Akzeptanz / Tests.** Bash-Kommando mit 300 Zeichen vollständig im Detail; Subagent-Prompt sichtbar, Enter
öffnet die Kind-Session; Turn-Text in Zoom *Normal* sichtbar; `!contains("…")` am Ende eines 300-Zeichen-Kommandos.
Klasse-1-Test `foreign_text_in_arguments_is_sanitized_and_truncated` (Reader): Sanitisierung bleibt;
die Kürzungsgrenze wird als Parameter geführt, der Test prüft weiter, dass gekürzt wird.

## C9 · Changes v2 — die Zeilen mit ihrer Evidenz lesen

*Grundlage: Screenshot vom 16.10. nach `d` auf dem Commit `609aca8` (`c6f7c19`).*

### Befund

1. **Die Landung ist leer.** `d` setzt den Fokus auf FILES und den Diff-Cursor auf den Hunk-Header. Die Box
   „WHY THIS LINE?“ zeigt deshalb nur „Hunk header — ] / [ jump between hunks“ plus Commit/Change/Review.
   Dabei kann die Box für eine Codezeile schon viel (Session, Agent, Step, At, Intent, „Agent said“, „Asked for“ —
   `changes.rs::draw_why`). Der Nutzer sieht davon nichts, bis er zufällig in den Diff wechselt und eine Zeile trifft.
2. **Kein sichtbarer Cursor im Diff.** Der Hunk-Header ist nicht hervorgehoben, der Fokus-Pane nicht markiert.
3. **Fläche falsch verteilt**: FILES (25 %) und WHY (25 %) sind fast leer, der Diff schneidet jede längere Zeile
   mit `…` ab.
4. **Grüner Block**: 29 Zeilen mit grünem Hintergrund — die Farbe trägt nichts (jede Zeile ist `+`) und frisst
   den Cursor. Dazu 29-mal `◇` im Gutter, obwohl die Legende schon sagt „reported 29“.
5. **Drei Wörter für eine Sache**: Overview sagt `stated`, der Diff `reported`, die Why-Box
   „claimed without a hash“. Wortwahl im Theme und in Klasse-1-Tests: **reported** (`◇ reported only`).
6. **Tasten passen nicht zum Zustand**: `n next unexplained` bei 0 unexplained (29 reported). `[ ] commit`
   wechselt den Commit, aber `FILES · 609aca8 · 1/1` erklärt nicht, 1/1 wovon.
7. **Breadcrumb wächst bei jeder Aktion** (`Overview › 609aca8 › Overview › 609aca8 › …`). Blocker, zuerst fixen.
8. Review `⚠ open` in Gelb in Box und Fußzeile (Normalzustand, laut). Change-Id 40 Zeichen, umgebrochen.

### Ziel

Ein Reviewer liest den Diff **mit** der Evidenz jeder Zeile: Wer hat sie geschrieben, in welchem Schritt,
auf welchen Auftrag hin — und was davon ist beobachtet, was nur berichtet.

### C9.1 · Landung und Cursor

- `d` (und `Enter` aus Overview-NEXT) landet im **Diff-Fokus**, Cursor auf der **ersten Codezeile** des ersten
  Hunks — nie auf dem Header. Kommt der Nutzer über NEXT „reported/unexplained“, auf der ersten solchen Zeile.
- Cursor = `theme::cursor()` über die ganze Zeile; fokussierter Pane mit hellem Rahmen (B7).
- `n`/`N`: nächste/vorige Zeile, die **nicht `●` observed** ist (reported, fs-only, not observed). Die Fußzeile
  nennt den Zustand mit Zahl: `n next not-observed (29)`; bei 0 fällt die Taste aus der Fußzeile.
- Auf dem Hunk-Header zeigt WHY die **Datei-Zusammenfassung** (siehe C9.3), nie nur einen Tastenhinweis.

### C9.2 · Layout

| Breite | Aufteilung |
|---|---|
| ≥ 160 | FILES 28 Spalten · Diff Rest · WHY 56 Spalten rechts |
| 120–159 | FILES als Zeile über dem Diff (`src/sort.py +29 · src/test_sort.py +66`, `Tab` wechselt) · Diff · WHY 48 rechts |
| < 120 | FILES-Zeile · Diff; WHY als Ebene per `Enter` auf der Zeile (Esc zurück) |

- Der Diff bekommt immer die größte Fläche. Lange Zeilen: `…` erst, wenn WHY sein Minimum hat.
- `+`/`-` als **Vordergrundfarbe** auf Marker und Zeilennummer, **kein** Zeilen-Hintergrund (Hintergrund ist
  für den Cursor reserviert). Entfernte Zeilen `-` rot im Vordergrund.
- Gutter-Glyph je Zeile bleibt (Farbe nie allein), aber **gedimmt**, solange er dem Datei-Zustand entspricht;
  nur Abweichungen (eine `◦`-Zeile in einer `●`-Datei) in voller Farbe.
- Titel: `FILES · 2 files · +95 -0` statt `FILES · 609aca8 · 1/1`. Der Commit steht im Breadcrumb.

### C9.3 · WHY THIS LINE — feste Struktur

Auf einer Codezeile (Reihenfolge fest, leere Abschnitte entfallen):

```
src/sort.py:28  ◇ REPORTED
The agent's Write tool reported this content. No hash was
recorded, so minds cannot prove the file on disk matched.
With a witness this line would be ● observed.

WRITTEN BY   ◉CC claude-code · turn 7 · 10:38:12Z
             Write src/sort.py (29 lines)   → Enter: graph
SESSION      b3-75f030fa · ◈ record sealed
REQUIREMENT  ⚑ ac8f1bb7 · in scope src/**
ASKED FOR    ◌ "Implementiere die Anforderung aus …"
AGENT SAID   ◌ "Ich sortiere stabil über einen Sekundär…"
REVIEW       ◦ not reviewed yet
```

- Zeile 1: Pfad:Zeile + Glyph + WORT. Zeile 2–3: der Grund **in einem Satz** (die Sätze aus `draw_why` bleiben,
  bekommen aber den Satz „with a witness this line would be ● observed“ bei reported/fs-only — das ist die
  Brücke zur Demo).
- `Enter` auf der Zeile springt in den Graph auf genau diesen Tool-Aufruf (Rückweg per Esc, B2).
- Auf dem Hunk-Header / Datei: `src/sort.py · 29 added lines · all reported by one Write (turn 7) · 0 observed`
  plus NEXT-Zeile wie in C3.7.
- Keine Tastenhinweise im Panel (wie C3.4). Change-Id gekürzt. Review ohne Gelb.

**Nachtrag (Screenshot 16.10., Cursor auf Zeile 5):** Die Box ist auf einer Codezeile gefüllt, aber in der
falschen Reihenfolge und mit vier Fehlern, die der Entwurf oben ebenfalls beseitigt:
- `◇ REPORTED ONLY` steht ohne Erklärsatz. Für *reported* und *fs only* fehlt in `draw_why` der Satz, den die
  Unexplained-Fälle haben. Pflicht: jeder Zustand hat seinen Satz (reported: „The agent's tool reported this
  content; no hash was recorded, so minds cannot prove the file on disk matched.“).
- `Agent  claude-code · unknown`: Die Regel aus C3.6 („nie `unknown` drucken“) gilt für **jede** View. Test:
  ein Render aller Views mit einer Session, deren Modell `unknown` ist, enthält das Wort nirgends.
- `At  2026-10-16T10:39:09.879Z`: rohes RFC 3339 mit Millisekunden. C4 gilt auch hier: `10:39:09Z · today`
  bzw. `2026-10-16 10:39 UTC`.
- `Intent  anchor b3-ac8f1bb7024… (witness event)` gegenüber `anchor ac8f1bb7` in der Overview: **ein**
  Id-Format für Anker überall (Kurzform wie in der Overview; die Herkunft „witness event“ bleibt als Zusatz).
- `Agent said` fehlt — nicht wegen der TUI, sondern weil das Transkript nicht gelesen wurde (K1). Nach dem
  K1-Fix erscheint der Abschnitt von selbst; bis dahin: `AGENT SAID  — (transcript not read)`.
- Der Cursor ist nur ein `▸` im Gutter auf grünem Block — praktisch unsichtbar (→ C9.1/C9.2).

### C9.4 · Wortwahl und Zähler

- **Ein Wort je Zustand, tabübergreifend**: `● observed` · `◍ fs only` · `◇ reported` · `◦ not observed`.
  Overview C3.4/C3.6 wird angepasst: `◇ 95 of 95 added lines reported, not observed` (statt „stated“).
  Die Erklärung „claimed without a hash“ bleibt als Satz, nicht als Zustandswort.
- Die Legende über dem Diff zählt die Datei, der FILES-Titel den Commit, Overview den Commit — alle drei aus
  derselben Reader-Funktion (C3.1).

### Betroffene Tests
- Klasse 2: Snapshots auf die Landung (Fokus FILES, Hunk-Header-Satz), auf grünen Zeilen-Hintergrund,
  auf `FILES · <commit> · 1/1`, auf „stated“ in der Overview.
- Klasse 1: `◇ reported only` und alle Sätze, die Heuristik/Claim als solche kennzeichnen
  („heuristic, not evidence“, „unverified“); Removals bleiben unattributiert.

### Schnitt
1. `fix(tui): breadcrumb never repeats the same tab and focus` (Blocker, vor allem anderen)
2. `feat(tui): changes lands on the first line with the why panel filled` (C9.1)
3. `feat(tui): changes layout by width, no line background` (C9.2)
4. `feat(tui): why-this-line in fixed order with the witness bridge` (C9.3)
5. `fix(tui): one word per evidence state across tabs` (C9.4)

## C10 · Die Why-Kette wird abgeschafft

*Entscheidung vom 17.10. nach dem Screenshot der Why-Ebene (`Enter` in der Why-Box, `w`).*

**Begründung.** Alle acht Glieder (LINE, COMMIT, CHANGE, SESSION, AGENT, INTENT, EVIDENCE, REVIEW) stehen seit
C3/C9 in der Why-Box (Zeile) und im Overview-Detail (Commit). Die Ebene fügt nichts hinzu, rahmt einen fehlenden
Review als „Gap“ (falsch: Normalzustand) und zeigt Code-Jargon (`● ? observed [unchecked]`).

**Änderung.**
1. `view/why.rs` und `View::Why` entfallen; `Action::Why` und die Taste `w` entfallen aus `input.rs`, Hilfe und
   allen Fußzeilen. `Enter` auf einer Codezeile öffnet den Graph auf dem schreibenden Tool-Aufruf (Batch 2, Paket 5);
   `Enter` auf einem Commit in der Overview öffnet wie bisher die Entstehung (Graph der Session).
2. Der Reader-Code (`minds-reader::why`) bleibt — `minds why` (CLI) nutzt ihn weiter.
3. **Tests umziehen, nicht löschen.** Die Versprechen der Kette bekommen eine neue Heimat:
   - „ein Claim ist als CLAIM markiert, nie mit ✓“ (`why_shows_the_chain…`, A2) → Why-Box (`ASKED FOR ◌`, `AGENT SAID ◌`) und Overview-PROMPT.
   - „eine Lücke wird benannt, nicht versteckt“ → Overview-EVIDENCE (jede `◦`/`✗`-Zeile mit Grund) und Why-Box (Satz je Zustand).
   - „eine lückenlose Kette sagt das“ (`a_fully_backed_chain_says_so…`) → Overview-EVIDENCE: alle Zeilen `✓` → NEXT zeigt `Enter: how it was made`.
   - Ein fehlender Review ist **nie** ein Gap: negative Assertion in Overview-EVIDENCE (`◦ not reviewed yet`, kein `⚠`).
   Jeder umgezogene Test behält seinen Kommentar („Aussage != Beweis“ usw.); im Commit-Body: alt → neu.
4. `docs/commands.md`: Abschnitt zur Why-Ebene entfernen, `minds why` bleibt dokumentiert. `CHANGELOG.md`:
   Eintrag unter *Removed* mit dem Hinweis, wo die Inhalte jetzt stehen.

**Reihenfolge.** Nach Batch 2 (die Why-Box muss die Inhalte tragen, bevor die Kette verschwindet). Ein Commit:
`refactor(tui)!: remove the why chain view — its content lives in changes and overview`.

## C11 · Session Story — die Schrittliste ersetzt den Graph-Baum

*Grundlage: Screenshot vom 17.10. (F2 → Enter auf der Session, Zoom 3).*

### Befund

Der Inhalt ist richtig — lesen, schreiben, testen, linten, nachbessern, linten — aber die Form versteckt ihn:
1. **Der Baum hat immer dieselbe Gestalt** (YOU → AGENT → TURN → Tools → CHANGE → REVIEW). Eine feste Hierarchie,
   als Graph gezeichnet, trägt keine Information; die Verbindungslinien erklären nichts. Verzweigung gibt es nur bei
   Subagenten und Handovers.
2. **Der Prompt steht dreimal**: YOU-Box, YOU-Knoten, TURN USER — alle drei abgeschnitten.
3. **Kopfzeile als Jargon-Suppe**: `◈ sealed · ● complete · ✓ interpreted ⚠ open ● ? observed [unchecked] claude-code · unknown · 2…`,
   am Ende abgeschnitten; die Fußzeile wiederholt den Strip und hängt einen Erklärsatz zum Trailer an.
4. **Absolute Pfade mit Benutzername** (`/Users/pd/dev/minds-demo/src/sort.py`) — PII in der Anzeige, Rauschen im Lesefluss.
5. **Keine Ergebnisse**: Testläufe ohne „passed“, Lint ohne Befund, obwohl `ExecOutcome` sie kennt; keine Zeiten, keine Dauer.
6. **EDIT für Write-Aufrufe**: Der Prompt sagt „mit dem Write-Tool“, die Zeile sagt EDIT (Effekt statt Tool).
7. `CHANGE I8d166b6d…` (40 Zeichen) statt des Commits, den jeder kennt; `⚠ REVIEW open` wieder laut; `unknown`.
8. Kein sichtbarer Cursor; die Sessions-Liste links belegt 40 % für drei Zeilen, während rechts jedes Kommando abschneidet.

### Zielbild

Die Frage der Seite: **„Was hat der Agent zwischen Prompt und Commit wirklich getan — in welcher Reihenfolge,
mit welchem Ergebnis?“** Antwort: eine nummerierte Schrittliste, keine Baumzeichnung.

```
SESSION b3-75f030fa · claude-code · claude-opus-4-1 · 2026-10-16 10:37–10:40 UTC (3 min) · ◈ ● ◐
PROMPT       ◌ Implementiere die Anforderung aus anforderung.md. Arbeite nur unter
               src/. Lege die Dateien mit dem Write-Tool an und führe danach die
               Tests aus. Nicht committen.
REQUIREMENT  ⚑ ac8f1bb7 · scope src/**

STEPS (10)                                                         started 10:37:12Z
  1  ▶ ran     cat anforderung.md && ls -la && find . -type f …              +0:00
  2  ✎ wrote   src/sort.py                          29 lines  ◇ reported    +1:28
  3  ✎ wrote   src/test_sort.py                     66 lines  ◇ reported    +1:41
  4  ▶ ran     which uv && uv --version && …                                 +1:55
  5  ▶ tests   python -m unittest discover -s src              ✓ 8 passed   +2:03
  6  ▶ tests   pytest -q src                                   ✓ 8 passed   +2:10
  7  ▶ lint    ruff check src && ruff format --check src       ✗ 2 findings +2:18
  8  ✎ edited  src/test_sort.py                                 ◇ reported   +2:31
  9  ✎ edited  src/test_sort.py                                 ◇ reported   +2:40
 10  ▶ lint    ruff check src && ruff format --check src       ✓ clean      +2:52

RESULT   2 files · +95 -0 · commit 609aca8 „feat: stable_sort mit Tests“ · ◦ not reviewed yet
```
*(Werte illustrativ; Ergebnisse nur, wenn `ExecOutcome` sie kennt — sonst leer, nie geraten.)*

Unter der Liste die **Detailbox** zum ausgewählten Schritt (fest, nicht aufklappend):
Kommando/Argumente vollständig (C8), Zeitpunkt, Effekt, Capture-Status, Ergebnis, „stdout/stderr not recorded
(by design)“, bei Subagent der Prompt an ihn, bei `wrote`/`edited` der Evidenz-Zustand mit Satz (C9.3).

### Änderung

1. **Schrittliste statt Baum** (`view/graph.rs` → `view/story.rs`): ein Schritt je Tool-Aufruf, in Session-Reihenfolge,
   nummeriert, mit Spalten *Nr · Glyph+Verb · Ziel · Umfang/Ergebnis · Evidenz · Zeit seit Start*.
   Verben aus dem Tool und dem Effekt: `ran` (Bash), `tests`/`lint`/`build` (bekannter Runner laut `ExecOutcome`),
   `wrote` (Write), `edited` (Edit/MultiEdit), `read` (Read), `searched` (Grep/Glob), `fetched` (WebFetch),
   `delegated` (Task/Subagent), `tool` (unbekannt, mit Namen).
2. **Verzweigung nur, wo sie existiert**: Subagenten und Handovers als eingerückte Unterliste unter dem Schritt, der sie
   auslöst (`delegated → 3 steps · ◈ sealed`), `Enter` öffnet die Kind-Session als Story (Esc zurück, B2).
3. **Turns als Trenner, nicht als Knoten**: Bei mehreren User-Turns eine gedimmte Zwischenzeile mit dem nächsten Prompt;
   der Antworttext des Modells (`Turn::text`, nach K1) als gedimmte `◌`-Zeile vor den Schritten des Turns —
   in Zoom *Verbose*; in *Normal* nur die Schritte; in *Summary* gleichartige Folgen zusammengefasst
   (`3 test runs · 2 edits of src/test_sort.py`). `z`/`Z` bleiben.
4. **Ein Prompt**: Abschnitt PROMPT (voll, umgebrochen, `◌`), YOU-Box und YOU-/TURN-USER-Zeilen entfallen.
5. **Kopf**: eine Zeile Identität + Trust Strip (`Short`); keine Erklärsätze, kein `[unchecked]`; Modell nach K1-Regel.
   Fußzeile Zeile 1: der ausgewählte Schritt (`step 5 · tests · ✓ 8 passed`), Zeile 2: Tasten.
6. **Pfade repo-relativ** (cwd-Präfix aus `Lineage::cwd` abziehen; außerhalb des Repos: `…/<basename>`), Kommandos
   vollständig in der Detailbox, in der Liste mit `…` am Zeilenende.
7. **RESULT-Zeile** statt CHANGE/REVIEW-Knoten: Dateien, Zeilen, Commit (Kurz-Id + Betreff), Review-Zustand leise.
   Enter auf einem `wrote`/`edited`-Schritt öffnet den Changes-Tab auf dieser Datei (B2-Rückweg).
8. **Timeline (`t`) entfällt** — die Story ist die Timeline. `t` wird frei.
9. **Layout**: Ist eine Session geöffnet, schrumpft die Liste links auf 32 Spalten (Zeit + Kurz-Headline) oder
   verschwindet unter 140 Spalten; die Story bekommt den Rest. Cursor `theme::cursor()` über die ganze Zeile.

### Akzeptanz / Tests
- Render 160×40 mit der Demo-Fixture: 10 Schritte, Verben korrekt, Testergebnis an Schritt 5/6, kein absoluter Pfad,
  der Prompt genau einmal, kein `unknown`, kein `⚠` an Review.
- Subagent-Fixture (`xtask` Demo hat `a_spawned_subagent_hangs_off_the_agent_when_resolvable` als Vorlage im Reader):
  Unterliste eingerückt, Enter öffnet die Kind-Session, Esc zurück.
- Zoom-Fixpunkt bleibt auf demselben Schritt (B5).
- Klasse 1 (bleiben, ggf. umziehen): Evidence-Glyph/Wort je Schritt nie nur Farbe; Claim-Zeilen `◌`; Sanitisierung
  fremder Texte (`foreign_text_in_arguments_is_sanitized_and_truncated`); Subagent-Zuordnung.
- Klasse 2: alle Snapshots auf Baumzeichen (`┣━`, `┗━`, `YOU`-Box, `TURN`, `CHANGE I…`, `EDIT` für Write).

### Reihenfolge
Nach Batch 2 und C10. Vier Commits: (1) `feat(reader): session steps with verb, target, outcome and relative time`
(Aggregation im Reader), (2) `feat(tui): session story replaces the graph tree`, (3) `feat(tui): subagents and
handovers as nested steps`, (4) `refactor(tui): remove timeline view, story is the timeline`.

## C12 · Evidence v2, Tab-Reihenfolge nach Workflow, Requirements-Tab

*Grundlage: Screenshots vom 19.10. (Evidence-Ebene `e`, Intent-Tab).*

### C12.1 · Evidence: ein Satz für Menschen, drei Achsen, Technik zum Aufklappen

**Befund.** Die Ebene besteht aus einem Kopfkasten, sieben VERDICT-Zeilen und einem INTEGRITY-Block mit
Algorithmus, Seal-Ids und zwei Sätzen über Oracles. Die Aussage ist nirgends in einem Satz zu lesen:
- Sieben Zeilen (INTEGRITY, COVERAGE, ARTIFACT, EPOCHS, SIGNATURE, INTERPRETATION, LIMITS) statt der drei Achsen;
  EPOCHS und SIGNATURE sind Teilaspekte von INTEGRITY, ARTIFACT gehört zu COVERAGE.
- `LIMITS  17 named limits at A1 observed`, `within witness/v1`, `hash-valid`, `derive_key, contexts
  minds/evidence/v1/*`, `anti-oracle` — Jargon ohne Satz.
- **Wortwiderspruch**: Kopf sagt „Cryptographically verified within the recorded observation boundary“, Zeile
  SIGNATURE sagt „validity is checked by `minds verify`“. Entweder ist verifiziert (dann steht das Urteil von
  `minds verify` da) oder nicht (dann heißt es „sealed, not yet verified“). Beides zugleich ist genau die
  Verwechslung, vor der die Overview-Fußzeile warnt.
- **Zahlenwiderspruch**: `ARTIFACT 95/95 lines explained` neben Changes `reported 29 · observed 0` und Overview
  `95 of 95 reported, not observed`. „explained“ ist hier „reported oder observed“ — ein drittes Wort für
  dieselbe Sache (C9.4).
- Fußzeile wiederholt den Kopfsatz und hängt einen weiteren Satz an, abgeschnitten.

**Zielbild** (Ebene `e` aus der Session Story, C11; später als Abschnitt in der Story):

```
EVIDENCE · session b3-75f030fa · ◈ ● ◐                     checked 10:42Z

This session's record is intact and signed, and all 24 events lie
inside the witness boundary. The 95 added lines are reported by the
agent; none were observed by a witness.
Verdict of minds verify: ✓ VERIFIED (signatures checked 10:42Z)

INTEGRITY       ✓ record intact · 1 seal, signed · chain closed (1 epoch)
COVERAGE        ✓ complete · 24 events · 0 gaps
                ◇ 95 of 95 added lines reported, 0 observed
INTERPRETATION  ✓ all tool calls interpreted
LIMITS          observed at A1 — no witness ran; the agent's own
                reports are the only source for file contents.

TECHNICAL (for auditors)                                   ▸ Space
```

Aufgeklappt (Space): Seal-Id, Chain-Root, Algorithmus und Kontexte, „externally verifiable: seal identity and
signature“, „chain root reproducible only locally (journal + session salt)“ — die heutigen INTEGRITY-Zeilen,
unverändert, nur eingeklappt.

**Regeln.**
- Zeile 1–3: der Satz für Menschen, **je Achse ein Satzteil**, kein Gesamturteil (Invariante). Zeile 4: das
  Urteil von `minds verify` mit seinen Wörtern (Klasse 1), oder `sealed, not yet verified — v: run verify`.
  Dieselbe `CommitVerify`-Quelle wie Overview-EVIDENCE; der Satz „Cryptographically verified“ entfällt.
- Genau die drei Achsen als Zeilen; EPOCHS und SIGNATURE werden Satzteile von INTEGRITY, ARTIFACT eine
  zweite Zeile unter COVERAGE mit den Wörtern aus C9.4 (`reported`/`observed`, nie „explained“).
- LIMITS als **Satz** (Glossar C1 je Assurance-Stufe A0–A3), nicht als Zahl benannter Limits.
- Fußzeile Zeile 1: nur `b3-75f030fa · ◈ ● ◐ · ✓ VERIFIED`; keine Sätze.
- Klasse 1 bleiben: kein VERIFIED ohne Signaturprüfung, `◈ sealed` nur wenn jedes Seal signiert ist,
  drei Achsen nie zusammengefasst, Farbe nie allein. Klasse 2: Snapshots auf die sieben VERDICT-Labels,
  „Cryptographically verified…“, `hash-valid` in der Ebene (das Wort bleibt in `minds verify` und im Verify-Urteil).

### C12.2 · Tabs in Workflow-Reihenfolge, Verify-Tab geht in die Overview auf

**Entscheidung.** `1 Overview · 2 Changes · 3 Sessions · 4 Requirements` (F1–F4 und `1`–`4`).
Das ist die Reihenfolge des Untersuchungswegs (C-Flow): was und warum → was genau → wie → unter welcher Anforderung.

- **Verify-Tab entfällt.** Vorher wandern seine einzigen Inhalte, die Overview-EVIDENCE noch nicht hat, dorthin:
  - `SCOPE` — Dateien außerhalb des deklarierten Scopes (`CommitVerify::out_of_scope`): eine Zeile
    `✓ all changes inside src/**` bzw. `◦ 2 files outside scope → d`.
  - `CONFORMANCE` — nur wenn eine Spec gebunden ist (`fill_conformance`): `✓ conforms` / `◦ 3 findings → …`.
  - Assurance-Stufe je Session als Zusatz in INTEGRITY (`· A1`), Glossar-Satz in LIMITS (C12.1).
  - `v` in der Overview startet die Prüfung (C5); der Worker aus B6 liefert sie.
- Tests: Verify-Tab-Snapshots → Klasse 2 mit Umzug der Versprechen (Urteilswörter der CLI, kein VERIFIED ohne
  Prüfung, `! VERIFIED, INCOMPLETE`-Parität) auf Overview-EVIDENCE. `the_verdict_is_the_one_of_minds_verify`
  wird auf den EVIDENCE-Block umgezogen, nicht gelöscht.
- Hilfe, Fußzeilen, `docs/commands.md`, README-Screenshots anpassen. Breaking Change im Changelog.

### C12.3 · Requirements-Tab: die Anforderung zuerst, dann ihr Status, Technik zuletzt

**Was der Tab aussagt:** Welche Anforderungen sind als Anker festgehalten, sind sie belegt und signiert, und welche
Sessions haben unter ihnen gearbeitet. Es ist das Register der Anforderungen — Verwaltung, kein Untersuchungsschritt.
Für die Demo beantwortet er: *Die Anforderung stand fest, bevor der Agent anfing; der Agent hat sich auf sie bezogen;
hier ist der Beleg.*

**Befund.** Der Anker-Hash in voller Länge zweimal (anchor, content), die Anforderung selbst steht ganz unten als
„Snapshot (204 bytes, as stored)“; `binding  not the local file binding (A1)` und die Satzketten zu proof/signature sind
nur für Kenner lesbar; `◉1` als Pille.

**Änderung.**
- Tab heißt **Requirements** (UI-Wort aus C3; CLI bleibt `minds intent …`). Breadcrumb `Requirements`.
- Liste: `✓ ac8f1bb7 · anforderung.md (HEAD) · src/** · 1 session` — Kurz-Id, Quelle, Scope, Sessions als Zahl.
  Status-Glyph: `✓` proven & signed, `◦` proven, not signed, `✗` snapshot mismatch.
- Detail, Reihenfolge fest:
  1. **REQUIREMENT** — der Snapshot-Text **oben**, vollständig (nur wenn proof ok; sonst der Grund, nie der Text — Invariante).
  2. **STATUS** — drei Zeilen mit Satz: `✓ proven — the stored snapshot matches the file at 996395ef`,
     `✓ signed — software key, checked against your allowed_signers (as minds verify does)`,
     `· binding A1 — the agent named this anchor in a witness event, not a local file`.
  3. **SESSIONS (1)** — wie C3.4, Enter öffnet die Story.
  4. **TECHNICAL** — anchor, content hash, source in voller Länge, zum Aufklappen (Space).
- Ein Id-Format (Kurzform wie überall), `◉1` als Text `1 session`.

### Reihenfolge
C12.2 (Tabs + Verify-Umzug) direkt nach Batch 2 — vor C10, weil beide Entfernungen dieselben Hilfe-/Fußzeilen-Tests
berühren und in einem Zug umziehen sollten. C12.1 mit C11 (die Evidence-Ebene hängt an der Story). C12.3 danach.

---

# Phase D — Demo-Fixture (optional, aber empfohlen)

- `xtask` bekommt ein Kommando, das ein reproduzierbares Demo-Repository erzeugt, mit drei Fällen:
  1. **sauber**: eine Session mit klarem Intent, drei verworfenen Ansätzen, belegter Verknüpfung;
  2. **Lücke**: unerklärte Zeilen bzw. ein Gap in der Kette;
  3. **Manipulation**: per Befehl nachträglich verändertes Seal-Material → Tamper-Banner (A5), Verify TAMPERED.
- Kein eigener Demo-Modus in der TUI — die echte TUI läuft gegen die Fixture.
- Die Render-Tests der Pakete dürfen dieselbe Fixture nutzen.

---

## Reihenfolge und Abhängigkeiten

```
A1 → A2 → A3 → A4 → A5 → A6      (vor der Demo)
B1 → B2 → B3 → B4 → B5 → B6 → B7
C1 → C2 → C3 → C4 → C5 → C6 → C7 → C8 → C9 → C12.2 → C10 → C11 + C12.1 → C12.3 (nach Batch 2)
D (parallel möglich, ab A5 sinnvoll)
```

A4 setzt A1 voraus; A6 setzt A1 und A4 voraus; B2 setzt B1 voraus; C2 setzt C1 und A6 voraus;
C3 (Overview v2) setzt A4, B1 (Esc-Reihenfolge) und B6 (Verify im Worker) voraus; B3 ist hilfreich, aber nicht zwingend.

## Definition of Done (je Paket)

- [ ] Ein Conventional Commit, CI-Triade grün
- [ ] Render-Tests in 80×24, 120×30, 160×40 für die betroffenen Tabs
- [ ] Keine Invariante verletzt; keine Achsen verschmolzen; kein Claim im Evidence-Stil
- [ ] Hilfe und Fußzeilen passen zum neuen Verhalten
- [ ] `CHANGELOG.md` ergänzt
- [ ] Kurzer Bericht mit Vorher/Nachher-Ausschnitt

---

# Befunde außerhalb der TUI (an Claude Code als eigene Bugs geben)

## K1 · `model not recorded`, obwohl 24 Events ankamen

`adapter.rs::read_transcript` nimmt den `transcript_path` des letzten Events und verschluckt jeden Fehler still
(`Err(_) => Transcript::default()`). Ergebnis: Modell, Antworttext und Token fehlen, und niemand erfährt warum.

**Fix.**
1. Der Grund wird als `CaptureNote` an der Session gespeichert („transcript not read: <path>: <io error>“ bzw.
   „no event carried a transcript path“) — die TUI zeigt ihn hinter `model not recorded` an.
2. Reproduktion im Demo-Repo: Enthalten die Journal-Events einen `transcript_path`? Existiert die Datei unter
   `~/.claude/projects/<slug>/<claude-session-id>.jsonl` zum Zeitpunkt des Checkpoints, und darf der Prozess,
   der den Checkpoint ausführt (Hook-Prozess vs. Witness-Service), sie lesen?
3. Fallback: Liefert kein Event den Pfad, das Transkript über den Claude-Slug des Repos suchen
   (`import.rs::claude_slug`) — nur lesen, nie importieren.

## K2 · Zwei fremde `open`-Sessions in einem frischen Repo

Die Overview listet Sessions **aus dem Store dieses Repos** (`refs/minds/…`), die keinem Commit zugeordnet sind.
Zwei Sessions mit dem Prompt „Setze LOG-2417 … um“ im Store bedeuten, dass sie in diesen Store geschrieben wurden.
Mögliche Wege, in dieser Reihenfolge prüfen:
1. **`minds import`** liest `~/.claude/projects/<slug>/` — der Slug ist der Verzeichnispfad. Wurde das Demo-Repo
   am selben Pfad neu angelegt, liegen dort noch die alten Claude-Code-Transkripte und werden importiert.
2. Ein **Child-Repo-Store** (`.git/config`), den schon ein anderes Repo benutzt hat.
3. `refs/minds/*` per `git fetch`/Clone mitgebracht.

Aufräumen: `minds forget <session>`. Der Reader soll solchen Sessions ihr Alter und ihren Ursprung
(`imported` / `captured`) ansehen lassen — `Lineage` kennt beides.
