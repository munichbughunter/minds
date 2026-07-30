# Betriebsmodell — Git ist die Quelle, GitLab ist die Projektion

*Schicht 3, R4. Gehört zu ADR-0009.*

## Der Satz, um den es geht

Das Verdict zu einer Änderung liegt content-adressiert und signierbar unter
`refs/minds/reviews`. Es wandert mit dem Repo, ist offline verifizierbar und
überlebt jede Plattform-Migration. **GitLab zeigt es an — es hält es nicht.**

Das ist keine Feinheit der Implementierung, sondern die These des Projekts. Wer
heute von GitLab wegzieht, verliert Reviews, Approvals und Diskussion, weil sie in
Postgres liegen und nicht im Repo. Mit Minds kommt diese Hälfte der Geschichte mit.

## Die zwei Richtungen sind nicht gleichwertig

```
   Repository  ──── minds gitlab mirror ───▶  GitLab-MR      (Standard, einweg)
   Repository  ◀─── minds gitlab webhook ───  GitLab-MR      (opt-in, manuell)
```

**Hinaus** ist der Normalfall und kann nichts kaputtmachen: Was im Repo steht, wird
als MR-Note sichtbar. Die Note trägt einen unsichtbaren Marker
(`<!-- minds:review:<hash> -->`), und vor dem Schreiben wird gelesen. Steht der
Marker schon da, passiert nichts. Weil der Hash das Verdict content-adressiert,
heißt „derselbe Marker" auch „derselbe Inhalt" — der Job darf also bei jedem Push
laufen.

**Herein** gibt es, aber nur bewusst. `minds gitlab webhook` liest eine Nutzlast von
stdin und deutet einen Kommentar der Form

```
/minds approve      Backoff ist jetzt korrekt
/minds reject       so nicht
/minds needs-work   bitte den Test nachziehen
```

als Verdict. Ohne `--write` wird nur gezeigt, was entstünde. Alles andere — jeder
gewöhnliche Kommentar, jedes andere Ereignis — erzeugt nichts und meldet nichts.

Warum nicht automatisch in beide Richtungen: Dann gäbe es zwei Quellen, und jemand
müsste entscheiden, welche gewinnt. Genau diesen Zustand vermeidet das Projekt.

## Es gibt keinen Dienst

`minds gitlab webhook` ist ein Kommando, kein Empfänger. Wer einen HTTP-Endpunkt
braucht, stellt einen beliebigen davor (ein CI-Job, ein `socat`, ein
Funktions-Endpunkt beim Kunden); wer keinen will, kippt gespeicherte Nutzlasten
hinein. Wir betreiben nichts, und es gibt nichts zu betreiben.

Das Kommando läuft in einem Checkout. Das ist Absicht: GitLab kennt Commit-Hashes,
ein Verdict hängt an einer **Change-Id**. Die Brücke zwischen beidem ist der Trailer
in der Commit-Message, und der steht im Repo.

## Woran ein Verdict hängt

An der Change-Id — nie am Commit. Ein Force-Push schreibt jeden Hash um; ein Verdict
am Hash wäre danach verwaist. `minds stack` zeigt den Stapel mit seinen einzelnen
Ständen, und der Test `a_force_push_of_the_stack_keeps_every_verdict` hält das fest.

Steht im Kommentar eine Change-Id (`I` + 40 Hex), gewinnt sie. Sonst wird der letzte
Commit des MR lokal zu seiner Change-Id aufgelöst. Findet sich keine, entsteht
**kein** Review — lieber nichts als ein Verdict, das an nichts hängt.

## Einrichten

```sh
git config minds.gitlabUrl     https://gitlab.example
git config minds.gitlabProject gruppe%2Fprojekt     # Id oder URL-kodierter Pfad
```

Der Token kommt **nur** aus einer Umgebungsvariablen (`MINDS_GITLAB_TOKEN`, oder was
`--token-env` nennt). Nie aus einem Argument: Das stünde in `ps` und in der
Shell-History. Auch an `curl` geht er über stdin, nicht über die Argumentliste.

Nötige Rechte: `api` für die Note, zusätzlich Approval-Rechte für `--approve`.

## In der CI

```yaml
minds:mirror:
  rules:
    - if: '$CI_MERGE_REQUEST_IID'
  script:
    - git fetch origin '+refs/minds/*:refs/minds/*' || true
    - minds gitlab mirror "$(minds stack --base "$CI_MERGE_REQUEST_TARGET_BRANCH_NAME" | …)" \
        --mr "$CI_MERGE_REQUEST_IID"
```

Die Spiegelung ist idempotent, also ist ein wiederholter Lauf folgenlos. Das
Policy-Gate (`minds fsck --require-review`, siehe `ci/minds-review-gate.gitlab-ci.yml`)
bleibt davon unberührt: Es prüft das **Repo**, nicht GitLab. Ein Verdict, das nur in
der Oberfläche steht, öffnet das Gate nicht.

## Was passiert, wenn man wegzieht

Nichts geht verloren. Die Notes bleiben in der alten Instanz zurück — sie waren nie
die Quelle. Verdicts, Thread und Signaturen liegen in `refs/minds/reviews` und
werden mit dem Repo geklont. `minds audit --export` bündelt sie zusätzlich als
portable Datei (siehe [Nachweis-Leitfaden](nachweis-leitfaden.md)).
