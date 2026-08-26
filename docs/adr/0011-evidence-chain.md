# ADR-0011 — Evidence Chain: beweisbare Beobachtung, beweisbare Lücken

- Status: angenommen
- Datum: 2026-08-24
- Betrifft: `minds-core`, `minds-capture`, `minds-store`, `minds-cli`, `minds-reader`, `minds-tui`
- Verwandt: ADR-0003 (Hooks statt Transkript-Parsing), ADR-0004 (Store-Index, Evidence-Klassen),
  ADR-0008 (Signierte Attribution), ADR-0010 (Ein Ref je Session);
  Umsetzungsplan: `briefing-evidence-chain.md` (Track EV)

## Kontext

Minds ist kryptographisch **adressiert**: `SessionId = BLAKE3(kanonische Session)`, Sessions
liegen content-adressiert unter `refs/minds/store/`, Attribution und Reviews sind signierbar.
Was ein Auditor damit prüfen kann, ist die Integrität dessen, **was gespeichert wurde**.

Was er nicht prüfen kann, ist der **Erfassungsbereich**: Woher weiß er, dass zwischen zwei
Events nichts fehlt? Dass ein Hook nicht ausgefallen ist? Dass eine Session nicht existierte,
deren Speicherung die Redaction abgelehnt hat? Heute erkennt das Journal Sequenz-Lücken beim
Lesen — aber `checkpoint` verwirft diese Erkenntnis, löscht das Journal danach, und ein
Redaction-Block hinterlässt außer einer `hook.log`-Zeile nichts. Eine erkannte Lücke ist kein
Beweis einer Lücke, und ein nicht vorhandenes Event kann nicht signiert werden.

Das Audit-Bündel sagt es selbst ehrlich: „beweist keine Vollständigkeit". Dieses ADR macht aus
dieser Einschränkung eine prüfbare Aussage.

## Grundsatz

> **Minds darf niemals aus dem Fehlen einer Evidence auf das Fehlen eines Ereignisses
> schließen.** Kein Evidence-Record bedeutet *unbekannt* — außer der Beobachtungsbereich ist
> versiegelt, und das Nichtvorhandensein ist damit selbst eine prüfbare Aussage.

Daraus folgen drei Trennungen, die überall gelten:

1. **VALID ≠ COMPLETE.** Integrität („die vorhandenen Records sind unverändert") und Coverage
   („der Beobachtungsbereich ist ohne bekannte Lücken erfasst") sind getrennte Urteile.
2. **Evidence is immutable. Interpretation is recomputable.** Gehasht und versiegelt wird nur
   Beobachtetes (Rohdaten-Fakten); jede Deutung (Adapter-Normalisierung, Kanten-Heuristik,
   Status-Hochstufung) ist versioniert und wiederholbar, ohne die Evidence anzufassen.
3. **Hash every event, sign every checkpoint.** Jedes Journal-Event trägt Hashes; signiert
   wird der Seal eines Bereichs, nie das einzelne Event — der Hot Path bleibt fail-open und
   billig.

Genauer sind es **drei Vertrauensachsen**, die nie in einem Status vermischt werden:

```text
              Evidence
                 │
    ┌────────────┼────────────┐
    ▼            ▼            ▼
 Integrität   Coverage     Deutung
 „verändert?" „fehlt was?" „was heißt es?"
    │            │            │
 Hashes/Seals  Gaps/Scope/  Adapter,
 Chain         Epochen      capture, DAG
```

Ein unbekanntes Tool ist ein **Deutungs**-Problem, ein Hook-Ausfall ein **Coverage**-Problem,
ein verändertes Journal ein **Integritäts**-Problem. `minds verify` spricht die drei Achsen
getrennt aus (`Integrität` / `Coverage` / `Deutung` / `Gesamt`); die Exit-Codes bleiben der
CI-Vertrag aus Integrität × Coverage — die Deutung wertet nie auf oder ab.

## Entscheidung 1: Evidence-Hashes im Hot Path, Verkettung beim Seal

Jedes Journal-Event bekommt beim Append zwei additive Felder:

- `payload_hash = derive_key("minds/evidence/v1/payload", payload)` — über den Payload **nach**
  der Secretwall (die Orakel-Regel aus `lineage.rs` gilt unverändert: für Secret-Dateien
  entsteht nie ein Hash über geheimen Inhalt, weil die Wall den Inhalt vorher ersetzt).
- `event_hash = derive_key("minds/evidence/v1/event", encode(seq, at, at_nanos, raw_kind,
  cwd, transcript_path, payload_hash))` — über eine **längenpräfixierte Binärkodierung**
  (u64-LE-Länge je Feld, Option-Tags), nicht über JCS: `at_nanos` überschreitet 2^53, und die
  kanonische JSON-Form lehnt das bewusst ab. Gehasht werden nur beobachtete Fakten — `kind`
  (die Klassifikation) ist Interpretation und bleibt draußen.

**Kein `prev_event_hash` im Event.** Die Seq-Vergabe ist lock-frei (`create_new`); der
Nachbar kann beim eigenen Append noch eine `.tmp`-Datei sein. Ein best-effort-prev-Link
erzeugte legitime „prev unbekannt"-Marker, die von Manipulation nicht unterscheidbar wären —
ein Prüfprimitive, das regulär falsch-positiv rauscht, ist wertlos. Die Verkettung entsteht
deterministisch beim Seal über die sortierte Seq-Folge:

```text
h_0 = derive_key("minds/evidence/v1/chain", 0^32 ‖ tag ‖ item_hash_0)
h_i = derive_key("minds/evidence/v1/chain", h_{i-1} ‖ tag ‖ item_hash_i)
```

mit `tag 0x01` = Event (`item_hash` = `event_hash`) und `tag 0x02` = Lücke (`item_hash` =
Hash des Gap-Records) — **eine Lücke ist selbst ein Kettenglied**, kein Schweigen.

**Der Fold ist gesalzen.** Der Root reist im Seal auf die Forge, und `seq` und
`last_event_at` stehen dort im Klartext daneben — ungesalzen wäre der Root für
eine Ein-Event-Epoche ein Offline-Orakel: Wer den Payload rät (kurzes Passwort,
PIN im Prompt), rechnet den Root nach und bestätigt die Vermutung. Deshalb
startet der Fold auf `derive_key(ctx, salt)` mit einem zufälligen 32-Byte-Salt
je Session, der **lokal** neben dem Epochen-Zustand liegt (0600, nie gepusht).
Der Preis ist gewollt: Ein Externer rechnet den Root nicht aus geratenen
Payloads nach — lokal (`fsck`, vor dem Discard) bleibt er nachrechenbar, denn
dort ist der Salt lesbar.

**Der Salt heilt nicht.** Sobald eine Epoche versiegelt ist, wird ein fehlender
oder beschädigter Salt **nicht** regeneriert: Ein neuer Salt versiegelte
dieselbe Evidence unter einem zweiten, abweichenden Root — ein Epoch-Fork
(same evidence, different cryptographic identity), der dem Determinismus-
Anspruch „gleiche Events ⇒ gleicher Seal" widerspräche. Stattdessen ist der
Verlust selbst der Befund: Der Checkpoint vertagt die Session sichtbar
(`hook.log`), das Journal bleibt liegen, die Epoche gilt als nicht mehr
reproduzierbar. Nur vor der ersten Versiegelung darf ein Salt entstehen oder
ersetzt werden — dann hat noch kein Seal auf einen Root committed.

**Benannter Trade-off:** Zwischen Append und Seal schützt nur das Dateisystem (0700/0600,
Symlink-Refusal) — exakt der heutige Zustand. Ein lokaler Angreifer mit Schreibrecht kann bis
zum Checkpoint fälschen, was dann „sauber" versiegelt wird. Die selbstbeschreibenden
event_hashes machen nachträglichen Payload-Tausch an *liegenden* Journalen erkennbar
(`fsck` rechnet nach); mehr — signierende Hooks — verletzte das Hot-Path-Budget und bleibt
Ausblick. Dieses Fenster steht im Nachweis-Leitfaden unter „beweist nicht".

## Entscheidung 2: Coverage als Epochen-Seals mit expliziten Lücken

Ein **Seal** versiegelt genau den Bereich, den der Checkpoint tatsächlich gelesen hat — nie
mehr. Zeilenbasiertes Format (wie `minds-attestation-v1`, Zeilenzahl fix = 12, Felder
fail-closed validiert nach dem #12-Muster):

```text
minds-seal-v1
root=b3-<64hex>          Chain-Root über Events und Gap-Records
agent=<agent>
first_seq=<n>            tatsächlich gelesener Bereich
last_seq=<n>
events=<n>
gaps=<n>                 fehlende/beschädigte Glieder, einzeln in der Chain
pre_chain=<n>            Alt-Events ohne gestempelte Hashes (Bestand)
outcome=stored | storage_policy_rejected_payload
session=b3-<64hex> | -
previous=b3-<64hex> | -  Seal der vorherigen Epoche derselben Session
last_event_at=<RFC3339>  aus dem letzten Event, keine Wanduhr
```

`seal_id = derive_key("minds/evidence/v1/seal", bytes)`. Der Seal ist deterministisch:
gleiche Events ⇒ gleicher Seal ⇒ idempotente Ablage.

**Epochen:** Nach `journal.discard` beginnt dieselbe Session wieder bei `seq 0` — jede
Checkpoint-Epoche wird eine eigene Session und ein eigener Seal, verkettet über `previous`
(lokaler Zustand unter `<git-dir>/minds/evidence/state/`, wird nie gepusht). Fehlt der
Zustand (frischer Clone), stehen Epochen unverbunden: Das Verdikt sagt dann ehrlich
„unvollständig". `verify` darf Epochen zur Lesezeit über `lineage.local_id` heuristisch
schließen — angezeigt als Heuristik, **niemals** verdikt-aufwertend. Was vor dem ersten
gelesenen Event verloren ging (Absturz vor jedem Gap-Record), wird nicht behauptet: Der Seal
claimt nur `first_seq..last_seq`; der Rest fällt unter „Epochenkette offen".

## Entscheidung 3: Ein Redaction-Block hinterlässt einen Seal

Lehnt die fail-closed-Redaction eine Session ab, entsteht **kein** Session-Objekt — aber ab
jetzt ein Seal mit `outcome=storage_policy_rejected_payload` und `session=-`. Er enthält
Chain-Root, Zählwerte, Agent, Zeitraum — **keinen** Intent, keine Pfade, nicht einmal den
Namen des Feldes, an dem die Redaction scheiterte (der `RedactionAudit` bleibt lokal). Der
Auditor sieht: Eine Session existierte, ihr Bereich ist versiegelt, die Speicher-Policy hat
die Nutzlast zurückgewiesen — Integrität valide, Coverage unvollständig. Gelingt der
Checkpoint nach einem Policy-Fix, verkettet der Erfolgs-Seal per `previous` auf den
Block-Seal: Die Geschichte bleibt nachvollziehbar. Das Journal bleibt wie bisher liegen.

## Entscheidung 4: Seals leben in einem eigenen Namespace und überleben `forget`

```text
refs/minds/evidence/<64hex seal_id>   elternloser Commit; Baum: seal [+ seal.sig]
refs/minds/store/<64hex session>      session.json, links.json, neu: evidence.json
```

`evidence.json` (veränderlich wie `links.json`, nie kanonisch) trägt die Rückverweise
Session → Seals. Der Seal-Namespace ist von der Payload entkoppelt: `forget` tilgt
`session.json`, der Seal bleibt als payload-freier Beweis stehen — er enthält nur Hashes und
Zählwerte, nichts Tilgbares. Seals werden nie getilgt und nie force-gepusht; `minds sync`
nimmt den Namespace automatisch mit (ein Push für alles, ADR-0010). Ref-Namen enthalten nie
ein `local_id`-Derivat — kein Orakel für fremdbestimmte Kennungen auf der Forge.

## Entscheidung 5: Signatur auf den Seal, optional und best-effort

Ist `user.signingkey` konfiguriert, signiert der Checkpoint die exakt abgelegten Seal-Bytes
(`ssh-keygen -Y sign -n minds`, ADR-0008) und legt `seal.sig` daneben — das Muster der
Review-Signaturen. Ohne Schlüssel bleibt der Seal **hash-valide** (Integrität kommt aus dem
content-adressierten Ref-Namen); die Signatur fügt die Urheber-Bindung hinzu. `minds sign
--seal <id>` rüstet nach. Ein Signaturfehler bricht den Checkpoint nicht.

## Entscheidung 6: Evidence in zwei Dimensionen — Quelle × Status

Das bisherige `Evidence`-Enum (`Inferred < Declared < Content < Observed`) vermischt zwei
Fragen: *Woher stammt die Aussage?* und *Wurde sie geprüft?* Es wird aufgeteilt:

```rust
EvidenceSource { Heuristic, HumanDeclared, ContentDerived, Observed }   // Ord = Vertrauen
EvidenceStatus { Missing, Unknown, Partial, Verified }                  // Ord
EvidenceMark   { source, status }
```

- **Verified heißt nachgerechnet** — kryptographisch oder über Content-Evidence, zur
  Verify-/Lesezeit. Es wird nie in gespeicherte Bytes eingefroren und nie ohne Prüfung
  vergeben. Deshalb mappen Legacy-Werte auf `Unknown`: `observed→(Observed, Unknown)`,
  `content→(ContentDerived, Unknown)`, `declared→(HumanDeclared, Unknown)`,
  `inferred→(Heuristic, Unknown)`. Keine Alt-Kante wurde je nachgerechnet; der Grundsatz
  verbietet, das Fehlen der Prüfung als Bestehen zu lesen.
- **Tolerant lesen, kanonisch schreiben:** Der Deserializer akzeptiert den Legacy-String und
  die Objektform; geschrieben wird immer die Objektform. `SCHEMA_VERSION = 2` — erstmals
  trennt der Bump auch real Lesbarkeit: Neuere Binaries lesen alle älteren Versionen,
  ältere Binaries lesen Schema 2 nicht. Bestand wird nicht migriert (content-adressiert =
  unveränderlich); Alt-Sessions sind der darstellbare Zustand „vor Evidence-Chain erfasst".
- **Merge statt `max()`:** Zwei Dimensionen haben keine Totalordnung. Regel: primär `source`
  (bisherige Vertrauensordnung), bei gleicher Source entscheidet `status`; die stärkere
  Source gewinnt mit ihrem kompletten Mark. Invariante: **gespeicherte** Marks tragen nie
  `Missing` — `Missing` existiert nur in Verify-/Reader-Ausgaben.
- Beobachtung und Deutung trennt zusätzlich `ToolCall.capture`
  (`interpreted | uninterpreted`, mit `adapter` und `adapter_version`): Ein Tool-Aufruf, den
  kein Adapter deutet, erscheint als „beobachtet, nicht gedeutet" statt still zu verschwinden
  — auch für Agents ohne eigenen Adapter (generischer Fallback).

## Entscheidung 7: Ein Verdikt in zwei Achsen, feste Exit-Codes

`minds verify <session-id>` (und `--evidence <seal-id>` für sessionlose Seals) urteilt in
der Matrix Integrität × Coverage:

| | Coverage vollständig | Coverage unvollständig/unbekannt |
|---|---|---|
| Integrität intakt | `VERIFIZIERT` | `VERIFIZIERT, UNVOLLSTÄNDIG` |
| Integrität verletzt | `MANIPULIERT` | `MANIPULIERT` |
| Kein Material | — | `NICHT VERIFIZIERBAR` |

Exit-Codes (CI-Vertrag): **0** VERIFIZIERT · **1** MANIPULIERT · **2** VERIFIZIERT,
UNVOLLSTÄNDIG · **3** NICHT VERIFIZIERBAR. Coverage vollständig ⇔ `gaps=0 ∧ pre_chain=0 ∧
outcome=stored ∧` Epochenkette geschlossen. Eine Alt-Session ohne Seal ist `NICHT
VERIFIZIERBAR (vor Evidence-Chain erfasst)` — ein Zustand, kein Fehler. `fsck` erhält die
Gegenstücke: Hash-Nachrechnung liegender Journale und Seal-Prüfung als **Befunde**,
`--require-seal` als Gate analog `--require-review`.

## Entscheidung 8: Adapter sitzen ÜBER der Chain, Deutung ist deterministisch (Phase 5)

Der `ToolAdapter`-Trait (Registry, je Agent ein Adapter, `adapter_version` aus der
Implementierung) deutet Journal-Events und gespeicherte Aufrufe — er verändert **nie** deren
Bytes, Hashes oder Identität: `Raw Evidence → Chain → Adapter → Deutung`, niemals umgekehrt.
Deutung ist deterministisch (gleiche Evidence + gleiche Adapter-Version ⇒ gleiche Deutung,
testfixiert) — sonst wäre `minds reinterpret` wertlos. `minds reinterpret <session>` ist die
Einlösung von „Interpretation is recomputable": strikt lesend, zeigt es je Aufruf die
Evidenz-Adresse (unverändert), die gespeicherte und die aktuelle Deutung nebeneinander.

## Entscheidung 9: Der Evidence-DAG ist eine Projektion (Phase 6)

Die Chain bleibt die temporale, append-only Provenance. Semantische Beziehungen darüber —
Content-Übergaben: „B las exakt die Bytes, die A schrieb" — werden **zur Lesezeit** aus den
gespeicherten Inhalts-Hashes projiziert (Write-Hash == Read-Hash am selben Pfad; dafür hashen
seit Phase 6 auch Read-Effekte — aber nur für von git **getrackte**, repo-relative Pfade:
Getrackter Inhalt ist für jeden Repo-Leser ohnehin sichtbar, sein Hash verrät nichts Neues.
Bloßes Lesen einer privaten oder repo-fremden Datei erzeugt nie einen Fingerabdruck — ein
ungesalzener Inhalts-Hash über eine kurze Datei wäre dasselbe Bestätigungsorakel, gegen das
der Chain-Root gesalzen ist. Secret-Ausnahme unverändert). Nichts davon wird gespeichert:
jederzeit neu berechenbar, deterministisch sortiert. Diese Kanten sind die erste Stelle, die
`(ContentDerived, Verified)` produziert — nicht beobachtet, nicht behauptet, sondern
**nachgerechnet**.

## Entscheidung 10: Proof-Bündel — und warum es kein `full` gibt (Phase 7)

`minds audit --export --mode proof` exportiert nur das Beweisgerüst (Ids, kanonische
Payload-Texte, Seals samt Signaturen, Verdict-Metadaten — kein Intent, keine Kommentare):
prüfbar, ohne Inhalt weiterzugeben. `redacted` bleibt der Default und das **Maximum** — ein
`full`-Modus existiert bewusst nicht, denn der Store hält ausschließlich redigierte Sessions
(fail-closed); mehr zu versprechen wäre leer oder ein Leck. `proves`/`does_not_prove` sind
Teil des Produktmodells, nicht der Doku: Sie reisen im Artefakt und verhindern die Drift von
„wir haben Evidence" zu „wir beweisen, dass nichts anderes geschah".

## Die Invarianten (testfixiert)

1. **Jedes Kettenglied hängt an genau einem Vorgänger** — im Fold: `h_i` deckt `h_{i-1}`
   (`invariant_each_chained_link_is_bound_to_exactly_one_predecessor`). Bewusst KEIN
   prev-Link im Event selbst (Entscheidung 1).
2. **Der Fold-Zustand umfasst den Vorgänger** — dito.
3. **Der Event-Hash umfasst die beobachteten Fakten** — und nur diese; `kind` ist Deutung
   (`invariant_the_event_hash_covers_the_observed_facts_and_only_those`).
4. **Eine Lücke ist selbst verifizierbare Evidence**
   (`invariant_a_gap_is_itself_verifiable_evidence`).
5. **Coverage ist immer gescoped** — ein Seal ohne `scope=` parst nicht; „vollständig" heißt
   vollständig innerhalb der Grenze, nie „alle Systemaktivität"
   (`invariant_coverage_is_always_scoped`).
6. **Deutung verändert nie Raw Evidence** — `reinterpret` bewegt keinen Ref
   (`reinterpret_is_read_only_and_deterministic`).
7. **Legacy bleibt Legacy** — expliziter Zustand statt `None`, nie nachträglich angedichtet
   (`invariant_legacy_stays_legacy`, `Provenance::Legacy`).
8. **„Nicht erfasst" ≠ „nicht passiert"** — der Grundsatz; Gap-Glieder, Block-Seals und die
   `does_not_prove`-Liste sind seine Umsetzung.

Die Hash-Domänen sind versionierte Namensräume (`minds/evidence/v1/…`,
`invariant_the_hash_domains_are_versioned_namespaces`): Ein künftiges `chain-v2` kann neben
v1 existieren, ohne historische Daten umzudeuten.

## Verworfene Alternativen

- **prev_hash im Event** (Vision §4): racy im lock-freien Journal, siehe Entscheidung 1.
- **Per-Event-Signaturen / signierender Hook:** verletzt das Hot-Path-Budget (fail-open,
  keine Wartezeit im Agenten); das Append-Seal-Fenster wird benannt statt wegsigniert.
- **Merkle-Tree über Event-Ranges:** lineare Kette genügt der Session-Größenordnung;
  selektive Teilbeweise einzelner Events sind kein aktueller Bedarf. Ausblick.
- **Externe Zeitanker (RFC 3161, OpenTimestamps):** Minds bleibt offline-/air-gap-tauglich;
  „wann wirklich" bleibt unbewiesen und steht im Nachweis-Leitfaden. Ausblick.
- **Transparency Log / globale Konsistenz:** machte aus lokaler Provenance ein
  Global-Consistency-Problem (Gossip, Witnesses, Fork Consistency, PKI) — massives
  Overengineering für ein lokales Evidence-System.
- **Rückwirkende Migration alter Sessions:** erzeugte falsche Sicherheit — eine heute
  erzeugte Chain über gestern beweist nichts über gestern. Legacy bleibt Legacy.
- **Seal nur als `seal.json` im Session-Ref:** scheitert am Redaction-Block (kein Session-Ref
  existiert) und koppelte den Beweis an das forget-Schicksal der Payload.
- **Legacy-Mapping auf `Verified`:** entwertete die Status-Dimension von Tag eins
  (Verified ohne Prüfung) und widerspräche dem Grundsatz.

## Konsequenzen

Minds kann künftig vier Aussagen unterscheidbar machen: *beobachtet* (Event mit Hashes in
versiegeltem Bereich), *nicht beobachtet* (Gap-Glied in der Chain bzw. offene Epochenkette),
*abgeleitet* (Heuristik, als solche markiert, nie aufgewertet) und *nicht erfassbar*
(Block-Seal, uninterpretierter Call). Das Audit-Bündel gewinnt die Aussage „für versiegelte
Bereiche sind Manipulation und Lücken kryptographisch erkennbar" — und behält ehrlich, was
weiterhin nicht bewiesen wird: die Integrität zwischen Append und Seal, Ereignisse außerhalb
versiegelter Bereiche, die reale Uhrzeit. Externe Prüfer brauchen kein Minds: `git cat-file`,
BLAKE3-`derive_key` und `ssh-keygen -Y verify` genügen (Rezept im Nachweis-Leitfaden).

Der Preis: Schema 2 ist für alte Binaries unlesbar (zentrale Verteilung, keine Bestandsnutzer
— akzeptiert), und die Verkettungs-Garantie beginnt erst am Seal, nicht am Append.
