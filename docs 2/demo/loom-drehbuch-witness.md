# Loom: Ein echter Change von Intent bis Verdict — und der Versuch, die Evidence zu fälschen

Ziel: Philipps Punkt 3 in fünf Minuten beantworten, ohne Folie. Der Zuschauer soll am Ende
einen Satz mitnehmen: **Der Agent schreibt seine Evidence nicht selbst. Er wird beobachtet —
von etwas, das er nicht anfassen kann. Und was er trotzdem behauptet, wird gegengeprüft.**

Dauer: 5:00 · Aufnahme: ein Take pro Szene, danach schneiden · Sprache: Deutsch, CLI-Ausgabe
englisch (so ist das Produkt).

---

## Setup (vor der Aufnahme)

**Voraussetzung:** Der Demo-Pfad aus Track EA ist umgesetzt, und `EA-22` (End-to-End-Pilot-Test)
ist grün. Nichts zeigen, was dieser Test nicht abdeckt.

**Bildschirm:** zwei Terminals nebeneinander, Schrift ≥ 18 pt, dunkles Theme.
- **Links, „Agent“:** Shell im Dev-Container, Claude Code darin. Prompt-Präfix sichtbar `agent@devcontainer`.
- **Rechts, „Witness“:** Host-Shell, `minds witness run --follow` zeigt einlaufende Events mit Seq und Chain-Head (gekürzt auf 8 Zeichen).

**Repo:** das bekannte Demo-Repo mit `fachliche-anforderung.md` (LOG-2417, sechs AC, AC5/AC6 kollidieren) —
Philipp kennt die Geschichte aus dem Paper. Frischer Branch `demo/witness`, Session muss neu und echt sein.

**Schlüssel:**
- Intent-Signatur: FIDO-Key (`sk-ssh-ed25519`), weil die Berührung im Bild der stärkste Beleg ist. Ohne FIDO-Key: Software-Key mit Passphrase und das im Video offen sagen.
- Witness-Key: liegt nur auf dem Host (`minds witness keygen` vorher erledigt).
- `allowed_signers` mit Namespaces, aus einer Datei außerhalb des Repos.

**Agentenlauf vorher einmal real durchspielen.** Wenn die Session länger als ~6 Minuten dauert,
die Aufnahme mit 8× Zeitraffer schneiden und das im Bild einblenden („8× Zeitraffer, ungeschnitten
im Log“). Ehrlichkeit ist hier das Argument.

**Probelauf-Check:** `minds doctor` rechts zeigt `profile: container · agent cannot reach witness dir: ok`.

---

## Szene 1 — Die Frage (0:00–0:25)

**Bild:** Beide Terminals, noch leer bis auf `minds doctor`-Ausgabe rechts.

**Sprechtext:**
„Philipp, du hast gefragt: Wenn der Agent seine eigene Evidence schreibt, ist sie am Ende nur
signiert, nicht bewiesen. Das stimmte. Deshalb zeige ich dir jetzt einen echten Change von der
Anforderung bis zum Verdict — und danach versuche ich, die Evidence zu fälschen.“

---

## Szene 2 — Intent binden und signieren (0:25–1:00)

**Rechts (Host):**
```bash
minds intent bind --file fachliche-anforderung.md --scope 'src/sort/**,tests/**'
minds intent sign            # FIDO-Key blinkt → Berührung im Bild
```

**Erwartete Ausgabe (sinngemäß):**
```text
intent  file:fachliche-anforderung.md@3f9c1e2a  content b3-7a41…
scope   src/sort/**, tests/**
signed  patrick@doering-it (sk-ssh-ed25519, user presence)
```

**Sprechtext:**
„Der Anfang der Kette ist nicht der erste Prompt, sondern diese Anforderung, in genau dieser
Version. Ich signiere sie mit einem Hardware-Key — der Agent kann das nicht, auch mit voller
Shell nicht, weil dafür ein Finger nötig ist. Was minds nicht beweisen kann: dass die Anforderung
gut ist. Es beweist, welche galt und wer sie freigegeben hat.“

