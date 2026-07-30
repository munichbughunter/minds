/
Minds

Minds
Auf Basis von https://entire.io nur in Rust und speziell für GitLab!

# Intent Layer für GitLab

*Eine Idee. Noch kein Produkt, noch keine Zeile Code.*

---

## Der Einzeiler

**GitLab weiß, *was* sich geändert hat. Es weiß nicht, *warum*. Solange Menschen den Code geschrieben haben, war das egal — der Grund saß im Kopf des Autors und im MR-Text. Jetzt schreiben Agents den Code, und der Grund verdampft, sobald das Terminal-Fenster zugeht.**

Die Idee: Den Kontext einer Agent-Session dort speichern, wo er hingehört — **in Git selbst**, neben dem Code — und daraus ein GitLab-natives Review-Erlebnis bauen.

---

## Das Problem, in vier Ausprägungen

### 1. Der Merge Request ist zum Ratespiel geworden

Ein MR zeigt einen Diff. Früher stand dahinter ein Mensch, den man fragen konnte. Heute steht dahinter ein Agent, eine Session, ein Prompt — und nichts davon ist im MR.

Der Reviewer sieht 400 geänderte Zeilen und eine Beschreibung, die der Agent selbst geschrieben hat. Er weiß nicht: Was wurde eigentlich verlangt? Welche Ansätze wurden verworfen und warum? Welche Constraints galten? Hat der Agent einen Test gelöscht, weil er falsch war — oder weil er rot war?

**Die Frage "was hat sich geändert" ist beantwortbar. Die Frage "war das die richtige Änderung" nicht.**

### 2. Diff-Review skaliert nicht mit

Ein Team, das ernsthaft mit Agents arbeitet, produziert kein Dutzend MRs pro Woche mehr, sondern Dutzende pro Tag. Zeile-für-Zeile-Review ist bei dem Volumen physisch nicht machbar.

Das Ergebnis ist heute schon zu beobachten: Reviews werden zum Durchwinken. Das Qualitätstor existiert formal weiter und filtert nichts mehr.

### 3. Kein Agent lernt aus dem, was der letzte gemacht hat

Jede Session fängt bei null an. Der Agent, der morgen dasselbe Modul anfasst, kennt die Diskussion von gestern nicht, kennt die verworfene Sackgasse nicht, und läuft ordentlich wieder rein.

Das Wissen war da. Es lag in einem Terminal-Buffer und ist weg.

### 4. Die Audit-Lücke

Wer hat diese Zeile geschrieben — Mensch oder Maschine? Welches Modell? Auf welche Anweisung hin?

`git blame` sagt "Patrick Döring". Das ist inzwischen eine Halbwahrheit. In regulierten Umgebungen ist das kein Schönheitsfehler, sondern ein Nachweisproblem — und die Anforderungen an Nachvollziehbarkeit von KI-gestützter Produktion werden nicht kleiner.

---

## Die Idee

**Intent wird zu einem versionierten Artefakt — genau wie Code.**

Drei Bausteine:

### Capture
Ein CLI-Hook fängt die Agent-Session ab: Prompt, Reasoning, Tool-Calls, verworfene Pfade, Token-Verbrauch. Secrets und PII werden lokal rausgefiltert, bevor irgendwas geschrieben wird.

### Store — und hier liegt der Trick
Das landet **nicht** in einer neuen Datenbank und **nicht** in einem SaaS-Silo. Es landet in Git:

- Session-Daten auf einem separaten Branch (reines JSON, kein Code, kein Diff-Lärm)
- Verlinkt über einen Commit-Trailer/Checkpoint, nicht über den Commit-Hash — überlebt damit Rebase, Squash und Cherry-Pick
- Während der Arbeit auf lokalen Shadow-Branches, die nie gepusht werden

Konsequenz: **Der Kontext wandert mit dem Repo.** Über Remotes hinweg, offline, im Air-Gap. Kein Vendor, kein Lock-in, kein zusätzlicher Dienst, der ausfallen kann.

### Surface
Die GitLab-Integration ist der eigentliche Wert:

