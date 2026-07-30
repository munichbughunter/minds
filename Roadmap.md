# Minds — Roadmap & Strategie

*Das vorzeigbare Dokument: für Partner, Investoren, Contributor. Es erklärt die
These, den Markt, die Verteidigbarkeit — und legt die vollständige technische
Roadmap offen, inklusive der großen Wette (Reviews als Git-Objekte).*

*Für die kommit-genaue Kurzfrist-Umsetzung siehe `Plan-v0.2.md`. Für die
Ursprungs-Vision siehe `Plan.md`.*

---

## 1. Der Einzeiler

**Git weiß, *was* sich geändert hat. Es weiß nicht, *warum*.** Solange Menschen den
Code schrieben, war das egal — der Grund saß im Kopf des Autors und im MR-Text.
Jetzt schreiben Agents den Code, und der Grund verdampft, sobald das Terminal-Fenster
zugeht. Minds schreibt den Grund dorthin, wo er hingehört: **in Git selbst, neben
den Code.**

## 2. Die Bruchstelle, die neu ist

Wenn ein Agent 2.000 Zeilen in zwanzig Minuten produziert, ist „lies den Diff" kein
Verfahren mehr, sondern eine Fiktion. Zeile-für-Zeile-Review skaliert nicht mit
Agent-Flotten. Das Qualitätstor existiert dann nur noch formal und filtert nichts.

Die Antwort ist nicht, den Diff schöner darzustellen. Die Antwort ist, **die Absicht
zum versionierten Artefakt zu machen und den Diff dagegen zu prüfen.** Der Reviewer
liest, was verlangt wurde, und prüft dann, ob der Code das tut.

## 3. Die These: mehr ins Repo, weniger in die Plattform

Betrachtet man die Schwächen des heutigen Git/GitLab-Modells nebeneinander, lösen
sie sich fast alle in **dieselbe Richtung** auf. Das ist kein Zufall — es ist eine
verwertbare Beobachtung.

| Heutige Bruchstelle | Wohin es gehört | Vorbild |
|---|---|---|
| Commit-Identität hängt am Hash; Rebase/Squash zerstören sie | **Change-Id** ins Repo | Gerrit, Jujutsu |
| Author ist ein unsigniertes Freitextfeld | **Signierte Attribution** ins Repo | sigstore, ssh-sig |
| Kontext einer Änderung verdampft mit der Session | **Kontext als Git-Objekt** | *Minds heute* |
| Reviews/Approvals/Diskussion liegen in Postgres | **Reviews als Git-Objekte** | Radicle, git-bug |
| Der MR ist zu grob (pro Branch statt pro Change) | **Review pro Change** | Gerrit |
| Secret/PII in der History sind für immer drin (DSGVO ⊥ Merkle) | **Redigierbare Nutzlast** | — |
| Der Zeilendiff ist die falsche Einheit | **Struktureller/AST-Diff** | Difftastic, Darcs/Pijul |
| YAML als Programmiersprache für CI | **Policy als Binary** | — |

Git ist nicht zu wenig — es wird **zu wenig benutzt**, weil die Plattformen kein
Interesse an einem repo-nativen Gedächtnis haben. Ihr Geschäftsmodell *ist* die
Datenhaltung in der eigenen Datenbank. Genau hier entsteht die Lücke.

**Leitlinie, aus der These abgeleitet:**
> Jedes neue Artefakt fragt zuerst „geht das als **Git-Objekt**?", erst dann „geht
> das als Plattform-Feature?". Ein Git-Objekt wandert mit dem Repo, überlebt
> Migration, funktioniert offline und im Air-Gap.

## 4. Warum das eine verteidigbare Position ist

- **GitLab ist da, wo die regulierten Läden sitzen** — Banken, Versicherer,
  öffentliche Hand, Automotive. Self-Managed, oft on-prem, teils air-gapped. Genau
  die Kundschaft, für die Nachweisbarkeit von KI-Beteiligung am Code demnächst
  Pflicht ist.
- **Diese Kundschaft kann keine SaaS-Lösung nehmen.** Das Tooling *muss*
  self-hostable sein. Das ist kein Feature, das ist ein struktureller Zwang.
- **Der Git-native Ansatz passt exakt darauf.** Liegen die Daten im Repo, ist
  Self-Hosting keine Portierungsarbeit, sondern der Normalfall. Das Dashboard ist
  ein Reader, kein Dienst mit eigenem Zustand.
- **Ein Plattform-Anbieter baut das ungern nach.** Wer aus dem Cloud-Modell kommt
  und auf die eigene Datenhaltung optimiert, hat kein Interesse an einem Modell,
  das die Daten bewusst *aus* der Plattform heraus ins Repo verlagert. Das ist der
  Graben.