---

## Szene 3 — Der Agent arbeitet, der Witness schaut zu (1:00–2:00)

**Links (Container):** Claude Code starten, Aufgabe: „Setze LOG-2417 um.“

**Rechts:** Events laufen live ein — `UserPromptSubmit`, `PostToolUse Write src/sort/merge.rs`,
`fs.observed src/sort/merge.rs b3-…`, `PostToolUse Bash cargo test`, `cargo bench`, verworfene
Varianten.

**Sprechtext (über dem Zeitraffer):**
„Links arbeitet der Agent in einem Container. Rechts, auf dem Host, läuft der Witness. Jedes
Event wird beim Eintreffen verkettet — nicht erst am Ende. Und der Witness hat ein zweites Auge:
Er sieht direkt im Dateisystem, was sich wirklich ändert, unabhängig davon, was der Agent meldet.
Der Agent kommt an nichts davon heran: nicht an das Journal, nicht an den Schlüssel.“

**Kurzer Beleg (5 Sekunden), links:**
```bash
ls /var/lib/minds-witness        # → No such file or directory
```

---

## Szene 4 — Commit und Verdict (2:00–2:40)

**Links:** Agent committet (oder du: `git commit`), Ausgabe `SESSION SEALED`.

**Rechts:**
```bash
minds verify
```

**Erwartete Ausgabe (sinngemäß):**
```text
  Verdict         VERIFIED
  Integrity       intact     (2 seals, 118 events, chain recomputed)
  Coverage        complete   (0 gaps · artifact 150/150 lines explained · 0 out of scope)
  Interpretation  all calls interpreted
  Assurance       A2 witnessed   (witness@host, profile container; intent signed, sk key)
  Not proven      model identity · correctness of the decision · actions outside the boundary
```

**Sprechtext:**
„Drei Achsen, getrennt: Wurde etwas verändert, fehlt etwas, wie wird es gedeutet. Neu ist die
Zeile ‚artifact 150 von 150 Zeilen erklärt‘: Jede Zeile im Commit wurde in der Session beobachtet —
vom Agenten gemeldet und vom Witness im Dateisystem gesehen. Und darunter steht immer, was nicht
bewiesen ist.“

---

## Szene 5 — Drei Fälschungsversuche (2:40–4:10)

### 5a — Der Agent meldet etwas, das nie passiert ist (2:40–3:10)

**Links (Container, als Agent):** gefälschtes Hook-Event einspeisen — genau das, was ein
manipulierter Agent könnte:
```bash
./demo/forge-write-claim.sh src/sort/merge.rs   # schickt einen PostToolUse-Write mit erfundenem Hash
```

**Rechts:**
```bash
minds verify
```
```text
  Coverage        complete   (… · 1 reported event uncorroborated)
    uncorroborated  seq 119  Write src/sort/merge.rs  b3-deadbeef…  no file-system observation
```

**Sprechtext:**
„Der Agent kann den Socket erreichen, also kann er Behauptungen einschleusen. Das ist gewollt
sichtbar: Er kann Lärm hinzufügen — aber nichts löschen, nichts umschreiben, und eine Behauptung
wird nicht dadurch wahr, dass er sie meldet. Ohne Beobachtung im Dateisystem bleibt sie
‚uncorroborated‘.“

### 5b — Ein Mensch ändert nachträglich eine Zeile (3:10–3:35)

**Rechts (Host, als Mensch):**
```bash
sed -i '88s/<=/</' src/sort/merge.rs && git commit --amend --no-edit -a
minds verify
```
```text
  Coverage        incomplete (artifact 149/150 lines explained)
    unexplained     src/sort/merge.rs:88   not observed in the session
```

**Sprechtext:**
„Das ist kein Vorwurf, das ist eine Tatsache: Diese Zeile hat die aufgezeichnete Session nicht
erzeugt. Genau die Frage ‚Hat der Agent das geschrieben?‘ ist damit beantwortbar — Zeile für Zeile.“

### 5c — Jemand manipuliert die gespeicherte Evidence (3:35–4:10)

