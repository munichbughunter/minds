# Briefing: `SemanticProvider` — semantischer Entity-Kontext als Reader-Abstraktion

**Projekt:** Minds (github.com/munichbughunter/minds)
**Zielgruppe dieses Briefings:** implementierender Agent
**Sprache im Code:** Deutsch (Doc-Kommentare, Fehlermeldungen, Commit-Bodies). Conventional Commits.

---

## 1. Kontext und Motivation

Minds erfasst die *Absicht* hinter Änderungen (Sessions, Evidence) und macht sie über
`show`, `why`, `blame`, `recap` abrufbar. Diese Kommandos arbeiten heute auf Datei-
und Zeilenebene.

Werkzeuge wie `entire-graph` (github.com/entireio/entire-graph, MIT, Go) zeigen, dass
Entity-Ebene das bessere Erlebnis ist: *„Diese Session hat die Signatur von
`validate_token` geändert, 14 Dependents"* statt *„Zeile 42 in auth.py"*. entire-graph
liefert genau das: Entity-Level-Diffs (`added/removed/renamed/signature-changed/
body-changed`), gerankte Code-Suche und einen NDJSON-Symbolgraphen — lokal, ohne
Netzwerk, unter einem eingefrorenen Schema-1.x-Vertrag für Downstream-Konsumenten.

**Architektur-Entscheidung (bereits getroffen, hier nur umzusetzen):**

> Semantischer Code-Kontext ist eine **Ableitung**, keine Evidence. Er ist jederzeit
> vollständig aus den Git-Trees rekonstruierbar und wird deshalb **niemals erfasst,
> redigiert oder gespeichert**. Er lebt ausschließlich hinter einer schmalen
> Provider-Abstraktion im Reader-Pfad. Default: kein Provider. Minds baut keinen
> eigenen Parser (kein Tree-sitter, keine Grammatiken) — es *konsumiert* optional
> die Ausgabe externer Provider-Binaries.

Das Muster ist im Projekt etabliert: `BlameProvider` in `minds-git` (Trait +
`GixBlame`/`ShellBlame`/`AutoBlame`) ist die Vorlage.

---

## 2. Leitplanken (nicht verhandelbar)

1. **Nie im Capture-Pfad.** Kein Hook, kein Journal, keine Redaction-Pipeline berührt
   diesen Code. Nichts aus dem Provider gelangt jemals in ein Session-Envelope, in
   den Store oder in irgendeinen Hash. (Damit ist auch das Float-Verbot der
   Kanonisierung irrelevant — Provider-Daten sind reine Anzeige.)
2. **Fail-soft im Reader.** Ein fehlender, kaputter oder langsamer Provider darf
   `show`/`why`/`blame` niemals scheitern lassen. Fehler ⇒ Sektion entfällt still,
   optional ein Hinweis auf `--verbose`/Debug-Ebene. (Analog zur Hook-Philosophie
   „fail-open", nur reader-seitig.)
3. **Kein neuer harter Dependency-Fußabdruck.** Das Versprechen „ein statisches
   Binary, eine harte Abhängigkeit: git" bleibt unangetastet. Keine Tree-sitter-,
   CGO- oder Grammatik-Abhängigkeiten. Externe Provider werden per Subprozess
   aufgerufen, wenn — und nur wenn — konfiguriert.
4. **Schema-tolerant parsen.** Fremde Provider-Ausgaben (JSON/NDJSON) mit
   `#[serde(default)]` / unbekannte Felder ignorieren einlesen. Minds pinnt sich
   nicht an eine fremde Schema-Minor-Version.
5. **`refs/minds/`-Namespace-Guard gilt weiter.** Dieser Code schreibt überhaupt
   keine Refs.
6. **Keine erfundene Git-Identität**, nirgends, auch nicht in Tests-Fixtures, die
   Commits erzeugen (Test-Identität explizit und als solche benannt setzen).

---

## 3. Scope

### In Scope

- **AP1 — ADR.** Neues ADR unter `docs/adr/` (nächste freie Nummer):
  *„Semantik ist Ableitung: Entity-Kontext hinter `SemanticProvider`, nie im
  Capture-Pfad, Default None."* Inhalt: Kontext (Agent-Ära, entire-graph als
  Referenz), Entscheidung, Konsequenzen (kein Parser im Kern, Consumer-Modell,
  fail-soft), verworfene Alternativen (Tree-sitter im Kern; Semantik als Evidence
  speichern).
- **AP2 — Datenmodell + Trait** (neues Modul in `minds-reader`, z. B.
  `crates/minds-reader/src/semantic/mod.rs` + Untermodule):
  Typen `EntityChange`, `EntityKind`, `ChangeKind`, `SemanticError`; Trait
  `SemanticProvider`.
- **AP3 — `NoSemantics`-Default** + Verdrahtung: eine zentrale Stelle
  (Konfiguration/Factory), die den Provider auflöst; Default ist `NoSemantics`.
- **AP4 — Integration in die Ausgabe:** `show` (und, wo trivial, `why`) rendern
  eine optionale Sektion „Semantische Änderungen", wenn der Provider verfügbar ist
  und Ergebnisse liefert. Leer/Fehler ⇒ Sektion entfällt.
- **AP5 — `ExternalJsonProvider`:** ruft ein konfigurierbares Binary mit
  konfigurierbaren Argumenten auf (Platzhalter für Commit-SHA und Repo-Pfad),
  parst dessen JSON-Ausgabe schema-tolerant in `Vec<EntityChange>`. Timeout
  (konfigurierbar, Default ~5 s), Exit-Code ≠ 0 oder Parse-Fehler ⇒
  `SemanticError` ⇒ fail-soft. Referenz-Aufruf, der funktionieren muss:
  `entire-graph commit <sha> --json` (mit `ENTIRE_REPO_ROOT` als Env).
