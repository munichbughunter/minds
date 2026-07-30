# ADR-0009 — Reviews als Git-Objekte (Schicht 3, Slice R1 + R5)

- Status: angenommen (R1–R6 umgesetzt)
- Datum: 2026-07-28, erweitert 2026-07-29
- Betrifft: `minds-core`, `minds-store`, `minds-cli`
- Baut auf: ADR-0006 (Change-Id), ADR-0008 (signierte Attribution); `Roadmap.md` Schicht 3

## Kontext

Das Projektgedächtnis von GitLab — Reviews, Approvals, Diskussion — liegt in
Postgres, nicht im Repo. Migriert man weg, verliert man die Hälfte der Geschichte.
Radicle und git-bug zeigen den anderen Weg: **Reviews als Git-Objekte.** Das ist
die große Wette der These „mehr ins Repo, weniger in die Plattform" — und das eine,
das ein SaaS-Anbieter strukturell ungern baut.

Dieser ADR begann mit dem ersten Durchstich (**R1** Review-Objekt, **R5**
Policy-Gate) und deckt seit dem 29.07. **R1–R6 vollständig** ab. Die Ergänzungen
stehen unten unter „Der Ausbau"; die Transportfrage (ein Ref je Session, ein Push
für alle) hat einen eigenen ADR bekommen: [ADR-0010](0010-ein-ref-je-session.md).

## Entscheidung 1 (R1): das Review-Objekt

`minds_core::Review` ist ein versioniertes, **content-adressiertes** Envelope:
Subjekt (bevorzugt eine **Change-Id**, ersatzweise eine Session-Id), Verdict
(`approve` / `reject` / `needs-work`), Reviewer-Identität, Zusammenfassung. Der
Hash ist `blake3` der kanonischen Form (dieselbe `b3-`-Textform wie Sessions).

- **An der Change-Id, nicht am Commit.** Nur so überlebt das Verdict den Rebase —
  genau der Grund, warum Schicht 2 die Change-Id vor Schicht 3 kam.
- **Eigener Ref `refs/minds/reviews`.** Reviews liegen getrennt vom Kontext-Store:
  eigene Zugriffsrechte, eigener Push-Weg, keine Vermischung mit der Session-Liste.
  Dasselbe content-adressierte Layout (`reviews/<2hex>/<rest>.json`), dedup-freundlich.
- **Kommandos:** `minds review <subject> --approve|--reject|--needs-work
  [--summary]` legt an, `minds reviews <subject>` listet.

Die Reviewer-Identität ist dieselbe, unter der signiert wird — R1 ist damit
anschlussfähig an die signierte Attribution aus ADR-0008 (ein signiertes Verdict
ist der additive nächste Schritt).

## Entscheidung 2 (R5): Policy als Binary, nicht als YAML

`minds fsck --require-review` verlangt für **jeden erreichbaren, agent-authored
Commit** (trägt ≥1 `Minds-Session-Id`) ein Approve — an seiner Change-Id oder einer
seiner Session-Ids. Fehlt es, ist der Rückgabewert ≠ 0.

Das mitgelieferte `ci/minds-review-gate.gitlab-ci.yml` ruft nur dieses Binary auf.
Ein Format, das keine Logik kann (YAML), soll auch keine tragen — die Regel lebt im
Binary, wo sie testbar ist.

## Der Ausbau (R1-Rest, R2, R3, R4, R6)

**Signierte Verdicts (R1 vollständig).** `review_payload` in `minds-core` ist der
kanonische, signierbare Text — dieselbe Bauform wie `attestation_payload`. Die
Signatur liegt als **Sidecar** neben dem Review (`reviews/<2hex>/<rest>.sig`), nicht
darin: Ein Feld im Envelope wäre zirkulär (der Hash deckt das Envelope, die Signatur
geht über den Hash). Folge: Der Hash ändert sich nicht, wenn jemand nachträglich
signiert, und mehrere Identitäten können dasselbe Verdict unterschreiben.
`minds review --sign`, `minds reviews --signers` prüft. **Ohne `--signers` wird
nicht geprüft, sondern nur gemeldet** — „signiert" und „gültig" dürfen nicht gleich
aussehen.

**Der Thread (R2).** `minds_core::Comment` ist eine append-only Operation,
content-adressiert, verankert an `datei:zeile`, an einem Turn oder am Change als
Ganzem. Der Anker ist Teil des Hashes: derselbe Text an zwei Stellen sind zwei
Kommentare. Der Merge zweier Logs ist eine **Mengenvereinigung**
(`ReviewStore::merge_from`) — kommutativ und idempotent, weil gleicher Pfad
gleichen Inhalt bedeutet. Die Anzeigereihenfolge kommt aus dem Inhalt (Zeit, dann
Hash), damit zwei Maschinen denselben Thread gleich zeigen. Kommentare liegen unter
demselben Ref wie die Verdicts: Ein zweiter Ref wäre ein zweiter Ort, an dem etwas
fehlen kann.

**Der Stapel (R3).** `minds stack` zeigt die Changes ab einer Basis mit ihrem
jeweiligen Stand. Weil das Verdict an der Change-Id hängt, überlebt es Rebase und
Force-Push — festgehalten im Test `a_force_push_of_the_stack_keeps_every_verdict`.

**Die Plattform als Cache (R4).** `minds-gitlab` spiegelt Verdicts als MR-Note,
**einweg und idempotent** über einen unsichtbaren Marker `<!-- minds:review:<hash> -->`.
Die Gegenrichtung (`minds gitlab webhook`) ist opt-in, zustandslos und schreibt ohne
`--write` nichts. Kein HTTP-Stack im Binary: `curl`, wie beim Signieren
`ssh-keygen`. Der Token kommt nur aus der Umgebung und geht über stdin an `curl` —
nie in eine Argumentliste. Betriebsmodell:
[betriebsmodell-gitlab.md](../betriebsmodell-gitlab.md).

**Der Audit-Export (R6).** `minds audit --export` bündelt Change → Commits →
Sessions → Attribution → Verdicts (+ Signaturen) + Thread als portables JSON. Es
enthält die **kanonischen Payloads**, ist also ohne dieses Werkzeug prüfbar
(`blake3`, `ssh-keygen -Y verify`). Die Grenzen stehen **im Artefakt** (`proves` /
`does_not_prove`), nicht nur in der Doku — sie wird weitergereicht, die Doku bleibt
zurück. Ausführlich: [nachweis-leitfaden.md](../nachweis-leitfaden.md).

## Konsequenzen

Ein Repo kann tragen, was bisher die Plattform-Datenbank hielt: das Verdict zu
einer Änderung, content-adressiert, mit dem Repo wandernd, in der CI erzwingbar.
Zusammen mit Change-Id, signierter Attribution und `minds forget` ist damit die
„mehr ins Repo"-These vollständig umgesetzt: Ein Repo trägt seine eigene,
kryptografisch nachweisbare Antwort auf „Wer hat das geschrieben, auf welche
Anweisung, wer hat es geprüft, und warum wurde es gemerged?" — ohne Plattform, ohne
Datenbank, offline verifizierbar.

Was bewusst offen bleibt: Die Signatur einer **Session**-Attribution wird von
`minds sign` nach stdout geschrieben, aber nicht abgelegt. Der Audit-Export liefert
deshalb den Payload und nimmt die Signatur von außen entgegen. Ein Sidecar wie beim
Review wäre der additive nächste Schritt.
