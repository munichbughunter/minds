# ADR-0010 — Ein Ref je Session, und ein Push für alle

- Status: angenommen
- Datum: 2026-07-29
- Betrifft: `minds-store`, `minds-git`, `minds-cli`
- Baut auf: ADR-0004 (Import und Store-Index), ADR-0009 (Reviews als Git-Objekte)

## Kontext

Beim Testen fiel auf, dass `git push` mit aktiviertem Minds spürbar länger dauert.
Gemessen gegen gitlab.com: Der `pre-push`-Hook kostete **1,7–1,9 s auf jedem Push**,
unabhängig davon, ob es überhaupt neuen Kontext gab. Ursache war ein zweiter,
serieller `git push` im Hook — ein vollständiger Verbindungsaufbau, nur um in der
Mehrzahl der Fälle „Everything up-to-date" zu hören.

Dahinter lag ein zweites, größeres Problem. Der gesamte Store hing an **einem** Ref
(`refs/minds/context`):

- Jeder Checkpoint schrieb dessen Baum mit *allen* N Sessions neu.
- Jeder Checkpoint las zusätzlich `index.json`, ergänzte eine Zeile und schrieb sie
  ganz zurück.
- Zwei Agents, die gleichzeitig eincheckten, liefen in einen Compare-and-Swap.
- Zwei Maschinen, die beide eincheckten, divergierten beim Push und mussten
  zusammengeführt werden.

Für einen Solo-Entwickler ist das unsichtbar. Für die Agent-Flotte, auf die Minds
zielt, ist es die Bauform, die zuerst bricht.

## Was andere gebaut haben

`entireio/cli` löst denselben Fall im `pre-push`-Hook — **synchron**, aber mit drei
Vermeidungen: Es vergleicht den lokalen Ref gegen den Remote-Tracking-Ref und
verzichtet ohne Unterschied auf jede Netzoperation; es führt eine
flock-geschützte Push-Queue, statt „alles anzubieten"; und es schickt alle fälligen
Refs in *einem* Round-Trip. Bewusst **kein** `ls-remote` im Hook, mit der Begründung,
ein Netz-Round-Trip dort könne einen SSH-Security-Key-Prompt auslösen.

`entireio/cli` hat außerdem genau die Migration hinter sich, um die es hier geht: weg
vom langlebigen Branch `entire/checkpoints/v1`, hin zu einem Ref je Checkpoint unter
`refs/entire/`. Ihre Begründung in `docs/architecture/ref-checkpoint-backend.md`:

> „That branch is a serialization point: every condensation rewrites its tip, every
> push races on one ref, and the whole history travels together."

`entireio/forgemark` misst denselben Gedanken von der Serverseite: Seine
Default-Strategie gibt jedem Agent einen **eigenen Ref**, „so it isolates the server's
per-repo ref-update path".

## Entscheidung 1: Die Nutzlast bekommt einen Ref je Session

Eine Session liegt unter `refs/minds/store/<voller Hash>`, ihr Baum trägt
`session.json`. Damit:

- **Schreiben ist O(1).** Der Baum hat einen Eintrag, egal wie groß der Store ist.
- **Kein Wettlauf.** Der Ref-Name *ist* der Inhalts-Hash. Zwei Agents mit
  verschiedenen Sessions fassen verschiedene Refs an; zwei mit derselben Session
  schreiben denselben Baum, und der zweite Lauf ist ein No-op.
- **Kein divergenter Push.** Ein Session-Ref entsteht genau einmal.

`refs/minds/store/` und nicht `refs/minds/sessions/`: Letzteres trägt weiterhin die
*browsbaren* Branches des Child-Backends (gekürzter Hash, mit `session.md`). Nutzlast
und Ansicht sind zwei Dinge.

Empirisch geprüft und damit eine frühere Annahme im Code widerlegt: **GitLab nimmt
Refs außerhalb von `refs/heads/*` an** — `refs/minds/context` liegt dort. Der neue
Namensraum kann deshalb identisch gepusht werden. Weil er nicht unter `refs/heads/`
liegt, kann eine Forge ihn weder als Default-Branch wählen noch in die Branch-Liste
des Nutzers stellen.

## Entscheidung 2: Die Kanten liegen bei ihrer Session

Statt einer gemeinsamen `index.json` trägt jeder Session-Ref eine `links.json` mit
*seinem* Anteil am Commit-Index. Der Gesamtindex ist die Vereinigung über alle
Session-Refs; er wird gelesen, nie geschrieben.

