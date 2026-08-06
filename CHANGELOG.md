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

### Behoben

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

  *Was das nicht abdeckt, ausdrücklich benannt:* Zeigt das **Verzeichnis** selbst
  über einen eingecheckten Symlink woandershin, entstehen unsere Hooks dort — und
  liegt am Ziel schon eine Datei desselben Namens, bekommt sie unseren Block und
  `0755`. Der Weg dorthin ist eine Frage des *Ortes*, nicht des Links; er wird
  geschlossen, wenn `enable` vor dem Schreiben außerhalb des Repos zurückfragt
  ([#64](https://github.com/munichbughunter/minds/issues/64), Details in
  [#66](https://github.com/munichbughunter/minds/issues/66)). Ebenfalls offen und
  unverändert gegenüber v0.1.0: Die **Agent-Konfigurationen**
  (`.claude/settings.json` & Co.) schreibt `enable` ohne diese Prüfung
  ([#65](https://github.com/munichbughunter/minds/issues/65)).

### Hinzugefügt

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
