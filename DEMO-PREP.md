# Demo-Vorbereitung Philipp — Minds v0.4.0

*Ziel: Jede Zeile auf dem Bildschirm erklären können, bevor Philipp danach fragt.
Alle Ausgaben in diesem Dokument sind wörtlich aus dem Code (Stand `16bad52`,
v0.4.0). Wo eine Zahl variiert, steht `<…>`.*

---

## 0. Vor der Demo — drei Dinge, die sonst schiefgehen

1. **Frisch bauen.** Das ausgepackte `minds-0.4.0-aarch64-apple-darwin/minds`
   meldet `minds 0.3.0` (vor dem Release-Commit gebaut), `target/release/minds`
   ist noch älter (`0.1.0`). Für die Demo:
   ```sh
   cargo build --release --bin minds
   ./target/release/minds --version
   ```
   Und wie immer: `command -v minds` prüfen — die globale in `~/.cargo/bin`
   könnte ein alter Stand sein.
2. **`minds checkpoint` einmal von Hand aufrufen.** Der `SESSION SEALED`-Block
   (der schönste Moment der Demo) ist nur beim manuellen Aufruf sichtbar — der
   post-commit-Hook leitet stdout nach `/dev/null`.
3. **Es gibt kein `minds seal`.** Versiegelt wird automatisch im Checkpoint.
   Nachträglich signieren: `minds sign --seal <seal-id>`. Wenn Philipp nach
   einem Seal-Kommando fragt, ist das die Antwort.

---

## 1. Die drei Begriffe — erst der eine Satz, dann die Tiefe

### Evidence — „Was ist tatsächlich passiert?"

**Ein Satz:** Jedes vom Agent-Hook beobachtete Ereignis bekommt beim Schreiben
zwei kryptographische Stempel; alle Ereignisse einer Epoche werden zu einer
Hash-Kette gefaltet — inklusive der Lücken.

**Die Tiefe:**
- Pro Journal-Event: `payload_hash` (über den Inhalt **nach** der Redaktion)
  und `event_hash` (über seq, Zeitstempel, Rohtyp, cwd, transcript-Pfad und den
  payload_hash). Beides blake3 `derive_key` mit eigenen Domänen-Kontexten
  `minds/evidence/v1/payload` bzw. `…/event`.
- **Eine Lücke ist ein Kettenglied, kein Fehler.** Fehlende seq-Nummern werden
  als `Missing{from,to}`, beschädigte Dateien als `Damaged` mit eigenem
  `gap_hash` in die Kette eingefaltet. Der Leitsatz aus ADR-0011:
  *„Minds must never infer the absence of an event from the absence of
  evidence."*
- Der Fold startet mit einem **lokalen Salt** (32 Byte, 0600, wird nie
  gepusht). Warum: Ohne Salt wäre der Chain-Root bei einer Ein-Event-Epoche ein
  Offline-Orakel über den Payload — man könnte Inhalte durchprobieren. Folge:
  Der Root ist **nur lokal** reproduzierbar; extern prüfbar sind Seal-Identität
  und Signatur (steht so in `docs/verification-guide.md`).
- Bewusst **nicht** gehasht: die Klassifikation (`kind`). Die ist Deutung, und
  Deutung ist neu berechenbar (`minds reinterpret`) — Evidence ist unveränderlich,
  Interpretation nicht. Das ist ADR-0011, Entscheidung 1.

### Seal — „Das versiegelte Zeugnis über einen Bereich"

**Ein Satz:** Am Ende jeder Epoche (beim Checkpoint) entsteht ein 13-zeiliger
Text, der Root, Abdeckung, Lücken, Outcome und den Rückverweis auf die
Vorepoche nennt — und seine eigene Identität ist der Hash dieses Textes.

