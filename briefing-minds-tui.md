# Briefing: `minds ui` — interaktive Session-TUI nach ghlens-Vorbild

**Projekt:** Minds (github.com/munichbughunter/minds)
**Zielgruppe dieses Briefings:** implementierender Agent
**Sprache im Code:** Deutsch (Doc-Kommentare, Fehlermeldungen, Commit-Bodies). Conventional Commits.

---

## 1. Kontext und Motivation

`minds recap` zeigt die letzten Sessions als Liste. Sessions sind aber hierarchisch —
Session → Tool-Aufrufe → berührte Dateien → Checkpoints → Commits → Review — und eine
statische Liste bildet das nicht ab. Referenz-Vorbild ist **ghlens**
(github.com/dmissoh/ghlens, MIT, Rust/ratatui): eine TUI über die Event-Historie
eines GitHub-Repos mit einem Glyph plus Farbe pro Event-Typ, einer Tages-Sparkline
und Live-Filtern pro Spalte.

Drei Interaktionsmuster daraus übernehmen wir, das Datenmodell nicht:

1. **Drill-down:** Enter auf einer Zeile klappt Kind-Zeilen ein/aus (bei ghlens:
   Push → Commit-Subjects; bei uns: Session → Tool-Aufrufe/Dateien/Checkpoints).
2. **Fokussieren (Ctrl-F):** ersetzt alle Filter durch einen einzigen Fokus-Filter
   auf das gewählte Objekt. Bei uns fokussiert Ctrl-F die **Change-Id** der
   gewählten Session und zeigt deren gesamten Strang: Session(s), Checkpoints,
   Review-Verdict, Commits.
3. **Piped-Fallback:** ist stdout kein Terminal, werden die gefilterten Zeilen
   schlicht gedruckt (tab-separiert), damit `minds ui … | grep` / `| fzf`
   funktionieren. Mensch bekommt TUI, Agent/Pipe bekommt Zeilen — gleiche
   Datenquelle, gleiche Filterlogik.

Entscheidender Unterschied zu ghlens: **keine API, kein Netz, keine Auth.** Alle
Daten kommen lokal aus `refs/minds/` über die bestehenden Reader-APIs. Offline und
im Air-Gap voll funktionsfähig — wie alles in Minds.

---

## 2. Leitplanken (nicht verhandelbar)

1. **Kein zweiter Datenpfad.** Die TUI liest ausschließlich über die öffentlichen
   APIs von `minds-reader` — dieselben, die `recap`/`show`/`why` nutzen. Sie
   enthält **keine** eigene Ref-, Store- oder Envelope-Leselogik. Fehlt der TUI
   eine Information, wird zuerst die Reader-API erweitert (eigener Commit), dann
   die TUI angebunden.
2. **Strikt lesend.** Die TUI schreibt nichts: keine Refs, keine Reviews, keine
   Konfiguration. v1 ist read-only.
3. **Nur redigierte, gespeicherte Daten.** Angezeigt wird ausschließlich, was der
   Store liefert (fail-closed redigiert). Kein Zugriff auf das Journal unter
   `.git/minds/journal/` und keine Roh-Payloads.
4. **Pure-Rust-Dependencies.** `ratatui` + `crossterm` (Versionen zentral in
   `[workspace.dependencies]` pinnen). Kein CGO, keine C-Bindings — das statische
   Binary bleibt statisch. Keine weiteren neuen Crates ohne Rückfrage.
5. **Fail-soft.** Ein kaputter einzelner Store-Eintrag (Parse-Fehler, Tombstone
   nach `forget`, unbekannte Schema-Felder) darf die TUI nie crashen: die Zeile
   wird als degradiert markiert (z. B. `⌦ gelöscht` / `? unlesbar`), der Rest
   funktioniert weiter. Terminal-Zustand wird auch bei Panic sauber
   wiederhergestellt (Panic-Hook, der Raw-Mode/Alternate-Screen verlässt).
6. **Kern-CLI bleibt schlank.** Die TUI lebt in einem **eigenen Crate
   `minds-tui`**, das `minds-cli` hinter dem Cargo-Feature `tui` einbindet
   (Default-Feature der Release-Builds; `--no-default-features` baut weiterhin
   ohne).

---

## 3. Scope

### In Scope