- **AP6 — Tests:** Unit-Tests fürs Parsen (Fixtures mit realistischem
  entire-graph-JSON, plus Fixtures mit unbekannten Feldern und kaputtem JSON);
  Integrationstest, der ein Fake-Provider-Skript (Shell/echo) als externes Binary
  nutzt und den fail-soft-Pfad abdeckt (Binary fehlt, Timeout, Müll-Ausgabe).

### Out of Scope (explizit NICHT bauen)

- Kein eigener Parser, kein Tree-sitter, keine Sprach-Grammatiken.
- Kein Symbolgraph-Store, kein Index, kein Cache.
- Keine Suche/`recall`-Integration (späteres Vorhaben, eigenes Briefing).
- Keine Änderung an `minds-core`, `minds-redact`, `minds-capture`, `minds-store`.
- Kein Nachbau von entire-graph-Funktionalität in Rust.

---

## 4. Technisches Design (Skizze — verfeinern erlaubt, Grenzen nicht)

```rust
/// Eine semantisch benannte Code-Einheit, deren Zustand sich zwischen zwei
/// Trees geändert hat. Reine Ableitung — geht nie in Envelope, Store oder Hash.
pub struct EntityChange {
    pub file: String,
    pub kind: EntityKind,           // Function | Method | Class | Struct | Trait | Type | Other(String)
    pub name: String,
    pub qualified_name: Option<String>,
    pub change: ChangeKind,         // Added | Removed | Renamed { from } | SignatureChanged | BodyChanged
    pub dependents: Option<u32>,    // heuristisch, darf fehlen
}

/// Liefert Entity-Kontext für einen Commit. Implementierungen müssen ohne
/// Netzwerk auskommen und dürfen den Aufrufer nie blockierend hängen lassen.
pub trait SemanticProvider {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn entities_for_commit(
        &self,
        repo_root: &Path,
        commit: &str,
    ) -> Result<Vec<EntityChange>, SemanticError>;
}
```

- `NoSemantics`: `is_available() == false`, `entities_for_commit` liefert `Ok(vec![])`.
- `SemanticError` mit `thiserror`, Varianten mindestens: `BinaryNotFound`,
  `Timeout`, `ProviderFailed { exit_code, stderr_excerpt }`, `InvalidOutput`.
  Fehlermeldungen auf Deutsch.
- Konfiguration im bestehenden Konfigurationsmechanismus des Readers, Form etwa:

```toml
[semantics]
provider = "none"              # "none" | "external"
command  = "entire-graph"      # nur bei "external"
args     = ["commit", "{commit}", "--json"]
timeout_secs = 5
```

- Rendering: eigene, klar abgesetzte Sektion in der `show`-Ausgabe; bei `--json`
  von `show` als optionales Feld (`"semantics": [...]`), das bei fehlendem
  Provider **weggelassen** wird (`skip_serializing_if`), nicht `null` —
  konsistent mit der bestehenden Additive-Schema-Regel.

---

## 5. Arbeitsweise und Definition of Done

- **Micro-Commit-Workflow:** je Arbeitspaket ein oder mehrere kleine Commits.
  Vor jedem Commit: `cargo fmt` → `cargo clippy -p minds-reader -- -D warnings`
  → `cargo test -p minds-reader` → bei AP4 zusätzlich `cargo test --workspace`.
- **Conventional Commits**, Body auf Deutsch. Beispiel:
  `feat(reader): SemanticProvider-Trait und Datenmodell für Entity-Kontext`.
- **Dateiplatzierung immer explizit benennen.** Nur geänderte/neue Dateien
  liefern; keine Voll-Ersetzung bestehender Dateien ohne vorherigen Blick auf
  den Ist-Stand (`git diff` / Datei lesen).
- **MSRV 1.85, Edition 2024, Workspace-Vererbung** für neue Dependencies (falls
  überhaupt nötig — voraussichtlich nur `std::process` + bestehendes
  `serde`/`thiserror`; **keine** neue Crate ohne Rückfrage).
- Integrationstests unter `crates/minds-reader/tests/`, Testhilfen nicht mit
  `src/` teilen (API-Grenze).
- **Code-Bug vs. Test-Bug** bei Fehlschlägen explizit unterscheiden, bevor ein
  Fix geliefert wird.

**Done, wenn:**

1. ADR liegt vor und benennt die Grenze (Ableitung, Reader-only, fail-soft).
2. `minds show` funktioniert unverändert ohne Konfiguration (Default `none`),
   Ausgabe byte-identisch zu vorher.
3. Mit konfiguriertem Fake-Provider erscheint die Semantik-Sektion; mit kaputtem
   Provider verschwindet sie still — kein Fehler, kein Absturz, kein Hänger.
4. `entire-graph commit <sha> --json`-Ausgabe (Fixture) wird korrekt in
   `EntityChange` abgebildet, unbekannte Felder werden ignoriert.
5. fmt/clippy(-D warnings)/tests grün im gesamten Workspace.

---

## 6. Referenzen

- entire-graph README, Abschnitte *Semantic Diff*, *Provider Contract*,
  *Current Limits* — als Vorbild fürs Datenmodell, nicht als Schema-Vertrag.
- `crates/minds-git/…` `BlameProvider` — als Struktur-Vorlage für Trait +
  Implementierungen + Auto-Auflösung.
- Bestehende ADRs unter `docs/adr/` — Ton und Format übernehmen.