### Warum nicht entire's Weg — die Hosting-Falle

Der nächstgrößere Spieler in diesem Feld, **entire.io** ($60M Seed), ist von
„Git-Companion" zu einem **eigenen, gehosteten, verteilten Git-Netzwerk** gekippt
(„agent-scale cloning without rate limits", „India's fastest Git hosting"). Das ist
für uns bewusst **kein** Vorbild: Ein gehosteter Dienst ist genau das, was
self-managed, on-prem und air-gapped arbeitende, regulierte Kunden nicht nehmen
dürfen. entire's Stärke ist unsere verbotene Zone — und unsere (repo-nativ,
self-hostable, plattform-fungibel) ist die, die ein SaaS-first-Anbieter mit viel
Kapital ungern baut. Wir schlagen sie nicht beim Hosting und versuchen es nicht; wir
besetzen das andere Ufer.

Bestätigung fällt trotzdem ab: entire speichert Checkpoints **ref-basiert** und
integriert Agents über **Hooks** — dieselben zwei Grundentscheidungen wie Minds. Der
Pfad stimmt, nur der Zielpunkt ist ein anderer. Und ihr Ökosystem zeigt den Wert
reicher, offener Daten: Dritt-Werkzeuge wie **Grain** (`scan` → `AGENTS.md`,
`audit` → Provenienz) bauen auf dem erfassten Session-Verlauf auf. Genau solche
Aufsätze wollen wir ermöglichen — repo-nativ statt an einen Host gebunden.

## 5. Wo Minds heute steht (ehrlich)

Ein Rust-Workspace (ein statisches Binary, harte Abhängigkeit nur `git`), der die
v0.1-Kette schließt:

- **Capture** hook-basiert: `minds enable` installiert Agent- + Git-Hooks; der heiße
  Pfad (`minds hook`) schreibt jedes Event ins lokale Journal, der kalte Pfad
  (`minds checkpoint`) redigiert → speichert → hängt einen Trailer an.
- **Redaction** fail-closed: Secrets/PII raus, *bevor* ein Byte in den Store geht.
- **Store** content-adressiert (`SessionId = blake3(canonical_json)`), zwei Backends
  hinter einem Trait: In-Repo (`refs/minds/context`) und Child-Repo. Jede Session
  erscheint zusätzlich als eigener, browserbarer Branch `minds/session/<hash>`.
- **Reader**: `minds render` baut eine zustandslose HTML-Seite — Zeile anklicken →
  Prompt dahinter. `minds show`/`why`/`fsck` schließen den Bug-Retrieval-Loop.

**Bekannte Baustellen** (in `Plan-v0.2.md` adressiert): die Tool-Interpretation ist
noch Claude-only, die Secret-Wall auf dem heißen Pfad kennt nur Claude-Feldnamen,
und der Reader zeigt den erfassten Gesprächsverlauf noch nicht.

## 6. Die Roadmap in Schichten

### Schicht 1 — Das Fundament real machen *(Kurzfrist, kommit-genau in `Plan-v0.2.md`)*
Multi-Agent-Capture (Gemini/Codex/OpenAI echt, nicht nur Claude) + der Session-Branch
als GitLab-nativ lesbares Artefakt (`session.md`). Vorgeschaltet: der Secret-Wall-Fix,
damit fail-closed für *alle* Agents gilt.

### Schicht 2 — Die These schärfen *(Kurzfrist, kommit-genau in `Plan-v0.2.md`)*
Die drei Bausteine, die Minds von „Kontext-Tool" zu „repo-nativer Vertrauensschicht"
heben — und die zugleich das Fundament für Schicht 3 legen:

- **Change-Id** — stabile Änderungs-Identität, überlebt Force-Push/Rebase/Squash.
- **Signierte Attribution** — „Agent X, Modell Y schrieb diese Zeilen", verifizierbar
  statt behauptet.
- **`minds forget`** — redigierbare Nutzlast: DSGVO-Löschung des Inhalts, während die
  Hash-Referenz auflösbar bleibt. Das, was reines Git strukturell nicht kann.

### Schicht 2b — CLI-Vollständigkeit & Kontext-Rückführung *(vor jeder UI)*
Die CLI muss vollständig sein, bevor eine UI kommt. Kern ist die
**Kontext-Rückführung**: `minds recall`/`distill` gibt den erfassten Kontext als
AGENTS.md-artigen Brief an den nächsten Agenten zurück und schließt damit
**Vision-Problem #3** („kein Agent lernt aus dem letzten"), das v0.1 offenließ. Dazu
Parität mit dem, was entire/Grain zeigen: `blame`, `log`, `search`, `recap`,
`agent-help`. Kommit-Zerlegung in `Plan-v0.2.md`.

