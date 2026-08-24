# Nachweis-Leitfaden — was das Audit-Bündel beweist, und was nicht

*Schicht 3, R6. Zu `minds audit --export`.*

Dieses Dokument ist bewusst so aufgebaut, dass die Grenzen genauso viel Platz
bekommen wie die Zusagen. Ein Nachweis-Artefakt, dessen Grenzen man erst beim
Nachfragen erfährt, richtet mehr Schaden an als keines.

Die wichtigsten Punkte stehen deshalb auch **im Bündel selbst** (`proves` /
`does_not_prove`): Es wird weitergereicht, dieses Dokument bleibt zurück.

## Was drin ist

```
Change ──▶ Commits ──▶ Sessions ──▶ Attribution ──▶ Verdicts (+ Signaturen)
                          │                     └──▶ Thread (Kommentare)
                          └──▶ Evidence-Seals (+ Signaturen)   [ADR-0011]

rejected_seals ──▶ Block-Seals zurückgehaltener Sessions
```

Pro Change-Id: die Commits, die Sessions dahinter (mit Agent, Modell und der
Anweisung), der kanonische Attestation-Payload je Session, die Verdicts mit ihrem
Review-Payload und — falls vorhanden — ihrer Signatur, und der Kommentar-Thread.
Seit Schema 2 zusätzlich je Session ihre **Evidence-Seals** (der byte-genaue
Seal-Text jeder Checkpoint-Epoche, samt Signatur) und unter `rejected_seals` die
Block-Seals: Sessions, deren Nutzlast die Speicher-Policy zurückwies — der Seal
beweist, dass sie existierten, ohne ihren Inhalt preiszugeben.

Erzeugen:

```sh
minds audit --export --out audit.json           # alles, ab HEAD erreichbar
minds audit --export --base main --out mr.json  # nur dieser Stapel
minds audit --export --mode proof --out p.json  # nur das Beweisgerüst
```

Zwei Zuschnitte: **`redacted`** (Default) trägt alles, was der Store hergibt —
also die redigierten Intents, Verdicts, Kommentare, Seals. **`proof`** trägt
nur das Beweisgerüst: Ids, kanonische Payload-Texte, Seals samt Signaturen,
Verdict-Metadaten — kein Intent, keine Zusammenfassungen, keine Kommentare.
Damit lässt sich extern prüfen, *dass* und *wie viel* passiert ist, ohne den
Inhalt weiterzugeben. Ein Modus „full" existiert bewusst nicht: Der Store hält
ausschließlich redigierte Sessions (fail-closed) — mehr als `redacted` gibt es
nicht zu exportieren.

Auch `proof` trägt weiterhin **Personenkennungen**: den Reviewer (nötig, um
Signaturen einer Identität zuzuordnen) sowie Agent- und Modellnamen in den
kanonischen Payloads. Wer das nicht weitergeben darf, redigiert das Bündel vor
der Weitergabe selbst. Inhalts-Hashes an Lese-Effekten entstehen nur für von
git **getrackte** Dateien — bloßes Lesen einer privaten Datei hinterlässt
keinen Fingerabdruck im Bündel.

## Was es beweist

**Integrität des Inhalts.** Jede `id` ist der blake3-Hash der kanonischen Form
ihres Inhalts. Wer die Session aus dem Store zieht, kann den Hash nachrechnen; wer
sie nachträglich editiert, fliegt dabei auf. Dasselbe gilt für Verdicts und
Kommentare.

**Prüfbarkeit ohne dieses Werkzeug.** `attestation_payload` und `review_payload`
sind byte-genau die Texte, über die signiert wird. Ein Auditor braucht nur
`ssh-keygen`:

```sh
jq -r '.changes[].verdicts[] | select(.signature) | .signature' audit.json > v.sig
jq -r '.changes[].verdicts[] | select(.signature) | .review_payload' audit.json \
  | ssh-keygen -Y verify -f allowed_signers -I anna@example.org -n minds -s v.sig
```

**Kontinuität über Rebase und Force-Push.** Verdicts hängen an der Change-Id, nicht
am Commit-Hash. Ein überarbeiteter Stapel verliert seine Prüfhistorie nicht.

**Nachweisbare Löschung.** Eine per `minds forget` getilgte Session steht als
`"payload": "forgotten"` weiterhin in der Kette. Die Referenz bleibt auflösbar, der
Inhalt ist weg — DSGVO-Löschung, ohne dass die Historie lügt.

## Was es nicht beweist

**Keine Vollständigkeit.** Der Aufzeichnungspfad ist fail-open: `minds hook` verliert
lieber ein Ereignis, als die Sitzung zu stören. Ein verlorenes Ereignis fehlt hier
**stillschweigend**. `minds fsck` macht Lücken sichtbar; ein Bündel ohne einen
begleitenden `fsck`-Lauf sagt nichts über Vollständigkeit.

**Keine Kausalität Zeile ↔ Session.** Die Zuordnung stammt aus zwei Quellen: dem
Trailer in der Commit-Message (`observed`) und einer Heuristik für importierte
Sessions (`inferred`). Die Herkunft steht an jeder Kante — sie darf nicht
eingeebnet werden. „Vermutet" heißt vermutet.

**Keine Aussage über das Modell.** Aufgezeichnet ist, was der Agent **gemeldet** hat.
Dass ein bestimmtes Modell tatsächlich einen bestimmten Text erzeugt hat, kann ein
Client-Werkzeug nicht beweisen; dafür bräuchte es eine Zusicherung des Anbieters.

