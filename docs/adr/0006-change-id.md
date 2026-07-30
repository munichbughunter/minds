# ADR-0006 — Change-Id: stabile Änderungs-Identität

- Status: angenommen
- Datum: 2026-07-28
- Betrifft: `minds-core`, `minds-cli`
- Verwandt: ADR-0005 (Kontext-Rückführung); Schicht 2 in `Plan-v0.2.md`

## Kontext

Der Commit-Hash ist keine Identität für eine *logische* Änderung: `rebase`,
`squash`, `amend` und `cherry-pick` erzeugen für dieselbe Absicht einen neuen
Hash. Das bricht stabile Verweise, Review-Kontinuität über Force-Push hinweg und
das Denken in „diese Änderung" vs. „diese Version dieser Änderung" — genau die
Bruchstelle, die Gerrit und Jujutsu mit Change-IDs lösen. Minds braucht dieselbe
stabile Klammer, damit später Reviews (Schicht 3) an *der Änderung* hängen können
und nicht an einer vergänglichen Commit-Version.

## Entscheidung 1: Gerrit-kompatibles Format

Eine Change-Id ist `I` + 40 Hex-Zeichen (`I<40 hex>`) — dieselbe Form, die Gerrits
`commit-msg`-Hook erzeugt. So greifen vorhandene Erwartungen und Regexe
(`I[0-9a-f]{40}`) ohne Anpassung. Der Trailer-Schlüssel bleibt im Minds-Namensraum:
`Minds-Change-Id`, konsistent mit `Minds-Session-Id` und `Minds-Attribution`.

Der Typ (`minds_core::ChangeId`) folgt derselben „lesen tolerant, schreiben
kanonisch"-Linie wie `SessionId`/`ContentHash`.

## Entscheidung 2: der Trailer trägt sie, nicht der Hash

Wie der `Minds-Session-Id`-Trailer steht die Change-Id im **Text** der
Commit-Message. Damit überlebt sie genau die Operationen, die den Hash ändern —
denn `rebase`/`squash`/`cherry-pick` führen die Message mit. Das teilt sich die
gesamte, schon getestete Trailer-Maschinerie (`extract_all`, Squash-Toleranz mit
eingerückten Rümpfen).

## Entscheidung 3: erzeugt im `prepare-commit-msg`-Hook

`minds prepare-commit-msg` (vom `enable`-Hook aufgerufen) hängt eine Change-Id an,
falls keine da ist, und lässt eine vorhandene unangetastet. So bekommt die erste
Version einer Änderung ihre Id, und jede spätere (amend, rebase) behält sie.

**Ehrliche Grenze:** Bei einem interaktiven Commit *ohne* `-m` ist die Message zum
Hook-Zeitpunkt noch leer; dann wird nichts angehängt (der Trailer würde sonst zum
Betreff). Sicher erfasst sind `-m`, `amend`, `rebase`, `cherry-pick`, `squash` —
genau die Operationen, um deren Überleben es geht. Ein `commit-msg`-Hook wäre für
den interaktiven Erst-Commit der genauere Ort und bleibt eine mögliche Ergänzung.

**Generierung:** aus Zeit + Prozess-Id, über splitmix64 gut verteilt. Eine
Change-Id ist kein Geheimnis — sie braucht Eindeutigkeit, nicht Unvorhersehbarkeit.
Eine Kollision bräuchte zwei Commits in derselben Nanosekunde aus demselben Prozess.

## Konsequenzen

`minds show` zeigt die Change-Id eines Commits an. Die Change-Id überlebt Rebase
und Squash (end-to-end verifiziert). Sie ist die Voraussetzung für Schicht 3
(Reviews hängen an der Change-Id, nicht am Commit) und für stacked changes.