### Schicht 2c — Metriken & Observability *(opt-in)*
`minds metrics` projiziert die schon erfassten Daten (Tokens, Schritte,
Session-Länge, Agent-Anteil, Redaction-Treffer, Kontext-Abdeckung) in ein
Standardformat (Prometheus/OpenMetrics) — für **Grafana beim Kunden**. Kein doppelter
Zustand, kein selbst betriebener Dienst: wir emittieren in Infrastruktur, die
reguliert arbeitende Teams ohnehin betreiben. Das ist zugleich die billigste
sichtbare Oberfläche, lange bevor eine eigene UI existiert.

### Schicht 3 — Reviews als Git-Objekte *(die große Wette — hier voll ausgearbeitet)*
Siehe Abschnitt 7.

### Schicht 4 — Struktureller Diff & AST-Attribution *(später)*
Der Zeilendiff ist die falsche Einheit. Difftastic-artiger struktureller Diff im
Reader; Attribution von der Zeile auf Symbol/AST-Knoten verfeinert. Löst einen
Großteil der „Konflikte", die in Wahrheit Artefakte des zeilenbasierten Modells sind.

### Schicht 5 — Sync & Mirror *(Transport, kein Ort)*
Ein Sync-Primitiv im git-sync-Stil spiegelt den `refs/minds/*`-Namespace zwischen
Remotes (SSH-Creds wiederverwenden), damit Kontext über eine Agent-Flotte/ein Team
und über Air-Gaps wandert — ohne das Code-Remote zu verschmutzen und **ohne dass wir
irgendetwas hosten**. Verallgemeinert das bestehende Child-Repo-Backend. Eine
optionale, self-hostable Aggregations-/Reader-Fläche über viele Repos ist denkbar,
läuft dann aber **beim Kunden**. Bewusst nach der CLI-Vollständigkeit (Schicht 2b).

---

## 7. Schicht 3 im Detail — Reviews als Git-Objekte

**Das Ziel:** Der Review eines Change — Verdict, Kommentare, Approval — liegt
content-adressiert und signiert unter `refs/minds/reviews/`, wandert mit dem Repo und
überlebt jede Plattform-Migration. GitLab wird zum *Cache* der Wahrheit, nicht zu
ihrer Quelle. Damit wird aus der These ein Produkt.

Baut direkt auf Schicht 2 auf: signierte Identität (wer reviewt) und stabile
Change-Ids (was wird reviewt) sind die Voraussetzung.

> **Stand 29.07.2026: R1–R6 umgesetzt.** Die Zerlegung unten bleibt als Beleg
> stehen, was jeweils gemeint war; die Entscheidungen stehen in
> [ADR-0009](docs/adr/0009-reviews-als-git-objekte.md), der Transport in
> [ADR-0010](docs/adr/0010-ein-ref-je-session.md).

### Phase R1 — Das Review-Objekt ✅
- `docs: ADR — Review als content-adressiertes, signiertes Git-Objekt`
- `feat(core): Review-Envelope (schema_version, subject: Change-Id|SessionId, reviewer: signierte Identität, decision: approve|reject|needs-work, summary, at)`
- `feat(store): put_review/get_review unter refs/minds/reviews/ (gleiches Layout wie Sessions, dedup per Hash)`
- `feat(cli): minds review <change> --approve|--reject|--needs-work [--summary]`
- `feat(cli): minds reviews <change|commit> — Verdicts auflisten, Signaturen prüfen`
- `test: Review-Roundtrip + Signaturprüfung + Rebase-Überleben (subject = Change-Id)`

### Phase R2 — Der Review-Thread (git-bug-Muster) ✅
Diskussion muss mergebar sein — zwei Reviewer offline, beide kommentieren, kein
Konflikt.
- `feat(core): Comment als append-only Operation (content-adressiert), Anker auf datei:zeile ODER Turn`
- `feat(store): Thread als Operations-Log; deterministischer Merge zweier Logs (kommutativ, konfliktfrei)`
- `feat(cli): minds comment <change> --on <datei:zeile|turn> "<text>"`
- `test: zwei divergente Threads mergen konfliktfrei zum selben Zustand`

### Phase R3 — Review pro Change, nicht pro Branch (Gerrit-Lehre) ✅
- `feat: Verdicts hängen an der Change-Id → stacked changes einzeln reviewbar, Kontinuität über Force-Push`
- `feat(cli): minds stack — abhängige Changes und ihren jeweiligen Review-Stand zeigen`
- `test: Force-Push eines Stacks erhält Verdicts pro Change`

