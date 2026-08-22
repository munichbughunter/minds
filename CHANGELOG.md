# Changelog

Alle nennenswerten Änderungen an Minds stehen hier.

Das Format folgt [Keep a Changelog](https://keepachangelog.com/de/1.1.0/), die
Versionierung [Semantic Versioning](https://semver.org/lang/de/).

> **Noch keine 1.0.** Solange die Führungsziffer `0` ist, gibt es keine
> Kompatibilitätszusage: Jede MINOR-Version (`0.1` → `0.2`) darf die CLI-Oberfläche
> und das Store-Layout brechen. PATCH-Versionen (`0.1.0` → `0.1.1`) enthalten nur
> Korrekturen.
>
> **Davon getrennt zu betrachten ist `schema_version`** in den abgelegten Objekten
> (Session, Review). Die Version des Binaries versioniert die *Oberfläche*, das
> Schema versioniert das *gespeicherte Objekt* — und ein Objekt lebt so lange wie das
> Repo. Es gilt: ein neueres Binary liest alle älteren Schema-Versionen; das Schema
> steigt nur bei einer brechenden Änderung an der Nutzlast, nie bei einem zusätzlichen
> Feld.

## [Unreleased]

## [0.1.3] — 2026-08-22 — „Unsichtbar, auch unter Last"

*Das Release nach dem ersten manuellen Testlauf. Die Befunde von dort haben
eine Gemeinsamkeit: Nichts davon war ein Leck, aber alles war eine Zusage, die
nur hielt, solange jemand daran dachte — die Redaktion an der Quelle statt an
der Senke, die Terminal-Härtung, die es nur für das Log gab, und ein Push, der
auf einen zweiten Transport wartete. Jetzt sind die Zusagen strukturell:
`hook.log` und die Anzeige entschärfen **dort, wo geschrieben wird**, und der
Kontext-Transport hängt nicht mehr am Push des Nutzers.*

Der Preis des letzten Punkts steht unten unter „Bekannte Einschränkungen":
Der Kontext kommt Sekunden nach dem Push an, nicht mehr garantiert mit ihm.

### Geändert

- **`git push` wartet nicht mehr auf den Kontext-Transport**
  ([#85](https://github.com/munichbughunter/minds/issues/85)). Mit fälligen
  minds-Refs öffnete der pre-push-Hook vor dem Push des Nutzers einen zweiten
  vollen Transport — gegen GitHub ~1,5 s gemessen, fast ausschließlich
  Verbindungsaufbau. Jetzt ruft der Hook `minds sync --detach`: Die Planung
  bleibt im Vordergrund (lokal, ~0,02 s), den Push übernimmt ein losgelöster
  Prozess ohne Terminal. **Der Kontext kommt damit Sekunden nach dem Push am
  Remote an, nicht mehr garantiert mit ihm** — wer das braucht, ruft
  `minds sync` vor dem Push von Hand auf. Der Hintergrundprozess läuft in
  eigener Session ohne Terminal; was darüber nicht geht — die SSH-Passphrase
  eines Schlüssels ohne Agent, der Touch eines Security-Keys —, lässt ihn
  scheitern, und er hinterlässt neben dem Log-Eintrag einen Marker: Der
  nächste Push läuft dann wieder synchron im Vordergrund, wo der Fehler
  sichtbar ist und die Anmeldung gelingen kann. Ohne fällige Refs bleibt es
  bei null zusätzlichen Kosten.

### Sicherheit

- **`minds show`/`minds why` entschärfen gespeicherten Fremdtext vor der
  Terminal-Ausgabe** ([#116](https://github.com/munichbughunter/minds/issues/116)).
  Die render-Schicht druckte Prompt, Agent- und Modell-Namen, Constraints,
  Dateipfade und vor allem die Kanten-Endpunkte (`edges[].to`, wörtlich aus
  dem Hook-Payload der Gegenseite) roh — die Redaktion aus #35 sucht
  Geheimnisse, keine Steuerzeichen, und so erreichten ANSI-Sequenzen, Bidi-
  und Zero-Width-Zeichen das Terminal des Lesers unverändert. Jetzt geht jeder
  fremde Wert an der Senke durch dieselbe Härtung wie `hook.log`
  (`text::sanitize`, Pfade über `sanitize_path`); welche Pfade entschärft werden
  und welche bewusst nicht, steht in der Modul-Doku von `render.rs`. Der volle
  Prompt (`--full`) behält dabei seine Zeilen und wird unter dem Ast
  eingerückt, statt den Baum zu zerreißen.
- **`hook.log` redigiert Zugangsdaten an der Senke, nicht mehr nur an der
  Quelle** ([#92](https://github.com/munichbughunter/minds/issues/92)). Bisher
  rief allein `minds sync` die URL-Redaktion auf, bevor ein Fehlertext ins Log
  ging; die übrigen Schreibstellen (`checkpoint`, `hook`, `brief`,
  `prepare-commit-msg`, die Parse-Fehler) verließen sich darauf, dass ihr Text
  keine Remote-URL trägt — eine Zusage, die nur hielt, solange jeder künftige
  Aufrufer daran dachte. Jetzt läuft jede Zeile durch dieselbe Redaktion, bevor
  sie in die Datei kommt, und ein Test beweist das für jede Quelle ohne Zutun
  des Autors. Redigiert wird **vor** dem Kürzen und immer über den ganzen
  Text, damit kein halbiertes Token — und kein PEM-Schlüssel ohne seinen
  `-----END`-Marker — die Formerkennung unterläuft; eine Meldung jenseits von
  256 Ki Zeichen wird deshalb nicht angeschnitten, sondern als Ganzes durch
  einen Marker ersetzt.
- **Signieren legt keine vorhersagbaren, welt-lesbaren Dateien mehr in /tmp ab**
  ([#26](https://github.com/munichbughunter/minds/issues/26)). Beim
  Signieren/Verifizieren landeten Payloads und Signaturen mit vorhersagbarem
  Namen (`minds-sign-<pid>-<nanos>`) und Default-Rechten (0644) direkt in
  `/tmp` — auf Mehrbenutzer-Systemen welt-lesbar plus Symlink-Race, obwohl
  Attestation-Payloads Intent-Text enthalten können, also genau die Daten, die
  die Redaction sonst schützt. Jetzt entsteht alles in einem privaten
  Temp-Verzeichnis (0700, zufälliger Name) mit Dateien im Modus 0600 und
  `create_new`-Semantik. Der Verfügbarkeits-Check ruft `ssh-keygen` außerdem
  nicht mehr argumentlos auf (das startete den interaktiven Keygen-Modus),
  sondern prüft nicht-interaktiv, ob `-Y sign` unterstützt wird. Die ssh-sig-
  Logik liegt dafür als eigene Crate `minds-attest` vor, damit CLI,
  `minds-gitlab` und ein künftiger CI-Verifier dasselbe Vertrauensmodell
  teilen, statt es zu duplizieren.
- **Der Reconcile-Zweig von `minds sync` hört nicht mehr auf Server-Text**
  ([#71](https://github.com/munichbughunter/minds/issues/71)). Ob ein
  fehlgeschlagener Push eine Ref-Divergenz war — der einzige Fall, in dem
  `sync` fremde Review-Stände holt und in den lokalen Store vereinigt —,
  entschied eine Substring-Suche im vermischten stdout+stderr von `git push`;
  ein Remote konnte den Zweig mit einem „rejected" in einer beliebigen
  `remote:`-Zeile öffnen. Jetzt fällt die Entscheidung auf der
  `--porcelain`-Struktur von stdout: Nur eine von git selbst im lokalen
  Vergleich festgestellte Abweisung (`[rejected]` mit
  non-fast-forward/fetch first/stale info) gilt als Divergenz; ein
  `[remote rejected]` — dessen Grund wörtlich vom Server stammt, etwa aus
  einem pre-receive-Hook — nicht mehr. stdout und stderr werden dabei nicht
  mehr vermischt; die Fehlermeldung kommt von stderr, Zugangsdaten werden
  weiterhin entfernt.
- **Die DSGVO-Löschung eines bereits gepushten Session-Refs erreicht jetzt die
  Forge** ([#102](https://github.com/munichbughunter/minds/issues/102)). Seit
  die Tombstones elternlos sind (#14), war ein getilgter Ref kein Fast-Forward
  mehr; `minds sync` (das nie mit `--force` pusht) ließ die Forge den Klartext
  als aktuelle, browsbare Ref-Spitze behalten — lokal getilgt, remote sichtbar,
  mit Erfolgsmeldung. Jetzt löscht `forget` die Push-Buchhaltung
  (`refs/minds/remotes/*`) der getilgten Session-Refs, statt sie auf den
  Tombstone umzusetzen, und `sync` überträgt genau diese Refs mit einer
  `+`-Refspec: nur wenn der zu pushende Stand nachweislich ein Tombstone an
  einem session-exklusiven Ref ist (fail-closed geprüft am Inhalt) und der
  zuletzt gepushte Stand keiner war — nie Klartext über Klartext, jeder andere
  Ref bleibt strikt fast-forward und echte Divergenz weiterhin zurückgestellt.
  Zur Verifikation gehört der Nachweis der Elternlosigkeit — ein Tombstone mit
  Historie reiste sonst samt Inhalt. Die Übertragung wird beim Push gemeldet
  und in `hook.log` vermerkt; weist die Forge den Force-Push ab (Protected
  Branch, Server-Hook), wird auch **das** bei jedem Lauf gemeldet, bis die
  Löschung durch ist — statt stumm Erfolg zu suggerieren. `forget` nimmt jetzt
  dasselbe Lock wie `sync`, damit ein laufender Push den eben gelöschten
  Tracking-Ref nicht am Klartext neu erschafft, und verspricht den Force-Push
  nur noch für die Orte, die ihn bekommen. Die `--force`-Zusage in
  `agent-help`, `--help` und der Datenschutz-Übersicht ist entsprechend
  präzisiert. Bewusst unverändert: Der geteilte Kontext-Ref eines Bestandsrepos
  wird nie force-gepusht (er trägt auch die übrigen Sessions), und der
  Store-Ref-Tombstone behält wie seit #14 seine `links.json` (Kanten
  `commit → Session`, keine Nutzlast) — sie reist mit dem Erasure-Push mit.
- **Der Backfill aus `minds enable` schreibt in `hook.log`, nicht mehr roh in
  `import.log` daneben** ([#69](https://github.com/munichbughunter/minds/issues/69)).
  Der Hintergrund-Import hängte stdout und stderr unverändert an eine zweite
  Datei im selben Verzeichnis — ohne die Zusagen, die `hook.log` seit #10 hat
  und auf die `fsck` und `docs/fuer-tester.md` verweisen: Steuerzeichen wurden
  durchgereicht, die Datei wuchs unbegrenzt und lag mit Umask-Rechten da. Jetzt
  ist der Backfill ein Hook-Pfad wie `checkpoint` (`Source::Import`): Seine
  Fehler gehen entschärft, gedeckelt, rotiert und mit 0600 in dieselbe Datei,
  `fsck` verweist darauf, ein Panic hinterlässt nur seinen Ort (der Prozess
  hält die rohen Transkripte im Speicher), und `import.log` entsteht nicht
  mehr — eine vorhandene aus älteren Ständen räumt `enable` weg. Dabei
  aufgefallen: Ein Transkript ohne Leserechte war bisher nur eine *Notiz*
  neben „kein Importer" und damit ebenfalls stumm; jetzt ist es ein Befund
  und steht im Log. Der Gutfall bleibt still — sonst zeigte `fsck` nach
  jedem `enable` einen Hinweis auf eine Datei, in der nichts Behebbares
  steht.

- **Das Rohdaten-Journal hält sein Rechte-Versprechen auf jeder Ebene**
  ([#49](https://github.com/munichbughunter/minds/issues/49)). Die
  Ereignisdateien waren 0600, aber `create_dir_all` legte die Verzeichnisse
  darüber mit Umask-Rechten an — andere lokale Nutzer sahen Agentnamen und
  Session-Kennungen. Jetzt entsteht jede Journal-Ebene direkt mit 0700
  (kein Umask-Fenster), jeder Append heilt Bestandsjournale mit — testbelegt
  — und der `.next`-Hinweis liegt mit 0600. Nach dem `rename` eines Events
  wird auch das **Verzeichnis** synchronisiert — ohne das konnte ein
  Stromausfall ein Event verschwinden lassen, obwohl der Hook Erfolg
  gemeldet hatte (Kostenabwägung im Code; Dateisysteme ohne
  Verzeichnis-fsync bleiben funktionsfähig). Zwei Schärfungen aus den
  Reviews: Gehärtet wird ab `journal/`, nicht ab `minds/` — sonst entzöge
  die Härtung in einem gruppen-geteilten Repo dem zweiten Nutzer Lock und
  Fehlerkanal —, und eine per Symlink umgelenkte Journal-Ebene wird
  verweigert statt beschrieben oder chmodded, dieselbe Invariante, die das
  `hook.log` bereits verteidigt.

- **Signierbare Payloads sind nicht mehr über Freitextfelder fälschbar**
  ([#12](https://github.com/munichbughunter/minds/issues/12)). Die
  zeilenbasierten Klartexte, über die `minds sign` und `review --sign`
  signieren, bauten sich aus unvalidierten Feldern — ein `reviewer` von
  `anna@example.org\ndecision=approve` erzeugte einen Payload mit zwei
  `decision=`-Zeilen: Die menschenlesbare Zusage war fälschbar, obwohl der
  Hash korrekt bindet. Beide Payload-Funktionen sind jetzt fail-closed und
  lehnen neben allen Zeilenumbrüchen (inkl. NEL) auch die Versteck- und
  Umdeutungszeichen ab (Bidi-Overrides, Unicode-Tags, Zero-Width, BOM —
  dasselbe Sentinel-Prädikat wie die Log-Entschärfung, NFD-Namen wie
  `Müller` in Zerlegungsform bleiben gültig). Die Zeilenzahl ist als
  Invariante testfixiert. Der Fehler benennt das Feld und zitiert nie den
  Wert. Dazu aus den Reviews: `minds audit` degradiert einen betroffenen
  Eintrag sichtbar (`unsignable`) statt repo-weit abzubrechen, die
  `reviews`-Statuszeile meldet „Signatur nicht prüfbar" statt „gültig", und
  `gitlab webhook --write` lehnt eine Netz-Nutzlast ab, deren Felder keinen
  signierbaren Payload ergäben — vergifteter Bestand entsteht gar nicht
  erst.

### Hinzugefügt

- **Integrationstests für die Kommandos des Pilot-Zuschnitts**
  (Teil von [#51](https://github.com/munichbughunter/minds/issues/51)).
  Bewusst nicht alle zwölf ungedeckten Kommandos — genau die Pfade, die beim
  Pilot-Partner nicht selbst debuggbar sind: `prepare-commit-msg` über einen
  echten `git commit` (inklusive Amend, der weder eine neue noch eine zweite
  Change-Id erzeugen darf), `blame`/`recap`/`search` je mit Happy-Path und
  benanntem Fehlerfall, das `brief --hook`-JSON-Envelope als Vertragsfläche
  (Schema **und** Session-Inhalt — Claude Code parst es stumm) und
  `gitlab mirror` über die ganze CLI-Strecke gegen einen lokalen HTTP-Stub
  mit echtem `curl`: Flags landen im richtigen URL-Segment, die Note im Body,
  der Token als Header — und eine fehlende Token-Variable wird beim Namen
  genannt. Der Rest von #51 (u. a. `verify`, `gitlab webhook`, `distill`)
  bleibt offen und im Issue dokumentiert.

- **Pilot-Leitfaden und Datenschutz-Übersicht**
  (`docs/pilot-leitfaden.md`, `docs/datenschutz-uebersicht.md`). Die eine
  Seite für die interne Freigabe beim Pilotpartner und der Leitfaden für den
  Pilot-Zuschnitt — jede belastbare Zusage gegen den Code verifiziert, die
  bekannten Lücken mit Issue-Nummern benannt statt versteckt.

### Entfernt

- **Die eingecheckte `site/` ist raus aus dem Repository**
  ([#60](https://github.com/munichbughunter/minds/issues/60)). 58 generierte
  HTML-Dateien — die Default-Ausgabe von `minds render` — veralteten mit
  jedem Commit gegenüber dem Code und widersprachen der eigenen Definition
  des Readers als „bei jedem Lauf neu gebaut, zustandslos". `site/` steht
  jetzt in der `.gitignore`; `minds render` erzeugt die Ausgabe weiterhin
  lokal. Die übrigen Streu-Dateien aus dem Issue (`hello.txt`, `test.txt`,
  `retest_szenario_1.txt`, `test-szenario-3`) waren bereits untracked und
  ignoriert.

### Behoben

- **Nebenläufige Kanten-Schreiber verlieren einander nicht mehr**
  ([#4](https://github.com/munichbughunter/minds/issues/4)). Zwei
  gleichzeitige Checkpoints derselben Session — der `post-commit`-Hook und
  ein Aufruf von Hand reichen — konnten eine Kante `Commit → Session`
  still verlieren: `why`/`show` fanden die Session über diesen Commit
  danach nicht mehr, obwohl beide Aufrufe Erfolg meldeten. Der Fix ging
  tiefer als das Issue: `GitStore::link` mergte außerhalb der
  CAS-Schleife (das beschriebene Lost Update), aber der Test gegen echte
  Threads zeigte, dass auch der Compare-and-Swap darunter nicht hielt —
  gix (0.85) verifiziert den Erwartungswert einer Ref-Transaktion gegen
  einen **vor** dem Lock gelesenen Stand, zwei Schreiber bekamen beide
  `Ok`. Jetzt teilen sich Lesen, Mergen und Schreiben einen beobachteten
  Commit (`update_blob_in_ref`), und der Ref-Wechsel verifiziert unter
  einem eigenen, prozessübergreifenden Lock — Staleness wird zu
  `RefRaced` und einem frischen Versuch, nie zu stillem Überschreiben.
  Das schützt denselben Pfad auch für `put` und `forget`. Die
  Wiederholungsgrenze steigt von drei auf zehn Versuche, weil verlorene
  Wettläufe seit der echten Durchsetzung der Normalfall unter Last sind.
  Drei Befunde aus den Reviews im selben Commit: Das Lock lebt im
  **geteilten** Git-Verzeichnis (`common_dir`), damit verlinkte Worktrees
  dasselbe nehmen — sonst wäre die Serialisierung genau in der Topologie
  wirkungslos, für die es sie braucht; eine nach hartem Prozessende
  liegengebliebene Lock-Datei nennt in der Fehlermeldung ihren Pfad samt
  Abhilfe; und eine unlesbare `links.json` wird beim Schreiben nicht mehr
  still durch eine frische Liste ersetzt, sondern scheitert benannt —
  Lesen bleibt tolerant.

- **`gitlab mirror` sendet den Body wieder — Header und Body getrennt**
  ([#7](https://github.com/munichbughunter/minds/issues/7)). Token-Header
  (`--header @-`) und JSON-Body (`--data-binary @-`) teilten sich dasselbe
  stdin — curl liest `-H @-` aber bis EOF: Der POST ging mit leerem Body raus
  (GitLab: „body is missing"), das Spiegeln funktionierte schlicht nicht, und
  der Notiz-Inhalt wanderte als kaputter HTTP-Header über die Leitung. Der
  Body geht jetzt über eine kurzlebige Tempdatei (unter Unix 0600), stdin
  gehört allein dem Token — der weiterhin nie in der Argumentliste und nie
  auf der Platte steht. Dazu zitiert die Fehlermeldung jetzt die
  Server-Antwort (`--fail-with-body` legt GitLabs `message` auf stdout,
  gekürzt auf 500 Zeichen) — vorher blieb die eigentliche Ursache, etwa
  „404 Project Not Found", unsichtbar. Vier neue Tests fahren echtes `curl`
  gegen einen lokalen HTTP-Stub und sichern den Pfad erstmals ab, darunter
  die Invariante „der Token taucht in keiner Fehlermeldung auf".

### Bekannte Einschränkungen

*Die Liste des Übergabestands. Sie ersetzt für den heutigen Stand die
älteren Listen unter v0.1.1 und v0.1.0 — die bleiben historisch stehen,
gelesen als „gilt heute" wird nur diese hier.*

- **Verlinkte Git-Worktrees:** Die Erfassung und `fsck` stimmen dort, aber
  `minds show` und `minds why` zeigen den Commit des Hauptbaums
  ([#20](https://github.com/munichbughunter/minds/issues/20)).
- **Kein natives Windows-Binary** — der unterstützte Weg ist WSL.
- **Tool-Ebene vollständig nur für Claude Code.** Andere Agents (Codex,
  Cursor, Gemini, opencode): Der Prompt wird erfasst, die Tool- und
  Datei-Ebene noch nicht gedeutet.
- **Die Review-Schicht braucht zwei Personen auf einem Repo** — allein
  bleiben Erfassung, `why` und `recall` testbar, Reviews nicht.
- **Rund um `forget` bleiben zwei Randfälle** — die Tilgung erreicht seit
  0.1.3 auch bereits gepushte Refs (#102, oben unter Sicherheit), offen sind
  der Kollisions-Randfall des Browse-Branches
  ([#100](https://github.com/munichbughunter/minds/issues/100), Richtung:
  über-tilgen, kein Leck) und das Rohdaten-Fenster im Journal
  ([#49](https://github.com/munichbughunter/minds/issues/49)) — Details in
  der [Datenschutz-Übersicht](docs/datenschutz-uebersicht.md).
- **`minds import` nutzt die eingebaute Standard-Policy**, nicht die
  repo-eigene `.minds/redact.json` — projektspezifische Denylists greifen
  beim Backfill nicht.
- **Der Kontext kommt Sekunden nach dem Push am Remote an, nicht mehr
  garantiert mit ihm** ([#85](https://github.com/munichbughunter/minds/issues/85))
  — der Transport läuft seit 0.1.3 im Hintergrund. Eine Pipeline, die dem
  Push unmittelbar folgt, kann frische Reviews knapp verpassen; wer die
  Garantie braucht, ruft `minds sync` vor dem Push von Hand auf.
- **`minds gitlab webhook` hat keine Token-Verifikation**
  ([#8](https://github.com/munichbughunter/minds/issues/8)) — als lokales
  Kommando enthalten (Default: Dry-Run), aber nicht verwenden; das
  CI-Gate (`fsck --require-review`) wird als Pipeline-Tor noch nicht
  empfohlen.
- **Kein Self-Update** — Versionswechsel laufen über `install.sh` mit
  `MINDS_VERSION`.

## [0.1.2] — 2026-08-12 — „Die Mauer hält — vorne und hinten"

*Das Release, das über die Freigabe entscheidet — nicht über die Begeisterung.
Die Redaktion ist das eine Versprechen, bei dem ein Fehlschlag nicht ärgerlich,
sondern schädlich ist; und was doch durchrutscht, muss entfernbar bleiben.
Deshalb zwei Hälften: **vorne** die Mauer — kein Geheimnis erreicht den Store,
auf keinem der beiden Eingangswege —, **hinten** die Tilgung — `minds forget`
hält, was die erste README-Seite verspricht, und lässt die Rückführung danach
weiterarbeiten.*

Fast jeder Fix hier ist im Code- oder Security-Review um eigene Folge-Befunde
gewachsen — dreimal waren es Regressionen, die erst durch den Fix entstanden
wären. Wo es zählte, wurde zusätzlich empirisch gegen das gebaute Binary
gemessen, nicht nur gegen die Testsuite.

### Sicherheit

**Vorne — die Redaktion:**

- **Kein Panic mehr bei Multibyte-Zeichen im Wert**
  ([#1](https://github.com/munichbughunter/minds/issues/1)).
  `PASSWORD=hunter€2` stürzte in der Windows-Pfad-Erkennung ab, weil mitten im
  UTF-8-Zeichen byte-indiziert wurde. Die Prüfung arbeitet jetzt auf
  char-Grenzen.

- **`curl -u user:pass` wird redigiert**
  ([#2](https://github.com/munichbughunter/minds/issues/2)). Neuer
  Short-Flag-Detektor für Authentifizierungs-Flags, in beiden Formen
  (`-u user:pass` und `-uuser:pass`), als eigener Schalter `short_flags` in
  der Policy — per Default an.

- **JSON-escapte Secrets leaken nicht mehr teilweise**
  ([#3](https://github.com/munichbughunter/minds/issues/3)). Tool-Argumente
  liegen im Envelope immer JSON-serialisiert vor — genau die Eingabeklasse,
  die die Muster nicht abdeckten: Ein escaptes Quote im Wert ließ `ter2` aus
  `hun\"ter2` stehen, ein PEM mit literalem `\n` matchte gar nicht. Die
  Reviews fanden vier Folge-Befunde derselben Klasse; drei davon wären erst
  durch den Fix entstanden — unter anderem kippte die Pfad-Ausnahme von einem
  Teil- auf ein Total-Leck.

- **Die Token-Regeln kennen jetzt die wahrscheinlichsten Formen**
  ([#33](https://github.com/munichbughunter/minds/issues/33)). Anthropic
  (`sk-ant-`), OpenAI (`sk-proj-`, `sk-svcacct-`, `sk-admin-`), SendGrid und
  die **GitLab-Familie** (`glcbt-`, `glptt-`, `glft-`, `glimt-`, `glagent-`,
  `glsoat-`) — Letztere fehlte fast vollständig, in einem Produkt, das auf
  GitLab zielt. Zwei gemessene Funde dazu: Die nicht-überlappende
  Vorfilter-Suche ließ ausgerechnet den Anthropic-Key komplett durchrutschen
  (`sk-` gewann gegen `sk-ant-`), und die Längen-Caps lagen unter der Realität,
  sodass Token-Schwänze stehen blieben. Gegen Fehlalarme verlangen die neuen
  Regeln Struktur (Typ-Sektion, Ziffern-Sektion, Wortanfang) — Prosa *über*
  Keys bleibt lesbar.

- **Tokens in URL-Queries erreichen das hook.log nicht mehr**
  ([#73](https://github.com/munichbughunter/minds/issues/73)). Die bei GitLab
  dokumentierte Form `?private_token=…` hat kein `@` und kam wörtlich in eine
  Datei, die nie gelöscht wird. Die Diagnose-Senke wendet die Redaction-Policy
  jetzt gezielt je `name=wert`-Paar an — plus den formbasierten
  Token-Detektor über den ganzen Text —, sodass Host und Fehlerursache lesbar
  bleiben. `token` zählt in dieser Senke zum Strict-Tier, damit auch
  präfixlose Tokens (self-hosted GitLab vor 16.x) fallen.

- **Die secretfile-Mauer kennt die gängigen Zugangsdaten-Dateien**
  ([#34](https://github.com/munichbughunter/minds/issues/34)).
  GCP-Service-Accounts, `credentials.json`, FIDO-SSH-Keys, `htpasswd`,
  `.netrc.gpg`, die Verzeichnisse `gcloud` und `/etc/wireguard/` — und
  Dateien, für die die Mauer die **einzige** Schicht ist, weil kein Detektor
  ihre Inhalte fangen kann: Ansible-Vault-Passwortdateien, `.dockercfg`,
  `rclone.conf`, `.s3cfg`, `.boto`. Der schwerste Befund kam aus dem Review
  und betraf den eigenen Fix: Die Segmentgrenzen-Prüfung ließ dekorierte
  Varianten (`credentials.bak`, `config-prod`) durchfallen — byte-gleiche
  Zugangsdaten. Die Regel ist umgedreht: Der Rest hinter dem Muster
  disqualifiziert nur noch, wenn er die Datei zu etwas anderem macht.

- **Die Redaktion prüft jetzt wirklich jedes Textfeld des Envelopes**
  ([#35](https://github.com/munichbughunter/minds/issues/35)).
  Ausgenommen waren bislang die Zeitstempel (`turns[].at`,
  `lineage.started_at`, `lineage.ended_at`), die Kennung `lineage.local_id`
  und die Endpunkte der Herkunftskanten — mit der Begründung, dort könne
  nichts stehen. Auf dem Hook-Pfad stimmte das; beim **Import** stammen diese
  Werte aus einer fremden Transkriptdatei, und der Endpunkt einer Kante kommt
  direkt aus dem Payload der Gegenseite.

  Sichtbare Folge: In seltenen Fällen kann dort jetzt `[redacted:…]` stehen,
  wo vorher ein Wert stand, und der Nenner in der Redaction-Meldung wächst.
  Wo dabei erstmals etwas gefunden wird, ändert sich der Envelope und damit
  die `SessionId` — ein erneuter Import derselben Session legt sie dann unter
  einer zweiten Kennung ab.

- **Die Mauer gilt auf beiden Eingangswegen**
  ([#93](https://github.com/munichbughunter/minds/issues/93)). `minds import`
  baute die Tool-Argumente direkt aus dem Transkript — ein `Write` auf eine
  Zugangsdaten-Datei trug den vollen Inhalt im `input`, und der stand wörtlich
  im Store. Prüfung, Heuristik und Ersatz-Form stehen jetzt an genau einer
  Stelle (`secretwall`), mit byte-gleicher Envelope-Form auf Hook- und
  Import-Weg. Drei Zusatzbefunde im selben Commit: Der Hook-Weg verlor Marker
  und Auslass-Grund im Envelope schon immer; die Pipeline redigierte den
  eigenen Auslass-Grund (`secret` im Feldnamen); doppelt serialisierter Input
  schlüpfte an der Mauer vorbei, während die Verbatim-Kopie den Inhalt
  mitnahm. `minds import` weist jetzt aus, wie viele Tool-Calls hinter der
  Mauer ausgelassen wurden.

- **Ein envelope-realistischer Korpus und zwei Property-Tests sichern die
  Regressionsgrenze** ([#36](https://github.com/munichbughunter/minds/issues/36)).
  Je 30.000 deterministische Eingaben, ohne neue Dependency: kein Panic auf
  beliebigem UTF-8, ein injiziertes Geheimnis überlebt nie, und
  `redact(redact(x)) == redact(x)`. Genau die Idempotenz-Invariante fand einen
  Bug, den 1037 bestehende Tests nicht sahen: Ein JSON-serialisierter
  `.env`-Inhalt kippte zwischen zwei Läufen die Kategorie
  (`secret` → `pii`), `redact_session` lehnte die Session als `Unstable` ab —
  ein stiller Erfassungsausfall. Ein bereits redigierter Platzhalter wird
  jetzt nicht ein zweites Mal getroffen.

**Hinten — die Tilgung:**

- **`forget` tilgt auch den Session-Branch**
  ([#5](https://github.com/munichbughunter/minds/issues/5)). Der browsbare
  Branch (`refs/minds/sessions/<hex>`) trägt `session.json` **und** eine
  gerenderte `session.md` — und blieb bei der Löschung stehen: „vergessen"
  gemeldet, Klartext weiter auf der Forge, für jeden mit Repo-Zugriff lesbar.
  `forget` prüft und tilgt jetzt alle drei Orte (Store-Ref, Session-Branch,
  Kontext-Baum), ersetzt den Branch-Baum vollständig und benennt in seiner
  Ausgabe jeden getilgten Ort.

- **Ein erneuter `put` reanimiert keine vergessene Session mehr**
  ([#6](https://github.com/munichbughunter/minds/issues/6)). Ein Capture auf
  einer zweiten Maschine oder ein Import überschrieb den Tombstone mit
  Klartext — eine DSGVO-Löschung mit Erfolgsmeldung, die nicht hielt. Der
  Store-Ref wird jetzt mit einem atomaren Guard geschrieben (ein `forget` im
  Fenster gewinnt), der Session-Branch dreifach gestaffelt geprüft; Import und
  Checkpoint überspringen Vergessene, ohne zu scheitern.

- **Der Tombstone ist ein elternloser Wurzel-Commit**
  ([#14](https://github.com/munichbughunter/minds/issues/14)). Vorher blieb
  der Klartext über `<ref>~1` regulär erreichbar und reiste bei jedem Sync auf
  alle Clones. Jetzt ist er über **keinen** Ref mehr erreichbar und nach
  `git gc` endgültig fort; auch die eigene Push-Buchhaltung
  (`refs/minds/remotes/*`) wird vom Klartext gelöst, sonst hielte sie ihn
  gc-immun. Bricht die Mehr-Ort-Tilgung ab, benennt `ForgetIncomplete` die
  getilgten und die offenen Orte — ein erneuter `forget` vollendet idempotent.
  Ein bereits auf die Forge **gepushter** Ref braucht weiterhin einen
  Force-Push ([#102](https://github.com/munichbughunter/minds/issues/102)).

### Behoben

- **Die Rückführung überlebt getilgte und defekte Sessions**
  ([#83](https://github.com/munichbughunter/minds/issues/83)). Eine einzige
  vergessene Session brach `brief`, `distill` und `recall` dauerhaft ab — wer
  die DSGVO-Löschung benutzte, verlor die Kontext-Rückführung vollständig,
  und der SessionStart-Hook scheiterte bei jedem Sitzungsstart. Jetzt gilt der
  Degrade-Vertrag von `show`/`why` auch hier: Übersprungen wird gezählt statt
  abgebrochen, jedes betroffene Kommando nennt die Zahl vor der Ausgabe
  (`minds brief: 1 vergessene Session übersprungen`; nur Defekte verweisen auf
  `minds fsck`), und `brief --hook` schreibt den Hinweis ins hook.log statt in
  die Sitzung.

## [0.1.1] — 2026-08-10 — „Der Hook feuert wirklich"

*Elf Reparaturen an dem einen Versprechen, das alle anderen trägt: dass ein
Commit erfasst wird. Keine neue Funktionalität — und trotzdem das Release, ohne
das jedes Feature unsichtbar geblieben wäre.*

Der rote Faden ist die **stille Falschheit**. Fast jeder Fehler hier meldete
Erfolg und tat nichts: `enable` schrieb Hooks in ein Verzeichnis, aus dem Git
nie liest; die Hooks fanden `minds` nicht und schwiegen; ein Tippfehler
schaltete das CI-Gate ab und ließ die Pipeline grün; ein eingecheckter
Fremdeintrag verhinderte die Registrierung, ohne dass es jemand sah. Was diese
Fälle verbindet, ist nicht ihre Ursache, sondern ihre Bauart — sie brechen
nichts, sie hören nur auf zu arbeiten.

`minds fsck` ist deshalb das Kommando, das in diesem Release am meisten
gewachsen ist: Es benennt inzwischen jeden dieser Zustände.

### Behoben

- Die Agent-Registrierungen haben eine **Soll-Quelle** bekommen, und die
  Erkennung liest zwei Wörter statt einer Teilzeichenkette. Daran hingen zwei
  Fehlklassen. Erstens: Ein eingecheckter Eintrag, der `minds hook` nur
  zufällig im Text trägt — `echo "minds hook ist nett"` —, galt als
  Registrierung; der echte Capture-Hook entstand nie, lautlos, bei jedem
  Kollegen, der das Repo klonte
  ([#78](https://github.com/munichbughunter/minds/issues/78)). Zweitens: Ein
  geänderter Aufruf erreichte **bestehende Installationen nie**, weil jede
  vorhandene Registrierung als „schon da" durchging
  ([#68](https://github.com/munichbughunter/minds/issues/68)). Beide sind
  dieselbe Codestelle, und ein halber Umbau wäre schlimmer als keiner gewesen:
  Ein exakter Vergleich ohne verlässlichen Besitztest hätte fremde
  Nutzerkonfiguration überschrieben.

  Jetzt gilt: Das erste Wort muss auf `minds` enden — nackt oder als Pfad —,
  das zweite genau `hook` bzw. `brief` sein; beim Recall-Eintrag zusätzlich
  `--hook`, denn `minds brief docs/ > brief.md` ist ein legitimer eigener
  SessionStart-Hook und gehört dem Nutzer. Verglichen wird der **Argumentteil**:
  Ein von Hand gepinnter Pfad bleibt stehen — `minds` hat dort nie einen
  geschrieben, und für die Agent-Registrierungen ist er die einzige Abhilfe
  gegen die PATH-Blindheit aus
  [#25](https://github.com/munichbughunter/minds/issues/25). Ein eigener Eintrag mit altem
  Wortlaut wird **an Ort und Stelle** korrigiert (Reihenfolge, `matcher` und
  Zusatzschlüssel des Nutzers bleiben), Fremdes bleibt unangetastet, und der
  Ersatz wird gemeldet — auch ohne `-v`, denn diese Zeile kann jemand von Hand
  geändert haben. Ein vorhandener Recall-Eintrag wird auch **ohne** `--recall`
  gepflegt: Der Schalter regiert das Anlegen, nicht die Wartung, sonst bliebe
  ein `fsck`-Hinweis stehen, den kein `minds enable` behebt.
- Ein **eigenes, aber veraltetes OpenCode-Plugin** wird wieder aktualisiert.
  Es trägt die Marke hinter `//`, verglichen wurde aber gegen die Shell-Fassung
  mit `#` — der Test war damit *immer* falsch, das Plugin galt als fremde
  Datei und blieb für immer auf dem alten Stand.
  ([#68](https://github.com/munichbughunter/minds/issues/68))
- `minds enable` sagt jetzt, wenn es an einer Stelle **nichts registrieren
  konnte**, weil dort Fremdes steht: ein `hooks`, das kein Objekt ist, ein
  Event, das kein Array ist, ein fremdes `minds.ts`. Bisher gingen diese Fälle
  als „unverändert" durch — eine Beruhigung, die nicht stimmte, denn der Agent
  journaliert dann nicht. Und ein kaputtes Event reißt die übrigen sechs nicht
  mehr mit. ([#68](https://github.com/munichbughunter/minds/issues/68),
  [#78](https://github.com/munichbughunter/minds/issues/78))

- **`minds brief --hook` verliert seine Fehler nicht mehr.** Der von
  `minds enable --recall` registrierte SessionStart-Hook lautet
  `minds brief --hook 2>/dev/null || true`: stderr ging ins Nichts, der
  Rückgabewert wurde verschluckt. Scheiterte `brief`, startete die Sitzung
  **ohne** den Kontext, den minds ihr mitgeben wollte — derselbe Stillausfall
  wie [#10](https://github.com/munichbughunter/minds/issues/10), nur auf dem
  Lese- statt dem Capture-Pfad. Die Fehler gehen jetzt nach
  `<git-dir>/minds/hook.log`, und ein Panic ebenfalls: Bisher schaltete der
  Prozess seit [#54](https://github.com/munichbughunter/minds/issues/54) zwar
  den Panic-Handler still, hatte aber keine Klammer, die ihn auffing — er war
  unterdrückt *und* nirgends aufgezeichnet. Vom Log bekommt dieser Pfad nur
  den **Ort**, nicht die Meldung: `brief` hält redigierte Sessions im
  Speicher. Ohne `--hook` bleibt alles beim Alten — dort steht ein Mensch
  davor, und der Fehler gehört auf stderr.
  ([#68](https://github.com/munichbughunter/minds/issues/68))

- `minds enable` funktioniert in **verlinkten Worktrees** (`git worktree add`).
  Dort ist `.git` eine Datei mit einem `gitdir:`-Verweis; sie wurde nicht
  aufgelöst, und `enable` meldete „kein Git-Repository gefunden" — in einem
  offensichtlichen Repository, mit einer Meldung, die faktisch falsch war.
  Agents arbeiten zunehmend in Worktrees. Die Hooks landen im **gemeinsamen**
  Git-Verzeichnis, wo Git sie für alle Arbeitsbäume ausführt; `enable` sagt das
  auch, weil es niemand aus einem Pfad herausliest. Ein Commit im Worktree
  erzeugt damit einen Checkpoint, und `minds fsck` meldet ihn dort als heil.
  Dieselbe Auflösung macht `minds enable` nebenbei in **Submodulen**
  brauchbar — auch dort ist `.git` eine Datei; die Hooks landen im
  `.git/modules/<name>` des Submoduls, das Super-Repo bleibt unberührt.
  ([#21](https://github.com/munichbughunter/minds/issues/21))

  > *Was das noch nicht abdeckt:* `minds show` und `minds why` zeigen im
  > Worktree den Commit des **Hauptbaums**, weil die Wurzel dort über
  > `<git-dir>/..` bestimmt wird und das in einem Worktree
  > `…/.git/worktrees` ergibt. Erfassung und Prüfung stimmen, das Nachschlagen
  > noch nicht — der Weg dahin ist
  > [#20](https://github.com/munichbughunter/minds/issues/20), das dieselbe
  > Berechnung an elf Stellen zusammenführt.

- Ein **Panic in `minds hook`** schreibt nichts mehr auf stderr. `catch_unwind`
  fing ihn zwar, aber zu spät: Der Standard-Handler von Rust hatte
  `thread 'main' panicked at …` samt Backtrace-Hinweis vorher schon
  ausgegeben — und stderr des Hooks gehört dem Agenten, Claude Code reicht ihn
  dem Modell zurück. Ein Rust-Backtrace mitten in der Sitzung des Nutzers ist
  genau das Rauschen, das der Hook vermeiden soll. Jetzt ist der Handler still,
  und der **Ort** des Panics (`hook.rs:99:9`) steht in
  `<git-dir>/minds/hook.log`, wo Diagnose hingehört — der Ort allein, nicht
  die Panic-Meldung: Sie könnte Nutzlast einbetten, und `hook.log` ist die
  Datei, die in einen Bug-Report wandert. Der kalte Pfad
  (`checkpoint`, `sync`, `prepare-commit-msg`) gewinnt dasselbe, behält aber
  die Meldung — dort liegt kein Mitschnitt im Speicher; und wer eines dieser
  Kommandos im **Terminal** aufruft, sieht seinen Panic weiterhin.
  ([#54](https://github.com/munichbughunter/minds/issues/54))
- Ein Argument, das **kein UTF-8** ist, lässt `minds` nicht mehr abstürzen.
  `std::env::args()` panickt daran — in der ersten Zeile, vor jeder eigenen
  Vorkehrung, mit Backtrace auf stderr und Exit 101. Für `minds hook` war das
  der schlimmste denkbare Ort: Die Agent-Registrierung ruft ihn ohne
  `2>/dev/null` auf. Solche Argumente werden jetzt verlustbehaftet gewandelt
  und laufen in die gewöhnliche „unbekanntes Flag"-Meldung.
  ([#54](https://github.com/munichbughunter/minds/issues/54))

- Ein Git-Hook, dem das **Execute-Bit** abhandengekommen ist, wird von `minds
  enable` wieder repariert — und von `minds fsck` benannt. Git überspringt eine
  nicht ausführbare Hook-Datei **stillschweigend**: kein Fehler, keine Meldung,
  nur kein Checkpoint. Das Bit geht auf gewöhnlichen Wegen verloren (ein
  `git archive`/Tarball, eine Kopie über ein Dateisystem ohne Modusbits, ein zu
  breites `chmod -R`), und bisher blieb der Hook danach für immer tot: `enable`
  kehrte bei textgleichem Inhalt zurück, bevor es die Datei überhaupt öffnete,
  und meldete „unverändert"; `fsck` verglich nur den Blocktext und meldete
  „installiert". Beide sehen jetzt hin. Die Reparatur wird **gemeldet**, auch
  ohne `-v`: `chmod -x` ist auch der Weg, einen Hook absichtlich stillzulegen,
  und diese Entscheidung wortlos zurückzunehmen wäre eine Überraschung. Kennt
  das Dateisystem gar keine Execute-Bits (CIFS, exFAT), bricht `enable` mit
  dieser Begründung ab, statt bei jedem Lauf dieselbe Reparatur zu melden.
  ([#52](https://github.com/munichbughunter/minds/issues/52))
- `minds enable` hängt seine Shell-Zeilen nicht mehr an einen Hook mit
  **fremdem Interpreter**. An eine Datei mit `#!/usr/bin/env python3` wurde der
  minds-Block bisher einfach angehängt — der Hook warf ab da Syntaxfehler, und
  das `|| true` im Block fängt in Python nichts. Jetzt bricht `enable` mit
  Begründung ab und nennt den Interpreter, statt einen fremden Hook zu
  beschädigen; Bourne-Verwandte (`sh`, `bash`, `dash`, `ksh`, `zsh` …), die
  Wrapper `env` und `busybox` sowie Dateien ohne Shebang werden weiterhin
  ergänzt. Geprüft wird **vor** der ersten Änderung: Ein fremder Hook an einer
  der drei Stellen lässt den Lauf nicht mehr mittendrin abbrechen, mit
  Agent-Konfiguration auf der Platte und ohne Store-Config. Und `minds fsck`
  meldet dieselbe Datei als abgelehnt — samt Grund —, statt „installiert" zu
  sagen oder zu einem `minds enable` zu raten, das garantiert abbräche.
  ([#52](https://github.com/munichbughunter/minds/issues/52))
- Der Codex-Schalter `codex_hooks = true` wird **exakt** gesetzt. Der Abgleich
  lief über einen Präfix-Vergleich und traf damit auch einen fremden Schlüssel
  wie `codex_hooks_timeout = 30` — dessen Zeile wurde durch `codex_hooks = true`
  *ersetzt*: Nutzerkonfiguration zerstört, und der eigentliche Schalter fehlte
  trotzdem. Zusätzlich gilt der Schalter jetzt als das, was er ist — top-level:
  Eine `codex_hooks`-Zeile unter `[profiles.test]` gehört einer anderen Tabelle
  und bleibt unangetastet, und ein fehlender Schalter wird **vor** der ersten
  Tabelle eingefügt statt ans Dateiende, wo er in der letzten Tabelle gelandet
  wäre. Und wo die Zeilenlogik an ihre Grenze kommt — mehrzeilige Werte,
  Arrays über mehrere Zeilen —, wird nicht geraten: `enable` sagt, dass der
  Schalter von Hand gehört, statt ihn womöglich in ein Literal zu schreiben.
  ([#52](https://github.com/munichbughunter/minds/issues/52))

- `minds enable` schreibt nicht mehr ungefragt in ein Hook-Verzeichnis
  **außerhalb des Repos** — und ein eingecheckter Symlink kann es nicht mehr
  unbemerkt dorthin umlenken. Entschieden wird über den **aufgelösten Ort**,
  nicht über Schreibweisen: Ob `core.hooksPath` direkt nach draußen zeigt
  (global gesetzt, `init.templateDir`) oder ein Symlink in der Arbeitskopie
  (`.husky` → anderswo, auch mit nachgestelltem Schrägstrich, Pfad-Alias oder
  Link im Vorfahren) — liegt das Ziel außerhalb von Arbeitskopie und
  Git-Verzeichnis, fragt `enable` nach, denn Hooks dort gelten für **alle**
  Repositories, die das Verzeichnis benutzen. Interaktiv als Rückfrage
  (Default: Nein), in Skripten per `--global-hooks`; ohne Zustimmung bricht
  `enable` ab, **bevor** irgendetwas geschrieben ist. Ein symlinktes `.git`
  und ein geteiltes `.git/hooks` bleiben dagegen ohne Rückfrage — dorthin
  kann ein Checkout nichts legen. `minds fsck` benennt ein Verzeichnis
  außerhalb des Repos jetzt ebenfalls.
  ([#66](https://github.com/munichbughunter/minds/issues/66),
  [#64](https://github.com/munichbughunter/minds/issues/64))
- Der Argument-Parser ist **strikt** geworden. Ein unbekanntes Flag war bisher
  Rauschen: `minds fsck --require-reviews` (Tippfehler) lief als nacktes `fsck`
  durch und lieferte Exit 0 — das CI-Policy-Gate war damit lautlos
  abgeschaltet, die Pipeline grün. Und ein Wert-Flag nahm blind das nächste
  Argument: `minds review I… --summary --sign` legte das Review mit der
  Zusammenfassung „--sign" an — **unsigniert**, mit Erfolgsmeldung. Jetzt
  bricht jedes Unterkommando bei einem Flag ab, das es nicht kennt, und nennt
  die bekannten; ein Wert-Flag, dem ein weiteres Flag folgt, ist ein Fehler
  statt einer Verwechslung. Positionale Argumente und Flags sind
  reihenfolgeunabhängig — `minds verify --sig s.sig b3-…` findet das Subjekt,
  nicht die Signatur-Datei. `--help` funktioniert jetzt auch hinter einem
  Unterkommando. Die Ausnahme bleibt `minds hook`: Ein Rekorder bricht nicht
  wegen eines Konfigurationsfehlers ab — der Fehler geht in
  `<git-dir>/minds/hook.log`, der Lauf macht mit dem Verwertbaren weiter.
  ([#11](https://github.com/munichbughunter/minds/issues/11))
- `minds agent-help` nennt jetzt **alle** öffentlichen Kommandos. Acht fehlten
  (`hook`, `checkpoint`, `blame`, `metrics`, `forget`, `sign`, `verify`,
  `gitlab`) — für eine Karte, deren einziger Zweck Vollständigkeit ist. Ein
  Test vergleicht sie künftig mit der Kommando-Tabelle des Parsers; wer ein
  Kommando ergänzt, ohne die Karte zu pflegen, wird rot statt still
  unvollständig. ([#11](https://github.com/munichbughunter/minds/issues/11))
- Die von `minds enable` geschriebenen Git-Hooks hängen nicht mehr am `PATH`.
  GUI-Clients (VS Code, Fork, Tower) und minimale CI-Shells starten Git ohne
  das Profil der Shell — `~/.local/bin` fehlt dort, der Aufruf `minds …` lief
  ins Leere, und `|| true` machte daraus einen **stillen Totalausfall**:
  Committen ging, erfasst wurde nichts, dauerhaft und ohne Hinweis. `enable`
  merkt sich jetzt den Ort des Binaries in der lokalen `.git/config`
  (`minds.binary`); die Hooks lösen ihn zuerst auf und suchen erst dann im
  `PATH`. Der gemerkte Ort **gewinnt** — damit kann auch eine veraltete
  globale `minds` die Hooks nicht mehr beschatten; es läuft die Version, deren
  `enable` die Hooks geschrieben hat. Zieht das Binary um, greift wieder die
  PATH-Suche, und `minds fsck` sagt, dass ein `minds enable` den Eintrag
  erneuert. Im Hook-Text selbst steht weiterhin **kein** absoluter Pfad: Seit
  die Hook-Datei in der Arbeitskopie liegen kann, wäre ein Home-Pfad darin
  versioniert — eingecheckt für alle, kaputt auf jeder anderen Maschine. Auch
  hier gilt: Bestehende Installationen brauchen einmal `minds enable`.
  ([#25](https://github.com/munichbughunter/minds/issues/25))
- Fehler aus dem **Hook-Pfad** verschwinden nicht mehr. `checkpoint`,
  `prepare-commit-msg` und `sync` laufen aus Git-Hooks, die ihre Ausgabe wegwerfen —
  ihre Fehler waren damit unsichtbar, obwohl die Doku ein Log versprach. Der
  teuerste Fall: Ein Tippfehler in `.minds/redact.json` bricht `checkpoint`
  *fail-closed* ab, ab da wird nie wieder eine Session eingecheckt, das Journal
  wächst, und an keiner Stelle steht warum. Jetzt schreiben alle vier Hook-Pfade
  ihre Fehler nach `<git-dir>/minds/hook.log` — dorthin, wo `minds hook` schon
  immer geloggt hat. ([#10](https://github.com/munichbughunter/minds/issues/10))
- Der `pre-push`-Hook kippt seine Fehlermeldungen nicht mehr roh in den
  Push-Output. Als einziger der drei Hooks leitete er stderr nicht um; ein
  unerreichbares Remote schrieb damit bei **jedem** Push fünf Zeilen zwischen die
  Ausgabe von `git push`, für einen Vorgang, den der Nutzer gar nicht angestoßen
  hat. Der Wortlaut steht jetzt im Log; stdout bleibt unangetastet, dort steht die
  Erfolgsmeldung. ([#10](https://github.com/munichbughunter/minds/issues/10))
- Ein **Panic** in `checkpoint`, `prepare-commit-msg` oder `sync` landet im Log,
  statt mit der weggeworfenen stderr zu verschwinden. `minds hook` hatte diese
  Klammer schon; für den kalten Pfad fehlte sie, und mit der stderr-Umleitung
  wäre ein Absturz dort vollständig lautlos gewesen.
  ([#10](https://github.com/munichbughunter/minds/issues/10))

  > **Bestehende Installationen brauchen einmal `minds enable`.** Der Rumpf eines
  > Hooks steht in der Hook-Datei, nicht im Binary — ein Update allein ersetzt ihn
  > nicht. `minds fsck` sagt jetzt von sich aus, wenn ein Block aus einer älteren
  > Version stammt, statt ihn als „installiert" durchgehen zu lassen.
- `minds enable` installiert die Git-Hooks jetzt im **effektiven** Hook-Verzeichnis.
  Repos mit `core.hooksPath` (husky, lefthook, pre-commit, globale Hooks über
  `init.templateDir`) bekamen den Hook bisher nach `.git/hooks` geschrieben — ein
  Verzeichnis, das Git dort nie liest. `enable` meldete Erfolg, und trotzdem entstand
  bei keinem Commit ein Checkpoint. Ist das Verzeichnis verschoben, sagt `enable` das
  auch ohne `-v` — und nennt es anders, wenn es *außerhalb* des Repos liegt, weil ein
  `enable` dann alle Repositories des Nutzers erfasst.
  ([#9](https://github.com/munichbughunter/minds/issues/9))
- `minds enable` bricht ab, statt an einen unbrauchbaren Ort zu schreiben — und zwar
  **bevor** die erste Datei entsteht, damit kein halb eingerichtetes Repo zurückbleibt.
  Betroffen sind ein gesetztes, aber leeres `core.hooksPath` (Git führt dann gar keine
  Hooks aus und meldet das nicht), ein Wert, der auf die Arbeitskopie-Wurzel auflöst
  (die Hooks lägen als ausführbare Dateien zwischen dem Quellcode), und ein
  Verzeichnis, in dem sich nicht schreiben lässt. Die Meldung nennt jeweils den Pfad
  und was zu tun ist. ([#9](https://github.com/munichbughunter/minds/issues/9))

### Sicherheit

- Die **Agent-Konfigurationen** (`.claude/settings.json`, `.cursor/hooks.json`,
  `.gemini/settings.json`, `.codex/hooks.json`, `.codex/config.toml`,
  `.opencode/plugin/minds.ts`) schreibt `enable` nicht mehr durch einen
  Symlink hindurch. Diese Dateien liegen in der versionierten Arbeitskopie —
  ein gemergter PR konnte an ihrer Stelle einen Link platzieren, im Diff
  sichtbar nur als Moduswechsel auf `120000`, und `enable` überschrieb dann
  die fremde Zieldatei vollständig. Geprüft wird jetzt **jedes Verzeichnis
  zwischen Repo-Wurzel und Datei**, nicht nur die Datei selbst: `.claude` als
  Link auf `~/.claude` war der wirksamere Angriff — das Blatt darunter ist
  eine reguläre Datei, und `enable` hätte in die *globale* Konfiguration des
  Nutzers geschrieben. Abgelehnt werden ebenso Sonderdateien (ein FIFO ließ
  `enable` unbegrenzt hängen) und Dateien jenseits jeder Konfigurationsgröße.
  Geschrieben wird über denselben Weg wie bei den Hooks — Nachbardatei mit
  `create_new`, dann `rename` —, und die Prüfung läuft **vor** der ersten
  Änderung: Ein Link auf die dritte Konfiguration bricht nicht mehr ab,
  nachdem die ersten beiden geschrieben sind.
  ([#65](https://github.com/munichbughunter/minds/issues/65))
- Eine bestehende Agent-Konfiguration **behält ihre Dateirechte**, und eine
  schreibgeschützte wird nicht mehr ersetzt. Das Ersetzen über `rename`
  tauscht den Inode und damit die Rechte: Eine `settings.json` mit `0600` —
  weil dort ein API-Key steht — wäre sonst als `0644` zurückgekommen und auf
  einer Mehrbenutzer- oder CI-Maschine für jeden lokalen Account lesbar
  gewesen. ([#65](https://github.com/munichbughunter/minds/issues/65))
- Fremder Text, den `minds` ausgibt oder protokolliert — Pfade aus der
  Arbeitskopie, `core.hooksPath`, der Wortlaut fremder Fehler —, wird jetzt
  vollständig entschärft. Bisher galt dafür `char::is_control`, und das ist nur
  die Unicode-Kategorie `Cc`: **U+2028** (LINE SEPARATOR) und **U+2029**
  (PARAGRAPH SEPARATOR) fielen durch. Rusts `str::lines` bricht daran nicht,
  Browser und Pythons `splitlines()` schon — im Job-Log einer GitLab-Pipeline
  ließ sich damit eine Zeile fälschen, etwa ein `fsck: in Ordnung`. Ebenfalls
  durchgerutscht sind die unsichtbaren Formatzeichen (`Cf`), darunter die
  Unicode-Tags `U+E0020`–`U+E007F`. Statt die Liste von Hand zu pflegen, wird
  jetzt `char::escape_debug` selbst gefragt — es deckt `Cc`, `Cf`, `Zl`, `Zp` und
  `Zs` ab, ergänzt um die unsichtbaren Variantenselektoren (`U+E0100`–`U+E01EF`
  & Co.) und die typografischen Anführungszeichen, in die `fsck` seine Pfade
  klammert. Ein Pfad kann damit weder eine Zeile öffnen, noch die Klammer
  schließen, noch unsichtbaren Text tragen.
  ([#10](https://github.com/munichbughunter/minds/issues/10))
- **Der Inhalt von `.minds/redact.json` erreicht das Log nicht.** In dieser Datei
  stehen per Design literale Geheimnisse — `deny_secrets` für den internen
  Hostnamen, `allow` für Werte, die fälschlich als Secret erkannt werden. Der
  naheliegendste Tippfehler dort (vergessene Array-Klammern) ist kein Syntax-,
  sondern ein Datenfehler, und `serde_json` zitiert dabei den Wert: `invalid
  type: string "glpat-…", expected a sequence`. Solange das auf einer
  weggeworfenen stderr landete, war es flüchtig; mit dem Log wäre es dauerhaft
  geworden. Die Meldung nennt jetzt Art, Zeile und Spalte — genug zum Reparieren,
  nichts zum Mitlesen. ([#10](https://github.com/munichbughunter/minds/issues/10))
- **Zugangsdaten aus einer Remote-URL erreichen das Log nicht.** `git push`
  schreibt die URL in seine Fehlermeldung, und in der **Username**-Position
  redigiert Git ein Token nicht: Aus `https://glpat-…@gitlab.com/…` wurde so
  `fatal: could not read Password for 'https://glpat-…@gitlab.com'` — und das
  ging seit dem Log-Eintrag oben auf die Platte, in eine Datei, auf die `fsck`
  verweist und die man in einen Bug-Report legt. Der Autoritätsteil wird jetzt
  an der Quelle herausgeschnitten, bevor der Text zu einer Meldung wird; Host
  und Pfad bleiben stehen, damit die Diagnose brauchbar bleibt.
  ([#10](https://github.com/munichbughunter/minds/issues/10))
- Ein `git`-Kindprozess erbt **keine Trace-Schalter** mehr (`GIT_TRACE`,
  `GIT_TRACE_CURL`, `GIT_CURL_VERBOSE` …). Mit ihnen protokolliert Git seinen
  ganzen Verkehr auf stderr — samt `Authorization: Basic …`, und das ist keine
  URL, die sich herausschneiden ließe. Ein `GIT_TRACE=1` in der Shell des
  Entwicklers hätte sonst genügt, um ein Token dauerhaft ins Log zu legen. Die
  Variablen werden **entfernt**, nicht auf `0` gesetzt: `GIT_CURL_VERBOSE` prüft
  Git auf Existenz, ein `=0` schaltete den Dump also ein.
  ([#10](https://github.com/munichbughunter/minds/issues/10))
- `hook.log` wird nicht durch einen Symlink hindurch beschrieben — weder wenn die
  Datei selbst einer ist, noch wenn `<git-dir>/minds` es ist. Nach dem Öffnen
  werden Gerät und Inode des Dateizeigers gegen den Namen geprüft; erst danach
  werden die Rechte angefasst. ([#10](https://github.com/munichbughunter/minds/issues/10))
- Beim Schreiben einer **Hook-Datei** folgt `minds enable` keinem Symlink mehr. Seit
  das Hook-Verzeichnis an `core.hooksPath` hängt, kann es in der Arbeitskopie liegen
  und damit **versioniert** sein — ein eingecheckter Symlink `.husky/post-commit →
  ~/.aws/credentials` hätte bisher dazu geführt, dass `enable` den minds-Block durch
  den Link in die private Datei schreibt und sie von `0600` auf `0755` setzt. Jetzt
  wird ein Symlink an dieser Stelle abgelehnt; geschrieben wird über eine
  Nachbardatei und `rename` (der Name wird ersetzt, nie durch ihn hindurch
  geschrieben), die Rechte werden auf dem offenen Dateizeiger gesetzt, und Dateien
  jenseits jeder Hook-Größe werden gar nicht erst eingelesen. `minds fsck` liest die
  Hook-Dateien über denselben Weg und meldet einen abgelehnten Hook mit Grund.
  ([#9](https://github.com/munichbughunter/minds/issues/9))

  Die beiden damals ausdrücklich benannten Lücken sind inzwischen zu: Das über
  einen Symlink umgelenkte Hook-**Verzeichnis** beantwortet die Ortsregel
  ([#66](https://github.com/munichbughunter/minds/issues/66),
  [#64](https://github.com/munichbughunter/minds/issues/64), siehe *Behoben*),
  und die **Agent-Konfigurationen** gehen seit
  [#65](https://github.com/munichbughunter/minds/issues/65) über denselben
  geschützten Schreibweg wie die Hooks (siehe unten).

### Hinzugefügt

- `minds fsck` prüft jetzt auch die **Agent-Registrierungen**. Bisher sah es
  nur auf die Git-Hooks; ob der Agent überhaupt journaliert, war für den
  Bericht unsichtbar. Gemeldet werden drei Zustände: eine Konfiguration ganz
  ohne minds-Eintrag (der Fall, den ein eingecheckter Fremdeintrag erzeugt —
  [#78](https://github.com/munichbughunter/minds/issues/78)), Einträge aus
  einer älteren Version, und eine unvollständige Registrierung. Der
  Recall-Eintrag bekommt einen eigenen Satz, wenn er veraltet ist — sein
  **Fehlen** dagegen nicht: `--recall` ist opt-in, und was niemand wollte,
  fehlt nicht.

  Höchstens eine Zeile je Agent, nie je Event, und eine Datei, die es gar
  nicht gibt, bleibt still. Ein Hinweis, kein Befund: Der Rückgabewert bleibt
  0, denn er ist das CI-Gate.
  ([#68](https://github.com/munichbughunter/minds/issues/68))
- `minds fsck` prüft die Hooks vom effektiven Hook-Verzeichnis aus und meldet, wenn
  `post-commit` oder `prepare-commit-msg` dort fehlen — samt Hinweis, wenn der
  minds-Block stattdessen im ignorierten `.git/hooks` liegt. Ein Hinweis, kein Befund:
  Der Rückgabewert bleibt 0, denn nicht jedes Repo will Hooks.
  ([#9](https://github.com/munichbughunter/minds/issues/9))
- `minds fsck` verweist auf `<git-dir>/minds/hook.log`, wenn dort Einträge stehen —
  mit ihrer Zahl und dem Pfad, aber **ohne den Wortlaut**: Die Ausgabe von `fsck`
  landet im CI-Log, ein Fehlertext aus dem Hook-Pfad kann einen Ausschnitt aus dem
  noch nicht redigierten Mitschnitt tragen. Ein Hinweis, kein Befund — ein alter
  Eintrag darf keine Pipeline anhalten.
  ([#10](https://github.com/munichbughunter/minds/issues/10))

  Das Log begrenzt sich selbst: Bei 1 MiB wird auf `hook.log.1` umgeschichtet, mehr
  als zwei Dateien entstehen nie. Mehrzeilige Fehlermeldungen bleiben *ein* Eintrag
  (Steuerzeichen werden als Escape-Sequenz geschrieben), und neu angelegt wird die
  Datei mit `0600`; eine vorhandene mit lockereren Rechten wird beim nächsten
  Schreiben nachgezogen, und durch einen Symlink hindurch wird nie geschrieben.
- Der `pre-push`-Hook meldet seinen **Fortschritt** jetzt auf stdout statt auf
  stderr — sonst hätte die stderr-Umleitung von oben mit den Fehlern auch die
  Erfolgsmeldungen verschluckt: was geschickt wurde, und vor allem, wie viele
  fremde Review-Verdicts ein Push übernommen hat. Letzteres wiegt schwer, weil
  genau dieser Merge den Review-Store füllt, den `minds fsck --require-review`
  als CI-Gate liest. Scheitert er, steht das jetzt im Log statt nirgendwo.
  ([#10](https://github.com/munichbughunter/minds/issues/10))
- `minds fsck` unterscheidet einen **veralteten** minds-Block von einem heilen: Der
  Rumpf zwischen den Marken wird gegen den verglichen, den diese Version schreiben
  würde. Bisher zählte das bloße Vorhandensein der Marke als „installiert" — ein
  Hook aus einer älteren `minds` sah damit heil aus, obwohl Git ihn ausführt und er
  nicht mehr tut, was er soll. Der Rat ist `minds enable`; ein Hinweis, kein Befund.
  ([#10](https://github.com/munichbughunter/minds/issues/10))

### Bekannte Einschränkungen

*Der Stand von v0.1.1. Die Liste unter v0.1.0 beschreibt den damaligen Stand und
wird nicht rückwirkend umgeschrieben — was dort steht, gilt heute teilweise nicht
mehr (die PATH-Abhängigkeit etwa ist mit
[#25](https://github.com/munichbughunter/minds/issues/25) weg).*

- **Die Redaktion hat bekannte Lücken.** `curl -u user:pass` wird nicht redigiert
  ([#2](https://github.com/munichbughunter/minds/issues/2)), JSON-escapte Secrets
  und PEM-Schlüssel mit literalem `\n` leaken teilweise
  ([#3](https://github.com/munichbughunter/minds/issues/3)), `sk-ant`/`sk-proj`
  fehlen in den Token-Regeln
  ([#33](https://github.com/munichbughunter/minds/issues/33)), und ein
  Multibyte-Zeichen im Wert (`PASSWORD=hunter€2`) löst einen Panic aus
  ([#1](https://github.com/munichbughunter/minds/issues/1)). **Das ist der
  Schwerpunkt der nächsten Version.** Wer heute mit fremdem oder besonders
  schutzbedürftigem Code arbeitet, sollte das wissen.
- **`minds forget` tilgt nicht überall.** Der Session-Branch auf der Forge bleibt
  ([#5](https://github.com/munichbughunter/minds/issues/5)), ein erneuter `put`
  reanimiert die Session ([#6](https://github.com/munichbughunter/minds/issues/6)),
  und der Klartext bleibt als Parent-Commit erreichbar
  ([#14](https://github.com/munichbughunter/minds/issues/14)). Zusätzlich liefern
  `recall`, `distill` und `brief` **nichts mehr**, sobald eine Session getilgt
  wurde ([#83](https://github.com/munichbughunter/minds/issues/83)).
- **`minds gitlab mirror` funktioniert nicht.** Der Notiz-Body geht als Header
  über die Leitung, GitLab lehnt mit „body is missing" ab
  ([#7](https://github.com/munichbughunter/minds/issues/7)).
- **Im verlinkten Worktree** zeigen `minds show` und `minds why` den Commit des
  Hauptbaums. Erfassung und `fsck` stimmen dort, das Nachschlagen nicht
  ([#20](https://github.com/munichbughunter/minds/issues/20)).
- **Ein `git push` öffnet zwei Netzwerkverbindungen**, wenn es neue Sessions zu
  übertragen gibt: eine für den Kontext, eine für den Code. Spürbar gegen
  entfernte Remotes ([#85](https://github.com/munichbughunter/minds/issues/85)).
  Ohne neue Sessions kostet der Hook nichts.
- **Kein Windows-Binary.** Gebaut werden macOS (Apple Silicon und Intel) und Linux
  (x86_64 und ARM64, musl/statisch). Unter Windows über WSL oder aus dem Quelltext.
- **Die Tool-Ebene ist Claude-Code-spezifisch.** Für Codex, Cursor, Gemini und
  opencode wird der Prompt erfasst, aber Tool-Aufrufe, berührte Dateien und
  Modell-/Token-Angaben werden nicht ausgewertet.
- **Die Review-Schicht braucht zwei Personen auf einem Repo**, um beansprucht zu
  werden.

## [0.1.0] — 2026-07-29

Die erste veröffentlichte Version — und die erste, die über einen Installer
ausgeliefert wird statt als handgebautes Archiv.

Minds schreibt den Kontext einer Agent-Session dorthin, wo er hingehört: in Git
selbst, neben den Code. Was eine Änderung veranlasst hat, wer sie geschrieben hat und
wer sie geprüft hat, liegt content-adressiert und signiert unter `refs/minds/` und
wandert mit dem Repo — ohne Datenbank, ohne Cloud, offline und im Air-Gap
verifizierbar.

> Frühere, von Hand gebaute Archive trugen bereits dieselbe Versionsnummer, liegen
> aber vor diesem Tag. `minds --version` meldet heute nur `0.1.0` und unterscheidet
> die beiden nicht — wer noch ein altes Archiv im Pfad hat, installiert bitte neu.

### Hinzugefügt

**Erfassung**

- **Hook-basiertes Capture.** `minds enable` registriert Agent- und Git-Hooks;
  idempotent und fremdschonend. Der heiße Pfad (`minds hook`) schreibt jedes Event
  ins lokale Journal und endet immer mit 0, der kalte Pfad (`minds checkpoint`)
  deutet, redigiert, speichert und hängt den Session-Id-Trailer an den Commit. Siehe
  [ADR-0003](docs/adr/0003-hooks-statt-transkript-parsing.md).
- **Fünf Agents registrierbar:** `claude-code`, `codex`, `cursor`, `gemini`,
  `opencode`. Die Deutung der Tool-Ebene ist zunächst Claude Code vorbehalten (siehe
  *Bekannte Einschränkungen*).
- **Redaction, fail-closed.** Secrets und personenbezogene Daten gehen raus, *bevor*
  ein Byte in den Store geht — im Zweifel blockiert Minds, statt zu riskieren. Regeln
  erweiterbar über `.minds/redact.json`.
- **Import bestehender Historie** mit heuristischer Zuordnung Session → Commit;
  vermutete Zuordnungen werden als *vermutet* ausgewiesen statt als Tatsache. Siehe
  [ADR-0004](docs/adr/0004-import-und-store-index.md).

**Speicherung**

- **Content-adressierter Store** (`SessionId = blake3(canonical_json)`) mit zwei
  Backends hinter einem Trait: in-repo unter `refs/minds/` und als separates
  Child-Repo.
- **Ein Ref je Session.** Kein gemeinsam beschriebener Ref, damit kein
  Serialisierungspunkt für Schreiben und Pushen: Der Ref-Name *ist* der Inhalts-Hash,
  zwei Agents fassen verschiedene Refs an, und ein Repo, das nur eincheckt, zahlt für
  den Hook 0,02 s. Siehe [ADR-0010](docs/adr/0010-ein-ref-je-session.md).
- **Browserbare Session-Branches.** Jede Session erscheint als
  `minds/session/<hash>` mit `session.json` (maßgeblich) und `session.md` (gerendert)
  — GitLab zeigt den Branch damit ohne jeden Reader-Deploy als lesbare Seite.
- **`minds forget <session> [--reason]`** — DSGVO-Löschung: Die Nutzlast wird durch
  einen Tombstone ersetzt, die Hash-Referenz bleibt auflösbar, getilgt wird an allen
  Ablageorten. `why`, `show` und `fsck` bleiben grün und degradieren ehrlich, statt zu
  brechen. Siehe [ADR-0007](docs/adr/0007-forget-redigierbare-nutzlast.md).

**Nachschlagen**

- **`minds why <datei>:<zeile>`** — die Session hinter einer einzelnen Zeile, über
  blame und Trailer aufgelöst.
- **`minds show [<commit>] [--full]`** — Absicht und Attribution hinter einem Commit.
- **`minds blame <datei>`** — Attribution je Zeile, nach Session aggregiert, mit
  Kontext-Abdeckung in Prozent.
- **`minds recap`** und **`minds search <query>`** — die jüngsten Sessions auf einen
  Blick; Absicht, Verlauf und Dateien durchsuchbar.
- **`minds render`** baut eine zustandslose HTML-Seite: Zeile anklicken, Prompt
  dahinter sehen, Gesprächsverlauf und Tool-Aufrufe aufklappbar.
- **`minds fsck`** prüft, ob jeder Trailer auflösbar ist, und meldet Journal-Lücken.

**Kontext-Rückführung**

- **`minds recall <ziel>`**, **`minds brief [<datei>…]`** und
  **`minds distill [--path] [--out]`** geben den erfassten Kontext an den nächsten
  Agenten zurück — als Brief zu einer Zeile, als größenbegrenzter Startblock oder als
  AGENTS.md-Entwurf aus der Repo-Historie. Deterministisch, ohne LLM-Aufruf, ohne
  Tokens; gleiche Eingabe ergibt byte-gleiche Ausgabe. Optional automatisch beim
  Session-Start über `minds enable --recall`. Siehe
  [ADR-0005](docs/adr/0005-kontext-rueckfuehrung.md).

**Identität und Nachweis**

- **Change-Id** als stabile Identität einer logischen Änderung, erzeugt und erhalten
  über `prepare-commit-msg` (Trailer `Minds-Change-Id`). Überlebt Rebase, Squash,
  Amend und Cherry-Pick. Siehe [ADR-0006](docs/adr/0006-change-id.md).
- **Signierte Attribution.** `minds sign <session>` signiert die kanonische
  Attribution per `ssh-sig` (kein Netz, air-gap-tauglich), `minds verify` prüft sie und
  endet bei Manipulation mit einem Rückgabewert ≠ 0. Aus „Agent X, Modell Y schrieb
  diese Zeilen" wird ein Nachweis statt einer Behauptung. Siehe
  [ADR-0008](docs/adr/0008-signierte-attribution.md).
- **`minds audit --export`** bündelt die Provenienz-Kette
  (Change → Session → Attribution → Verdict) als portable JSON-Datei mit den
  kanonischen Payloads und Signaturen — prüfbar ohne dieses Werkzeug. Was das Bundle
  beweist und was nicht, steht in
  [docs/nachweis-leitfaden.md](docs/nachweis-leitfaden.md).

**Review**

- **Reviews als Git-Objekte.** `minds review <subject> --approve|--reject|--needs-work`
  legt ein content-adressiertes, optional signiertes Verdict unter
  `refs/minds/reviews/` ab; `minds reviews <subject>` listet Verdicts und prüft
  Signaturen. Das Verdict hängt an der Change-Id und überlebt damit Rebase, Squash und
  Force-Push. Siehe [ADR-0009](docs/adr/0009-reviews-als-git-objekte.md).
- **Review-Thread.** `minds comment <subject> --on <datei:zeile|turn:n> "<text>"` —
  ein append-only Log content-adressierter Einträge. Zwei Reviewer, die offline
  kommentieren, erzeugen keinen Konflikt, sondern eine Vereinigung.
- **`minds stack`** zeigt die abhängigen Changes ab einer Basis mit ihrem jeweiligen
  Review-Stand.
- **GitLab-Brücke, einweg und idempotent.** `minds gitlab mirror <subject> --mr <nr>`
  spiegelt Verdicts als MR-Note (optional als Approval); `minds gitlab webhook` deutet
  einen MR-Kommentar (`/minds approve|reject|needs-work`) als Verdict — opt-in,
  zustandslos, kein Dienst. Das Token kommt ausschließlich aus der Umgebung.
  Betriebsmodell in [docs/betriebsmodell-gitlab.md](docs/betriebsmodell-gitlab.md).
- **Policy als Binary statt YAML.** `minds fsck --require-review` verlangt für jeden
  agent-geschriebenen Change ein gültiges Verdict und wird sonst rot. Dazu ein
  wiederverwendbarer CI-Include (`ci/minds-review-gate.gitlab-ci.yml`), der nichts tut
  als das Binary aufzurufen.

**Betrieb**

- **`minds sync [--remote]`** schickt Kontext und Reviews in einer Verbindung ans
  Remote — alle fälligen Refs auf einmal, nie mit `--force`. Ohne neue Refs kostet der
  Aufruf keine Verbindung. Führt zusammen, was ein `git fetch` an fremden Verdicts
  mitgebracht hat.
- **`minds metrics [--format prometheus|openmetrics|json]`** projiziert den Store
  on-demand ins Prometheus-Textformat — kein Daemon, kein Doppel-Speichern. Dazu ein
  importierbares Grafana-Dashboard (`dashboards/minds.json`) und ein opt-in
  CI-Include (`ci/minds-metrics.gitlab-ci.yml`).
- **`minds agent-help`** gibt die Kommando-Karte maschinenlesbar als JSON aus — für
  Agents, nicht für Menschen.

### Sicherheit

- **Die Secret-Wall auf dem heißen Pfad ist agent-agnostisch.** Der Datei-Pfad wird
  aus der Union bekannter Feldvarianten gezogen (`file_path`, `notebook_path`, `path`,
  `absolute_path`, `filepath`, …) plus einer Heuristik über den Feldnamen. Fail-closed
  gilt damit für alle Agents, nicht nur für den, dessen Feldnamen wir zuerst kannten.

### Bekannte Einschränkungen

- Die von `minds enable` eingetragenen Git-Hooks rufen `minds` **ohne Pfad** auf und
  fangen jeden Fehlschlag mit `|| true` ab — ein Rekorder darf keinen Commit scheitern
  lassen. Liegt das Binary **nicht im `PATH`**, laufen die Hooks deshalb **still** ins
  Leere: Committen funktioniert weiter, es gibt keine Fehlermeldung, aber auch keine
  Change-Id am Commit und keine erfasste Session. Dasselbe greift, wenn eine
  **veraltete** `minds` im `PATH` liegt — sie bedient die Hooks und schreibt
  gegebenenfalls ein älteres Store-Layout. `minds enable` prüft beides heute noch
  nicht; nachsehen lässt es sich mit `command -v minds` und `minds --version`.
- Die Deutung der **Tool-Ebene ist noch Claude-Code-spezifisch**. Für `gemini`,
  `codex`, `cursor` und `opencode` wird der Prompt erfasst, aber Tool-Aufrufe,
  berührte Dateien und Modell-/Token-Angaben werden noch nicht ausgewertet. Welcher
  Agent als nächstes vollständig unterstützt wird, richtet sich nach dem Bedarf der
  Testgruppe.
- Die **Review-Schicht braucht mindestens zwei Personen auf einem Repo**, um
  überhaupt beansprucht zu werden.
- Der Reader (`minds render`) zeigt Sessions, Dateien und den Gesprächsverlauf;
  **Übersichts-Kacheln und Diagramme fehlen noch**, obwohl `minds metrics` die
  Kennzahlen bereits liefert.
- Das Release enthält **Linux x86_64** (musl, statisch) und — sobald ein Mac-Runner
  registriert ist — **macOS für Apple Silicon und Intel**. **Windows und ARM-Linux
  werden derzeit nicht gebaut**; dort ist der Weg `cargo build --release --bin minds`.
