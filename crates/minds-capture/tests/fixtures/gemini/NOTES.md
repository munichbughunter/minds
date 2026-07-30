# Gemini-CLI — Format-Notizen (für Track A)

Quelle: echtes Gemini-CLI-Chat-Persistenz-Format, anonymisiert.
Ort auf der Platte: `~/.gemini/tmp/<projectHash>/chats/session-<ts>-<id>.json`.

Das ist das **Transkript**-Äquivalent zu Claude Codes `~/.claude/projects/<slug>/<id>.jsonl`
— aber **ein einzelnes JSON-Objekt** mit `messages`-Array, kein JSONL.

## Feld-Mapping → `minds_core::Session`

| Gemini | minds |
|---|---|
| `sessionId` | `Lineage.local_id` |
| `startTime` / `lastUpdated` | `lineage.started_at` / `ended_at` |
| `messages[].model` (`gemini-2.5-pro`) | `Model { provider: "google", id }` |
| `messages[].tokens.{input,output}` (summiert) | `usage.input_tokens` / `output_tokens` |
| `messages[].type == "user"` (echt) | `Turn { role: User, text: content }` |
| `messages[].type == "gemini"` | `Turn { role: Assistant, text: content }` (+ `thoughts[]`) |
| `messages[].type == "user"` mit `content = "[Function Response: <tool>]…"` | Tool-**Ergebnis** |

## Der Haken: Tool-Calls sind nicht strukturiert

Anders als Claudes strukturiertes `tool_use`/`tool_input` erscheinen bei Gemini
nur die Tool-**Ergebnisse** — als `type:"user"`-Nachricht mit Präfix
`[Function Response: <toolname>]`. Der Aufruf selbst und seine Argumente sind
nicht als Feld da; berührte Pfade stecken im Antworttext als
`--- <absoluter Pfad> ---`-Abschnitte (bei `read_many_files`).

Folge für den Adapter: `EffectKind`/Pfade müssen aus dem Function-Response-Text
geparst werden (die `--- … ---`-Marker), nicht aus einem sauberen `tool_input`.
`read_many_files` → `Read`; `replace`/`write_file` → `Write`; `run_shell_command`
→ `Exec`. Ob ein Function-Response *Schreiben* oder *Lesen* war, verrät der
Tool-Name im Präfix, nicht die Struktur.

## Was dieses Sample freischaltet

- **Gemini-Transkript-Reader** (A.6): Model, Tokens, Assistant-Text, Herkunft.
- **Gemini-Importer** (A.7): Backfill aus `.gemini/tmp/<hash>/chats/*.json`.

## Was es NICHT liefert

- Das **Hook-Payload**-Format (was `minds hook --agent gemini` auf stdin bekommt).
  Das ist für den Live-Effekt-Adapter (A.3/A.4) nötig und muss separat als Sample
  besorgt werden (ein Gemini-Hook-Event, konfiguriert über `.gemini/settings.json`).
