---
name: feature
description: >
  Vollständige Feature-Loop für Minds: Branch anlegen, Goal aus dem
  Milestone-Plan ableiten, implementieren, CI-Triade, Code-Review und
  Security-Review durch Subagents, iterieren bis clean, genau ein
  Conventional Commit. Aufrufen mit /feature <Feature-Beschreibung>
  oder /feature <Milestone-Referenz, z. B. M5.4>.
---

# Feature-Loop für Minds

Du führst ein Feature von der Idee bis zum sauberen Commit. Das Ergebnis
ist **genau ein Conventional Commit** auf einem eigenen Branch. Weiche
niemals von den nicht verhandelbaren Invarianten ab (siehe unten).

## Eingabe

Feature-Beschreibung oder Milestone-Referenz: $ARGUMENTS

Ist die Eingabe eine Milestone-Referenz (z. B. "M5.4"), lies zuerst den
zugehörigen Plan/ADR im Repository und leite das Goal daraus ab.

## Nicht verhandelbare Invarianten (Abbruchkriterien)

Jede dieser Verletzungen ist ein harter Blocker — nicht "später fixen",
sondern sofort korrigieren, bevor es weitergeht:

1. **Evidence vs. Ableitungen:** Beweismittel werden einmal erfasst,
   sind irreversibel und content-addressed. Ableitungen sind jederzeit
   rekonstruierbar und betreten **niemals** den Capture-Pfad
   (`minds-capture`). Lesezugriff nur über `minds-reader`.
2. **Fail-closed Redaction:** Kein Codepfad darf eine un-redigierte
   `Session` persistieren. `RedactedSession` bleibt ohne öffentlichen
   Konstruktor. Hooks bleiben fail-open (immer Exit 0, `catch_unwind`),
   Redaction bleibt fail-closed.
3. **Git-Invisibility:** `git fsck` bleibt clean. Alle Schreibzugriffe
   ausschließlich unter `refs/minds/`. Nutzer ohne Minds merken nichts.
4. **Static-Binary-Versprechen:** Keine Dependency, die musl-static
   bricht. Kein libgit2, kein OpenSSL. `gix` bleibt die Git-Schicht.
5. **Tolerant lesen, kanonisch schreiben:** Parser akzeptieren
   Varianten; Ausgabe ist immer kanonisch (RFC 8785 für Envelopes).

## Ablauf

### Schritt 1 — Branch

Leite aus dem Goal einen Slug ab und lege den Branch an:

    git switch -c feat/<slug>

Bei `fix`/`refactor`/`docs` entsprechend `fix/<slug>` usw. — der
Branch-Typ muss zum späteren Commit-Typ passen.

### Schritt 2 — Goal & Plan

- Formuliere das Goal in 2–3 Sätzen: Was ist danach möglich, das
  vorher nicht möglich war? Was ist explizit **nicht** Teil des
  Features (Non-Goals)?
- Prüfe das Goal gegen die Invarianten oben. Nenne explizit, welche
  Invarianten dieses Feature berührt.
- Liste die betroffenen Crates und Dateien auf.
- Bei instabilen API-Oberflächen (`gix` u. a.): konsultiere docs.rs,
  bevor du Code schreibst. Keine geratenen API-Formen.

### Schritt 3 — Implementierung

- Kleine, nachvollziehbare Schritte. Deutsche Doc-Kommentare.
- Tests gehören zum Feature: relative Tests (Roundtrip, Determinismus)
  plus, wo sinnvoll, absolute Golden-Tests (eingefrorene kanonische
  Form + Hash). Für Redaction-nahe Änderungen: Korpus-Tabellen
  (MUST_REDACT / MUST_SURVIVE) erweitern.
- Keine Full-File-Ersetzungen von Dateien, deren Diff du nicht
  geprüft hast.

### Schritt 4 — CI-Triade (lokal)

Führe aus und behebe alles, bis alle drei grün sind:

    cargo fmt --all
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

Erst wenn die Triade grün ist, geht es zu den Reviews. (Der
Stop-Hook erzwingt das zusätzlich — verlasse dich nicht darauf,
sondern prüfe aktiv.)

### Schritt 5 — Code-Review (Subagent)

Starte den Subagent `reviewer` mit ausschließlich diesem Kontext:

- `git diff main...HEAD` (vollständiger Feature-Diff)
- Das formulierte Goal inkl. Non-Goals aus Schritt 2

Der Reviewer bekommt bewusst **nicht** deinen Implementierungs-
Verlauf — er soll den Diff unvoreingenommen lesen, wie ein Kollege,
der nur den Merge Request sieht.

### Schritt 6 — Security-Review (Subagent)

Starte den Subagent `security-reviewer` mit demselben Diff. Er prüft
gezielt Redaction-Lücken, fail-closed-Verletzungen und Pfade, auf
denen sensible Daten das Envelope oder den Store erreichen könnten.

### Schritt 7 — Iterieren

- Sammle die Findings beider Reviews, sortiert nach Schwere.
- **Blocker** und **Major**: fixen, dann zurück zu Schritt 4
  (Triade erneut, Reviews erneut — mit frischen Subagents).
- **Minor/Nit**: fixen, wenn trivial; sonst als Follow-up notieren
  und im Commit-Body unter "Offen:" dokumentieren.
- Die Schleife endet erst, wenn beide Reviews ohne Blocker und
  Major zurückkommen. Maximal 5 Iterationen — danach stoppe und
  eskaliere an Patrick mit einer Zusammenfassung des Hängers.

### Schritt 8 — Commit

Genau **ein** Commit im Conventional-Commits-Format:

- Subject: englisch, `type(scope): beschreibung`,
  Scope = Crate-Name ohne `minds-`-Präfix wo eindeutig
  (z. B. `feat(redact): …`, `feat(capture): …`).
- Body: **deutsch**. Enthält: Was & Warum, berührte Invarianten,
  Review-Ergebnis (z. B. "reviewer: clean nach 2 Iterationen,
  security-reviewer: clean"), ggf. "Offen:"-Liste der Follow-ups.

Danach: Kurzer Abschluss-Report an Patrick — Branch-Name, Commit-
Subject, Anzahl Review-Iterationen, offene Follow-ups. **Kein Push,
kein Merge** ohne explizite Freigabe.

## Was du niemals tust

- Reviews überspringen oder "selbst reviewen" statt Subagents starten.
- Mehrere Features in einer Loop vermischen.
- Tests abschwächen oder `#[ignore]` setzen, um die Triade grün zu
  bekommen.
- Force-Push, Push oder Merge ohne Freigabe.
