# Briefing: Evidence Chain — Track EV

**Projekt:** Minds (github.com/munichbughunter/minds)
**Zielgruppe dieses Briefings:** implementierender Agent
**Sprache im Code:** Deutsch (Doc-Kommentare, Fehlermeldungen, Commit-Bodies). Conventional Commits.
**Architektur-Entscheidungen:** vollständig in `docs/adr/0011-evidence-chain.md` — dieses
Briefing wiederholt sie nicht, es schneidet sie in Slices.

---

## 1. Kontext in drei Sätzen

Minds beweist heute die Integrität dessen, was gespeichert wurde — nicht den
Erfassungsbereich. Die Evidence Chain macht Lücken, Epochen und zurückgewiesene Nutzlasten
zu prüfbaren, versiegelten Aussagen: *hash every event, sign every checkpoint*. Der
Grundsatz steht in ADR-0011 und gilt in jedem Slice: **Aus dem Fehlen einer Evidence wird
nie auf das Fehlen eines Ereignisses geschlossen — fehlende Prüfung heißt Unknown, nicht
bestanden.**

## 2. Leitplanken (nicht verhandelbar)

1. **Hot Path bleibt fail-open und billig.** `minds hook`: immer Exit 0, keine zusätzlichen
   fsyncs, kein Lock, kein Git. Erlaubt sind genau die zwei zusätzlichen BLAKE3-Aufrufe aus
   ADR-0011 (Payload- und Event-Hash).
2. **Cold Path bleibt fail-closed.** Kein unredigierter Inhalt erreicht den Store; ein Seal
   enthält nie Intent, Pfade oder Redaction-Feldnamen — nur Hashes, Zählwerte, Agent,
   Zeitraum.
3. **Orakel-Regel:** Für Secret-Dateien entsteht nie ein Hash über geheimen Inhalt. Der
   Payload-Hash entsteht **nach** der Secretwall; `hash_artifacts` behält seine
   `is_secret_file`-Ausnahme.
4. **Determinismus:** Alles Gehashte und Versiegelte entsteht ohne Wanduhr und ohne Zufall —
   gleiche Events ⇒ gleicher Seal. Zeit im Seal stammt aus dem letzten Event. Tests
   injizieren die Uhr (Muster `crates/minds-capture/tests/fixtures.rs`).
5. **Tolerant lesen, kanonisch schreiben.** Alt-Events ohne Hashes sind `pre_chain`,
   Alt-Sessions (Schema 1) bleiben lesbar, Legacy-Evidence-Strings werden gemappt
   (`→ Unknown`). Geschrieben wird immer die neue Form.
6. **Nur `refs/minds/`**, Seals werden nie getilgt und nie force-gepusht; Ref-Namen tragen
   nie ein `local_id`-Derivat.
7. **TUI liest nur öffentliche `minds-reader`-APIs** (Briefing minds-tui): Fehlt der
   Oberfläche eine Information, wird zuerst der Reader erweitert — eigener Slice, eigener
   Commit.
8. **Statisches Binary:** keine neuen Dependencies (blake3, serde, thiserror, gix genügen).

## 3. Slices

Reihenfolge: EV.1 → EV.2 → EV.3 → EV.4 → EV.5/EV.6 (unabhängig) → EV.7 → EV.8 → EV.9 →
EV.10 → EV.11 → EV.12 → EV.13 → EV.14 → EV.15 → EV.16. Je Slice: CI-Triade
(`fmt` · `clippy -D warnings` · `test --workspace`), Code-Review, ab EV.5 zusätzlich
Security-Review (minds-capture/minds-redact berührt). Ein Slice = ein Commit-Vorschlag;
committet wird durch den Maintainer.

