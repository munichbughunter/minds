# ADR-0008 — Signierte Attribution: Nachweis statt Behauptung

- Status: angenommen
- Datum: 2026-07-28
- Betrifft: `minds-core`, `minds-cli`
- Verwandt: Schicht 2 in `Plan-v0.2.md`; ADR-0006 (Change-Id), ADR-0007 (forget)

## Kontext

`author` ist in Git ein unsigniertes Freitextfeld. In einer Welt, in der Agents
committen, ist das genau die Grundlage, auf der man **nichts** nachweisen kann —
für die regulierte Zielgruppe (Nachweisbarkeit von KI-Beteiligung am Code) ist das
das Kernproblem. Die `SessionId` belegt bereits die **Integrität** des Inhalts
(content-adressiert). Was fehlt, ist die **Zuschreibung**: dass ein bestimmter
Schlüsselinhaber dafür einsteht, dass diese Session — mit diesem Agenten und Modell
— echt ist.

## Entscheidung 1: `ssh-sig`, nicht sigstore

Signiert wird mit `ssh-keygen -Y sign/verify` (SSH-Signaturen) — dasselbe
Verfahren, mit dem Git SSH-Commits signiert. Der Grund ist die Zielgruppe: `ssh` ist
überall da, **kein Netz, kein OIDC, air-gap-tauglich**. sigstore/gitsign wären
moderner, brauchen aber eine Online-Vertrauenskette, die self-managed und
air-gapped arbeitende Läden nicht haben. sigstore bleibt eine spätere Option hinter
demselben schmalen Naht-Interface.

## Entscheidung 2: signiert wird ein kanonischer Attestation-Payload

`minds_core::attestation_payload(id, session)` erzeugt einen deterministischen Text:

```
minds-attestation-v1
session=b3-<hash>
agent=<name> <version>
model=<provider>/<id>
```

Weil die `SessionId` der Hash der kanonischen Session ist (Agent und Modell
inklusive), bindet die Signatur den **vollständigen Inhalt**; Agent und Modell
stehen zusätzlich im Klartext, damit ein Mensch die Zusage lesen kann. Ändert sich
der Inhalt, ändert sich die Id — der Payload passt nicht mehr, die Signatur bricht.
Das Versions-Präfix macht ein späteres Format-Update sauber unterscheidbar.

## Entscheidung 3: `sign`/`verify` als eigene Kommandos

- `minds sign <session> [--key]` schreibt die armierte Signatur nach stdout
  (Schlüssel aus `--key` oder `git config user.signingkey`).
- `minds verify <session> --sig <datei> [--signers] [--identity]` rekonstruiert den
  Payload aus der (hash-geprüften) Session im Store und verifiziert; Defaults für
  `--signers`/`--identity` kommen aus `git config` (`gpg.ssh.allowedSignersFile`,
  `user.email`).

Bewusst **nicht** in v0.2: automatisches Signieren beim Checkpoint und ein
`Minds-Attribution-Sig`-Trailer am Commit. Das ist eine reine Verdrahtung (wie der
Session-Trailer) auf demselben Fundament — der kryptografische Kern und seine
Prüfbarkeit stehen hier vollständig und sind end-to-end getestet (echte Signatur
verifiziert, fremde/manipulierte Signatur fällt durch).

## Konsequenzen

„Agent X, Modell Y schrieb diese Zeilen, und Person Z steht dafür ein" wird
**beweisbar**. Zusammen mit `minds forget` (DSGVO) und der Change-Id ist das der
Attributions-/Vertrauens-Teil der These „mehr ins Repo, weniger in die Plattform" —
und die Grundlage, auf der Schicht 3 (Reviews als Git-Objekte) signierte Verdicts
aufsetzt.
