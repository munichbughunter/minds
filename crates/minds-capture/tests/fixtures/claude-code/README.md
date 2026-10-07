# Claude Code — Hook-Payload-Fixtures (EA-01a)

Quelle: eine echte, nicht-interaktive Claude-Code-Session (`claude -p`) in einem
Scratch-Repo mit `PreToolUse`-/`PostToolUse`-Hooks, die den rohen stdin-Payload
unverändert mitgeschrieben haben. Anschließend anonymisiert: Nutzerpfade →
`/home/anna/…`. Sonst nichts verändert — Schlüsselreihenfolge, Zahlen und
Escapes sind die des Agenten.

- **Claude Code 2.1.282**, aufgezeichnet am 2026-09-25.
- Paare `<fall>.pre.json` / `<fall>.post.json` gehören über `tool_use_id`
  zusammen. Alle Events stammen aus **einer** Session (`session_id` gleich).
- `<datei>.after-<fall>` ist die Datei, wie sie **nach** dem Tool-Aufruf auf der
  Platte lag — das Vergleichsmaterial für `Effect.written`.

| Fall | Tool | Was der Post-Payload trägt |
|---|---|---|
| `write-create` | `Write` (neue Datei) | `tool_input.content`; `tool_response.{type:"create", content, originalFile:null}` |
| `write-update` | `Write` (überschreibt) | wie oben, `type:"update"`, `originalFile` = vorheriger Inhalt |
| `write-create-notebook` | `Write` (`.ipynb`) | wie `write-create` |
| `edit-single` | `Edit`, `replace_all:false` | `tool_response.{originalFile, oldString, newString, replaceAll, structuredPatch}` — **kein** Nachher-Inhalt |
| `edit-replace-all` | `Edit`, `replace_all:true` | wie oben, `replaceAll:true`; `beta` kam zweimal vor |
| `notebook-edit` | `NotebookEdit` | `tool_response.{original_file, updated_file, old_source, new_source, …}` — `updated_file` ist der Nachher-Inhalt (vom Tool neu serialisiert, 1-Space-Einrückung) |

## Was es NICHT gibt

- **`MultiEdit`:** In Claude Code 2.1.x existiert das Tool nicht mehr
  (`ToolSearch select:MultiEdit` → keine Treffer, siehe Aufzeichnung). Ohne
  Fixture wird die Payload-Form nicht geraten; der Adapter meldet
  `written_unavailable: payload-without-content`.
- **CRLF-Dateien:** Nicht aufgezeichnet. Claude Code passt `newString` an die
  Zeilenenden des Originals an; ohne Fixture rekonstruiert der Adapter nicht,
  sondern meldet `reconstruction-failed`.
- **`Edit` mit leerem `old_string`** (Datei anlegen): nicht aufgezeichnet,
  ebenfalls `reconstruction-failed`.
- **`Edit`, das eine Zeile löscht** (`new_string: ""`): nicht aufgezeichnet.
  Claude Code nimmt dabei vermutlich den Zeilenumbruch mit; der Adapter baut
  das nicht nach, sondern prüft jede Rekonstruktion gegen den
  `structuredPatch` des Payloads — widerspricht sie ihm, meldet er
  `reconstruction-failed` statt eines falschen Hashes.
- **`userModified: true`** (Nutzer ändert den Vorschlag im IDE-Diff): nicht
  aufgezeichnet, in allen Aufnahmen `false`. Der Adapter meldet dann
  `user-modified`.

## Neu aufzeichnen

**Nur in einem Wegwerf-Scratch-Repo, nie in einem echten Arbeitsverzeichnis:**
Die Hooks schreiben die rohen Payloads samt vollständiger Dateiinhalte
(`originalFile`, `updated_file`) ungeschützt auf die Platte.

```sh
mkdir scratch && cd scratch && git init
cat > .claude/settings.json <<'JSON'
{"hooks":{"PreToolUse":[{"matcher":"","hooks":[{"type":"command","command":"cat >> $PWD/pre.jsonl; printf '\\n' >> $PWD/pre.jsonl"}]}],
          "PostToolUse":[{"matcher":"","hooks":[{"type":"command","command":"cat >> $PWD/post.jsonl; printf '\\n' >> $PWD/post.jsonl"}]}]}}
JSON
claude -p --dangerously-skip-permissions "Use Write/Edit/NotebookEdit on …"
```

Danach Pfade anonymisieren und die entstandenen Dateien als `*.after-*` ablegen.

---

# Bash-Runner-Fixtures (EA-18a)

`bash-<fall>.pre.json` / `bash-<fall>.post.json` — `Bash`-Aufrufe bekannter
Test-/Benchmark-Runner, für `ToolCall::outcome`.

## Herkunft — was echt ist und was nicht

