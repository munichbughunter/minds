# Minds — Datenschutz-Übersicht

*Für die interne Freigabe beim Pilotpartner. Stand: v0.1.3. Jede Aussage hier
ist aus dem Code belegbar; die bekannten Lücken stehen am Ende — mit
Issue-Nummern, nicht in einer Fußnote.*

---

## 1. Was wird erfasst?

Minds zeichnet **Agent-Sessions** auf (im Piloten: Claude Code) und legt sie
als strukturiertes Objekt ab. Das Objekt enthält:

- **Prompts** im Volltext und die **Text-Antworten** des Agenten. Interne
  „Thinking"-Blöcke des Modells werden nicht übernommen.
- **Tool-Aufrufe mit ihren Argumenten** — darunter Dateipfade, Shell-Kommandos
  im Klartext und bei Schreib-Werkzeugen (`Write`/`Edit`) auch die
  geschriebenen Inhalte. Alles davon durchläuft vor dem Speichern die
  Redaktion (Abschnitt 3).
- **Tool-Ergebnisse nicht**: Was ein Werkzeug zurückgab — etwa der Inhalt
  einer gelesenen Datei — erreicht das gespeicherte Objekt nicht. Das
  geschriebene Datei-Artefakt selbst wird zusätzlich nur als BLAKE3-Hash
  referenziert — für Zugangsdaten-Dateien nicht einmal das.
- **Metadaten**: Agent und Version, Modell, Token-Zähler, Zeitstempel und das
  Arbeitsverzeichnis. Der Verzeichnispfad geht durch die PII-Prüfung; die
  erkennt dort aber nur E-Mail-Formen und Denylist-Begriffe — ein
  gewöhnlicher Benutzername im Pfad (`/Users/<name>/…`) bleibt stehen. Wer
  das nicht will, trägt den Namen in die Denylist der Policy ein
  (`deny_pii` in `.minds/redact.json`).

Nicht Teil des Minds-Objekts, aber wie in jedem Git-Repo vorhanden: die
Git-Identität (`user.name`/`user.email`) an den Commits. Reviews tragen die
E-Mail-Adresse des Reviewers aus der Git-Konfiguration.

## 2. Wo liegen die Daten?

Alles bleibt **im Repository und in dessen `.git`-Verzeichnis** — es gibt
keine Datenbank, keinen Dienst, keine Cloud-Komponente.

| Ort | Inhalt | Schutz |
|---|---|---|
| `.git/minds/journal/…` | **Rohdaten vor der Redaktion**, inklusive Tool-Ergebnissen im Klartext | Dateien 0600; wird nach erfolgreichem Einchecken gelöscht (Lücke: Abschnitt 6) |
| `.git/minds/hook.log` | Diagnosezeilen der Hooks | 0600, rotiert bei 1 MiB; URL-Zugangsdaten werden entfernt |
| `refs/minds/store/<hash>` | die **redigierte** Session (`session.json`) samt Kanten zum Commit | erreicht den Store nur nach der Redaktion (typerzwungen) |
| `refs/minds/sessions/<hex>` | browsbare Kopie (inkl. gerenderter `session.md`) | erscheint beim Push als regulärer Branch `minds/session/<hex>` |
| `refs/minds/context` | Index über die Sessions | wie Store |
| `refs/minds/reviews` | Review-Verdicts: Entscheidung, Reviewer-E-Mail, freie Zusammenfassung | **nicht redigiert** — was der Reviewer in `--summary` schreibt, steht wörtlich im Ref und in der gespiegelten MR-Notiz; Verantwortung liegt beim Reviewer |
| Commit-Trailer | `Minds-Session-Id` / `Minds-Change-Id` | nur Hashes, nie Inhalt |

Dazu kommen ausschließlich **nutzergesteuerte Exporte**: `minds render`,
`minds distill --out`, `minds audit --out` schreiben nur auf expliziten
Aufruf an den angegebenen Ort — und aus bereits redigierten Daten.

## 3. Die Redaktion läuft vor dem Speichern

Ein Secret, das nie in den Store kommt, muss nie gelöscht werden. Darauf ist
der Datenpfad gebaut:

- **Fail-closed, typgetragen:** Der Store nimmt nur Objekte an, die die
  Redaktions-Pipeline durchlaufen haben — das erzwingt das Typsystem, nicht
  eine Konvention. Beide Eingangswege (Live-Erfassung und `minds import`)
  führen durch dieselbe Mauer.
