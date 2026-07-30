# ADR-0003 — Capture über Agent-Hooks statt Transkript-Parsing

- Status: angenommen
- Datum: 2026-07-23
- Betrifft: `minds-capture`, `minds-cli`
- Ersetzt: die M5-Skizze aus `Plan.md` (nachträgliches Lesen der Transkript-Dateien)

## Kontext

Die erste M5-Skizze wollte die Logdateien der Agents im Nachhinein parsen: ein
`SessionAdapter`-Trait pro Agent, gefüttert aus dessen Transkript. Beim Bau
zeigten sich drei Probleme, von denen das dritte den Ausschlag gab.

1. **Vergänglichkeit.** Claude Code löscht seine Transkripte nach 30 Tagen. Was
   nicht rechtzeitig übernommen wurde, ist unwiederbringlich weg. Ein Capture,
   der von einem Nachher lebt, verliert Beweismittel durch bloßes Warten.

2. **Formatvielfalt.** Jeder Agent hat ein eigenes Log, und die ändern sich
   ständig. Das ist die Fleißarbeit, die schon die Vision als schwerste Last
   benennt.

3. **Ordnung über Agents hinweg — der eigentliche Grund.** Ein Transkript-Parser
   sieht immer nur *ein* Transkript. Er kann deshalb prinzipiell nicht wissen,
   dass Codex zwischen zwei Claude-Turns ein Review geschrieben hat. Die für
   Minds zentrale Aussage „Claude plant, Codex reviewt, Claude implementiert die
   Review-Punkte" ist aus einem einzelnen Log nicht belegbar — nur *vermutbar*.

## Entscheidung

Wir übernehmen den Hook-Ansatz von [entire.io](https://entire.io), umgesetzt in
Rust: **Minds installiert Hooks im Agenten selbst** und nimmt jedes Event live
über ein winziges Kommando `minds hook` entgegen.

- Ruft *jeder* Agent-Hook dasselbe Binary auf, das in *dasselbe* Journal
  schreibt, dann sind Ereignisse verschiedener Agents von **einem Beobachter mit
  einer Uhr** aufgezeichnet. Die Kante zwischen ihnen wird damit
  `Evidence::Observed` statt `Inferred` — beobachtet, nicht geraten.
- Das Journal liegt unter `<git-dir>/minds/journal/` (0600, kein Git-Objekt,
  nicht im Worktree). Der Hook ist **fail-open** und endet immer mit 0; er darf
  die Sitzung des Nutzers nie abbrechen.
- Das Transkript wird nicht überflüssig, es wechselt die Rolle: Der Hook liefert
  Zeitpunkt, Reihenfolge und Kausalität; das Transkript liefert den reichen
  Inhalt (Volltext, Thinking, Token-Zähler). Beide Hälften werden erst beim
  Checkpoint (kalt, fail-closed) zusammengeführt.

### Installation je Agent (`minds enable`)

| Agent       | Ziel                                                            |
|-------------|-----------------------------------------------------------------|
| Claude Code | `.claude/settings.json` (`hooks`)                               |
| Codex       | `.codex/hooks.json` + `codex_hooks = true` in `config.toml`     |
| Cursor      | `.cursor/hooks.json`                                            |
| Gemini      | `.gemini/settings.json`                                         |
| OpenCode    | TypeScript-Plugin                                               |

Zusätzlich Git-Hooks (`post-commit`/`prepare-commit-msg`): Ein Checkpoint
entsteht, wenn du oder der Agent committen. Alle Merges sind **idempotent** — ein
zweites `minds enable` ändert nichts, und eine fremde Konfiguration in denselben
Dateien bleibt erhalten.

## Konsequenzen

**Gut.** Beweismittel entstehen live und vollständig; Reihenfolge über Agents
hinweg ist beobachtet, nicht geraten; ein neuer Agent kostet eine Hook-Registrierung
statt eines Transkript-Parsers.

**Preis.** Fail-open heißt: ein Event *kann* fehlen. Deshalb trägt jedes Event
eine lückenlose Sequenznummer, und `ReadOutcome::gaps` macht Fehlendes sichtbar —
ehrlich lückenhaft schlägt still vollständig. `minds enable` muss fünf Agent-Formate
kennen und sauber mergen; das ist die verbleibende Fleißarbeit, aber eine, die pro
Agent einmal anfällt und nicht bei jedem Format-Wechsel neu.

**Additiv am Kern.** Die core-Erweiterungen (`Lineage`, `Turn.parent/at`,
`ToolCall.effect`, `Vec<Edge>`) tragen alle `skip_serializing_if`. Eine Session
ohne Herkunft serialisiert byte-identisch wie vor M5 und behält ihre `SessionId`;
`SCHEMA_VERSION` bleibt bei 1.