- **Echt:** die Runner-Ausgaben und Exit-Codes. Jedes Kommando lief über das
  `Bash`-Tool von **Claude Code 2.1.292** (2026-10-07) in einem Scratch-Crate
  (`sortlib`: zwei Tests, einer `#[ignore]`, einer hinter dem Feature
  `broken` absichtlich rot; criterion-Benches; zwei pytest-Dateien). Das
  `tool_response`-Objekt bzw. der Fehlertext ist das `toolUseResult` aus dem
  Claude-Code-Transkript — derselbe Wert, den der Hook bekommt —
  unverändert bis auf die Pfad-Anonymisierung (`/home/anna/scratch`).
  Versionen: cargo/libtest stable, cargo-nextest 0.9.146, criterion 0.5,
  pytest 9.1.1 (Python 3.13).
- **Nachgebaut:** der Hook-Umschlag (`session_id`, `prompt_id`, `cwd`,
  `permission_mode`, `tool_use_id`, `duration_ms`, `description`) — nach den
  echten EA-01a-Aufnahmen oben und dem `PostToolUseFailure`-Beispiel der
  Claude-Code-Hook-Doku (`"error": "Exit code 1\n…"`, `is_interrupt`).
  Das Kommando im Payload ist das, was ein Agent schreiben würde
  (`pytest …` statt `.venv/bin/pytest …`, ohne `cd`-Präfix).
- **Warum nicht direkt aufgezeichnet:** Der Hook-Mitschnitt braucht eine
  verschachtelte `claude -p --dangerously-skip-permissions`-Session; die war
  in der Umgebung, in der diese Fixtures entstanden, nicht freigegeben.
  **Offen:** mit dem Rezept unten echt aufzeichnen und die Dateien ersetzen
  — die Tests müssen danach unverändert grün sein.

| Fall | Kommando | Event | Ergebnis |
|---|---|---|---|
| `cargo-test-pass` | `cargo test` | `PostToolUse` | 2 passed, 0 failed, 1 ignored (Unit + Doc-Tests summiert) |
| `cargo-test-fail` | `cargo test --features broken` | `PostToolUseFailure` | Exit 101; 2/1/1 |
| `nextest-pass` | `cargo nextest run` | `PostToolUse` | 2/0/1 aus der `Summary`-Zeile |
| `nextest-fail` | `cargo nextest run --features broken` | `PostToolUseFailure` | Exit 100; 2/1/1 |
| `criterion-bench` | `cargo bench --bench sort` | `PostToolUse` | `sort/1k` 298 ns, `sort/reverse_sorted_input/10000` 2863 ns, `sort/10` 26 ns |
| `pytest-pass` | `pytest py/test_calc.py` | `PostToolUse` | 2/0/1 |
| `pytest-fail` | `pytest py` | `PostToolUseFailure` | Exit 1; 2/1/1 |
| `unknown-command` | `ls src` | `PostToolUse` | kein Ergebnis, kein Hinweis |
| `compound-command` | `cargo test 2>&1 \| tail -3` | `PostToolUse` | kein Ergebnis, Hinweis `compound command not interpreted` |
| `env-prefix` | `RUST_LOG=debug cargo test` | `PostToolUse` | wie `cargo-test-pass`, argv ohne Präfix |

## Was die Aufnahmen über die Payload-Form zeigen

- **Erfolg** (`PostToolUse`): `tool_response = {stdout, stderr, interrupted,
  isImage, noOutputExpected}`. stderr ist in `stdout` **eingemischt**
  (`Compiling …`/`Finished …` stehen dort); `stderr` trug nur
  Harness-Hinweise. **Kein Exit-Code** — `exit_code` bleibt `None`.
- **Fehlschlag** (Exit ≠ 0): kein `tool_response`, sondern
  `PostToolUseFailure` mit `error = "Exit code <n>\n<Ausgabe>"`. Im Transkript
  steht davor `Error: `; der Parser toleriert beides.
- **Farben bleiben an:** nextest und pytest färben ihre Zusammenfassung
  (ANSI-SGR) — der Parser entfernt Escape-Sequenzen vor dem Lesen.
- **Große Ausgaben** (ab etwa 30 000 Zeichen): Claude Code behält nur den
  **Anfang** von `stdout` und ergänzt `persistedOutputPath`/
  `persistedOutputSize` (im Transkript dieser Session beobachtet: 65 951
  Zeichen → 29 761 behalten). Die Zusammenfassung am Ende fehlt dann; der
  Adapter liefert kein Ergebnis statt Teilsummen und liest die Datei nie
  (ihr Pfad kommt vom Agenten). Folge, bewusst fail-closed: `cargo test`
  eines mittelgroßen Workspace liefert in der Regel kein Ergebnis, sondern
  den Hinweis `result-not-captured` — kein Bug. Dasselbe für `cargo test -q`
  (keine Kopfzeilen).
- **criterion:** Namen über 23 Zeichen stehen auf eigener Zeile, `time:`
  eingerückt darunter; Einheit `µs` ist U+00B5. `change:`-Zeilen eines
  Folgelaufs werden ignoriert.

## Neu aufzeichnen

Wie oben (Hooks in `.claude/settings.json`, nur im Wegwerf-Repo), zusätzlich
ein Hook für `PostToolUseFailure`; `PATH` muss `cargo-nextest` und `pytest`
enthalten. Dann die zehn Kommandos der Tabelle je als eigenen `Bash`-Aufruf
ausführen lassen und Pfade anonymisieren.
