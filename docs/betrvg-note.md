# Minds-Witness und Betriebsverfassung — Hinweise für Betriebsrat und Arbeitgeber

*Für die Vorbereitung einer Betriebsvereinbarung. Stand: unveröffentlichter
Stand nach v0.4.0 (ADR-0012, Track EA). Dieses Dokument beschreibt, was die
Software tut. Eine Rechtsberatung ist es nicht. Die rechtliche Bewertung
bleibt bei den Betriebsparteien und ihren Beratern.*

Die technischen Einzelheiten mit Belegen im Code stehen in der
[Privacy Overview](privacy-overview.md) (englisch). Was die Prüfstufen
belegen und was nicht, steht in der [Verification Guide](verification-guide.md#witnessed-evidence).

---

## 1. Worum es geht

Minds zeichnet auf, was ein **KI-Agent** (z. B. Claude Code) in einem
Git-Repository getan hat: welcher Auftrag ihn gestartet hat, welche
Werkzeuge er aufgerufen hat, welche Dateien er geschrieben hat. Seit
ADR-0012 gibt es optional einen **Witness** („Zeugen"). Das ist ein eigener
Prozess, der außerhalb der Reichweite des Agenten liegt, sofern er unter
einem eigenen Konto läuft (Profile `container` und `user`). Er nimmt die Ereignisse des
Agenten entgegen, verkettet sie und beobachtet zusätzlich das
Arbeitsverzeichnis des Agenten. So muss sich niemand allein auf den
Selbstbericht des Agenten verlassen.

Gegenstand der Beobachtung ist der **Agent und sein Arbeitsbereich**,
nicht der Mensch, der mit dem Agenten arbeitet.

## 2. Was der Witness beobachtet

| Quelle | Was festgehalten wird |
|---|---|
| Hook-Ereignisse des Agenten | dieselben Ereignisse, die Minds ohne Witness über die Hooks erfasst: Auftrag (Prompt), Antworten, Werkzeugaufrufe, bei schreibenden Werkzeugen auch der geschriebene Dateiinhalt. Vor der Ablage im Repository wird redigiert (fail-closed). Bis zum Checkpoint liegt das Ereignis roh im Journal des Witness (Abschnitt 4, Punkt 6). Erkannt werden Geheimnisse (Tokens, Passwörter, Schlüssel) und E-Mail-Adressen. **Namen, Telefonnummern und andere personenbezogene Angaben** in Prompts oder Werkzeugaufrufen werden nur entfernt, wenn sie in einer Denylist stehen (`deny_pii` in `.minds/redact.json`). Bekannte Erkennungslücken nennt die [Privacy Overview](privacy-overview.md), Abschnitt 6. |
| Datei-Beobachter | je Änderung im Arbeitsverzeichnis: laufende Nummer, Zeitpunkt nach der Uhr des Witness, **repo-relativer Pfad**, **Hash** des Inhalts (blake3) oder der Grund, warum kein Hash vorliegt |
| Intent (optional) | die freigegebene Anforderung (z. B. Ticket-Fassung), redigiert, mit der Signatur der Person, die sie freigegeben hat |
| CI (optional) | Gegenzeichnung durch die Pipeline: Projekt, Pipeline-Nummer, Zeitpunkt; bei Replay die entscheidenden Test-/Build-Befehle und ihre Ergebnisse |

## 3. Was der Witness **nicht** beobachtet

- **Keine Tastatureingaben, keine Bildschirminhalte, keine Mausbewegungen.**
- **Keine Dateiinhalte durch den Datei-Beobachter.** Der Datei-Beobachter
  speichert Pfad und Hash, nie den Inhalt. Was der Agent selbst schreibt,
  steht dagegen redigiert in seinen Werkzeugaufrufen (Abschnitt 2). Für Geheimnis-Dateien (`.env`, Schlüssel, …) entfällt sogar der
  Hash. Dasselbe gilt für Dateien, deren Inhalt die Redaction ändern würde,
  und für Nicht-Text-Dateien.
- **Keine Dateien außerhalb des Repositorys durch den Datei-Beobachter**,
  nichts unter `.git/`, und nichts, was per `.gitignore` oder
  `.git/info/exclude` ausgeschlossen ist. Der Datei-Beobachter führt diese
  Pfade gar nicht auf. Ein Symlink im Repository, der nach außen zeigt,
  erscheint nur mit seinem eigenen Pfad und dem Grund `outside_repo`, ohne
  dass das Ziel gelesen wird. **Aber:** Liest oder schreibt der Agent selbst
  eine solche Datei, stehen Pfad und Inhalt in seinem Werkzeugaufruf, roh im
  Journal des Witness bis zum Checkpoint und redigiert in der Session. Davor
  schützt `.gitignore` nicht.
- **Keine Prozessbeobachtung außerhalb der Hook-Ereignisse, kein
  Netzwerkverkehr, keine anderen Rechner.** Welche Shell-Befehle der Agent
  ausführt, steht in seinen Werkzeugaufrufen (Abschnitt 2). Andere Prozesse
  auf dem Rechner sieht der Witness nicht.
- **Keine eigene Zeiterfassung.** Es gibt keine Anwesenheits-, Aktivitäts-
  oder Leerlauferfassung und keine Messung, wie lange jemand arbeitet. Die
  Ereignisse tragen aber Zeitstempel, auch die Prompts, die ein Mensch
  eingibt (Abschnitt 4, Punkt 2).
- **Keine Zuordnung zu Personen.** Eine Beobachtung enthält keinen
  Benutzernamen, keine Benutzerkennung und keinen Prozess. Der Beobachter
  kann eine Schreiboperation des Agenten nicht von der eines Menschen im
  selben Verzeichnis unterscheiden und versucht es auch nicht.

## 4. Was trotzdem auf Personen beziehbar ist

Diese Punkte gehören offen auf den Tisch. Sie entscheiden darüber, ob und
wie eine Vereinbarung nötig ist.

1. **Git-Identität.** Wie jeder Eintrag unter `refs/minds/` steckt auch ein
   Beobachtungsobjekt in einem Git-Commit. Der trägt die Git-Identität (Name,
   E-Mail) des schreibenden Prozesses und eine Commit-Zeit. Bei Einträgen des
   Witness ist das die Identität aus der Konfiguration des beobachteten
   Repositorys oder, wenn die keine setzt, aus der globalen Git-Konfiguration
   des Witness-Kontos. Das kann der persönliche Name der entwickelnden Person
   sein. Auch die Sessions selbst hängen an Commits mit Autor.
2. **Zeitstempel.** Prompts, Beobachtungen, Sessions und Seals tragen
   Zeitpunkte. Ein Prompt wird von einem Menschen eingegeben, sein
   Zeitstempel zeigt also, wann diese Person mit dem Agenten gearbeitet hat.
   Zusammen mit Session und Commit lässt sich ablesen, **wann** in einem
   Arbeitsbereich gearbeitet wurde.
3. **Prompts.** Der Auftrag an den Agenten ist Text, den ein Mensch
   geschrieben hat. Er wird redigiert gespeichert, wobei Namen nur über die
   Denylist entfernt werden (Abschnitt 2). Das gilt aber schon für Minds ohne
   Witness.
4. **Signaturen.** Wer einen Intent freigibt (`minds-intent`) oder ein Review
   signiert, tut das bewusst mit einem eigenen Schlüssel. Diese Zuordnung
   ist gewollt (Freigabe, Vier-Augen-Prinzip).
5. **Witness-Schlüssel.** Der Name des Witness-Schlüssels lautet
   `minds-witness@<Rechnername>`. Ist der Rechner einer Person zugeordnet,
   ist es auch der Name.
6. **Roh-Journal auf dem Host.** Bis zum Checkpoint liegen die
   unredigierten Hook-Ereignisse im Verzeichnis des Witness (0700/0600). Das
   umfasst Prompts, Werkzeugaufrufe und deren Ergebnisse, also auch Ausgaben
   von Shell-Befehlen und gelesene Dateiinhalte. Dazu kommen absolute Pfade
   wie das Arbeitsverzeichnis, die meist den Benutzernamen enthalten. Wer das
   Witness-Konto auf dem Host kontrolliert, kann sie lesen. Läuft der Witness
   unter dem Konto der entwickelnden Person (Profil `managed` oder von Hand
   gestartet), kann das auch der Agent.

Ein Hinweis zur Einordnung: Nach der Rechtsprechung zu § 87 Abs. 1 Nr. 6
BetrVG kommt es darauf an, ob eine Einrichtung **objektiv geeignet** ist,
Verhalten oder Leistung zu überwachen. Ob das beabsichtigt ist, spielt keine
Rolle. Wegen der Punkte 1 und 2 sollte man diese Eignung nicht verneinen.
Minds beansprucht keine Ausnahme. Die Software ist so gebaut, dass eine
Vereinbarung die Nutzung eng fassen **kann**, ersetzt sie aber nicht.

## 5. „unexplained" ist nie eine Aussage über eine Person

`minds verify` gleicht die Zeilen eines Commits mit dem ab, was Agent und
Witness berichtet haben. Jede Zeile bekommt eine Klasse:

| Klasse | Bedeutung |
|---|---|
| `explained` | Der Witness hat den Stand beobachtet, und ein Werkzeugaufruf des Agenten hat genau diese Bytes geschrieben. |
| `explained (fs only)` | Der Witness hat den Stand beobachtet, ohne passenden Werkzeugaufruf. |
| `reported only` | Keine Beobachtung des Witness; der Bericht des Agenten passt. |
| `unexplained` | Die Belege fehlen oder widersprechen dem committeten Stand. |

`unexplained` heißt: **Die Aufzeichnung erklärt diese Zeile nicht.** Es heißt
nicht, dass jemand etwas falsch gemacht hat. Typische Ursachen sind
nachträgliche Änderungen durch Menschen (ADR-0012 erwartet sie
ausdrücklich: Sie sollen sichtbar sein, nicht verboten), Formatierer,
Code-Generatoren, ein Witness, der kurz nicht lief, oder ein Commit, der
gar nicht aus der Session stammt. Dasselbe gilt für `uncorroborated`,
`out of scope` und `claim not reproduced`: Das sind Aussagen über den
**Bericht des Agenten** und seine Belege, nie über eine Person.

Auch die **Assurance-Stufe** (A0–A3) beschreibt nur, **wer beobachtet hat**
(nur die Hooks des Agenten, ein Witness, zusätzlich CI). Über die Qualität
der Arbeit oder über die Person am Rechner sagt sie nichts.

## 6. Empfohlene Konfiguration für eine Betriebsvereinbarung

Die folgenden Punkte lassen sich technisch umsetzen und in einer
Vereinbarung festhalten:

1. **Zweckbindung.** Zweck ist die Nachvollziehbarkeit von Änderungen durch
   KI-Agenten (Herkunft, Freigabe, Prüfbarkeit für Audits). Ausdrücklich
   ausgeschlossen: Leistungs- und Verhaltenskontrolle, Arbeitszeiterfassung,
   Rankings oder Kennzahlen pro Person.
2. **Getrennter Arbeitsbereich für den Agenten.** Der Witness beobachtet das
   Arbeitsverzeichnis, in dem der Agent arbeitet. Empfohlen ist das Profil
   `container` (`minds enable --witness container`): Der Agent arbeitet in
   einem Container, der Witness läuft außerhalb, und beobachtet wird nur
   dieser Arbeitsbereich. Stand heute ist das Profil auf Linux noch nicht
   abschließend qualifiziert. Auf macOS ist die Aufzeichnung im
   Container-Profil noch abgeschaltet. Wo Menschen selbst arbeiten, sollte der Agent einen
   eigenen Worktree (`git worktree add …`) haben. Dann fallen menschliche
   Änderungen dort gar nicht erst an.
3. **Git-Identität festlegen.** Vorab klären, welche Git-Identität die
   Commits unter `refs/minds/` tragen. Wo möglich, ist eine Funktionskennung
   (Team- oder Dienstkonto) einer persönlichen vorzuziehen.
4. **Witness-Schlüssel auf Dienst-Hosts.** Den Witness auf einem Build- oder
   Dienst-Host betreiben und den Schlüssel nach dem Host benennen, nicht nach
   einer Person.
5. **Keine Auswertung der Zeitstempel nach Personen.** Zeitpunkte dienen
   der Reihenfolge und Prüfung der Belege. Eine Auswertung nach Personen oder
   Arbeitszeiten wird ausgeschlossen.
6. **„unexplained" ist kein Anlass für Personalmaßnahmen** (Abschnitt 5).
   Befunde gehen an das Team, das den Prozess verantwortet, nicht in
   Personalakten.
7. **Zugriff auf das Witness-Verzeichnis.** Nur der Betrieb des Hosts hat
   Zugriff. Es enthält bis zum Checkpoint unredigierte Rohdaten.
8. **Private Dateien ausschließen.** Der Datei-Beobachter beachtet nur
   `.gitignore` und `.git/info/exclude` des Repositorys, **nicht** globale
   Excludes. Private Notizdateien deshalb dort eintragen. Das hält sie aus
   den Beobachtungen heraus, nicht aus den Werkzeugaufrufen, wenn der Agent
   sie liest. Private Dateien gehören deshalb nicht in den Arbeitsbereich des
   Agenten.
9. **Löschung.** Sessions lassen sich mit `minds forget` löschen.
   Beobachtungsobjekte, Intents, Gegenzeichnungen und Replay-Records nicht.
   Beobachtungen und Gegenzeichnungen enthalten keine Inhalte. Ihr Löschen
   ließe sich von einer Manipulation nicht unterscheiden: Gelöschte
   Beobachtungen erklären nichts mehr, und eine gelöschte Gegenzeichnung
   kostet die Stufe A3. Ein Intent enthält den redigierten
   Anforderungstext. Ein Replay-Record enthält die Befehlszeilen der
   entscheidenden Test- und Build-Befehle aus der (redigierten) Session, und
   diese bleiben auch nach `minds forget` der Session stehen. Anforderungen,
   deren Text später löschbar sein muss, nicht als Intent binden.
10. **Aufbewahrung.** Alles unter `refs/minds/` wandert mit jedem Klon und
    Push des Repositorys. Fristen deshalb für das Repository festlegen, nicht
    für ein separates System. Ein separates System gibt es nicht.

## 7. Stand der Umsetzung

Der Witness, der Datei-Beobachter, Intents und CI-Gegenzeichnungen sind
umgesetzt. Die Stufe **A2 witnessed** vergibt `minds verify` aus realem
Material heute noch nicht: Der Witness hält sein Isolationsprofil noch
nicht im signierten Material fest. Für die Beobachtung selbst (Abschnitte
2–4) ändert das nichts.