- **MR-Widget**: Intent zuerst, Diff zweitens. Der Reviewer liest, was verlangt wurde, und prüft dann, ob der Code das tut.
- **`blame` bis zum Prompt**: Zeile anklicken → die Session sehen, die sie erzeugt hat.
- **Attribution pro Commit**: „73% Agent (146/200 Zeilen)" — maschinenlesbar, als Trailer/Checkpoint im Commit.
- **CI-Gate**: Pipeline-Regel „kein agent-authored Change ohne erfassten Kontext". Policy as Code, in der `.gitlab-ci.yml`.

---

## Warum GitLab — und warum das kein Zufall ist

Das ist der Teil, der die Idee von „nettes Tool" zu „verteidigbare Position" macht:

**GitLab ist da, wo die regulierten Läden sitzen.** Banken, Versicherer, öffentliche Hand, Automotive, Industrie. Self-Managed, oft on-prem, teils air-gapped. Genau die Kundschaft, für die Nachweisbarkeit von KI-Beteiligung am Code keine Kür ist, sondern demnächst Pflicht.

**Und genau diese Kundschaft kann keine SaaS-Lösung nehmen.** Das Tooling *muss* self-hostable sein. Das ist keine Feature-Entscheidung, das ist ein struktureller Zwang — und einer, den ein SaaS-first-Anbieter ungern erfüllt. Wer aus dem Cloud-Modell kommt, baut das nicht gern nach.

**Der Git-native Ansatz passt exakt darauf.** Wenn die Daten im Repo liegen, ist Self-Hosting keine Portierungsarbeit — es ist der Normalfall. Das Dashboard ist ein Reader, kein Dienst mit eigenem Zustand.

**GitLab hat die Andockpunkte schon**: MR-Widgets, CI, Webhooks, die API. Man muss GitLab nicht ersetzen. Man setzt sich obendrauf.

---

## Wie es technisch aussehen würde

| Baustein | Ansatz |
|---|---|
| **CLI** | Rust, auf `gix` (gitoxide) — pure Rust, keine libgit2-Abhängigkeit, ein statisches Binary. Alles, was gebraucht wird, ist client-seitiges Git: Refs, Packs, History-Walk, Diff. Genau gix' Stärke. |
| **Storage** | Git. Sonst nichts. |
| **Dashboard** | Reader über den Kontext-Branch. Zustandslos, damit trivial deploybar. |
| **GitLab-Anbindung** | MR-Widget via API + Webhooks. CI-Job für das Policy-Gate. |
| **Deployment** | Container, K8s, per Argo CD ausgerollt. Passt in den Stack, den diese Läden sowieso fahren. |

**Der Einstieg ist klein**: Das Dashboard ist ein Leseproblem — Branch fetchen, JSON parsen, rendern. Kein verteiltes System, keine Datenbank, kein Betrieb. Das ist ein Wochenendprojekt für die erste Version, kein Firmengründungsakt.

---

## Der ehrliche Teil

**Was daran schwer ist:**

- **Die Agent-Adapter.** Jeder Agent hat sein eigenes Transkript-Format, und die ändern sich ständig. Das ist Fleißarbeit ohne Ende — und es ist der eigentliche Aufwand, nicht die schöne Architektur.
- **GitLab könnte es selbst bauen.** Kann jederzeit passieren. Gegenargument: Git-nativ und self-hostable ist strukturell schwerer für einen Anbieter, der auf seine eigene Datenhaltung optimiert.
- **Adoption braucht das Team, nicht den Einzelnen.** Der Wert entsteht im Review. Ein Solo-Dev braucht das nicht.
- **Die These könnte zu früh sein.** Heute schmerzt es Teams, die aggressiv mit Agent-Flotten arbeiten. Das sind noch nicht viele.

**Was daran gut ist:**

- Der Schmerz ist real und heute schon spürbar, nicht in drei Jahren.
- Der Einstieg ist klein und liefert sofort Wert — auch ohne dass irgendwer eine Plattform adoptiert.
- Git-nativ heißt: nichts kaputt zu machen. Wer es nicht nutzt, merkt nichts. Ein Branch, ein Trailer/Checkpoint, fertig.
- Die Regulierungs-Welle kommt sowieso. Attribution wird von „interessant" zu „gefordert".

---

## Der Pitch in drei Sätzen