**Rechts:**
```bash
git reset --hard HEAD@{1}                      # Amend zurück, sauberer Stand
./demo/tamper-seal.sh                          # ändert ein Byte im gespeicherten Seal
minds verify ; echo "exit $?"
```
```text
  Verdict         TAMPERED
    seal b3-91c0…  expected b3-91c0…  found b3-4e7d…
exit 1
```

**Sprechtext:**
„Und wer nach dem Versiegeln etwas verändert, fliegt auf — mit erwartetem und gefundenem Hash.
Exit-Code 1, damit bricht jede CI-Pipeline.“

---

## Szene 6 — Die Pipeline prüft nach (4:10–4:35)

**Bild:** GitLab-MR, Job `minds-evidence` grün, Log aufgeklappt.
```text
replay   3/3 decisive test runs reproduced (cargo test -p sort …)
verify   VERIFIED · A2 witnessed · 150/150 explained · 1 uncorroborated (seq 119)
```

**Sprechtext:**
„Zum Schluss führt die CI die Tests, auf die der Agent seine Entscheidung gestützt hat, selbst
noch einmal aus. ‚Tests grün‘ ist dann keine Behauptung des Agenten mehr, sondern nachgerechnet.“

---

## Szene 7 — Die Grenze und der Satz (4:35–5:00)

**Bild:** nur die `Not proven`-Zeile, groß.

**Sprechtext:**
„Was bleibt unbewiesen: welches Modell wirklich geantwortet hat, ob die Entscheidung fachlich
richtig war, und alles außerhalb der Beobachtungsgrenze. Das steht in jedem Export mit drin.
Der Punkt ist: Der Agent schreibt seine Evidence nicht mehr selbst. Er wird beobachtet — von
etwas, das er nicht anfassen kann. Und was er behauptet, wird gegengeprüft.“

---

## Hilfsskripte für die Demo (in `demo/`, Teil von EA-22)

- `forge-write-claim.sh` — baut einen syntaktisch gültigen `PostToolUse`-Payload (Write, erfundener
  Content-Hash) und pipet ihn in `minds hook`. Absichtlich simpel: Genau das kann jeder Agent.
- `tamper-seal.sh` — liest den Seal-Blob unter `refs/minds/evidence/<id>`, ändert ein Byte in
  `events=`, schreibt einen neuen Commit und biegt den Ref um.
- Beide Skripte enthalten oben einen Kommentar „Demo-Angriff, nicht für Produktivrepos“.

## Risiken bei der Aufnahme

- **Agent weicht vom Plan ab** (committet nicht, nimmt `sort_by`): egal — dann ist das der echte
  Change. Nicht nachstellen. Szene 4 funktioniert mit jedem Ergebnis.
- **Nicht 150/150:** Wenn Formatter oder Generatoren Dateien anfassen, erscheinen sie als
  `explained (fs only)`. Stehen lassen und einen Satz dazu sagen — das zeigt, dass das zweite Auge arbeitet.
- **macOS-Bind-Mount-Latenz:** Szene 3 auf dem Linux-Rechner aufnehmen, falls EA-S2 auf macOS wackelt.

---

## Begleitnachricht an Philipp

> Moin Philipp,
>
> hier ist der Change von Intent bis Verdict, 5 Minuten: <Loom-Link>
>
> Zu deinem dritten Punkt, weil er der wichtigste war: Du hattest recht — solange der Agent
> seine Evidence im eigenen Prozessraum schreibt, ist sie nur signiert. Deshalb schreibt sie
> jetzt ein Witness außerhalb seiner Reichweite, mit eigenem Schlüssel, und prüft gegen das
> Dateisystem, den Commit und einen CI-Nachlauf. Im Video versuche ich an drei Stellen zu
> fälschen; du siehst, was hängen bleibt — und was minds ausdrücklich nicht beweist.
>
> Zu Punkt 1 und 2 schreibe ich dir separat, das verdient mehr als einen Nebensatz.
>
> Welche Fälschung würdest du als Nächstes versuchen?
>
> Beste Grüße
> Patrick