- **AP0 — Read-Model prüfen/ergänzen.** Sichten, welche Reader-API die nötigen
  Felder liefert (siehe Spalten unten, plus Change-Id, Checkpoint-Liste je
  Session, Review-Verdict falls vorhanden). Lücken zuerst als saubere, getestete
  Erweiterung in `minds-reader` schließen — additive Felder, `skip_serializing_if`
  wo serde im Spiel ist.
- **AP1 — Crate-Gerüst.** Neues Crate `crates/minds-tui` (Workspace-Vererbung,
  Edition 2024, MSRV 1.85). `minds-cli` bekommt das Subkommando `minds ui`
  hinter Feature `tui`; ohne Feature existiert das Subkommando nicht (sauberer
  `cfg`-Schnitt, keine Laufzeit-Fehlermeldung nötig).
- **AP2 — Listenansicht.** Eine Zeile pro Session, ein Glyph + Farbe pro
  Session-Art/Zustand. Spalten: **Datum · Agent · Modell · Change-Id (gekürzt) ·
  Verdict · Zusammenfassung/Prompt-Anriss**. Spaltenbreiten fix mit flexender
  letzter Spalte (wie ghlens' Detail-Spalte). Navigation: ↑↓, PgUp/PgDn,
  Home/End; Ctrl-Q beendet, Esc löscht Filter bzw. beendet ohne Filter.
- **AP3 — Drill-down.** Enter (Markierung `▸`/`▾`) klappt unter der Session
  eingerückte `↳`-Zeilen auf: berührte Dateien mit Effekt-Art, Tool-Aufrufe
  (Anzahl/Namen), Checkpoints mit Commit-SHA. Daten kommen aus dem bereits
  geladenen Session-Objekt — **kein** Nachladen pro Expand nötig (anders als
  ghlens, das dafür eine API-Runde braucht).
- **AP4 — Filter.** Live-Substring-Filter mit ghlens-Semantik: Tab/Shift-Tab
  wechselt die aktive Spalte (Alle → Datum → Agent → Verdict → Detail);
  innerhalb eines Filters UND-verknüpfte, leerzeichengetrennte Terme; Filter
  über Spalten hinweg UND-verknüpft. Aktive Spalte im Header hervorgehoben,
  gesetzte Filter als Chips im Footer. Ein CLI-Positionsargument seedet den
  „Alle"-Filter (`minds ui glpat-rotation`).
- **AP5 — Change-Id-Fokus (Ctrl-F).** Ersetzt alle Filter durch einen Fokus auf
  die Change-Id der gewählten Zeile; die Ansicht zeigt dann den kompletten
  Strang dieser Änderung. Esc hebt den Fokus wieder auf.
- **AP6 — Sparkline.** Ein Balken pro Tag über die *gefilterte* Historie,
  Blocktitel nennt die Skala (voller Balken = Peak-Tag), Achse mit Start-,
  Mittel-, End-Datum. Tag der selektierten Zeile hervorgehoben.
- **AP7 — Piped-Fallback.** `!stdout.is_terminal()` ⇒ gefilterte Zeilen
  tab-separiert drucken, Exit 0. Keine ANSI-Sequenzen im Pipe-Modus.
- **AP8 — Tests.** Render-Smoke-Tests über ratatuis `TestBackend` (Snapshot der
  gezeichneten Puffer für: leere Liste, gefüllte Liste, expandierte Zeile,
  gesetzte Filter, degradierte Zeile). Unit-Tests für die Filterlogik (Terme,
  Spalten, Fokus) — die Filterlogik dafür als reines, UI-freies Modul schneiden.
  Integrationstest für den Piped-Fallback.

### Out of Scope (explizit NICHT bauen)

- Kein Schreiben: keine Review-Aktionen, kein `forget`, kein `sign` aus der TUI.
- Kein Live-Refresh (einmaliges Laden beim Start genügt für v1 — ghlens hat das
  auch nicht).
- Keine Maus-Unterstützung in v1 (Tastatur vollständig; Maus ist ein sauber
  abtrennbares Folge-AP).
- Kein Ctrl-O „im Browser öffnen" (GitLab-Projektion ist ein eigenes Vorhaben).
- Keine GitLab-/Netzwerk-Datenquelle. Nur lokale Refs über `minds-reader`.
- Keine eigene Konfigurationsdatei; höchstens bestehende Reader-Konfiguration
  mitnutzen.

---

## 4. Technisches Design (Skizze — verfeinern erlaubt, Grenzen nicht)

```text
crates/minds-tui/
  src/
    lib.rs          // Einstieg: run(reader: &…) -> Result<…>
    model.rs        // Zeilen-Modell: SessionRow, ChildRow, RowState (reine Daten)
    filter.rs       // Filterlogik: Spalten, Terme, Fokus — UI-frei, voll getestet
    view.rs         // ratatui-Rendering: Liste, Header, Footer, Chips
    sparkline.rs    // Tages-Aggregation + Rendering
    input.rs        // Key-Events -> App-Aktionen (reines Mapping, testbar)
    app.rs          // Event-Loop, App-State, Terminal-Setup/-Teardown, Panic-Hook
    pipe.rs         // Nicht-Terminal-Ausgabe (tab-separiert)
```

- **Trennung Logik/Rendering:** `filter.rs`, `input.rs`, `sparkline.rs`
  (Aggregation) sind UI-frei und unit-testbar. `view.rs` wird nur über
  `TestBackend`-Snapshots geprüft.
- **Glyphen** (Vorschlag, konsistent dokumentieren): `●` Session ·
  `↳` Kind-Zeile · `✔`/`✖`/`○` Verdict approved/rejected/offen ·
  `⌦` per `forget` getilgt · `?` unlesbar/degradiert. Nur Unicode, das in
  gängigen Monospace-Fonts sicher ist; keine Emoji.
- **Zeitzone/Datum:** Anzeige lokal, Aggregation für die Sparkline pro lokalem
  Kalendertag; Quelle bleiben die RFC-3339-Zeitstempel des Readers (keine neue
  Datums-Crate — die bestehende `clock`-Linie respektieren).
- **Performance:** alle Sessions einmal laden, danach nur In-Memory-Filterung.
  Erst ab spürbaren Problemen (>~5 000 Sessions) über Lazy-Loading nachdenken —
  nicht vorab bauen.

---

## 5. Arbeitsweise und Definition of Done

- **Micro-Commit-Workflow** je AP: `cargo fmt` →
  `cargo clippy -p minds-tui -- -D warnings` (bei AP0 zusätzlich
  `-p minds-reader`) → `cargo test -p minds-tui` → am Ende
  `cargo test --workspace` und ein Build mit `--no-default-features` von
  `minds-cli`, der weiterhin grün sein muss.
- **Conventional Commits**, Body Deutsch. Beispiel:
  `feat(tui): Listenansicht mit Spaltenfiltern und Sparkline`.
- **Dateiplatzierung immer explizit benennen**; nur geänderte/neue Dateien
  liefern; bestehende Dateien vor Änderungen im Ist-Stand ansehen.
- **Neue Dependencies:** genau `ratatui` und `crossterm`, zentral in
  `[workspace.dependencies]`, im Crate per Workspace-Vererbung. Vor dem ersten
  Code die aktuellen API-Stände auf docs.rs prüfen (gleiche Disziplin wie bei
  gix — nicht raten).
- **Keine erfundene Git-Identität**, auch nicht in Test-Fixtures.

**Done, wenn:**

1. `minds ui` startet in einem Repo mit Sessions, zeigt Liste + Sparkline, und
   Drill-down/Filter/Fokus verhalten sich wie in Abschnitt 3 beschrieben.
2. `minds ui <filter> | cat` druckt die gefilterten Zeilen ohne TUI und ohne
   ANSI-Codes.
3. Ein Repo ohne Sessions zeigt einen freundlichen Leerzustand (Hinweis auf
   `minds enable`), kein Fehler.
4. Ein künstlich beschädigter Store-Eintrag und ein Tombstone erscheinen als
   degradierte Zeilen; die TUI läuft weiter.
5. Nach Panic im Event-Loop ist das Terminal benutzbar (kein hängender Raw-Mode).
6. fmt/clippy(-D warnings)/Tests grün im Workspace, `--no-default-features`-Build
   von `minds-cli` grün.

---

## 6. Referenzen

- ghlens README — Interaktionsmuster (Tab-Spaltenfilter, Ctrl-F-Fokus, `▸`/`▾`
  Drill-down, Sparkline, Piped-Fallback) als Vorbild; Datenmodell und
  `gh`-Anbindung ausdrücklich **nicht** übernehmen.
- `minds recap` / `minds show` in `minds-cli` — die bestehende Reader-Nutzung,
  an die sich die TUI anzuhängen hat.
- ratatui-Doku (`TestBackend`) für die Snapshot-Tests.