> Zwanzig Jahre lang hatte Software-Entwicklung eine Form: Diff, Review, Merge. Diese Form hält gerade nicht mehr, weil Agents mehr Code produzieren, als ein Mensch lesen kann — und weil der Grund für jede Änderung verschwindet, sobald die Session endet.
>
> Die Idee ist unspektakulär: den Kontext dorthin schreiben, wo er hingehört — in Git, neben den Code. Dann kann der Reviewer die Absicht lesen statt nur den Diff, der nächste Agent lernt aus dem letzten, und der Auditor bekommt eine Antwort.
>
> Und GitLab ist der richtige Ort dafür, weil dort die Leute sitzen, die das nachweisen müssen — und die nichts nehmen können, was in fremder Cloud läuft.

---

## Der erste Schritt

Ein Reader für den Kontext-Branch. Rust, `gix`, liest, rendert, sonst nichts.

Wenn *das* Ding auf einem echten Repo den Moment erzeugt, in dem jemand auf eine Zeile klickt und den Prompt dahinter sieht — dann lohnt sich die zweite Frage. Wenn nicht, war es ein Wochenende.


Wie kann ich dir heute helfen?
Zuletzt verwendet
Kanonische JSON-Serialisierung implementieren
vor 13 Minuten
Session-Envelope mit serde und schema_version
vor 31 Minuten
Rust Projekt Setup mit einfacher Installation
vor 2 Stunden
Schrittweise Umsetzung mit modularer Repo-Architektur
gestern
Anweisungen

Anweisungen hinzufügen, um Claudes Antworten anzupassen
Speicher
Nur du

Projekterinnerung wird hier nach ein paar Chats angezeigt.
Kontext
1% der Projektkapazität verwendet

Geplant

Richte wiederkehrende Aufgaben für dieses Projekt ein.

    Unterhaltung nicht gefunden.

Plan v0.1.0
# Minds — Umsetzungsplan v0.1
 
*Begleitdokument zur `Vision`. Kleine Schritte, kleine Commits, tragfähige Architektur.*

> **Dokumentenkarte — Lesereihenfolge:**
> 1. `Plan.md` (dieses Dokument) — Vision + Umsetzung bis v0.1.
> 2. `Plan-v0.2.md` — der nächste, kommit-genaue Zyklus (Multi-Agent-Capture,
>    Change-Id, signierte Attribution, `minds forget`, Bereitstellung).
> 3. `Roadmap.md` — das vorzeigbare Strategie-Dokument mit der großen Wette
>    (Reviews als Git-Objekte) vollständig in Phasen zerlegt.
>
> Leitlinie ab v0.2: *mehr ins Repo, weniger in die Plattform* — jedes neue
> Artefakt fragt zuerst „geht das als Git-Objekt?".
 
---
 
## TL;DR — die zwei Entscheidungen aus dem Kollegen-Feedback
 
**1. Parent-Repo (Code) vs. Child-Repo (Kontext).**
Nicht entweder/oder. Der Speicher liegt hinter einem `ContextStore`-Trait, beide Backends
benutzen **dasselbe content-adressierte Layout**. Damit ist der Unterschied nur eine
Config-Zeile, kein Rewrite.
 
- Der **Verweis** (Trailer/Checkpoint `Minds-Session-Id: <hash>`) bleibt *immer* im Production-Commit.
  Er ist winzig und wandert mit dem Code — auch über Rebase, Squash, Cherry-Pick.
- Die **Nutzlast** (die Session als JSON) liegt wahlweise im selben Repo (`refs/minds/context`)
  oder im Child-Repo. Umschaltbar ohne Code-Änderung.
- **v0.1: In-Repo-Backend** (null Setup für den Tester). Child-Repo kommt als Config-Option
  direkt danach (M4).
 
**2. Tokens / Headroom.**
Zwei Fragen, die nicht dasselbe sind:
 
- *Verbrennt Minds Tokens?* In v0.1 **nein** — der Intent wird deterministisch extrahiert,
  kein LLM. Token-Minimierung ist also kein v0.1-Thema.
- *Verbrennen die Agents Tokens?* Ja, massiv — das ist Headrooms Job. Headroom sitzt **vor**
  dem LLM (komprimiert Input/Output), Minds sitzt **hinter** der Session (dauerhafter Record).
  Sie überlappen nicht, sie komponieren: Agent mit Headroom wrappen, Session mit Minds capturen.
 
Details zu beidem weiter unten in eigenen Abschnitten.
 