**Der Text (exakt 13 Zeilen):**
```
minds-seal-v1
root=b3-<64hex>          ← der Chain-Root aus dem Fold
agent=claude-code
scope=agent-hooks/v1     ← die Beobachtungsgrenze (siehe unten)
first_seq=0
last_seq=1
events=2
gaps=0
pre_chain=0
outcome=stored           ← oder storage_policy_rejected_payload (Block-Seal)
session=b3-<64hex>       ← Rückverweis auf die Session
previous=b3-<64hex>|-    ← seal_id der Vorepoche; „-" = ehrlich „nicht belegt"
last_event_at=<RFC3339>  ← aus dem letzten Event, nie aus der Wanduhr
```
- `seal_id = blake3_derive_key("minds/evidence/v1/seal", seal_text)` — und
  dieser Hash ist zugleich der **Ref-Name**:
  `refs/minds/evidence/<seal_id>` (elternloser Commit, Baum mit Blob `seal`,
  optional `seal.sig` daneben — nie darin, das wäre zirkulär).
- **Darum fliegt Manipulation auf:** Der Name des Refs ist der Hash des
  Inhalts. Wer den Text ändert, ändert den Hash — der Ref heißt aber noch wie
  der alte. Beim Lesen wird nachgerechnet → `TAMPERED`.
- Auch bei einem **Redaction-Block** entsteht ein Seal (`rejected (payload)`):
  Der Bereich ist bezeugt, auch wenn der Inhalt nie gespeichert wurde. Und:
  ohne Seal kein Journal-Discard — der Beweis muss die Löschung überleben.
- `previous` verkettet die Epochen einer Session. `last_event_at` aus dem
  Event statt der Uhr macht den Seal **deterministisch**: gleiche Events ⇒
  byte-gleicher Seal ⇒ idempotente Ablage.