- **Zugangsdaten-Dateien erreichen das gespeicherte Objekt nie.** Wer
  `.env`, `id_rsa`, `credentials.json`, Keystores oder Ähnliches anfasst,
  hinterlässt im Objekt nur `[omitted:secret-file]` samt Regelname. Die
  Grenze dieser Mauer — sie ist pfadbasiert — steht in Abschnitt 6.
- **Detektoren** (alle per Default aktiv): bekannte Token-Formen — darunter
  die GitLab-Familie (`glpat-`, `glcbt-`, …), Anthropic, OpenAI, AWS-Key-IDs,
  Slack, JWT, PEM-Blöcke —, ein Entropie-Auffangnetz, Zuweisungen
  (`PASSWORD=…`), URL-Zugangsdaten in der Userinfo (`https://user:pw@…`),
  Auth-Flags (`curl -u`) und E-Mail-Adressen (PII). Query-Parameter wie
  `?private_token=…` fallen nur, wenn der Wert nach Zugangsdaten aussieht —
  ein rein alphabetischer Wert kann stehen bleiben (in der Diagnose-Senke
  `hook.log` gilt dort eine strengere Regel). Erweiterbar pro Repo über
  `.minds/redact.json` (Denylist/Allowlist).
- **Kaputte Konfiguration stoppt die Ablage.** Ein Tippfehler in
  `redact.json` führt zum Abbruch mit Zeilenangabe — nie zu einem stillen
  Weiter mit weniger Schutz. Die Fehlermeldung zitiert keine Werte.
- Das gespeicherte Objekt enthält über die Redaktion nur **Zähler**
  (wie viele Funde), niemals die gefundenen Werte.

## 4. Was verlässt die Maschine? Nichts von selbst.

Das Binary enthält **keinen HTTP-Stack, keine Telemetrie, keinen
Update-Check**. Es existieren genau zwei Netzpfade, beide vom Nutzer
ausgelöst:

1. **`git push`** — der `pre-push`-Hook überträgt `refs/minds/*` auf genau
   das Remote, auf das der Nutzer ohnehin pusht. Gibt es nichts Neues, wird
   keine Verbindung geöffnet. Es wird nie mit `--force` gepusht. Abschaltbar
   per `git config minds.sync false`.
2. **`minds gitlab mirror`** — nur auf expliziten Aufruf. Übertragen wird ein
   Review-Verdict als Merge-Request-Notiz (Entscheidung, Reviewer,
   Zusammenfassung, Hash) — **keine Session-Inhalte**, aber die
   Zusammenfassung des Reviewers wörtlich und unredigiert (siehe Tabelle in
   Abschnitt 2). Der API-Token kommt ausschließlich aus einer
   Umgebungsvariablen und taucht weder in der Prozessliste noch auf der
   Platte noch in Fehlermeldungen auf.

Der `pre-push`-Hook überträgt dabei ausschließlich Minds-eigene Refs; die
browsbaren Session-Refs erscheinen am Remote als reguläre Branches
`minds/session/<hex>` (siehe Tabelle in Abschnitt 2).

Die Daten liegen damit ausschließlich im Repository des Partners und auf
dessen Forge — beim Hersteller von Minds kommt nichts an.

## 5. Löschen: `minds forget`

`minds forget <session>` ersetzt die Nutzlast an allen drei Ablageorten durch
einen **elternlosen Tombstone-Commit** — der Klartext ist danach auch über
die Ref-Historie (`~1`) nicht mehr erreichbar, und `git rev-list --objects
--all` findet den Payload-Blob nicht mehr (testgesichert). Ein erneutes
Einspielen derselben Session wird abgelehnt; `show`/`why`/`fsck` bleiben
funktionsfähig und benennen die Session als vergessen, statt zu
scheitern. Physisch
entfernt das Objekt erst das nächste `git gc`; bis dahin ist es unerreichbar,
aber vorhanden. Git führt für `refs/minds/*` in der Standard-Konfiguration
kein Reflog.

## 6. Bekannte Lücken — Stand v0.1.3

Die Liste, die eine Freigabe-Entscheidung braucht. Nichts davon ist
verschwiegen, alles ist als Issue öffentlich:

- **Gepushte Sessions erreicht `forget` nicht automatisch**
  ([#102](https://github.com/munichbughunter/minds/issues/102)). Die
  Löschung schreibt die Ref-Kette neu und ist damit kein Fast-Forward;
  `sync` force-pusht grundsätzlich nie. Ein bereits auf die Forge gepushter
  Session-Ref behält dort den Klartext, bis jemand von Hand
  `git push --force` ausführt — und unterliegt danach der Objekt-Retention
  der Plattform (Backups, Mirrors), die außerhalb der Kontrolle von Minds
  liegt. **Empfehlung für den Piloten:** `forget` vor dem ersten Push wirkt
  vollständig; nach einem Push gehört der Force-Push zum Löschprozess dazu.
- **Das Rohdaten-Journal ist das eine Klartext-Fenster.** Zwischen Erfassung
  und Einchecken liegen die unredigierten Rohdaten — einschließlich
  Tool-Ergebnissen wie der Ausgabe von `cat .env` — unter
  `.git/minds/journal/` (Dateien 0600, nur lokal). Im Normalbetrieb endet
  das Fenster mit dem nächsten Commit; scheitert das Einchecken (z. B.
  kaputte `redact.json`), bleibt es offen, bis der Checkpoint nachgeholt
  wird. `forget` erreicht das Journal nicht — eine nie eingecheckte Session
  hat keine Kennung. Dazu
  [#49](https://github.com/munichbughunter/minds/issues/49): Die
  Verzeichnisse über den Ereignisdateien entstehen mit Umask-Rechten — auf
  Mehrbenutzer-Maschinen sind Agent-Namen und Session-Kennungen (nicht die
  Inhalte) für andere lokale Nutzer sichtbar.
- **`minds import` nutzt die eingebaute Standard-Policy, nicht die
  repo-eigene `.minds/redact.json`.** Beim Nachimport alter Transkripte
  greifen die Standard-Detektoren und die Zugangsdaten-Mauer, aber keine
  projektspezifische Denylist (etwa Kundennamen). Für den Piloten:
  Backfill nur nach Rücksprache.
- **Die Zugangsdaten-Mauer ist pfadbasiert.** Sie schlägt bei Pfad-Feldern
  der Tool-Aufrufe an. `cat .env` in einem Shell-Kommando nennt die Datei
  nur beim Namen: Die *Ausgabe* erreicht das gespeicherte Objekt nicht
  (Tool-Ergebnisse werden dort nie abgelegt), liegt aber bis zum Einchecken
  im Rohdaten-Journal (siehe oben). Secrets, die im Kommando selbst stehen,
  fängt die Redaktions-Pipeline. Nicht deutbare oder an der Größengrenze
  abgeschnittene Ereignisse gehen unverändert ins Journal — auch dort greift
  erst die Pipeline vor dem Speichern.
- **Kollisions-Randfall beim Browse-Branch**
  ([#100](https://github.com/munichbughunter/minds/issues/100)). Der
  browsbare Branch trägt nur die ersten 16 Hex-Zeichen der Kennung; bei
  einer Kollision würde `forget` den falschen Browse-Branch mit-tilgen. Die
  Richtung des Fehlers ist über-löschen, nie leaken — der maßgebliche
  Ablageort ist voll adressiert.
- **`hook.log`** durchläuft nicht die volle Redaktions-Pipeline. Es ist auf
  Diagnosezeilen beschränkt (0600, gekürzt, URL-Zugangsdaten entfernt) und
  per Bauform payload-frei; ein eigener Test sichert, dass Transkript-Inhalte
  es nicht erreichen.

## 7. Kurzfassung für die Freigabe

Erfasst werden Prompts, Agent-Antworten und Tool-Aufrufe — redigiert, bevor
irgendetwas den Store erreicht, und ausschließlich lokal im Repository
abgelegt. Es gibt keinen Kanal nach außen außer dem `git push` des Nutzers
und dem explizit aufgerufenen GitLab-Spiegel (nur Review-Verdicts — deren
freie Zusammenfassung verantwortet der Reviewer selbst, sie wird nicht
redigiert).
DSGVO-Löschung existiert und wirkt lokal vollständig; ihre bekannte Grenze
ist der bereits gepushte Stand (#102). Vertrauliche Rückfragen und Befunde
mit Session-Inhalten gehen an den benannten Ansprechpartner, nicht in den
öffentlichen Issue-Tracker.