---
 
## Zielbild v0.1 (das, was dein Kollege testet)
 
Auf einem echten Repo:
 
1. Eine Agent-Session wird erfasst (**ein** Adapter reicht — der, den ihr benutzt).
2. Secrets/PII werden **vor dem Schreiben** rausgefiltert (fail-closed).
3. Die Session landet content-adressiert in Git (In-Repo-Backend).
4. Der Production-Commit bekommt einen Trailer/Checkpoint, der auf die Session zeigt.
5. `minds show <commit>` zeigt den Intent + Attribution.
6. `minds why <datei>:<zeile>` zeigt die Session, die genau diese Zeile erzeugt hat.
7. `minds render` baut eine statische HTML-Seite: **Zeile anklicken → Prompt sehen.**
 
Kein GitLab, keine Datenbank, kein Dienst. Genau der Moment aus der Vision — nicht mehr.
GitLab-Widget, Child-Repo, Headroom-Anbindung sind alles *nach* v0.1.
 
---
 
## Architektur
 
### Prinzipien (die Entscheidungen, die alles andere tragen)
 
1. **Content-Adressierung.** `SessionId = blake3(canonical_json(session))`. Der Hash *ist* die ID.
   Folgen: Dedup gratis, verifizierbar, Trailer/Checkpoint überlebt Rebase/Squash/Cherry-Pick (er steht in
   der Commit-Message, nicht am Hash), und Caching (z. B. von Summaries) fällt kostenlos ab.
2. **Speicher ist ein Trait, kein Ort.** `ContextStore` mit zwei Implementierungen, die sich nur
   im Git-Handle unterscheiden. Siehe Kollegen-Entscheidung 1.
3. **Redaction ist Pflicht und fail-closed.** Läuft *vor* jedem Byte, das in den Store geht.
   Kann sie nicht laufen, bricht Capture ab. Kein „später filtern".
4. **Adapter hinter einem Trait, Schema versioniert.** `SessionAdapter` pro Agent, jede Session
   trägt `schema_version`. Der Reader toleriert alte Versionen. Das ist die Antwort auf den
   „ehrlichen Teil" der Vision (Formate ändern sich ständig).
5. **gix für Reads + Objekt-Writes; `git`-Shell nur als Fallback hinter einem Trait**
   (z. B. Blame, solange gitoxide-Blame jung ist). Ziel bleibt das eine statische Rust-Binary;
   pragmatisch dahin, ohne sich früh zu verrennen.
6. **Der Reader ist zustandslos.** Ref fetchen, JSON parsen, rendern. Kein Betrieb.
 
### Workspace-Layout
 
```
minds/                      # cargo workspace
├─ crates/
│  ├─ minds-core/           # Domänen-Typen, Session-Envelope, Hashing, Trailer/Checkpoint — KEIN I/O
│  ├─ minds-redact/         # Redaction-Pipeline (Secrets/PII), fail-closed
│  ├─ minds-git/            # dünne gix-Helfer: Refs, Revwalk, Blob-RW, Trailer/Checkpoint, Diff, Blame-Trait
│  ├─ minds-store/          # ContextStore-Trait + InRepoStore + ChildRepoStore
│  ├─ minds-capture/        # SessionAdapter-Trait + Adapter (erstmal einer)
│  ├─ minds-cli/            # das `minds`-Binary
│  └─ minds-reader/         # statischer HTML-Renderer über den Store (die "Surface" für v0.1)
├─ xtask/                   # Dev-Tasks (Fixtures, Release)
└─ Cargo.toml
```
 
Später: `minds-gitlab` (API + Webhooks), `minds-ci` (Policy-Gate). Bewusst noch nicht angelegt.
 
### Abhängigkeits-Richtung (wichtig für saubere Schnitte)
 
```
core  ←  redact
core  ←  git
core, git  ←  store
core, redact  ←  capture
core, store, capture, git, redact  ←  cli
core, store, git  ←  reader
```
 
`minds-core` hängt von nichts (außer serde/blake3) und hat **kein I/O**. Alles Testbare ist dort
oder in `redact` — beide ohne Git, ohne Netz, reine Funktionen mit Golden-Tests.
 
### Das Session-Envelope (Schema v1, Skizze)
 