- Fail-closed in beide Richtungen: Beim Schreiben *und* Lesen werden
  Zeilenumbrüche, Bidi-/Zero-Width-/Steuerzeichen abgewiesen (das ist der
  #12-Fix, Newline-Injection).

### Provenance — „Woher weiß Minds das, und wie gut?"

Der Begriff ist bewusst auf **drei Ebenen** belegt — Philipp wird genau hier
bohren, also sauber trennen:

1. **Session-Provenance** (Lese-Modell): `Legacy` oder `Chained`.
   `Legacy` = vor der Evidence-Chain erfasst; solche Sessions bekommen **nie**
   nachträglich eine Kette angedichtet. Ihr Satz:
   *„Captured before the evidence chain — cryptographic verification is not
   available."*
2. **Kanten-Provenance** (wie ist ein Commit mit einer Session verknüpft):
   `● observed` (Hook hat es gesehen) · `◆ content` (Inhaltsabgleich) ·
   `◇ declared` (Trailer sagt es) · `○ inferred` (Heuristik) · `· unlinked`.
   Dazu ein Prüfstatus: `✓ recomputed`, `~ partially checked`, `? unchecked`,
   `✗ evidence missing`. **Der Merksatz: „observed heißt nicht recomputed"** —
   Quelle und Prüfung sind zwei Achsen.
3. **Provenance-Kette im Audit-Bundle**: `change → session → attribution →
   verdict` (`minds audit --export`).

### Die ganze Kette an einem Bild (zum Aufmalen)

```
Payload (nach Redaktion) ─▶ payload_hash ─▶ event_hash ─┐
Lücke (Missing/Damaged)  ─▶ gap_hash ───────────────────┤
                                    Fold (Start: Salt) ─▶ root
root + Abdeckung + outcome + session + previous ─▶ Seal-Text (13 Zeilen)
Seal-Text ─▶ seal_id  ═══  Ref-Name refs/minds/evidence/<seal_id>
   ├─ previous ─▶ Vorepoche (Epochenkette)
   ├─ session ──▶ refs/minds/store/<session> (session.json, evidence.json, links.json)
   └─ seal.sig ─▶ optionale ssh-Signatur über exakt diese Bytes
Commit ─▶ Trailer „Minds-Session-Id: b3-…" (überlebt Rebase/Squash/Cherry-Pick)
```

---

## 2. Demo-Drehbuch

### Beat 1 — Erfassen und versiegeln (5 Min.)

Echte kleine Änderung mit Claude Code, dann:
```sh
git add … && git commit -m "…"
git log -1 --format=%B        # Trailer: Minds-Change-Id, Minds-Session-Id
minds checkpoint              # von Hand, wegen der Quittung:
```
```
  SESSION SEALED
    Seal       b3-<64hex>
    Root       b3-<64hex>
    Events     2 event(s) · seq 0–1
    Scope      agent-hooks/v1
    Signature  unsigned — `minds sign --seal` adds it
    ✓ no gaps in the observed range
    ✓ first epoch of this session (chain start)
    ✓ seal recorded — check it with `minds verify <session-id>`
```
**Erklärpunkt:** Jede ✓-Zeile ist eine **Tatsache über das Schreiben**
(„recorded", „chain start"), nie ein Prüfergebnis — die Wörter „valid" und
„VERIFIED" kommen hier absichtlich nicht vor. Prüfen tut nur `verify`.

### Beat 2 — Nachschlagen (5 Min.)

```sh
minds show                    # commit → Sessions → Intent, Agent, Modell, Token
minds why <datei>:<zeile>     # Blame → Trailer → dieselbe Herkunft
minds inspect                 # die TUI
```
TUI-Tour (Tasten: `w` Why, `e` Evidence, `t` Graph↔Timeline, `/` Suche,
`1/2/3` Zoom, `?` Hilfe):
- **Liste:** Spalten `TIME · SESSION · AGENT · [SIZE] · SEAL · VERDICT`.
  SEAL zeigt Verdikt-Glyph + bester Kanten-Beleg; VERDICT ist das
  **Review**-Verdikt (`⚠ open`, `✓ approved`, `↻ needs work`, `✕ rejected`) —
  zwei verschiedene Dinge, gern verwechselt.
- **Footer-Badge** unten links folgt der fokussierten Session:
  `◈ sealed` (grün) · `! incomplete` (orange) · `✗ TAMPERED` (rot) ·
  `· legacy` (grau).
- **Why (`w`):** die Herkunftskette. Unter dem Intent steht
  `◌ CLAIM — as recorded, not verified evidence` — **eine aufgezeichnete
  Aussage darf nie wie ein Beweis aussehen.** Ohne Lücke:
  „Every link is attested — the chain closes without guesswork."
- **Evidence (`e`):** Kopfkarte `SESSION SEALED` mit Leitsatz
  „Cryptographically verified within the recorded observation boundary.",
  darunter der VERDICT-Block mit sechs Achsen:
  `INTEGRITY · COVERAGE · EPOCHS · SIGNATURE · INTERPRETATION · LIMITS`.
  Mit ↑↓ in COVERAGE gehen — dort steht die Beobachtungsgrenze ausgeschrieben
  („subprocesses / network / window between append and seal — not captured,
  not a gap") und der wichtigste Satz des Produkts:
  **„Missing evidence does not prove that nothing happened — it means: Minds
  cannot attest it."**

### Beat 3 — `minds verify` im Gutfall (3 Min.)

```
Session        b3-<64hex>
Payload        in store (schema 2)
Seal           b3-<id>: seq 0–1, 2 event(s), 0 gap(s), stored — unsigned
Integrity      intact
Coverage       complete within the boundary (boundary: agent-hooks/v1 — activity outside it is not captured)
Interpretation complete
Overall        VERIFIED
```
Die **drei Vertrauensachsen** erklären:
- **Integrity** — stimmen die Hashes? (binär: intact / VIOLATED)
- **Coverage** — ist der beobachtete Bereich lückenlos? Immer **innerhalb der
  Grenze** `agent-hooks/v1`; „vollständig" heißt nie „alles auf der Maschine".
- **Interpretation** — sind alle Tool-Calls gedeutet? **Deutung wertet nie auf
  oder ab** — bei Deutungslücke steht `Overall VERIFIED — interpretation
  partial`, Exit bleibt 0.

Exit-Codes (der CI-Vertrag): `0` VERIFIED · `1` TAMPERED · `2` VERIFIED,
INCOMPLETE · `3` NOT VERIFIABLE (Alt-Session ohne Seal) · `4` operativer
Fehler (kollidiert nie mit 1).

### Beat 4 — Die Manipulation (der Höhepunkt, 10 Min.)

Live fälschen, mit Git-Plumbing — genau wie ein Angreifer mit Repo-Zugriff
(das Rezept ist der E2E-Test `end_to_end.rs:3025`):
```sh
REF=$(git for-each-ref --format='%(refname)' refs/minds/evidence/ | head -1)
git show "$REF:seal" | sed 's/events=2/events=9/' > /tmp/forged
BLOB=$(git hash-object -w /tmp/forged)
TREE=$(printf '100644 blob %s\tseal\n' "$BLOB" | git mktree)
git update-ref "$REF" "$(git commit-tree "$TREE" -m forged)"

minds verify <session>        # Exit 1
```
```
Seal           b3-<requested>: TAMPERED — the stored text does not hash to this id
  expected     b3-<requested>
  found        b3-<hash der tatsächlichen Bytes>
  claimed      (UNVERIFIED — the tampered text's statement, not evidence)
               session=b3-… scope=agent-hooks/v1 seq 0–1 · 9 event(s) · 0 gap(s) · stored
  cross-check  the claimed session matches the session under verification
  cross-check  claimed previous b3-<prev>: hash-valid in the store
…
Integrity      VIOLATED
Overall        TAMPERED
```
Zeile für Zeile erklären können:
- **expected** = die Id, unter der der Seal abgelegt ist (der Ref-Name) — der
  Hash, den die Bytes ergeben *müssten*.
- **found** = der Hash, den die vorgefundenen Bytes tatsächlich ergeben.
- **claimed** = was der manipulierte Text *behauptet* — ausdrücklich als
  UNVERIFIED gelabelt. Parst er nicht mal als Seal:
  `claimed unreadable — the stored bytes are not even a well-formed seal`.
- **cross-check** = nennt der gefälschte Text überhaupt die richtige Session,
  und ist sein `previous` im Store hash-valide? Das ordnet die Fälschung ein,
  ohne ihr zu glauben.
- **Die Ehrlichkeitsgrenze** (Philipps wahrscheinlichste Bohr-Frage): Warum
  steht nicht „erwartet events=2, vorgefunden events=9"? **Weil Minds das
  nicht wissen kann.** Der Originaltext ist nach dem Journal-Discard nicht
  rekonstruierbar, der Hash nicht invertierbar — *welches Feld* geändert
  wurde, weiß nur der Angreifer. Nur der Hash-Unterschied ist Beweis; ein
  behaupteter Originalwert wäre selbst eine unbelegte Behauptung. Der Test
  prüft sogar, dass `events=2` **nirgends** in der Ausgabe steht.
- Der gefälschte Text wird vor der Anzeige gehärtet (Seal-Parser + Terminal-
  Sanitizer) — ein `scope=agent-\x1b[2J…` erreicht das Terminal nie.

Dann dieselbe Fälschung in der TUI zeigen: rote Zeile `✗ TAMPERED` in der
Liste, roter Footer-Badge, Evidence-Karte `SESSION TAMPERED` mit
„Seal material was altered — cryptographic verification fails." — CLI und
Read-Model sind paritätsgeprüft (derselbe Forge, beide sagen TAMPERED).

Und `minds fsck` obendrauf: `TAMPERED: seal <id> does not hash to its id`,
Summenzeile `Evidence: <n> seal(s), <f> tampered, <b> withheld`.

**Bonus, wenn Zeit:** Fälschung *plus* Löschen des Rückverweises
(`evidence.json`) — Verdikt bleibt TAMPERED, wird **nicht** zum milderen
NOT VERIFIABLE abgeschwächt (Fix `a26a0b7`: der Namensraum-Fallback nimmt
auch hash-invalide Seals in die Prüfmenge, wenn ihr Text die Session nennt).

Aufräumen nach dem Beat: Demo in einem Wegwerf-Klon fahren, oder den alten
Commit-Hash des Refs vorher notieren und mit `git update-ref` zurücksetzen.

### Beat 5 — Unterschrift und Löschung (5 Min.)

```sh
minds sign --seal <seal-id>            # „Seal <id> signed"
minds verify <session> --signers ~/.ssh/allowed_signers
```
Signatur-Wortlaute und was sie bedeuten:
- `unsigned` — und die TUI sagt dazu: **„unsigned ≠ invalid"**. Die Seals sind
  content-adressiert selbstkonsistent; die Signatur fügt hinzu, *wer* dafür
  einsteht.
- `signed (unchecked — verify with --signers)` — Signatur da, niemand hat sie
  geprüft. „Presence is not verification."
- `signature valid` / `SIGNATURE INVALID` — Letzteres nur bei **explizit**
  genannter `--identity`; eine *geratene* Identität (aus `git config
  user.email`), die fehlschlägt, heißt ehrlich
  `signed (not attributable — pass --identity)` und ist **kein**
  Manipulationsbefund. Das ist Absicht: Der Seal speichert keinen Principal.
- Technik: `ssh-keygen -Y sign/verify`, Namespace `minds`, kein Netz nötig.

`minds forget <session>` danach: Payload weg (`Payload forgotten (<reason>) —
the seal remains the evidence`) — **der Seal überlebt die DSGVO-Löschung** als
payload-freier Beweis, dass der Bereich existierte.

---

## 3. Philipps Bohrfragen — kurze, belastbare Antworten

**„Was heißt hier eigentlich ‚vollständig'?"**
Immer: vollständig **innerhalb der Beobachtungsgrenze** `agent-hooks/v1` (die
steht im Seal selbst, Zeile `scope=`). Subprozesse außerhalb der Hooks,
Netzwerkaktivität, das Fenster zwischen Append und Seal — nicht erfasst, und
das ist **keine Lücke**, sondern eine benannte Grenze. Lücke heißt: Innerhalb
der Grenze fehlt etwas, das da sein müsste (seq-Sprung, beschädigte Datei).

**„Kann ich das ohne Minds nachprüfen?"**
Ja, zwei Dinge: Seal-Identität (`git cat-file blob
refs/minds/evidence/<id>:seal`, blake3-derive_key nachrechnen — Rezept in
`docs/verification-guide.md`) und Signatur (`ssh-keygen -Y verify -n minds`).
Der **Chain-Root nicht** — der braucht Journal + lokalen Salt, absichtlich
(Anti-Orakel).

**„Was, wenn jemand das Journal vor dem Checkpoint manipuliert?"**
`minds fsck` rechnet die Event-Stempel nach und meldet
`<n> TAMPERED (stamp does not match)` — auch Rohdaten sind gebunden. Aber
ehrlich bleiben: Ein lokaler Schreibzugriff, der Event **samt Stempel**
konsistent fälscht, bevor versiegelt wurde, ist nicht erkennbar — steht
wörtlich in den Limits („the integrity between append and seal").

**„Was beweist Minds NICHT?"** (steht im Produkt selbst: TUI-Sektion LIMITS,
`does_not_prove` im Audit-Bundle — proaktiv zeigen, das ist der stärkste
Vertrauensbeweis)
- Nicht, dass die Aufzeichnung vollständig ist — der Hot Path ist fail-open.
- Nicht, wer die Signaturschlüssel kontrolliert — ohne vertrauenswürdige
  `allowed_signers` ist eine Signatur nur Selbst-Attestierung.
- Nicht die Integrität zwischen Append und Seal.
- Nicht, dass außerhalb versiegelter Bereiche nichts geschah.
- Keine echte Wanduhrzeit, keine Kausalität Zeile↔Session, keine Aussage über
  das Modell.

**„Warum ist der `kind` nicht im Hash?"**
Weil er Deutung ist. Evidence ist unveränderlich, Interpretation neu
berechenbar (`minds reinterpret`) — eine bessere Deutung darf die Beweise
nicht ungültig machen. Deshalb wertet die Interpretations-Achse auch nie das
Verdikt.

**„Alte Sessions?"**
`Provenance::Legacy`, Verdikt `NOT VERIFIABLE` (Exit 3), TUI `· legacy`.
Nie nachträglich angedichtet — „its honest answer is this state."

**„Und wenn die Redaktion einen Inhalt blockt?"**
Block-Seal: `outcome=rejected (payload)`, Verdikt `VERIFIED, INCOMPLETE`. Der
Bereich ist bezeugt, der Inhalt wurde nie gespeichert. TUI-Liste zeigt
`⛔ <n> session(s) withheld (redaction) — coverage sealed, details: minds fsck`.

**„Was ist mit dem Salt, wenn der verloren geht?"**
Kein neuer Seal, kein stilles Weiterlaufen: Journal bleibt liegen, Fehler ins
`hook.log` (seit 0.3.0: „Salt loss no longer heals — kein Epoch-Fork"). Der
Verification-Guide sagt: „The loss is a finding, not something to repair."

**„Note: heuristic — reconstructed proximity"?**
Falls die Zeile auftaucht: Heuristik-Funde (gleiche local_id) werden benannt,
werten das Verdikt aber **nie** auf. Rekonstruierte Nähe ist keine Evidence.

---

## 4. Wissens-Anker (falls es ganz tief geht)

- Hash überall: blake3 `derive_key` mit fünf Domänen-Kontexten
  `minds/evidence/v1/{payload,event,gap,chain,seal}` — Domain Separation, ein
  Hash einer Sorte kann nie als andere Sorte durchgehen.
- Event-Kodierung binär, längenpräfixiert (nicht JSON/JCS) — `at_nanos`
  überschreitet 2⁵³, JCS lehnt solche Zahlen ab.
- Fold-Tags: `0x01` Event, `0x02` Gap, `0x03` PreChain; Reihenfolge ist Teil
  des Vertrags (Events in seq-Reihenfolge, Missing-Läufe an ihrer Stelle,
  Beschädigtes deterministisch ans Ende).
- Signierbare Attestation (`minds sign <session>`): 4 Zeilen
  `minds-attestation-v1 / session= / agent= / model=`, testfixiert.
- Ablage: `refs/minds/evidence/<seal_id>` (Seal), `refs/minds/store/<session>`
  (Payload + `evidence.json`-Rückverweis + `links.json`),
  `refs/minds/sessions/<16hex>` (browsbar).
- verify-Betriebsarten: `minds verify <session>` · `--evidence <seal-id>`
  (einzelner Seal, auch ohne Session) · `<session> --sig <file>`
  (Attestation: `valid`/`INVALID`).
- `minds fsck --require-seal` existiert und funktioniert, fehlt aber in
  `--help` und `docs/commands.md` (Doku-Lücke — nicht in der Demo stolpern).
- ADR-0011 nennt 12 Seal-Zeilen ohne `scope=` — der Code hat **13 mit**
  `scope=`. In der Demo: 13 sagen; das ADR hinkt.
- `briefing-minds-tui.md` beschreibt eine nie so gebaute TUI (`minds ui`,
  Sparklines, Ctrl-F) — nicht daraus zitieren. Das Kommando heißt
  `minds inspect`.
- Eine deutsche Restzeile gibt es noch (`main.rs:649`, nur in Builds ohne
  `tui`-Feature) — in der Demo unsichtbar.