Der Tausch ist bewusst: Der heiße Pfad (Checkpoint) wird O(1) und konfliktfrei, die
kalten Pfade (`show`, `why`, `fsck`, `render`) lesen dafür N kleine Blobs statt einen
großen. Ein Repo, das nur eincheckt, hat danach **keinen einzigen gemeinsam
beschriebenen Ref** mehr.

Bestandsrepos und der Import schreiben weiterhin eine `index.json`; der Lesepfad
kennt beide Orte und vereinigt sie. Ebenso liest `get` die Nutzlast erst am
Session-Ref und dann im alten Kontext-Baum — niemand muss migrieren.

`forget` tilgt an **beiden** Orten. Nur einen zu treffen wäre die schlimmste Sorte
Fehler, die dieses Kommando machen kann: Es meldete „vergessen", und der Klartext
stünde weiter im anderen Baum.

## Entscheidung 3: `minds sync` statt Git-Kommandos im Hook

Der Hook lautet nur noch `minds sync --remote "$1" || true`. Das Binary:

1. **entscheidet ohne Netz**, ob etwas fällig ist — über eigene Tracking-Refs unter
   `refs/minds/remotes/<remote>/*`. Für `refs/heads/*` führt Git selbst Buch, für
   `refs/minds/*` nicht; also führen wir es selbst, als Refs und nicht als Datei
   daneben. Geht einer verloren, ist die Folge ein überflüssiger, idempotenter Push.
2. schickt **alle** fälligen Refs in *einem* `git push --no-verify --porcelain`.
3. pusht **nie** mit `--force`. Wird der Review-Log abgewiesen, wird der fremde Stand
   geholt und **vereinigt** (`ReviewStore::merge_from`, konfliktfrei, weil der Pfad
   eines Eintrags sein Inhalts-Hash ist) und erneut gepusht — wieder fast-forward.
4. meldet Fortschritt auf stderr. Ein Push, der zehn Sekunden schweigt, sieht aus wie
   ein hängender Push.

**Synchron, nicht abgelöst.** Ein Hintergrundprozess wäre schneller, hat aber kein
Terminal — Credential-Helper, SSH-Passphrase und Security-Key-Touch brauchen genau
das. Ein Sync, der im Hintergrund still an der Authentifizierung scheitert, ist
schlimmer als einer, der zwei Sekunden braucht und es sagt.

Der Fetch-Refspec unterscheidet zwei Fälle: Die Nutzlast wird direkt geholt
(content-adressiert, kann nichts überschreiben), **Reviews** landen im
Tracking-Namensraum und überschreiben den lokalen Log nie — ein `git fetch` darf ein
lokal entstandenes, noch nicht gepushtes Verdict nicht wegräumen. Zusammengeführt
wird beim nächsten `minds sync`.

## Ergebnis (gemessen)

| Vorgang | vorher | nachher |
|---|---|---|
| `pre-push`, nichts Neues | 1,86 s | **0,02 s** |
| `pre-push`, neuer Kontext | 1,86 s | 2,03 s (eine Verbindung) |
| Kontext + Reviews | zwei Verbindungen | **eine** |
| Checkpoint schreibt | Baum mit N Einträgen + Index | ein Baum mit zwei Einträgen |
| gemeinsam beschriebene Refs | 1 | **0** |

Nebenbei behoben: `refs/minds/reviews` wurde bisher **nie** gepusht — Schicht 3 war
damit nicht teamfähig.

## Konsequenzen

- Wer von einer älteren Version kommt, muss nichts tun: Gelesen wird an beiden Orten.
  Neu geschrieben wird nur am neuen. Ein Repo konvergiert also von selbst, ohne dass
  eine Migration je läuft.
- Die kalten Pfade werden O(N) in den Session-Refs. Bei einigen tausend Sessions ist
  das ein Ref-Scan und N kleine Blob-Reads. Sollte das stören, ist ein zwischen-
  gespeicherter Index eine *Ergänzung* — abgeleitet, jederzeit neu baubar, und damit
  kein Rückschritt zu einem gemeinsam beschriebenen Ref.
- Der Fetch zieht jetzt einen Glob (`refs/minds/store/*`). Wer nur eine einzelne
  Session braucht, kann sie einzeln fetchen — das ist der Vorteil, den ein Ref je
  Session mitbringt, und die Grundlage für ein späteres On-Demand-Laden.