```
Session {
  schema_version: 1,
  agent: { name, version },
  model: { provider, id },
  intent: {
    request: String,          // was verlangt wurde (deterministisch extrahiert)
    constraints: [String],
    discarded: [String],      // verworfene Pfade, best-effort
  },
  turns: [ Turn { role, text, tool_calls: [...] } ],
  usage: { input_tokens, output_tokens },
  produced: { commit_hint?, files: [String] },
  redaction: { applied: true, counts: { secrets, pii } }  // Zähler, nie Werte
}
```
 
Trailer/Checkpoint im Production-Commit:
```
Minds-Session-Id: b3-<hash>
Minds-Attribution: agent=73% (146/200)
```
Mehrere Sessions pro Commit ⇒ mehrere Trailer/Checkpoint-Zeilen. Squash mehrerer Commits ⇒ die Trailer/Checkpoint
sammeln sich, was genau richtig ist (mehrere Sessions haben beigetragen).
 
### Speicher-Layout im Store (für beide Backends identisch)
 
```
sessions/b3/<rest-des-hashes>.json     # content-adressiert, flach, dedup-freundlich
index.json                             # optional: Hash → {commit, files, ts} für schnellen Reader
```
 
- **In-Repo:** dieser Baum unter dem eigenen Ref `refs/minds/context` (kein normaler Branch,
  taucht nicht in `git branch` auf, wird nur mit expliziter Refspec gepusht/gefetcht).
