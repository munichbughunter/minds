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
                                                └──▶ Thread (Kommentare)
```

Pro Change-Id: die Commits, die Sessions dahinter (mit Agent, Modell und der
Anweisung), der kanonische Attestation-Payload je Session, die Verdicts mit ihrem
Review-Payload und — falls vorhanden — ihrer Signatur, und der Kommentar-Thread.

Erzeugen:

```sh
minds audit --export --out audit.json          # alles, ab HEAD erreichbar
minds audit --export --base main --out mr.json # nur dieser Stapel
```

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
5. **Herkunft lesen.** `observed` und `inferred` unterscheiden. Wer beides gleich
   behandelt, hat das Bündel nicht verstanden.

## Aufbewahrung

Das Bündel ist eine Momentaufnahme. Es ersetzt das Repo nicht: Die Quelle bleibt
`refs/minds/*`, und die reist bei jedem Klon mit. Wer langfristig aufbewahren muss,
bewahrt das Repo auf — das Bündel ist die Form, in der man es einem Prüfer in die
Hand gibt, der kein Git bedienen will.