### Phase R4 — Die Plattform wird zum Cache ✅
Einweg-Brücke: Git-native Verdicts in GitLab-MR-Notes/Approvals spiegeln, für Teams,
die in der GitLab-UI leben. Quelle der Wahrheit bleibt Git. Migriert man weg, kommt
die Review-Historie mit.
- `feat(minds-gitlab): Verdict → MR-Note/Approval (einweg, idempotent)`
- `feat(minds-gitlab): Webhook-Empfänger (zustandslos) — MR-Kommentar → Review-Objekt (optional, opt-in)`
- `docs: Betriebsmodell — Git ist Quelle, GitLab ist Projektion`

### Phase R5 — Policy als Binary, nicht als YAML ✅
- `feat(cli): minds fsck --require-review — kein agent-authored Change ohne signiertes Verdict`
- `feat(ci): wiederverwendbarer .gitlab-ci.yml-Include, der nur das Binary aufruft (keine YAML-Logik)`
- `test: Gate rot bei fehlendem/ungültigem Verdict, grün sonst`

### Phase R6 — Audit-Export für regulierte Umgebungen ✅
- `feat(cli): minds audit --export — signierte Provenienz-Kette (Change → Session → Attribution → Verdict) als portables Bundle`
- `docs: Nachweis-Leitfaden (was das Bundle beweist, was nicht)`

**Ergebnis von Schicht 3:** Ein Repo trägt seine eigene, kryptografisch nachweisbare
Antwort auf „Wer hat das geschrieben, auf welche Anweisung, wer hat es geprüft, und
warum wurde es gemerged?" — ohne Plattform, ohne Datenbank, offline verifizierbar.

**Nachtrag aus der Umsetzung (29.07.2026).** Beim Testen fiel auf, dass `git push`
mit aktiviertem Minds ~1,9 s länger dauerte — auf *jedem* Push, auch ohne neuen
Kontext. Die Ursache war nicht der Hook, sondern die Datenstruktur: Der ganze Store
hing an einem Ref, und der war damit Serialisierungspunkt für Schreiben *und*
Pushen. Aufgelöst in [ADR-0010](docs/adr/0010-ein-ref-je-session.md): ein Ref je
Session, Kanten bei ihrer Session, ein Push für alle Refs. Ein Repo, das nur
eincheckt, hat danach **keinen gemeinsam beschriebenen Ref mehr**; der Hook kostet
ohne neuen Kontext 0,02 s statt 1,86 s. Dieselbe Lehre, die `entireio/cli` beim
Umstieg von `entire/checkpoints/v1` auf `refs/entire/` gezogen hat.

Nebenbei behoben: `refs/minds/reviews` wurde vorher nie gepusht — Schicht 3 war
damit nicht teamfähig.

---

## 8. Der ehrliche Teil (die Risiken)

- **Die Agent-Adapter sind Fleißarbeit ohne Ende.** Jeder Agent hat sein eigenes
  Format, und die ändern sich ständig. Gegenmittel: ein Adapter-Trait mit
  Golden-Fixtures pro Agent, versioniertes Schema, toleranter Reader (Schicht 1).
- **GitLab könnte es selbst bauen.** Gegenargument: Git-nativ und self-hostable ist
  strukturell schwer für einen Anbieter, der auf die eigene Datenhaltung optimiert.
- **Adoption braucht das Team, nicht den Einzelnen.** Der Wert entsteht im Review.
  Ein Solo-Dev braucht Schicht 3 nicht. Gegenmittel: Schicht 1+2 liefern schon
  Einzelwert (sicherer, signierter, durchsuchbarer Record).
- **Die These könnte zu früh sein.** Heute schmerzt es Teams, die aggressiv mit
  Agent-Flotten arbeiten. Das sind noch nicht viele — aber es werden schnell mehr,
  und die Regulierungs-Welle kommt ohnehin.

## 9. Der Pitch in drei Sätzen

> Zwanzig Jahre lang hatte Softwareentwicklung eine Form: Diff, Review, Merge. Diese
> Form hält gerade nicht mehr, weil Agents mehr Code produzieren, als ein Mensch
> lesen kann — und weil der Grund für jede Änderung verschwindet, sobald die Session
> endet.
>
> Minds schreibt den Kontext, die Identität und am Ende auch den Review dorthin, wo
> sie hingehören: in Git, neben den Code — als signierte, redigierbare, mit dem Repo
> wandernde Objekte. Nicht in eine fremde Cloud, nicht in eine Plattform-Datenbank.
>
> Und GitLab ist der richtige Einstiegspunkt, weil dort die Leute sitzen, die KI-
> Beteiligung am Code nachweisen müssen — und die nichts nehmen können, was in
> fremder Cloud läuft.
