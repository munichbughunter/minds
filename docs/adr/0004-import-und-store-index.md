# ADR-0004 — Transkript-Import als Backfill, verlinkt über einen Store-Index

- Status: angenommen
- Datum: 2026-07-24
- Betrifft: `minds-capture`, `minds-store`, `minds-cli`, `minds-reader`
- Ergänzt: ADR-0003 (Hooks statt Transkript-Parsing)

## Kontext

ADR-0003 hat das nachträgliche Transkript-Parsing durch Live-Hooks ersetzt. Das
löst die Erfassung *ab* dem Zeitpunkt, an dem `minds enable` lief — aber nicht
davor. Wer Minds erst spät in einem Repo einrichtet, hätte für die gesamte
bisherige Arbeit keinen Kontext, obwohl die Transkripte der Agents noch auf der
Platte liegen (Claude Code: `~/.claude/projects/<slug>/<session>.jsonl`).

Zugleich soll der erfasste Kontext auf einem geteilten GitLab-Repo ankommen —
nicht nur der winzige Trailer in der Commit-Message, sondern die Sessions selbst.

## Entscheidung 1: Import ist Backfill, nicht Rückkehr

Der Import liest bestehende Agent-Transkripte und baut daraus Sessions. Das ist
**kein Widerruf von ADR-0003**: Der Live-Weg bleibt der Hook. Der Import ist die
einmalige Nachernte für das, was *vor* der Einrichtung geschah — best effort,
ausdrücklich als solches gekennzeichnet.

- Ausgelöst wird er **automatisch von `minds enable`**, im Hintergrund. Kein
  Extra-Befehl.
- Claude Code hat einen echten Reader (Format bekannt). Für Codex, Cursor,
  Gemini und OpenCode sind die Reader Gerüste mit besten Format-Annahmen; wo ein
  Format nicht passt, wird **nichts** importiert (fail-open) und das ehrlich
  gemeldet, statt Falsches abzulegen.

## Entscheidung 2: Verlinkung über einen Store-Index, nie über History-Rewrite

Eine Hook-erfasste Session bekommt ihren Trailer per `amend` an **HEAD** — sicher,
weil nur der frischeste Commit umgeschrieben wird. Ein *importierter* Beitrag
gehört zu **alten** Commits. Den Trailer dort einzutragen hieße, die History ab
dem frühesten Treffer umzuschreiben; auf einem gepushten Repo bricht das jeden
Klon. Das ist ausgeschlossen.

Stattdessen: ein **Store-Index** als Daten neben den Sessions.

```text
refs/minds/context/
  sessions/b3/<hash>.json     # die Session (wie bisher)
  index.json                  # Commit → [ {session, evidence} ]
```

- Der Index wird **nicht** in Commit-Messages geschrieben. Alte Commits bleiben
  Byte für Byte, wie sie sind.
- `minds show`/`why`/`render`/`fsck` lesen **beide** Quellen: den Trailer
  (`Evidence::Observed`, die verbindliche Richtung) und den Index
  (`Evidence::Inferred`, die heuristische). Der Reader zeigt „vermutet" grau.
- Die Zuordnung Session → Commit ist heuristisch: die von der Session
  geschriebenen Dateien, geschnitten mit den Dateien eines Commits im
  Zeitfenster der Session. Deshalb `Inferred` und nicht `Observed` — es ist eine
  gute Vermutung, keine Beobachtung.

Der Index ist damit zugleich die Einlösung des „Store-Index", auf den
`checkpoint` und `edges` schon verwiesen (die symbolische Auflösung Session ↔
Commit). Und weil er in `refs/minds/context` liegt, reist er beim Push des Refs
mit — genau das, was Szenario 2 (Kontext auf GitLab) braucht.

## Entscheidung 3: Sessions erreichen GitLab über eine Push-/Fetch-Refspec

`git push` schickt `refs/minds/context` nicht von selbst mit. `minds enable`
konfiguriert deshalb eine Push-/Fetch-Refspec (`remote.<name>.push` bzw.
`fetch`), sodass der Kontext-Ref beim normalen `git push`/`git fetch` mitreist —
in-repo direkt, beim Child-Repo in dessen Remote. Das schließt die Lücke gegen
Definition-of-Done-Punkt 1 („`minds enable` konfiguriert Backend **+ Refspec**").

## Konsequenzen

**Gut.** Wer Minds spät einrichtet, sieht rückwirkend Kontext. Der Kontext
erreicht GitLab. Nichts davon schreibt Code-History um.

**Preis.** Importierte Links sind Vermutungen und als solche markiert; eine
falsch geratene Zuordnung ist möglich und im Reader als „vermutet" erkennbar.
Die vier Nicht-Claude-Reader sind bis zur Format-Verifikation Platzhalter. Der
Store bekommt mit `index.json` seinen ersten Nachbarn neben `sessions/` — das
Layout hatte das vorgesehen, `id_of_path` ignoriert ihn bereits.
