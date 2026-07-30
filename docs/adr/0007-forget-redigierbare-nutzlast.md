# ADR-0007 — `minds forget`: redigierbare Nutzlast

- Status: angenommen
- Datum: 2026-07-28
- Betrifft: `minds-store`, `minds-cli`, `minds-reader`
- Verwandt: Schicht 2 in `Plan-v0.2.md`

## Kontext

Ein Secret oder personenbezogene Daten in der Git-Historie sind für immer drin —
DSGVO-Löschung und Merkle-Kette schließen sich strukturell aus. Das ist eine der
Bruchstellen, die reines Git nicht auflöst. Minds trennt aber **Referenz** (der
Trailer im Production-Commit) von **Nutzlast** (die Session als content-adressiertes
JSON im Store). Genau diese Trennung macht das Löschen des Inhalts möglich, **ohne**
die Referenz zu brechen — etwas, das ein SaaS-Anbieter aufwändig nachbauen müsste
und Git von Haus aus nicht bietet.

## Entscheidung: Tombstone statt Objekt-Löschung

`minds forget <session> [--reason]` ersetzt die Nutzlast durch einen **Tombstone**
— ein kleines Marker-JSON mit dem Grund. Der content-adressierte Pfad bleibt, aber
sein Inhalt ist weg.

- **`exists` bleibt `true`, `get` meldet `Forgotten`.** Die Referenz löst weiter auf
  — auf einen Tombstone, nicht auf Inhalt. `minds fsck` sieht deshalb **keinen**
  verwaisten Trailer.
- **`why`/`show`/der Reader zeigen „vergessen".** Graceful degradation, kein Fehler.
  Der Reader zählt eine vergessene Session ohnehin schon über seinen `Err(_)`-Zweig
  als „nicht lesbar" — die Seite fällt nicht.
- **Append-only bleibt gewahrt.** Der Tombstone wird als neuer Commit *angehängt*;
  es wird kein Git-Objekt gelöscht. Das passt zur append-only-Linie des Stores —
  Löschen heißt hier *überschreiben*, nicht *entfernen*.

Der Tombstone kommt beim Lesen **vor** dem Hash-Test: er hasht bewusst nicht auf die
Id (der Inhalt ist ersetzt), ist aber kein Defekt (`Corrupt`), sondern eine
Löschung (`Forgotten`).

## Ehrliche Grenzen (bewusst nicht in v0.2)

1. **Historie des Kontext-Refs.** Der alte Blob überlebt in der *Historie* von
   `refs/minds/context`, bis ein History-Rewrite (BFG/filter-repo, oder ein
   Re-Orphan des Refs) ihn tilgt. Der *aktuelle Stand* ist sofort inhaltsfrei — für
   den Reader und jeden regulären Zugriff ist die Session weg.
2. **Gepushte Session-Branches.** Ein bereits in die Forge gepushter
   `minds/session/<hash>`-Branch (Child-Backend) trägt `session.json`/`session.md`
   weiter, bis er dort separat entfernt wird.
3. **Re-Capture.** Wird exakt dieselbe Session erneut erfasst, entsteht wieder
   derselbe content-adressierte Inhalt und überschreibt den Tombstone. `forget`
   zielt auf historische Daten, die nicht neu entstehen.

Ein späteres `minds forget --purge` (History-Rewrite + Branch- und Remote-Bereinigung)
schließt 1 und 2. Der Tombstone ist die vollständige Antwort für den *aktuellen
Stand* und die ehrliche Teil-Antwort für die *Historie*.

## Konsequenzen

Minds kann, was reines Git nicht kann: den Inhalt einer Änderung entfernen und die
Referenz auf ihn behalten. Das ist — neben signierter Attribution und Reviews als
Git-Objekten — ein Baustein, der die These „mehr ins Repo, weniger in die Plattform"
für regulierte Umgebungen konkret macht.
