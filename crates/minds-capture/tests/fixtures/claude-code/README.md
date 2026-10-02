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