| Slice | Ziel | Kern-Dateien | DoD / Test |
|---|---|---|---|
| **EV.1** | ADR-0011 + dieses Briefing | `docs/adr/0011-evidence-chain.md`, `briefing-evidence-chain.md` | ADR beantwortet alle Designfragen referenzierbar |
| **EV.2** | `EvidenceSource`/`EvidenceStatus`/`EvidenceMark` + tolerante Serde (String- und Objektform), Merge-Regel, Legacy-Mapping; altes Enum bleibt vorerst Producer | `minds-core/src/lineage.rs` | Roundtrip beider Formen; Golden-Strings der 4 Legacy-Werte; Merge-Tabelle; „gespeichert nie Missing"; kein Byte Output-Änderung |
| **EV.3** | Producer auf `EvidenceMark`: `Edge`/`SessionLink`/`IndexLink`; `SCHEMA_VERSION=2`; Redaction-Destrukturierung mitziehen; Reader/TUI-Mapping minimal lauffähig | `minds-core/src/{lineage,session,id}.rs`, `minds-capture/src/{edges,import,match_commits}.rs`, `minds-store/src/{index,git_store}.rs`, `minds-redact/src/session.rs`, `minds-reader/src/{evidence,model,index}.rs` | Neue Golden-Vectors (Schema 2); Schema-1-Fixtures lesbar; Mixed-Store-Roundtrip; fsck über gemischten Bestand |
| **EV.4** | Hash-Primitive: Domain-Kontexte, längenpräfixierte Kodierung, `payload_hash`/`event_hash`/Gap-Hash/Chain-Fold — pure Funktionen, kein I/O | `minds-core/src/evidence.rs` (neu) | Golden-Vectors je Domain; 1-Bit-Änderung ändert Root; Domain-Trennung testfixiert |
| **EV.5** | Hot-Path-Stempel: `append` stempelt beide Hashes (nach `reserve`, vor `.tmp`-Write); `read` toleriert Alt-Events als `pre_chain` | `minds-capture/src/journal.rs` | E2E-fail-open bleibt grün; Secretfile wird über **gewallte** Fassung gehasht (Test); Fixture-Tests mit injizierter Uhr |
| **EV.6** | `ToolCall.capture` (`interpreted`/`uninterpreted`, `adapter`, `adapter_version`); `CLAUDE_ADAPTER_VERSION`; generischer Fallback-Adapter: unbekannter Agent → ToolCalls `uninterpreted` statt Stille | `minds-core/src/session.rs`, `minds-capture/src/{normalize,adapter}.rs`, `minds-redact/src/session.rs` | Fremd-Agent-Fixture erzeugt Session **mit** ToolCalls; Claude-Fixtures stabil bis auf neues Feld |
| **EV.7** | Brücke `ReadOutcome → Vec<ChainItem>`: Gaps/Damaged werden Kettenglieder; `Coverage` claimt nur die gelesene Range; drained Prefix erzeugt keine falsche Lücke | `minds-core/src/evidence.rs`, `minds-capture` | Golden-Vectors: lückenlos / lückenhaft / damaged / pre-chain gemischt |
| **EV.8** | Checkpoint konsumiert volles `ReadOutcome`; Epochen-State `<git-dir>/minds/evidence/state/<agent>/<b3-16hex>` (0700/0600, nie gepusht); `previous=-` ohne State | `minds-cli/src/checkpoint.rs` | Fixture „Session mit zwei Checkpoints" (seq-0-Neustart) erzeugt verkettete Epochen; State überlebt `discard` |
| **EV.9** | `put_seal`/`seals_of` im Store; `refs/minds/evidence/<seal_id>` (elternlos, Baum `seal`); `evidence.json` neben `session.json`; **Seal vor `discard`**; Idempotenz | `minds-store/src/{git_store,store}.rs`, `minds-cli/src/checkpoint.rs` | forget-Test: Session getilgt, Seal + `evidence.json` bleiben; sync pusht den Namespace; Doppel-Checkpoint ⇒ AlreadyPresent |
| **EV.10** | Redaction-Block-Seal im Fehlerpfad von `store_one` (`outcome=storage_policy_rejected_payload`, `session=-`); Journal bleibt liegen; hooklog nennt die Seal-Id | `minds-cli/src/checkpoint.rs` | E2E mit blockierender Regel; Negativ-Assertion über Seal-Bytes (kein Input-Fragment); Policy-Fix ⇒ Erfolgs-Seal verkettet auf Block-Seal |
| **EV.11** | Seal-Payload-Fn (#12-Validierung) + `seal.sig` best-effort beim Checkpoint; `minds sign --seal <id>` zum Nachrüsten | `minds-core/src/attest.rs`, `minds-attest` (Wiederverwendung), `minds-cli/src/{checkpoint,sign_cmd}.rs` | Extern verifizierbar via `ssh-keygen -Y verify -n minds`; manipulierte Zeile fällt durch; ohne Key kein Fehler |
| **EV.12** | `minds verify <session-id>` → Verdikt-Matrix (Exit 0/1/2/3, ADR-0011 E7) mit Details (Range, Gaps, Epochen inkl. heuristischem Schluss, Signaturstatus); `--evidence <seal-id>`; Alt-Session ⇒ `NICHT VERIFIZIERBAR (vor Evidence-Chain erfasst)`, kein Fehler | `minds-cli/src/verify_cmd.rs`, `main.rs` (Spec) | Alle vier Verdikte per Golden-Output-Test erreichbar (inkl. manipuliertem Seal-Baum) |
| **EV.13** | fsck: Hash-Nachrechnung liegender Journale (Mismatch = **Befund**), Seal-Refs repo-weit (`seal_id` vs. Ref-Name), Block-Seals sichtbar, `--require-seal` | `minds-cli/src/fsck.rs` | fsck-Golden-Tests mit präparierten Schäden; manipuliertes Journal-Event wird gefunden |
| **EV.14** | Reader-Lesemodell: `Inspection` lädt Seals und rechnet je Session das Verdikt; `SessionCard` mit Verdikt + Coverage; `GapKind` additiv `SealedGap`/`UnsealedRange`/`PayloadRejected`; `evidence_sentence` erklärt auch den Status; `capture=uninterpreted` erreicht das Lesemodell | `minds-reader/src/{index,model,evidence,query}.rs` | Verdikt je Fixture (alle 4); Block-Seal erscheint als `PayloadRejected`-Gap; Alt-Session ⇒ „vor Evidence-Chain erfasst" |
| **EV.15** | TUI als Evidence-UI: Verdikt-Spalte in der Activity; Session-Kopf mit Integritäts-/Coverage-Zeile; Evidence-Inspector je Schritt (Chain-Position, Hash-Präfixe, Seal); Gap-Bubble mit „Fehlende Evidence beweist nicht, dass nichts geschah"; Glyphen: Source bleibt (`● ◆ ◇ ○ ·`), Status als Modifikator (`✓ ~ ? ✗`), neu `◐` beobachtet-nicht-gedeutet, `!` Gap — Glyph **und** Wort, nie nur Farbe; Pipe: `verdict`- und erweiterte `gap`-Zeilen | `minds-tui/src/{theme,layout}.rs`, `minds-tui/src/view/{activity,graph,why}.rs`, `minds-tui/src/pipe.rs` | Snapshot-Tests je Ansicht; Pipe-Golden (`grep '^verdict'`/`'^gap'`); jeder Zustand unterscheidet sich in Glyph oder Wort (testfixiert) |
| **EV.16** | `audit --export`: Seals ins Bundle, `proves`/`does_not_prove` aktualisiert; `docs/nachweis-leitfaden.md`: externes Verifikationsrezept (`git cat-file` + derive_key + `ssh-keygen -Y verify`) | `minds-cli/src/audit.rs`, `docs/nachweis-leitfaden.md` | Bundle mit Block-Seal beweist Existenz ohne Inhalt; Bundle-Golden-Test |

## 4. Out of Scope (explizit NICHT bauen)

- Kein Merkle-Tree, keine externen Zeitanker (RFC 3161/OpenTimestamps), keine
  Per-Event-Signaturen, kein Transparency-Log, keine PKI/Key-Rotation.
- Keine Migration von Alt-Sessions (`pre_chain`/„vor Evidence-Chain erfasst" sind
  darstellbare Zustände, keine Mängel).
- Kein ToolAdapter-Trait (Plan-v0.2 A.2), kein `minds reinterpret`, kein Evidence-DAG,
  keine Audit-Bundle-Stufen full/redacted/proof — Phase 5–8, eigene Briefings.

## 5. Verifikation Ende-zu-Ende (nach EV.16)

1. Wegwerf-Klon, frisch gebautes Binary: Session erzeugen → committen → `minds verify <id>`
   ⇒ `VERIFIZIERT`; Journal-Event vor dem Checkpoint löschen ⇒ `VERIFIZIERT, UNVOLLSTÄNDIG`
   mit Gap-Detail; Seal-Baum manipulieren ⇒ `MANIPULIERT`; blockierende Redaction-Regel ⇒
   Block-Seal in fsck und `verify --evidence`.
2. Externes Rezept aus dem Nachweis-Leitfaden ohne Minds durchspielen.
3. Gemischter Bestand (Schema 1 + 2): `show`/`why`/`inspect`/`fsck` funktionieren,
   Alt-Kanten zeigen `(…, Unknown)`.
4. TUI-Durchstich: Verdikt in Activity und Session-Kopf, Gap-Bubble, `◐` an einem
   uninterpretierten Call; `minds inspect | grep '^verdict'` / `'^gap'` zeigt dasselbe wie
   der Bildschirm.