- **Child-Repo:** identischer Baum, nur in einem separaten Repo.
- **Lokales Staging:** während der Arbeit unter `refs/minds/local/*` — nie in der Push-Refspec,
  bleibt also lokal (die „Shadow-Branches" der Vision).
 
---
 
## Kollegen-Entscheidung 1: Parent/Child im Detail
 
**Warum das Child-Repo eine gute Idee ist** (dein Kollege liegt richtig):
 
- Production-Repo bleibt sauber und klont schnell — Session-Transkripte können groß werden.
- Eigene Access-Control und eigene Retention möglich.
- Kein CI-Lärm im Parent durch Kontext-Pushes.
 
**Warum man es trotzdem abstrahiert statt festlegt:**
 
- Der Wert der Vision ist „Git-nativ, self-hostable". Ein Child-Repo *in derselben GitLab-Instanz*
  bleibt beides — kein Vendor, kein SaaS. Die Value-Prop ist bei beiden Backends intakt.
- Das In-Repo-Backend ist bei *Single-Clone / Air-Gap* im Vorteil (alles in einem Klon).
- Das Child-Repo ist bei *großen Orgs* im Vorteil (Trennung, Performance).
- Weil das Layout identisch ist, ist die Wahl reine Config.
 
**Die eine Regel, die man nicht brechen darf:** Der **Trailer/Checkpoint bleibt im Parent-Commit**, egal
welches Backend. Sonst bricht Bug-Retrieval, sobald jemand nur das Parent ausgecheckt hat.
 
### Bug-Retrieval-Flow (die explizite Kollegen-Anforderung)
 
```
Buggy-Zeile
  → git blame            → Commit
  → Trailer/Checkpoint lesen        → Minds-Session-Id
  → Store auflösen       → Session-JSON  (In-Repo-Ref ODER Child-Repo, on demand gefetcht)
  → rendern              → Prompt, Reasoning, verworfene Pfade
```
 
`minds why <datei>:<zeile>` macht das in einem Kommando. Beim Child-Backend fetcht das Tooling
den Child-Ref bei Bedarf. Ist das Child-Repo gerade nicht erreichbar (Air-Gap), hat man immer
noch Trailer/Checkpoint + Commit und kann später nachladen — **graceful degradation**, kein harter Fehler.
 
---
 
## Kollegen-Entscheidung 2: Tokens & Headroom
 
**Was Headroom ist** (kurz, nach Blick ins Repo): ein lokaler Kompressions-Layer, der Tool-Outputs,
Logs, RAG-Chunks und History komprimiert, *bevor* sie ans LLM gehen — als Library, Proxy oder
MCP-Server, reversibel (Originale liegen mit TTL im Cache), und mit einem `wrap`-Kommando für
Claude Code, Codex, Cursor, Aider u. a. Primär Python, Kern teils Rust, Apache-2.0.
 
**Wo Tokens in Minds überhaupt anfallen:**
 
| Stelle | Tokens? | v0.1-Antwort |
|---|---|---|
| Capture (Transkript parsen) | 0 | rein deterministisch |
| Intent-Extraktion für Widget | 0 | erste User-Message + finale Assistant-Message extrahieren, kein LLM |
| Optionale LLM-Summary | ja | **nicht in v0.1**; wenn, dann mit Caching (s. u.) |
 
**Rollen-Trennung Headroom ↔ Minds (sie überlappen nicht):**
 
- Headroom sitzt **vor** dem LLM und schrumpft den Kontext im laufenden Betrieb.
- Minds sitzt **hinter** der Session und schreibt den dauerhaften Record.
- Headrooms Cache ist **ephemer (TTL)**. Minds ist **permanent, versioniert, audit-fähig**.
  Für Bug-Retrieval willst du den dauerhaften — also Minds, nicht Headrooms TTL-Cache.
  Sie reichen sich sauber die Hand.
 
**Zwei konkrete Synergien (für nach v0.1, M8):**
 
1. **Ein Adapter statt zwölf.** Headroom normalisiert bereits viele Agents hinter *einem* Proxy.
   Minds könnte Headrooms Event-Stream als *einen* cross-agent Capture-Adapter anzapfen — und
   damit genau die Fleißarbeit umgehen, die die Vision als schwerste benennt (jeder Agent ein
   eigenes Format). Optional, nicht Pflicht.
2. **Token-Minimierung, falls Summaries kommen.** Dann: (a) Summary pro `SessionId` **cachen** —
   gleiche Session = einmalige Kosten, nie wieder; (b) vor dem Modell deterministisch
   vorfiltern (Tool-Call-Rauschen raus); (c) nur on-demand summarizen. Punkt (a) fällt aus der
   Content-Adressierung gratis ab.
 
**Wichtige Einschränkung:** Headroom nicht ins Minds-Binary einbetten (Python vs. „ein statisches
Rust-Binary"). Als *externen, optionalen Prozess* behandeln. Minds' harte Abhängigkeit bleibt:
Git. Sonst nichts — wie in der Vision.
 
---
 
## Schritt-für-Schritt: Milestones bis v0.1
 
Jede Zeile ist ein kleiner, in sich abgeschlossener Commit (Conventional Commits).
Reihenfolge ist so gewählt, dass jeder Commit grün bleibt.
 
### M0 — Gerüst & Schienen
- `chore: cargo workspace, rust-toolchain, edition, MSRV festlegen` (implementiert)
- `chore(ci): fmt + clippy -D warnings + test als GitLab-CI-Job`    (implementiert)
- `chore: cargo-deny (deny.toml) für Lizenzen/Advisories`
- `chore: commitlint / Conventional-Commits-Konvention`
- `docs: ADR-0001 content-adressiert + Trailer/Checkpoint als Verlinkung`
- `docs: ADR-0002 Speicher-Backend als Trait (In-Repo default, Child-Repo Option)`
 
### M1 — `minds-core` (kein I/O)
- `feat(core): Session-Envelope + serde + schema_version` (implementiert)
- `feat(core): kanonische JSON-Serialisierung (stabile Key-Reihenfolge)` (implementiert)
- `feat(core): blake3-Hash → SessionId` (implementiert)
- `test(core): Golden-Tests Kanonisierung + Hash-Stabilität` (implementiert)
- `feat(core): Trailer/Checkpoint-Typen + parse/format` (implementiert)
- `feat(core): Attribution-Modell (human/agent-Zeilen, Modell-ID, Prompt-Ref)` (implementiert)
 
### M2 — `minds-redact` (fail-closed)
- `feat(redact): Redactor-Trait + Pipeline-Skelett` (implementiert)
- `feat(redact): Secret-Detektoren (High-Entropy, bekannte Token-Formen)` (implementiert)
- `feat(redact): PII-Detektoren (E-Mail etc.) + Allow-/Denylist-Config` (implementiert)
- `feat(redact): fail-closed-Garantie + Redaction-Audit (nur Zähler, keine Werte)` (implementiert)
- `test(redact): Korpus-Tests inkl. False-Positive/Negative-Fixtures` (implementiert)
 
> Minimal-Pfad zu v0.1: Secrets-Erkennung reicht zunächst, solange **fail-closed** steht.
> PII-Feinschliff kann direkt nach v0.1 kommen.
 
### M3 — `minds-git` (gix)
- `feat(git): Repo öffnen, HEAD auflösen, Revwalk` (implementiert)
- `feat(git): Blobs/Trees unter einem Ref lesen/schreiben` (implementiert)
- `feat(git): Custom-Ref refs/minds/context anlegen/aktualisieren (orphan)` (implementiert)
- `feat(git): Trailer/Checkpoint aus Commit-Message lesen → SessionId` (implementiert)
- `feat(git): Trailer/Checkpoint beim Commit anhängen (+ amend-Helfer zum Nachrüsten)` (implementiert)
- `feat(git): BlameProvider-Trait; gix-Impl, git-Shell-Fallback dahinter` (implementiert)
- `test(git): Integrationstests auf Temp-Repo` (implementiert)
 
### M4 — `minds-store`
- `feat(store): ContextStore-Trait (put/get/exists/list nach SessionId)` (implementiert)
- `feat(store): InRepoStore über refs/minds/context, content-adressierte Pfade` (implementiert)
- `feat(store): idempotentes put (Dedup per Hash)` (implementiert)
- `test(store): Roundtrip + Dedup + Rebase-Simulation (Trailer/Checkpoint überlebt)` (implementiert)
- `feat(store): ChildRepoStore (gleiches Layout, separates Repo-Handle) — per Config` (implementiert)
- `test(store): Child-Repo-Roundtrip` (implementiert)
 
### M5 — `minds-capture` (Hook-basiert, mehrere Agents)

> **Kurswechsel gegenüber v0.1-Skizze (ADR-0003).** Der ursprüngliche Plan las
> die Transkript-Dateien der Agents im Nachhinein. Das wird ersetzt durch den
> **Hook-Ansatz** (wie [entire.io](https://entire.io)): Minds installiert Hooks
> *im Agenten selbst* und nimmt jedes Event live über `minds hook` entgegen. Der
> Grund steht in `docs/adr/0003-hooks-statt-transkript-parsing.md` — kurz: nur
> ein Beobachter mit *einer* Uhr, der jeden Agent-Hook empfängt, kann die
> Reihenfolge über Agents hinweg **beobachten** statt raten. Das Transkript
> bleibt Zweitquelle für den reichen Inhalt (Volltext, Thinking, Token).

- M5.0 `docs: Plan.md — M5 von Transkript-Parsing auf Hooks umgestellt (ADR-0003)`
- M5.1 `feat(core): Lineage, Turn.parent/at, ToolCall.effect, Vec<Edge> (additiv)`
- M5.2 `feat(capture): JournalEvent + Journal (Datei pro Event, 0600, fsck-fähig)`
- M5.3 `feat(cli): minds hook — stdin lesen, anhängen, Exit 0, immer`
- M5.4 `feat(capture): Normalisierer Claude Code (SessionStart/Prompt/Tool*/Stop/Subagent*)`
- M5.5 `feat(capture): Secretfile-Mauer bei PreToolUse`
- M5.6 `feat(capture): Journal + Transkript → Vec<Session> (Turns, Usage, Intent)`
- M5.7 `feat(capture): Kanten — Subagent (Observed), Artefakt-Hash, Commit`
- M5.8 `feat(cli): minds enable --agent <name> (Hook-Config mergen, idempotent)`
  - Claude Code → `.claude/settings.json`, Codex → `.codex/hooks.json`
    (+ `codex_hooks = true` in `config.toml`), Cursor → `.cursor/hooks.json`,
    Gemini → `.gemini/settings.json`, OpenCode → TypeScript-Plugin.
  - Zusätzlich Git-Hooks (`post-commit`/`prepare-commit-msg`): Checkpoints
    entstehen, wenn du oder der Agent committen.
- M5.9 `test(capture): Fixtures — single, subagent, zwei Agents parallel`
 
### M6 — `minds-cli` (verdrahtet End-to-End, Hook-basiert)

> **Kurswechsel gegenüber v0.1-Skizze (ADR-0003).** Der ursprüngliche M6 hatte
> ein `minds capture <transkript>`, das ein Transkript-Argument einliest. Unter
> dem Hook-Ansatz gibt es das nicht mehr: Der heiße Pfad (`minds hook`, M5.3)
> hat die Events schon ins **Journal** geschrieben; der kalte Pfad liest das
> Journal (+ Transkript) und heißt deshalb **`minds checkpoint`**. Die
> Git-Hooks sind bereits von `minds enable` (M5.8) installiert — M6 implementiert
> nur noch die Kommandos, die sie aufrufen.

- M6.0 `docs: Plan.md — M6 auf den Hook-Ansatz (checkpoint statt capture <transkript>)`
- M6.1 `feat(cli): minds enable schreibt Backend/Ref in .git/config; Store-Config laden`
- M6.2 `feat(cli): minds checkpoint — Journal → redact → Store → Trailer (post-commit)`
- M6.3 `feat(cli): minds show <commit> → Intent + Attribution`
- M6.4 `feat(cli): minds why <datei>:<zeile> → Session hinter der Zeile (blame → Trailer)`
- M6.5 `feat(cli): minds fsck — jeder Trailer auflösbar? Journal-Lücken? Waisen melden`
- M6.6 `test(cli): End-to-End auf Scratch-Repo — hook → checkpoint → show/why schließt den Loop`
 
### M7 — `minds-reader` (der Magic Moment) → Tag v0.1
- `feat(reader): Kontext-Ref fetchen, Sessions parsen, Index bauen`
- `feat(reader): statisches HTML — Datei-View, Zeile klickbar → Session-Panel`
- `feat(reader): Intent-first-Zusammenfassung (deterministisch, 0 Tokens)`
- `feat(reader): minds render --out ./site (zustandslos, ein Binary)`
- `polish(reader): Styling, Empty-States`
- `chore: Release v0.1-alpha taggen`
 
**Minimaler Pfad zu v0.1**, falls die Zeit knapp wird: M0 → M1 → M2 (nur Secrets) → M3 → M4
(nur InRepoStore) → M5 (ein Adapter) → M6 → M7. Alles mit „Option/später" markierte weglassen.
 
---
 
## Danach (nach v0.1)
 
### M8 — Token-/Summary-Strategie & Headroom
- Deterministische Extraktion bleibt Default (0 Tokens).
- Optionaler LLM-Summary-Pfad **mit Content-Hash-Caching** (gleiche Session nie zweimal).
- Optional: Summary-Calls durch Headroom-Proxy; Session vorher deterministisch entrümpeln.
- Optional: **Headroom-Proxy als cross-agent Capture-Adapter** (spart Adapter-Fleißarbeit).
 
### M9 — GitLab-Surface
- MR-Widget via API (Status/Note/Report-Artefakt).
- Webhook-Empfänger (zustandslos).
- CI-Policy-Gate als wiederverwendbarer `.gitlab-ci.yml`-Include: `minds fsck --require-context`
  („kein agent-authored Change ohne erfassten Kontext").
 
### M10 — Attribution & Audit-Härtung
- Optional signierte Sessions, Provenance, `minds audit`-Export für regulierte Umgebungen.
 
---
 
## Definition of Done — v0.1 (Testprotokoll für den Kollegen)
 
Auf einem echten Repo muss reproduzierbar gelten:
 
1. `minds enable` konfiguriert Backend + Refspec, ohne den normalen Git-Workflow zu stören.
2. Eine echte Agent-Session wird via `minds capture` erfasst; Redaction läuft und ist fail-closed
   (Test: absichtlich ein Fake-Secret einbauen → darf **nicht** im Store landen).
3. Der Commit trägt einen `Minds-Session-Id`-Trailer/Checkpoint; nach einem `git rebase` ist er noch da.
4. `minds show <commit>` zeigt Intent + Attribution.
5. `minds why <datei>:<zeile>` zeigt die richtige Session.
6. `minds render` erzeugt eine HTML-Seite; ein Klick auf eine Zeile zeigt den Prompt dahinter.
7. `minds fsck` läuft grün (kein Waisen-Trailer/Checkpoint).
8. Wer Minds nicht nutzt, merkt nichts: keine neuen sichtbaren Branches, kein geänderter
   Standard-Workflow.
 
Wenn Punkt 6 auf einem echten Repo den Moment erzeugt — Zeile anklicken, Prompt sehen —,
lohnt sich die zweite Frage. Wenn nicht, war es ein Wochenende.
 