**Keine Aussage über die Schlüssel.** Eine Signatur ist nur so viel wert wie die
`allowed_signers`-Datei, gegen die geprüft wird. Kommt sie aus demselben Repo wie
das Bündel, ist sie eine Selbstauskunft. Sie muss aus einer Quelle stammen, der der
Prüfer unabhängig traut (Verzeichnisdienst, ausgerollte Datei, Schlüsselzeremonie).

**Kein Vertrauen für Unsigniertes.** Ein unsigniertes Verdict ist content-adressiert
— es ist unverändert. Aber niemand steht mit einem Schlüssel dafür ein. Wer
Verbindlichkeit braucht, verlangt `minds review --sign` und prüft mit
`minds reviews --signers`.

**Kein Zeitnachweis.** Die Zeitstempel stammen von der Uhr der Maschine, die den
Eintrag geschrieben hat. Sie ordnen; sie beweisen nichts. Wer beweisbare Zeit
braucht, braucht einen Zeitstempeldienst — den gibt es hier nicht.

## Wie ein Prüfer damit arbeitet

1. **Lücken zuerst.** `minds fsck` laufen lassen und die Ausgabe zum Bündel legen.
   Ohne sie ist jede Aussage über Abdeckung unbelegt.
2. **Schlüssel besorgen.** Die `allowed_signers` aus unabhängiger Quelle, nicht aus
   dem Repo.
3. **Signaturen prüfen** (siehe oben). Unsigniertes gesondert behandeln.
4. **Hashes nachrechnen**, falls der Store mitgeliefert wird — der Klon reicht:
   `git cat-file blob refs/minds/store/<hash>:session.json | b3sum`.
5. **Herkunft lesen.** `observed` und `inferred` unterscheiden — und seit
   ADR-0011 auch den Status: `beobachtet` heißt nicht `nachgerechnet`. Wer
   beides gleich behandelt, hat das Bündel nicht verstanden.
6. **Seals prüfen** (Abschnitt unten): Identität nachrechnen, Signatur
   verifizieren, Coverage lesen. Ein Block-Seal in `rejected_seals` ist eine
   Aussage, kein Fehler: Diese Session existierte, ihre Nutzlast wurde von der
   Speicher-Policy zurückgewiesen.

## Die Evidence-Chain ohne Minds nachrechnen

Der Proof gehört nicht Minds: Alles im Seal ist mit Standard-Werkzeugen
prüfbar. Die Hashes sind `blake3::derive_key` mit festen Kontext-Strings —
hier als Python, weil `b3sum` keinen derive_key-Modus hat:

```python
# pip install blake3
from blake3 import blake3

def derive(context: str, material: bytes) -> str:
    return blake3(material, derive_key_context=context).hexdigest()
```

**1. Die Seal-Identität.** Der Ref-Name muss der Hash des Textes sein:

```sh
git for-each-ref refs/minds/evidence/            # Seals auflisten
git cat-file blob refs/minds/evidence/<id>:seal  # den Text holen
```

```python
assert derive("minds/evidence/v1/seal", seal_text_bytes) == ref_name_hex
```

**2. Die Signatur** (falls `seal.sig` daneben liegt) — exakt die abgelegten
Bytes, geprüft wie eine Git-SSH-Signatur:

```sh
git cat-file blob refs/minds/evidence/<id>:seal      > seal.txt
git cat-file blob refs/minds/evidence/<id>:seal.sig  > seal.sig
ssh-keygen -Y verify -n minds -I <identität> \
  -f allowed_signers -s seal.sig < seal.txt
```

**3. Der Chain-Root** — nur **lokal** nachrechenbar, mit dem noch liegenden
Journal **und** dem Session-Salt (`<git-dir>/minds/evidence/state/…/*.salt`;
der Fold startet auf `derive("minds/evidence/v1/chain", salt)`). Das ist
Absicht, kein Mangel: Ohne Salt wäre der Root ein Offline-Orakel — wer einen
kurzen Payload rät, könnte ihn gegen den Root bestätigen. Nach dem Checkpoint
ist das Journal weg; dann bindet der Root die damals gelesenen Events, und
Manipulation am *Seal* fällt über Schritt 1 auf:

```python
payload_hash = derive("minds/evidence/v1/payload", payload_bytes)
# event_hash: längenpräfixierte Felder (u64 LE) — Schema in
# crates/minds-core/src/evidence.rs, Kontext "minds/evidence/v1/event".
# Fold: state = derive("minds/evidence/v1/chain",
#                      state ‖ tag ‖ glied)   # tag 0x01 Event, 0x02 Lücke,
#                                             # 0x03 pre-chain; Start: 32 × 0x00
```

**4. Das Verdikt lesen.** `gaps=0`, `pre_chain=0`, `outcome=stored` und eine
über `previous=` geschlossene Epochenkette ⇒ vollständig. Alles andere ist
`VERIFIZIERT, UNVOLLSTÄNDIG` — und `minds verify <session-id>` sagt dasselbe
mit Exit-Codes (0 verifiziert, 1 manipuliert, 2 unvollständig, 3 nicht
verifizierbar), für CI-Gates zusätzlich `minds fsck --require-seal`.

Was auch der Seal **nicht** beweist, steht im Bündel unter `does_not_prove` —
insbesondere: nichts über Ereignisse außerhalb versiegelter Bereiche, und
nichts über das Fenster zwischen Append und Seal.

## Aufbewahrung

Das Bündel ist eine Momentaufnahme. Es ersetzt das Repo nicht: Die Quelle bleibt
`refs/minds/*`, und die reist bei jedem Klon mit. Wer langfristig aufbewahren muss,
bewahrt das Repo auf — das Bündel ist die Form, in der man es einem Prüfer in die
Hand gibt, der kein Git bedienen will.
