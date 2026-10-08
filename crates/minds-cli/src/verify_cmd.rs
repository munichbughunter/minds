//! `minds verify` — vom Signatur-Check zum Evidence-Verdikt (ADR-0011).
//!
//! Drei Betriebsarten:
//!
//! - `minds verify [<session|rev>]` — das **Evidence-Verdikt**: Integrität ×
//!   Coverage über die Seals der Session(s), als Matrix mit festen Exit-Codes.
//!   Ohne Ziel werden die mit HEAD verknüpften Sessions geprüft.
//! - `minds verify <session> --sig <datei> [--signers] [--identity]` — der
//!   bisherige Attestation-Pfad: eine signierte Attribution prüfen.
//! - `minds verify --evidence <seal-id>` — das Verdikt eines einzelnen Seals,
//!   auch ohne Session (der Redaction-Block-Fall).
//!
//! Im ersten Modus hängt die Coverage-Zeile zusätzlich die **Artefakt-
//! Coverage** an (EA-02, siehe [`artifact`]): wie viele geänderte Zeilen des
//! Commits durch Evidenz erklärt sind, gefolgt von den unerklärten Stellen.
//! Der Commit ist `--commit <rev>`, sonst die aufgelöste Revision, sonst — bei
//! einer direkt genannten Session — der jüngste Commit mit ihrem Trailer.
//! `--require-explained <prozent>` macht daraus ein Gate: verfehlt ⇒ Exit 2,
//! außer das Verdikt ist schon schlechter (das Schlechteste gewinnt, W6).
//!
//! Erklärt der gebundene Intent-Anker einen Bereich (`scope=`), zählt die
//! Coverage-Zeile außerdem die Pfade außerhalb davon (EA-17, siehe
//! [`scope`]): geänderte Dateien des Commits, Schreib-Claims und
//! Beobachtungen der Session. `--require-in-scope` ist das Gate dazu —
//! verfehlt oder nicht beurteilbar ⇒ Exit 2, nie über 1/3/4 hinweg.
//!
//! Unter den drei Achsen stehen die **Assurance** der Session (EA-11/EA-12,
//! siehe [`assurance`]: wer das Material beobachtet hat, mit dem ersten
//! Grund, warum es nicht mehr ist) und **`Not proven`** — die Grenzen, die
//! auf dieser Stufe gelten (`--limits`: ausgeschrieben). Bestätigt der
//! Witness einen Schreib-Claim nicht, steht er als `uncorroborated` unter
//! der Coverage-Zeile — ein Coverage-Fakt, kein Abzug an der Stufe.
//! `--require-assurance <A0|A1|A2|A3>` ist ein Gate wie
//! `--require-explained` (Exit 2, maskiert nie 1/3/4). `--witness-home
//! <dir>` gleicht das Ledger des Witness mit dem Repository ab: Ein dort
//! genannter, hier fehlender Seal ist `TAMPERED`.
//!
//! # VALID ≠ COMPLETE
//!
//! Die zwei Achsen sind getrennte Urteile (ADR-0011, Entscheidung 7):
//!
//! | | Coverage vollständig | Coverage unvollständig/unbekannt |
//! |---|---|---|
//! | Integrität intakt | `VERIFIED` | `VERIFIED, INCOMPLETE` |
//! | Integrität verletzt | `TAMPERED` | `TAMPERED` |
//! | Kein Material | — | `NOT VERIFIABLE` |
//!
//! Exit-Codes (CI-Vertrag): **0** VERIFIED · **1** TAMPERED ·
//! **2** VERIFIED, INCOMPLETE · **3** NOT VERIFIABLE ·
//! **4** operativer Fehler (Store nicht lesbar, ssh-keygen fehlt, …) — ein
//! flakiger Runner darf nie als „manipuliert" durchgehen, deshalb kollidiert
//! der Fehlerpfad nicht mit Code 1.
//!
//! Eine Alt-Session ohne Seal ist ein **Zustand**, kein Fehler: Sie wurde vor
//! der Evidence-Chain erfasst; das Verdikt sagt genau das. Der heuristische
//! Epochen-Schluss über `lineage.local_id` erscheint nur als Hinweis und
//! wertet das Verdikt **nie** auf — Heuristik bleibt Heuristik.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, ExitCode};

use minds_core::evidence::{Seal, SealOutcome};
use minds_core::intent_anchor::{IssueHistory, IssueVersion};
use minds_core::{ContentHash, Session, SessionId};
use minds_git::CommitId;
use minds_store::{ContextStore, StoreError};

use crate::context::Context;

mod anchors;
mod artifact;
pub(crate) mod assurance;
mod scope;
pub(crate) mod witness_trust;

use artifact::Artifact;
use minds_core::EvidenceSource;
use minds_reader::assurance::{Assurance, FsCoverage, LedgerCheck};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Das Verdikt der Matrix, mit seinem Exit-Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Integrität intakt, Coverage vollständig.
    Verified,
    /// Integrität verletzt — unabhängig von der Coverage.
    Tampered,
    /// Integrität intakt, aber bekannte Lücken, offene Epochen oder eine
    /// zurückgewiesene Nutzlast.
    Incomplete,
    /// Kein Material, über das sich urteilen ließe.
    Unverifiable,
}

impl Verdict {
    // Delegiert an die eine Wortquelle (minds-core), damit CLI und
    // Lesemodell dasselbe Verdikt sprechen.
    fn word(self) -> &'static str {
        match self {
            Verdict::Verified => minds_core::evidence::Verdict::Verified.word(),
            Verdict::Tampered => minds_core::evidence::Verdict::Tampered.word(),
            Verdict::Incomplete => minds_core::evidence::Verdict::Incomplete.word(),
            Verdict::Unverifiable => minds_core::evidence::Verdict::Unverifiable.word(),
        }
    }

    fn exit(self) -> ExitCode {
        match self {
            Verdict::Verified => ExitCode::from(0),
            Verdict::Tampered => ExitCode::from(1),
            Verdict::Incomplete => ExitCode::from(2),
            Verdict::Unverifiable => ExitCode::from(3),
        }
    }

    fn severity(self) -> u8 {
        match self {
            Self::Verified => 0,
            Self::Incomplete => 1,
            Self::Unverifiable => 2,
            Self::Tampered => 3,
        }
    }
}

/// Die Artefakt-Optionen des Verdikt-Modus (EA-02).
#[derive(Debug, Default, Clone, Copy)]
pub struct ArtifactOptions<'a> {
    /// `--commit <rev>`: gegen diesen Commit abgleichen.
    pub commit: Option<&'a str>,
    /// `--require-explained <prozent>`: das Gate, roh wie übergeben.
    pub require_explained: Option<&'a str>,
    /// `--all`: alle Detailzeilen, ungekappt.
    pub all: bool,
    /// `--require-in-scope`: das Scope-Gate (EA-17).
    pub require_in_scope: bool,
}

impl ArtifactOptions<'_> {
    fn any(&self) -> bool {
        self.commit.is_some()
            || self.require_explained.is_some()
            || self.all
            || self.require_in_scope
    }
}

/// Die Assurance-Optionen des Verdikt-Modus (EA-12).
#[derive(Debug, Default, Clone, Copy)]
pub struct AssuranceOptions<'a> {
    /// `--witness-home <dir>`: das Ledger des Witness abgleichen.
    pub witness_home: Option<&'a str>,
    /// `--require-assurance <A0|A1|A2|A3>`: das Gate, roh wie übergeben.
    pub require_assurance: Option<&'a str>,
    /// `--limits`: die Grenzen ausgeschrieben statt der Kurzzeile.
    pub limits: bool,
    /// `--online`: die Fassung eines Issue-Ankers bei GitLab prüfen (EA-16).
    pub online: bool,
    /// `--gitlab-url <url>`: die Instanz dafür (nur mit `--online`).
    pub gitlab_url: Option<&'a str>,
}

impl AssuranceOptions<'_> {
    fn any(&self) -> bool {
        self.witness_home.is_some()
            || self.require_assurance.is_some()
            || self.limits
            || self.online
            || self.gitlab_url.is_some()
    }
}

/// Was ein Verdikt-Lauf für jede Session mitbringt.
struct RunOptions<'a> {
    signers: Option<&'a str>,
    identity: Option<&'a str>,
    /// Die Witness-Prüfung — einmal je Lauf, ihr Urteil je Seal gecacht.
    trust: witness_trust::WitnessTrust<'a>,
    ledger: assurance::Ledger,
    limits: bool,
    /// `--all`: auch die `uncorroborated`-Zeilen ungekappt.
    all: bool,
    /// Die Versionsprüfung von Issue-Ankern — je Anker einmal je Lauf.
    issue: crate::intent_issue::OnlineCheck,
    /// Der Abgleich der Erstsicht-Gegenzeichnungen mit den MR-Notes
    /// (EA-19) — je Commit einmal je Lauf.
    anchors: anchors::OnlineAnchors,
}

/// Liest `--require-explained`: eine ganze Zahl 0–100.
fn parse_required(raw: &str) -> Result<u8, String> {
    // Nur Ziffern: `+50` wäre für `u8::from_str` gültig.
    let digits = !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit());
    let parsed = digits.then(|| raw.parse::<u8>().ok()).flatten();
    parsed.filter(|p| *p <= 100).ok_or_else(|| {
        format!(
            "--require-explained expects an integer from 0 to 100, got \"{}\"",
            crate::text::sanitize(raw)
        )
    })
}

/// Führt `minds verify` aus.
pub fn run(
    target: Option<&str>,
    sig: Option<&str>,
    signers: Option<&str>,
    identity: Option<&str>,
    evidence: Option<&str>,
    options: ArtifactOptions<'_>,
    assurance_options: AssuranceOptions<'_>,
) -> ExitCode {
    // Die Artefakt- und Assurance-Flags gehören nur zum Verdikt-Modus. Ein
    // Fehlgebrauch ist ein operativer Fehler (4), nie ein Verdikt — ein
    // CI-Gate, das still wegfällt, wäre schlimmer als ein lauter Abbruch.
    if (options.any() || assurance_options.any()) && (evidence.is_some() || sig.is_some()) {
        eprintln!(
            "minds verify: --commit, --require-explained, --require-in-scope, --all, --witness-home, --require-assurance, --limits, --online and --gitlab-url apply only to the evidence verdict (not with --evidence/--sig)"
        );
        return ExitCode::from(4);
    }
    if assurance_options.gitlab_url.is_some() && !assurance_options.online {
        eprintln!("minds verify: --gitlab-url applies only with --online");
        return ExitCode::from(4);
    }
    let required = match options.require_explained.map(parse_required).transpose() {
        Ok(required) => required,
        Err(err) => {
            eprintln!("minds verify: {err}");
            return ExitCode::from(4);
        }
    };
    let required_assurance = match assurance_options
        .require_assurance
        .map(assurance::parse_required)
        .transpose()
    {
        Ok(required) => required,
        Err(err) => {
            eprintln!("minds verify: {err}");
            return ExitCode::from(4);
        }
    };
    match (evidence, target, sig) {
        // Ein einzelner Seal, auch ohne Session.
        (Some(seal_id), None, None) => match verify_seal(seal_id, signers, identity) {
            Ok(verdict) => verdict.exit(),
            Err(err) => operational_failure(err.as_ref()),
        },
        (Some(_), _, _) => {
            eprintln!("minds verify: --evidence stands alone (without <session-id>/--sig)");
            ExitCode::FAILURE
        }
        // Der Attestation-Pfad, unverändert.
        (None, Some(target), Some(sig)) => match verify_attestation(target, sig, signers, identity)
        {
            Ok(true) => {
                println!("valid");
                ExitCode::SUCCESS
            }
            Ok(false) => {
                println!("INVALID");
                ExitCode::FAILURE
            }
            Err(err) => {
                eprintln!("minds verify: {err}");
                ExitCode::FAILURE
            }
        },
        // Session-Id oder Revision; ohne Argument gilt HEAD.
        (None, target, None) => verify_target(
            target.unwrap_or("HEAD"),
            signers,
            identity,
            options,
            required,
            assurance_options,
            required_assurance,
        ),
        (None, None, Some(_)) => {
            eprintln!("minds verify: --sig expects <session-id>");
            ExitCode::FAILURE
        }
    }
}

fn verify_target(
    target: &str,
    signers: Option<&str>,
    identity: Option<&str>,
    options: ArtifactOptions<'_>,
    required: Option<u8>,
    assurance_options: AssuranceOptions<'_>,
    required_assurance: Option<Assurance>,
) -> ExitCode {
    let ctx = match Context::open() {
        Ok(ctx) => ctx,
        Err(err) => return operational_failure(err.as_ref()),
    };
    // Das Ledger zuerst: Ist es nicht lesbar, gibt es kein Verdikt (4) —
    // und fehlt ein bezeugter Seal, ist das Repository verändert, gleich
    // welche Session dieser Lauf prüft.
    let ledger = match assurance_options.witness_home {
        Some(home) => match assurance::check_ledger(Path::new(home), ctx.store.as_ref()) {
            Ok(ledger) => ledger,
            Err(err) => return operational_failure(err.as_ref()),
        },
        None => assurance::Ledger::not_checked(),
    };
    if ledger.torn {
        println!("{}", assurance::TORN_LEDGER_NOTE);
    }
    // Ohne Commit-Kontext (eine Session-Id) gibt es keine Verknüpfung, deren
    // Herkunft zählte.
    let (links, revision): (Vec<(SessionId, Option<EvidenceSource>)>, _) =
        if let Ok(id) = target.parse::<SessionId>() {
            (vec![(id, None)], None)
        } else {
            match sessions_of_revision(&ctx, target) {
                Ok((commit, ids)) => (
                    ids.into_iter()
                        .map(|(id, source)| (id, Some(source)))
                        .collect(),
                    Some(commit),
                ),
                Err(err) => return operational_failure(err.as_ref()),
            }
        };
    let ids: Vec<SessionId> = links.iter().map(|(id, _)| *id).collect();
    if ids.is_empty() {
        // Ohne Session bleibt der Ledger-Befund trotzdem einer: Das
        // Repository hat bezeugte Seals verloren.
        let findings = assurance::ledger_findings(&ledger);
        if let Some((first, rest)) = findings.split_first() {
            println!("Integrity      VIOLATED  {first}");
            for line in rest {
                println!("               {line}");
            }
            println!("{}", Verdict::Tampered.word());
            return Verdict::Tampered.exit();
        }
        if let Some(required) = required_assurance
            && let Some(line) = assurance::gate_failure(&[], required)
        {
            println!("{line}");
        }
        if options.require_in_scope {
            println!("Gate           scope not assessed (no linked session) — required in scope");
        }
        return Verdict::Unverifiable.exit();
    }
    let run = RunOptions {
        signers,
        identity,
        trust: witness_trust::WitnessTrust::new(ctx.store.as_ref(), signers).with_anchor_program(
            std::env::var("PATH")
                .ok()
                .and_then(|path| crate::replay_cmd::find_ssh_keygen(&path, &ctx.root)),
        ),
        ledger,
        limits: assurance_options.limits,
        all: options.all,
        issue: crate::intent_issue::OnlineCheck::new(
            assurance_options.online,
            assurance_options.gitlab_url,
            ctx.root.clone(),
        ),
        anchors: anchors::OnlineAnchors::new(
            assurance_options.online,
            assurance_options.gitlab_url,
            &ctx.root,
        ),
    };
    let artifact = match artifact_of(&ctx, &options, required, revision, &ids, signers) {
        Ok(artifact) => artifact,
        Err(err) => return operational_failure(err.as_ref()),
    };

    let mut worst = Verdict::Verified;
    // Ein gescheiterter Abgleich ist operativ (4) — die Session-Blöcke
    // stehen trotzdem da, das Integritäts-Urteil soll er nicht verschlucken.
    let mut failed = match &artifact {
        ArtifactState::Failed(err) => {
            eprintln!("minds verify: {err}");
            true
        }
        _ => false,
    };
    let mut levels = Vec::new();
    let mut scopes = Vec::new();
    // Der Commit, gegen den Replay-Records zählen (EA-18b): derselbe wie
    // für die Coverage — `--commit`, sonst die geprüfte Revision.
    let replay_commit = match options.commit {
        Some(rev) => match ctx.resolve_rev(rev) {
            Some(commit) => Some(commit),
            None => {
                let err: Box<dyn std::error::Error> =
                    format!("no such revision: {}", crate::text::sanitize(rev)).into();
                return operational_failure(err.as_ref());
            }
        },
        None => revision,
    }
    .map(|commit| commit.to_string());
    for (i, (id, link)) in links.into_iter().enumerate() {
        if i > 0 {
            println!();
        }
        match verify_session(&ctx, id, &run, link, &artifact, replay_commit.as_deref()) {
            Ok((verdict, level, scope)) => {
                if verdict.severity() > worst.severity() {
                    worst = verdict;
                }
                levels.push(level);
                scopes.push(scope);
            }
            Err(err) => {
                operational_failure(err.as_ref());
                failed = true;
            }
        }
    }
    // Das Assurance-Gate urteilt über die schwächste Session des Laufs.
    let assurance_failed = required_assurance.is_some_and(|required| {
        assurance::gate_failure(&levels, required)
            .map(|line| println!("{line}"))
            .is_some()
    });
    // Das Gate steht einmal, nach allen Blöcken: Es urteilt über den Commit,
    // nicht über eine einzelne Session.
    // `--require-explained 0` verlangt nichts — auch keinen beurteilbaren
    // Commit.
    let gate_failed = required.filter(|r| *r > 0).is_some_and(|required| {
        // Nicht beurteilbar heißt nicht bestanden (fail-closed).
        let not_assessed = |why: &str| {
            Some(format!(
                "Gate           not assessed ({why}) — required {required}%"
            ))
        };
        let line = match &artifact {
            ArtifactState::Assessed(artifact) => artifact.gate_failure(required),
            ArtifactState::NoCommit => not_assessed("no linked commit"),
            ArtifactState::Unavailable(why) => not_assessed(why),
            ArtifactState::Failed(_) => not_assessed("error"),
        };
        line.map(|line| println!("{line}")).is_some()
    });
    // Das Scope-Gate: jede Session gegen ihren Bereich, fail-closed.
    let scope_failed = options.require_in_scope
        && scope::gate_failure(&scopes, &artifact)
            .map(|line| println!("{line}"))
            .is_some();
    if failed {
        ExitCode::from(4)
    } else if (gate_failed || assurance_failed || scope_failed) && worst == Verdict::Verified {
        // Ein verfehltes Gate ist Exit 2 — es maskiert nie 1/3/4 (W6).
        Verdict::Incomplete.exit()
    } else {
        worst.exit()
    }
}

/// Was über die Artefakt-Coverage eines Laufs sagbar ist.
enum ArtifactState {
    /// Der Commit ist abgeglichen.
    Assessed(Artifact),
    /// Kein Commit ist mit der Session verknüpft.
    NoCommit,
    /// Nicht bestimmbar, aber kein Fehler: der Elternteil fehlt im Klon
    /// (Shallow Clone), oder das Gate verlangt einen eindeutigen Commit.
    Unavailable(&'static str),
    /// Der Abgleich scheiterte (Git, Store) — schon entschärft.
    Failed(String),
}

/// Der Commit, gegen den abgeglichen wird, samt Reconciliation.
///
/// Nur eine unauflösbare `--commit`-Revision ist ein sofortiger Fehler (ein
/// Bedienfehler, wie eine unbekannte Revision als Ziel). Scheitert der
/// Abgleich selbst, wird das zu [`ArtifactState::Failed`].
///
/// Claims stammen nur aus den Sessions `ids`, deren Verdikt dieser Lauf
/// ausspricht (siehe [`artifact::assess`]).
fn artifact_of(
    ctx: &Context,
    options: &ArtifactOptions<'_>,
    required: Option<u8>,
    revision: Option<CommitId>,
    ids: &[SessionId],
    signers: Option<&str>,
) -> Fallible<ArtifactState> {
    let failed = |err: Box<dyn std::error::Error>| {
        ArtifactState::Failed(crate::text::sanitize(&err.to_string()))
    };
    let commit = match (options.commit, revision, ids) {
        (Some(rev), _, _) => ctx
            .resolve_rev(rev)
            .ok_or_else(|| format!("no such revision: {rev}"))?,
        (None, Some(commit), _) => commit,
        (None, None, [id]) => match trailer_commits(ctx, *id) {
            Ok(commits) => match commits[..] {
                [] => return Ok(ArtifactState::NoCommit),
                // Ein Gate urteilt nicht über einen geratenen Commit: Wer
                // einen späteren Commit mit demselben Trailer anlegt, könnte
                // sonst den Prüfgegenstand wählen.
                // Für das Scope-Gate ebenso: Ein früherer Commit derselben
                // Session bliebe sonst ungeprüft.
                [_, _, ..] if required.is_some_and(|r| r > 0) || options.require_in_scope => {
                    return Ok(ArtifactState::Unavailable(
                        "several commits carry this session — pass --commit",
                    ));
                }
                [newest, ..] => newest,
            },
            Err(err) => return Ok(failed(err)),
        },
        (None, None, _) => return Ok(ArtifactState::NoCommit),
    };
    // Gelesen wie im Verdikt-Block; was dort nicht lesbar ist, meldet der
    // Block selbst — hier trägt es schlicht keine Claims bei.
    let loaded: Vec<(SessionId, Session)> = ids
        .iter()
        .filter_map(|id| Some((*id, ctx.store.get(*id).ok().flatten()?)))
        .collect();
    let sessions: Vec<&Session> = loaded.iter().map(|(_, session)| session).collect();
    // Beobachtungen des Datei-Beobachters (EA-08) — nur aus
    // `witness-fs/v1`-Seals mit gültiger Witness-Signatur, geprüft gegen eine
    // Signer-Datei, die nicht aus der Repo-Konfiguration stammt, für
    // Principals, die auf `minds-witness` beschränkt sind (`witness_trust`),
    // und nur in den Fenstern der vom Witness bezeugten Sessions
    // (`witness_windows`). Unter `refs/minds/` kann auch der Agent schreiben,
    // und die Zeitpunkte unbezeugter Sessions setzt er selbst; eine
    // ungeprüfte Beobachtung könnte eine menschliche Zeile „erklären" und das
    // Gate aushebeln. Ohne vertrauenswürdige Signer: keine.
    // Das Urteil je Seal einmal: `witness_windows` und
    // `observations_in_windows` fragen teils dieselben Seals, und jede
    // Prüfung startet `ssh-keygen`.
    let check = witness_trust::observation_trust(ctx.store.as_ref(), signers);
    let verdicts = std::cell::RefCell::new(std::collections::HashMap::new());
    let trusted = |seal_id: &ContentHash, text: &str| {
        if let Some(verdict) = verdicts.borrow().get(seal_id) {
            return *verdict;
        }
        let verdict = check(seal_id, text);
        verdicts.borrow_mut().insert(seal_id.clone(), verdict);
        verdict
    };
    let pairs: Vec<(SessionId, &Session)> =
        loaded.iter().map(|(id, session)| (*id, session)).collect();
    let windows = minds_reader::observations::witness_windows(ctx.store.as_ref(), &pairs, &trusted);
    let observations =
        minds_reader::observations::observations_in_windows(ctx.store.as_ref(), &windows, &trusted);
    // Derselbe Lauf, ungekappt: das Ziel (Revision oder Session) und der
    // Commit, beides in voller Länge.
    let target = match (revision, ids) {
        (Some(revision), _) => revision.to_string(),
        (None, [id]) => id.to_string(),
        (None, _) => commit.to_string(),
    };
    let rerun = if target == commit.to_string() {
        format!("minds verify {commit} --all")
    } else {
        format!("minds verify {target} --commit {commit} --all")
    };
    Ok(
        match artifact::assess(ctx, commit, &sessions, &observations, options.all, rerun) {
            Ok(Ok(artifact)) => ArtifactState::Assessed(artifact),
            Ok(Err(why)) => ArtifactState::Unavailable(why),
            Err(err) => failed(err),
        },
    )
}

/// Die von HEAD erreichbaren Commits, deren **Trailer** diese Session nennt —
/// die beobachteten Kanten, jüngster (topologisch) zuerst. Die
/// Store-Index-Kanten zählen hier bewusst nicht: Sie sind vermutet, und eine
/// Vermutung wählt keinen Prüfgegenstand.
fn trailer_commits(ctx: &Context, id: SessionId) -> Fallible<Vec<CommitId>> {
    let Some(head) = ctx.repo.head()?.commit() else {
        return Ok(Vec::new());
    };
    Ok(ctx.repo.commits_with_session(head, id)?)
}

/// Die Sessions der Revision, je mit der Herkunft ihrer Verknüpfung: aus
/// dem Trailer (`Observed`) oder — ohne Trailer — die Quelle, die der
/// Store-Index für die Kante festhält (ein Import steht dort als
/// `Heuristic`, und eine Vermutung hebt keine Stufe über A0). Beides kann
/// auch der Agent schreiben; über den Commit sagt die Stufe deshalb nichts
/// (das tut die Coverage-Achse, siehe `minds_reader::assurance`).
pub(crate) fn sessions_of_revision(
    ctx: &Context,
    rev: &str,
) -> Fallible<(CommitId, Vec<(SessionId, EvidenceSource)>)> {
    // resolve_rev passes one argument after --end-of-options and peels to a commit.
    let commit = ctx
        .resolve_rev(rev)
        .ok_or_else(|| format!("no such revision: {rev}"))?;
    let mut ids: Vec<(SessionId, EvidenceSource)> = ctx
        .repo
        .session_ids_of(commit)?
        .into_iter()
        .map(|id| (id, EvidenceSource::Observed))
        .collect();
    if ids.is_empty() {
        ids = ctx
            .store
            .index()?
            .links_of(&commit.to_string())
            .iter()
            .map(|link| (link.session, link.evidence.source))
            .collect();
    }
    let mut seen = BTreeSet::new();
    ids.retain(|(id, _)| seen.insert(*id));
    if ids.is_empty() {
        println!(
            "No session is linked to {} ({}).",
            crate::text::sanitize(rev),
            &commit.to_string()[..7]
        );
    }
    Ok((commit, ids))
}

/// Der operative Fehlerpfad: Exit **4**, nie 1 — und der Fehlertext läuft
/// durch die Terminal-Härtung, weil er Repo-Inhalte zitieren kann (etwa den
/// Tombstone-Grund in der Display-Form von `StoreError::Forgotten`).
fn operational_failure(err: &dyn std::error::Error) -> ExitCode {
    eprintln!("minds verify: {}", crate::text::sanitize(&err.to_string()));
    ExitCode::from(4)
}

// ---------------------------------------------------------------------------
// Das Evidence-Verdikt einer Session
// ---------------------------------------------------------------------------

/// Ein gelesener Seal samt Signaturstatus — die Zwischenform der Prüfung.
struct CheckedSeal {
    id: ContentHash,
    seal: Seal,
    /// Der gespeicherte Text — gegen ihn prüft die Witness-Signatur.
    text: String,
    signature: SignatureState,
}

/// Was über die Signatur eines Seals sagbar ist.
enum SignatureState {
    /// Keine `seal.sig` — hash-valide, aber ohne Urheber-Bindung.
    Unsigned,
    /// Signatur liegt, aber ohne allowed_signers ist sie nur eine Behauptung.
    Unchecked,
    /// Gegen allowed_signers geprüft und gültig.
    Valid,
    /// Mit **explizit** genannter Identität geprüft und ungültig —
    /// Manipulation.
    Invalid,
    /// Mit der **geratenen** Identität (lokales `user.email`) nicht
    /// verifizierbar. Der Seal speichert keinen Principal; wer den Seal
    /// eines Kollegen prüft, rät hier systematisch falsch — das ist kein
    /// Manipulationsbeweis, sondern eine offene Zuordnung (ADR-0011: ein
    /// Prüfprimitive, das regulär falsch-positiv rauscht, ist wertlos).
    NotAttributable,
    WitnessValid(String),
    WitnessUnchecked,
    WitnessInvalid,
    WitnessUnsigned,
}

impl SignatureState {
    fn is_invalid(&self) -> bool {
        matches!(
            self,
            Self::Invalid | Self::WitnessInvalid | Self::WitnessUnsigned
        )
    }

    fn word(&self) -> String {
        match self {
            SignatureState::WitnessValid(principal) => {
                return format!("witness-signed ({})", crate::text::sanitize(principal));
            }
            SignatureState::WitnessUnchecked => "signature not checked",
            SignatureState::WitnessInvalid => "witness seal not signed under minds-witness",
            SignatureState::WitnessUnsigned => "witness seal missing signature",
            SignatureState::Unsigned => "unsigned",
            SignatureState::Unchecked => "signed (unchecked — verify with --signers)",
            SignatureState::Valid => "signature valid",
            SignatureState::Invalid => "SIGNATURE INVALID",
            SignatureState::NotAttributable => "signed (not attributable — pass --identity)",
        }
        .to_owned()
    }
}

fn verify_session(
    ctx: &Context,
    id: SessionId,
    run: &RunOptions<'_>,
    link: Option<EvidenceSource>,
    artifact: &ArtifactState,
    commit: Option<&str>,
) -> Fallible<(Verdict, Assurance, scope::ScopeState)> {
    println!("Session        {id}");
    let store = ctx.store.as_ref();

    // 1. Die Session selbst: vorhanden, vergessen (payload-freier Beweis
    //    bleibt) — oder manipuliert.
    let mut tampered = false;
    let mut payload_missing = false;
    let mut notes: Vec<String> = Vec::new();
    let mut lineage: Option<(String, String)> = None;
    // Die dritte Achse: Deutung. Ein unbekanntes Tool ist KEIN Integritäts-
    // und KEIN Coverage-Problem — es ist eine Deutungslücke, und sie bekommt
    // ihre eigene Zeile statt das Verdikt zu vermischen (ADR-0011).
    let mut interpretation: Option<(usize, usize)> = None; // (gedeutet, ungedeutet)
    let mut loaded: Option<Session> = None;
    match ctx.store.get(id) {
        Ok(Some(session)) => {
            if let Some(l) = &session.lineage {
                lineage = Some((session.agent.name.clone(), l.local_id.clone()));
            }
            // `capture: None` ist ein Vor-Chain-Zustand und zählt in KEINER
            // Achse mit — dieselbe Semantik wie im Reader (`◐`-Zählung).
            let calls: Vec<_> = session
                .turns
                .iter()
                .flat_map(|t| t.tool_calls.iter())
                .filter(|c| c.capture.is_some())
                .collect();
            let uninterpreted = calls
                .iter()
                .filter(|c| {
                    c.capture
                        .as_ref()
                        .is_some_and(|cap| cap.status == minds_core::CaptureStatus::Uninterpreted)
                })
                .count();
            interpretation = Some((calls.len() - uninterpreted, uninterpreted));
            println!(
                "Payload        in store (schema {})",
                session.schema_version
            );
            loaded = Some(session);
        }
        Ok(None) => payload_missing = true,
        Err(StoreError::Forgotten { reason, .. }) => {
            // Der Grund ist fremdbestimmter Repo-Inhalt — Terminal-Härtung
            // an der Senke (#116), wie in render/reader.
            println!(
                "Payload        forgotten ({}) — the seal remains the evidence",
                crate::text::sanitize(&reason)
            );
        }
        Err(StoreError::Corrupt { .. }) => {
            println!("Payload        TAMPERED — content does not hash to its id");
            tampered = true;
        }
        Err(err) => return Err(err.into()),
    }

    // 2. Die Seals: der Rückverweis **und** der Namensraum. Der Rückverweis
    //    ist best-effort und agent-schreibbar — ließe er einen Seal aus, der
    //    diese Session nennt, bliebe dessen Befund (Lücke, Ablehnung, erster
    //    Bereich der Kette) ungeprüft.
    let (seal_ids, unreferenced) = session_seal_ids(store, id)?;
    if !unreferenced.is_empty() && unreferenced.len() == seal_ids.len() {
        notes.push(
            "the seal back-reference (evidence.json) was missing — found via the namespace".into(),
        );
    } else {
        for seal_id in &unreferenced {
            notes.push(format!(
                "seal {seal_id} names this session but is missing from the back-reference (evidence.json) — checked anyway"
            ));
        }
    }
    // Ein bezeugter Seal, der im Repository fehlt, trifft jede Session des
    // Laufs: Das Ledger nennt keine Session, und ein verändertes Repository
    // ist für keine intakt.
    let ledger_findings = assurance::ledger_findings(&run.ledger);
    let ledger_tampered = !ledger_findings.is_empty();
    let legacy = seal_ids.is_empty() && !tampered;
    if legacy {
        println!("Seals          none — captured before the evidence chain");
    }
    if legacy && !ledger_tampered {
        let intent = assurance::intent_state(&run.trust, id, loaded.as_ref(), &[]);
        // Ohne Seals ist das Verdikt NOT VERIFIABLE (3): Über den Bereich
        // ist nichts sagbar, und das Gate kann hier nie zu 0 führen.
        let scope = scope::ScopeState::NotAssessed(minds_reader::scope::NoScope::NoEvidenceChain);
        let report = assurance::report(
            &run.trust,
            &assurance::Facts {
                id,
                session: loaded.as_ref(),
                seals: &[],
                legacy: true,
                tampered: false,
                complete: false,
                chain_closed: true,
                link,
                ledger: &run.ledger.check,
                observations: &FsCoverage::Unavailable { cause: None },
                intent: &intent,
                commit,
            },
        );
        print_assurance(&report, run);
        println!("{}", Verdict::Unverifiable.word());
        return Ok((Verdict::Unverifiable, report.overall, scope));
    }

    let (mut checked, mut incomplete_reasons, seal_tampered) = check_seals(
        store,
        &seal_ids,
        run.signers,
        run.identity,
        &ctx.root,
        Some(id),
    )?;
    tampered |= seal_tampered;
    // In zeitlicher Ordnung — `range N` der Assurance-Zeile zählt genauso.
    checked.sort_by_key(|c| assurance::range_order(&c.seal));

    // 3. Jeder stored-Seal muss DIESE Session nennen — und wenn ein Seal
    //    `stored` sagt, muss die Nutzlast auch auffindbar sein (kein
    //    Tombstone, einfach weg): Sonst ist die Integritäts-Achse für den
    //    Payload nicht prüfbar, und „VERIFIZIERT" über-claimte.
    for c in &checked {
        if let SealOutcome::Stored { session } = &c.seal.outcome {
            if session != &id.to_string() {
                incomplete_reasons.push(format!("seal {} names a different session", c.id));
            } else if payload_missing {
                incomplete_reasons
                    .push("the seal says stored, but the payload is not in this store".into());
                payload_missing = false; // einmal genügt
            }
        }
    }
    if payload_missing {
        notes.push("the payload is not in this store".into());
    }

    // 4. Coverage: gap-frei je Seal + geschlossene Epochenkette.
    let pairs: Vec<(&ContentHash, &Seal)> = checked.iter().map(|c| (&c.id, &c.seal)).collect();
    let chain = coverage_complete(store, &pairs, &mut incomplete_reasons)?;
    let complete = chain.complete;

    for c in &checked {
        print_seal_line(c);
        if let Some(line) = anchors::seal_line(&run.trust.first_sight_checked(&c.id)) {
            println!("{line}");
        }
        if c.signature.is_invalid() {
            tampered = true;
        }
    }
    // `--online`: Nennt eine gültig signierte MR-Note einen Seal, dessen
    // Gegenzeichnung im Repository fehlt, wurde sie entfernt — ein
    // Integritätsbefund wie ein fehlender bezeugter Seal (EA-19).
    // Der geprüfte Commit und jeder, dessen Trailer die Session nennt.
    let mut anchor_commits: Vec<String> = commit.map(str::to_owned).into_iter().collect();
    if run.anchors.online() {
        for found in trailer_commits(ctx, id)? {
            let found = found.to_string();
            if !anchor_commits.contains(&found) {
                anchor_commits.push(found);
            }
        }
    }
    let anchor_notes = run
        .anchors
        .check(store, &run.trust, &anchor_commits, &seal_ids);
    let mut anchor_findings = Vec::new();
    if let anchors::NotesState::Checked(check, _, stale) = &anchor_notes {
        for finding in &check.findings {
            if finding.integrity() {
                anchor_findings.push(finding.text());
            } else {
                notes.push(finding.text());
            }
        }
        for text in stale {
            notes.push(format!(
                "{text} — may be an older clone (fetch refs/minds/* and rerun)"
            ));
        }
    }
    if let Some(line) = anchors::notes_line(&anchor_notes) {
        println!("{line}");
    }
    for note in &notes {
        println!("Note           {note}");
    }
    for reason in &incomplete_reasons {
        println!("Gap            {reason}");
    }

    // Ein entfernter Anker ist verändertes Material: Integrität verletzt.
    if !anchor_findings.is_empty() {
        tampered = true;
    }

    // 5. Heuristischer Epochen-Hinweis — wertet NIE auf.
    if !complete && !tampered && !ledger_tampered {
        if let Some((agent, local_id)) = lineage {
            let siblings = sibling_sessions(ctx.store.as_ref(), id, &agent, &local_id)?;
            if siblings > 0 {
                println!(
                    "Note           heuristic: {siblings} other session(s) with the same local_id \
                     found — reconstructed proximity, not evidence; the verdict stays unchanged"
                );
            }
        }
    }

    let verdict = if tampered || ledger_tampered {
        Verdict::Tampered
    } else if complete && incomplete_reasons.is_empty() {
        Verdict::Verified
    } else {
        Verdict::Incomplete
    };

    // Die drei Vertrauensachsen, getrennt ausgesprochen: Integrität („wurde
    // es verändert?"), Coverage („wissen wir, ob etwas fehlt?" — immer
    // innerhalb der Beobachtungsgrenze) und Deutung („was bedeutet es?").
    // Das Gesamt-Verdikt und die Exit-Codes bleiben der CI-Vertrag aus
    // Integrität × Coverage; die Deutung wertet nie auf oder ab.
    let integrity_findings: Vec<String> = ledger_findings
        .iter()
        .cloned()
        .chain(anchor_findings)
        .collect();
    match integrity_findings.split_first() {
        Some((first, rest)) => {
            println!("Integrity      VIOLATED  {first}");
            for line in rest {
                println!("               {line}");
            }
        }
        None => println!(
            "Integrity      {}",
            if tampered { "VIOLATED" } else { "intact" }
        ),
    }
    let boundaries: Vec<String> = {
        // Zweite Schicht neben der Parse-Härtung: Der Scope stammt aus dem
        // Repo — fremdbestimmt, also entschärft ausgeben.
        let mut s: Vec<String> = checked
            .iter()
            .map(|c| crate::text::sanitize(&c.seal.scope))
            .collect();
        s.sort_unstable();
        s.dedup();
        s
    };
    // Was der Witness sah: Fenster und Beobachtungen nur aus Seals, die er
    // gültig signiert hat (`witness_trust`) — dieselbe Quelle für die
    // Korroboration, die Scope-Befunde und die Abdeckung, aus der die Stufe
    // rechnet.
    let trusted = |seal_id: &ContentHash, text: &str| {
        matches!(
            run.trust.signature(seal_id, text),
            minds_reader::assurance::SealSignature::Witness { .. }
        )
    };
    // Ohne lesbare Nutzlast gibt es weder Fenster noch Bindung — der
    // Bereich ist dann nicht beurteilbar (`session payload unreadable`).
    let (windows, window_observations) = match &loaded {
        Some(session) => {
            minds_reader::observations::session_observations(store, id, session, &trusted)
        }
        None => (
            Vec::new(),
            minds_reader::observations::WindowObservations {
                observations: Vec::new(),
                complete: true,
            },
        ),
    };
    let seals: Vec<(ContentHash, Seal, String)> = checked
        .iter()
        .map(|c| (c.id.clone(), c.seal.clone(), c.text.clone()))
        .collect();
    // Einmal je Session: Der Bereich der Scope-Befunde und die Stufe
    // stammen aus demselben Anker.
    let intent = assurance::intent_state(&run.trust, id, loaded.as_ref(), &seals);
    let scope = scope::assess(
        store,
        &ctx.repo,
        &ctx.root,
        &intent,
        loaded.as_ref(),
        artifact,
        &window_observations,
    );
    let mut segments = Vec::new();
    if !boundaries.is_empty() {
        segments.push(format!(
            "boundary: {} — activity outside it is not captured",
            boundaries.join(", ")
        ));
    }
    if let ArtifactState::Assessed(artifact) = artifact {
        // Store-Daten: sättigend summieren, nie überlaufen.
        let gaps = checked
            .iter()
            .fold(0u64, |sum, c| sum.saturating_add(c.seal.gaps));
        segments.push(format!("{gaps} {}", if gaps == 1 { "gap" } else { "gaps" }));
        segments.push(artifact.coverage_segment());
    }
    // Aus verändertem Material sind Scope-Befunde keine Fakten: Bei
    // verletzter Integrität fehlen Segment und Detailzeilen.
    let scope_shown = !(tampered || ledger_tampered);
    let scope = if scope_shown {
        scope
    } else {
        scope::ScopeState::NotAssessed(minds_reader::scope::NoScope::IntegrityViolated)
    };
    if scope_shown {
        segments.extend(scope::segment(&scope));
    }
    let boundary = if segments.is_empty() {
        String::new()
    } else {
        format!(" ({})", segments.join(" · "))
    };
    println!(
        "Coverage       {}{boundary}",
        if tampered || ledger_tampered {
            "not assessable"
        } else if complete && incomplete_reasons.is_empty() {
            "complete within the boundary"
        } else {
            "incomplete"
        }
    );
    if let ArtifactState::Assessed(artifact) = artifact {
        for line in artifact.detail_lines() {
            println!("{line}");
        }
    }
    if scope_shown {
        for line in scope::detail_lines(&scope, run.all, &scope::rerun(artifact, id)) {
            println!("{line}");
        }
    }
    match artifact {
        ArtifactState::Assessed(_) => {}
        ArtifactState::NoCommit => println!("Artifact       not assessed (no linked commit)"),
        ArtifactState::Unavailable(why) => println!("Artifact       not assessed ({why})"),
        ArtifactState::Failed(err) => println!("Artifact       not assessed (error: {err})"),
    }

    if let (Some(session), false) = (&loaded, windows.is_empty()) {
        for line in assurance::uncorroborated_lines(
            &ctx.root,
            id,
            session,
            &window_observations.observations,
            run.all,
        ) {
            println!("{line}");
        }
    }
    let interpretation_note = match interpretation {
        Some((_, 0)) => {
            println!("Interpretation complete");
            None
        }
        Some((done, open)) => {
            println!(
                "Interpretation partial — {open} of {} tool call(s) observed but not interpreted (◐)",
                done + open
            );
            Some(" — interpretation partial")
        }
        None => {
            println!("Interpretation not assessable (payload unreadable)");
            None
        }
    };
    let observations = assurance::fs_coverage(store, &windows);
    let report = assurance::report(
        &run.trust,
        &assurance::Facts {
            id,
            session: loaded.as_ref(),
            seals: &seals,
            legacy,
            tampered,
            complete: complete && incomplete_reasons.is_empty(),
            // Ein genannter, aber nicht lesbarer Seal ist eine Range, deren
            // Stufe niemand kennt — wie eine offene Kette.
            chain_closed: chain.chain_closed && checked.len() == seal_ids.len(),
            link,
            ledger: &run.ledger.check,
            observations: &observations,
            intent: &intent,
            commit,
        },
    );
    print_assurance(&report, run);
    println!(
        "Overall        {}{}",
        verdict.word(),
        interpretation_note.unwrap_or("")
    );
    // `--online` verlangt, aber die Prüfung der MR-Notes fand nicht statt:
    // operativ (4), nie ein grünes Verdikt — der Block steht trotzdem da.
    if let anchors::NotesState::Failed(reason) = &anchor_notes {
        return Err(format!("anchor notes could not be checked: {reason}").into());
    }
    Ok((verdict, report.overall, scope))
}

/// Die `Assurance`-, `Intent`-, `Issue version`- und `Not proven`-Zeilen
/// eines Session-Blocks.
fn print_assurance(report: &minds_reader::assurance::AssuranceReport, run: &RunOptions<'_>) {
    println!("{}", assurance::assurance_line(report));
    println!("{}", assurance::intent_line(report));
    if let Some(line) = issue_version_line(report, run) {
        println!("{line}");
    }
    for line in assurance::not_proven_lines(report.overall, run.limits) {
        println!("{line}");
    }
}

/// Die `Issue version`-Zeile (EA-16) — nur, wenn der gebundene Anker im
/// Store liegt und auf ein GitLab-Issue zeigt. Ein Befund neben dem Verdikt,
/// keine Aufwertung: Er geht weder in Assurance noch Exit-Code ein.
fn issue_version_line(
    report: &minds_reader::assurance::AssuranceReport,
    run: &RunOptions<'_>,
) -> Option<String> {
    let minds_reader::assurance::IntentState::Bound {
        anchor_id,
        signature,
        snapshot_matches,
        ..
    } = &report.intent
    else {
        return None;
    };
    let stored = run.trust.store().get_intent(anchor_id).ok().flatten()?;
    let version = run.issue.version(anchor_id, &stored.anchor)?;
    Some(format!(
        "Issue version  {}",
        issue_version_text(
            version,
            signature,
            *snapshot_matches,
            snapshot_has_placeholders(&stored.snapshot)
        )
    ))
}

/// Der Text der `Issue version`-Zeile. Ein Befund, der wie eine Bestätigung
/// klingt (`current`, `confirmed`), nennt, was er **nicht** sagt: Ein
/// Anker ohne gültige Signatur kann der Agent selbst gebunden haben; passt
/// der abgelegte Snapshot nicht zum Anker, ist offen, was redigiert war; und
/// wo der Snapshot Platzhalter trägt, sagt der Hash nichts über die Stelle.
fn issue_version_text(
    version: IssueVersion,
    signature: &minds_reader::assurance::IntentSignature,
    snapshot_matches: bool,
    placeholders: bool,
) -> String {
    use minds_reader::assurance::IntentSignature;
    let positive = matches!(
        version,
        IssueVersion::Current | IssueVersion::Changed(IssueHistory::Confirmed)
    );
    let mut notes = Vec::new();
    if positive {
        if !snapshot_matches {
            notes.push("stored snapshot does not match the anchor, redaction state unknown");
        } else if placeholders {
            notes.push("redacted spans not compared");
        }
        match signature {
            IntentSignature::Valid(_) => {}
            IntentSignature::Unsigned => notes.push("anchor unsigned"),
            IntentSignature::NotChecked => notes.push("anchor signature not checked"),
            IntentSignature::Invalid => notes.push("anchor signature invalid"),
        }
    }
    if notes.is_empty() {
        version.text()
    } else {
        format!("{} ({})", version.text(), notes.join(", "))
    }
}

/// Ob der abgelegte Snapshot Redaction-Platzhalter trägt.
fn snapshot_has_placeholders(snapshot: &[u8]) -> bool {
    [
        minds_redact::Category::Secret.placeholder(),
        minds_redact::Category::Pii.placeholder(),
    ]
    .iter()
    .any(|placeholder| {
        snapshot
            .windows(placeholder.len())
            .any(|window| window == placeholder.as_bytes())
    })
}

/// Benennt einen manipulierten Seal, so weit die Repo-Lage es hergibt:
/// erwarteter vs. vorgefundener Hash, dann — falls die abgelegten Bytes noch
/// als Seal parsen — ihre **behaupteten** Felder samt Kreuzchecks gegen noch
/// intakte Daten. Mehr ist ehrlich nicht sagbar: Der Originaltext ist nach
/// dem Journal-Discard nicht rekonstruierbar, und der Hash ist nicht
/// invertierbar — welches Feld sich änderte, weiß nur der Angreifer.
fn report_tampered_seal(
    store: &dyn ContextStore,
    requested: &ContentHash,
    actual: &ContentHash,
    target: Option<SessionId>,
) {
    println!("Seal           {requested}: TAMPERED — the stored text does not hash to this id");
    println!("  expected     {requested}");
    // „found" und Claim aus DERSELBEN Lesung: Der Mismatch-Fehler stammt aus
    // einer früheren; bewegt sich der Ref dazwischen, beschrieben Hash und
    // Claim sonst verschiedene Bytes (TOCTOU der Diagnose — das Verdikt
    // selbst trägt weiterhin der Fehler, nicht diese Zweitlesung).
    let text = store
        .seal_bytes(requested)
        .ok()
        .flatten()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    let found = text.as_deref().map(Seal::id_of_text);
    println!("  found        {}", found.as_ref().unwrap_or(actual));
    if let Some(found) = &found
        && found != actual
    {
        println!(
            "  note         the ref moved during verification — hash and claim describe the current bytes"
        );
    }
    let claimed = text.as_deref().and_then(|text| Seal::parse(text).ok());
    let Some(claimed) = claimed else {
        println!("  claimed      unreadable — the stored bytes are not even a well-formed seal");
        return;
    };
    // `Seal::parse` erzwingt Einzeiligkeit und verbietet Steuer-/Versteck-
    // zeichen in den Freitextfeldern — ein geparster Claim ist terminal-
    // sicher; `sanitize` bleibt als zweite Schicht (wie beim Scope oben).
    let session = match &claimed.outcome {
        SealOutcome::Stored { session } => session.as_str(),
        SealOutcome::ObservationsStored { observations } => observations.as_str(),
        SealOutcome::Rejected => "-",
    };
    println!("  claimed      (UNVERIFIED — the tampered text's statement, not evidence)");
    println!(
        "               session={} scope={} seq {}–{} · {} event(s) · {} gap(s) · {}",
        crate::text::sanitize(session),
        crate::text::sanitize(&claimed.scope),
        claimed.first_seq,
        claimed.last_seq,
        claimed.events,
        claimed.gaps,
        claimed.outcome.human_word()
    );
    if let Some(target) = target {
        // Geparste Ids vergleichen, nicht Strings: `SessionId::from_str`
        // normalisiert (Groß-Hex) — ein String-Vergleich meldete sonst
        // „does NOT match" für die semantisch identische Session.
        // Nur `stored` nennt eine Session; die Id eines Observation-Objekts
        // hat dieselbe Form, ist aber keine.
        let names = matches!(claimed.outcome, SealOutcome::Stored { .. })
            && session.parse::<SessionId>().ok() == Some(target);
        let word = if names { "matches" } else { "does NOT match" };
        println!("  cross-check  the claimed session {word} the session under verification");
    }
    if let Some(prev) = &claimed.previous {
        let state = match store.seal_text(prev) {
            Ok(Some(_)) => "hash-valid in the store",
            Ok(None) => "not in the store",
            Err(StoreError::SealMismatch { .. }) => "itself altered",
            Err(_) => "unreadable",
        };
        println!("  cross-check  claimed previous {prev}: {state}");
    }
}

/// Liest und prüft die genannten Seals. Liefert die lesbaren Seals, die
/// Unvollständigkeits-Gründe und ob Manipulation vorliegt. `target` ist die
/// Session, um die es geht — der Tamper-Report gleicht die Behauptung des
/// manipulierten Texts gegen sie ab.
fn check_seals(
    store: &dyn ContextStore,
    seal_ids: &[ContentHash],
    signers: Option<&str>,
    identity: Option<&str>,
    root: &Path,
    target: Option<SessionId>,
) -> Fallible<(Vec<CheckedSeal>, Vec<String>, bool)> {
    let mut checked = Vec::new();
    let mut reasons = Vec::new();
    let mut tampered = false;

    for id in seal_ids {
        let text = match store.seal_text(id) {
            Ok(Some(text)) => text,
            Ok(None) => {
                reasons.push(format!("seal {id} is referenced but not in the store"));
                continue;
            }
            Err(StoreError::SealMismatch { actual, .. }) => {
                report_tampered_seal(store, id, &actual, target);
                tampered = true;
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        let seal = match Seal::parse(&text) {
            Ok(seal) => seal,
            Err(err) => {
                // Hash stimmt, Form nicht: ein Artefakt, das wir nie so
                // geschrieben hätten — der Ref wurde fremdbelegt.
                println!("Seal           {id}: TAMPERED — {err}");
                tampered = true;
                continue;
            }
        };
        let signature = signature_state(store, id, &text, &seal.scope, signers, identity, root)?;
        checked.push(CheckedSeal {
            id: id.clone(),
            seal,
            text,
            signature,
        });
    }
    Ok((checked, reasons, tampered))
}

/// Prüft die Signatur eines Seals, so weit die Umgebung es hergibt.
///
/// Ohne allowed_signers wird **nicht** geraten: „signiert (ungeprüft)" ist
/// eine andere Aussage als „gültig" — dieselbe Trennung wie bei
/// `minds reviews` (fail-closed, #12).
fn signature_state(
    store: &dyn ContextStore,
    id: &ContentHash,
    text: &str,
    scope: &str,
    signers: Option<&str>,
    identity: Option<&str>,
    root: &Path,
) -> Fallible<SignatureState> {
    let witness = matches!(scope, "witness/v1" | "witness-fs/v1");
    let Some(signature) = store.seal_signature(id)? else {
        return Ok(if witness {
            SignatureState::WitnessUnsigned
        } else {
            SignatureState::Unsigned
        });
    };
    let Some(signers) = resolve_signers_optional(signers, root) else {
        return Ok(if witness {
            SignatureState::WitnessUnchecked
        } else {
            SignatureState::Unchecked
        });
    };
    if witness {
        // The witness identity comes from the trusted signer file, never the
        // developer's --identity or user.email. Discovery alone is not proof.
        let signers = Path::new(&signers);
        for principal in minds_attest::ssh_find_principals(&signature, signers)? {
            if minds_attest::ssh_verify_ns(
                text,
                &signature,
                signers,
                &principal,
                minds_attest::NS_WITNESS,
            )? {
                return Ok(SignatureState::WitnessValid(principal));
            }
        }
        return Ok(SignatureState::WitnessInvalid);
    }
    let explicit = identity.is_some();
    let Some(identity) = identity
        .map(str::to_string)
        .or_else(|| git_config(root, "user.email"))
    else {
        return Ok(SignatureState::Unchecked);
    };
    if !minds_attest::ssh_keygen_available() {
        return Ok(SignatureState::Unchecked);
    }
    match minds_attest::ssh_verify(text, &signature, Path::new(&signers), &identity)? {
        true => Ok(SignatureState::Valid),
        // Nur eine explizit genannte Identität macht aus „verifiziert nicht"
        // einen Manipulationsbefund; die geratene ist eine offene Zuordnung.
        false if explicit => Ok(SignatureState::Invalid),
        false => Ok(SignatureState::NotAttributable),
    }
}

/// Coverage vollständig ⇔ jeder Seal gap-frei und `stored`, und die
/// `previous`-Kette schließt sich — mit der Epochen-Semantik aus ADR-0011:
/// Epochen sind eigene Sessions, ein aufgelöster `stored`-Vorgänger schließt
/// die Kette, ein Block-Seal mit identischem Root ist ein Policy-Fix (keine
/// Lücke), alles Baumelnde bleibt offen.
fn coverage_complete(
    store: &dyn ContextStore,
    checked: &[(&ContentHash, &Seal)],
    reasons: &mut Vec<String>,
) -> Fallible<ChainState> {
    let mut complete = true;
    // Nur die Ketten-Befunde (Gabelung, offener oder zurückgewiesener
    // Vorgänger, mehrere Anfänge) — Lücken innerhalb eines Seals schließen
    // die Kette nicht auf (EA-11: Eingang `chain_closed`).
    let mut chain_open = false;

    for (id, seal) in checked {
        if seal.gaps > 0 {
            reasons.push(format!(
                "seal {id}: {} gap(s) in range {}–{}",
                seal.gaps, seal.first_seq, seal.last_seq
            ));
            complete = false;
        }
        if seal.pre_chain > 0 {
            reasons.push(format!(
                "seal {id}: {} event(s) captured before the evidence chain (unbound)",
                seal.pre_chain
            ));
            complete = false;
        }
        if matches!(seal.outcome, SealOutcome::Rejected) {
            reasons.push(format!("seal {id}: payload rejected by the storage policy"));
            complete = false;
        }
    }

    // Epochenkette: Epochen sind per Design EIGENE Sessions (ADR-0011 E2) —
    // ein `previous`, das auf einen auflösbaren, hash-validen stored-Seal
    // führt, SCHLIESST die Kette (sie setzt sich in der Vorgänger-Session
    // fort). Ein Block-Seal als Vorgänger ist nur dann Geschichte, wenn sein
    // Root identisch ist (Policy-Fix: dieselben Events wurden später doch
    // gespeichert); sonst eine zurückgewiesene Epoche. Baumelnde oder
    // unlesbare Vorgänger bleiben offen. Innerhalb der Menge: eine Linie,
    // kein Fork.
    let in_set: BTreeMap<&ContentHash, &Seal> = checked.iter().copied().collect();
    let mut entry_points = 0usize;
    let mut internal_targets: std::collections::BTreeSet<&ContentHash> =
        std::collections::BTreeSet::new();
    for (id, seal) in checked {
        match &seal.previous {
            None => entry_points += 1,
            Some(prev) if in_set.contains_key(prev) => {
                if !internal_targets.insert(prev) {
                    reasons.push(format!(
                        "epoch fork: multiple seals build on {prev} — order not attested"
                    ));
                    complete = false;
                    chain_open = true;
                }
            }
            Some(prev) => match store.seal_text(prev) {
                Ok(Some(text)) => match Seal::parse(&text) {
                    Ok(prev_seal) => match &prev_seal.outcome {
                        // Eine `witness-fs/v1`-Epoche folgt auf die
                        // vorige des Witness-Streams: gespeichert wie
                        // `stored`, also ein geschlossener Vorgänger.
                        SealOutcome::Stored { .. } | SealOutcome::ObservationsStored { .. } => {
                            entry_points += 1
                        }
                        SealOutcome::Rejected if prev_seal.root == seal.root => {
                            entry_points += 1;
                        }
                        SealOutcome::Rejected => {
                            reasons.push(format!(
                                "the epoch before seal {id} was rejected (block seal {prev})"
                            ));
                            complete = false;
                            chain_open = true;
                        }
                    },
                    Err(_) => {
                        reasons.push(format!("predecessor seal {prev} is unreadable"));
                        complete = false;
                        chain_open = true;
                    }
                },
                Ok(None) => {
                    reasons.push(format!(
                        "predecessor seal {prev} is not in the store — epoch chain open"
                    ));
                    complete = false;
                    chain_open = true;
                }
                Err(StoreError::SealMismatch { .. }) => {
                    reasons.push(format!("predecessor seal {prev} was altered"));
                    complete = false;
                    chain_open = true;
                }
                Err(err) => return Err(err.into()),
            },
        }
    }
    // Mehrere Anfänge öffnen die Kette auch neben anderen Befunden; als
    // Grund steht es (wie bisher) nur da, wenn sonst nichts fehlt.
    if !checked.is_empty() && entry_points != 1 {
        chain_open = true;
        if complete {
            reasons.push(format!(
                "the epoch chain has {entry_points} starting points instead of one — order not attested"
            ));
            complete = false;
        }
    }

    Ok(ChainState {
        complete: complete && !checked.is_empty(),
        chain_closed: !chain_open,
    })
}

/// Was [`coverage_complete`] über die Seals einer Session sagt.
struct ChainState {
    /// Coverage vollständig: gap-frei, gespeichert, Kette geschlossen.
    complete: bool,
    /// Nur die Kette: kein offener, veränderter oder zurückgewiesener
    /// Vorgänger, keine Gabelung, genau ein Anfang.
    chain_closed: bool,
}

/// Die Seals einer Session: der Rückverweis (`evidence.json`) vereinigt mit
/// allen Seals des Namensraums, die diese Session nennen — in dieser
/// Reihenfolge, ohne Dubletten. Dazu die, die nur der Namensraum kennt.
pub(crate) fn session_seal_ids(
    store: &dyn ContextStore,
    id: SessionId,
) -> Fallible<(Vec<ContentHash>, Vec<ContentHash>)> {
    let mut ids = store.seals_of(id)?;
    let mut unreferenced = Vec::new();
    for seal_id in seals_naming(store, id)? {
        if !ids.contains(&seal_id) {
            ids.push(seal_id.clone());
            unreferenced.push(seal_id);
        }
    }
    Ok((ids, unreferenced))
}

/// Alle Seals des Namensraums lesen und
/// die behalten, deren `session=`-Zeile diese Session nennt.
///
/// Auch ein **manipulierter** Seal zählt hier, wenn sein (unverifizierter)
/// Text diese Session behauptet: Er wandert in die Prüfmenge, wo
/// `check_seals` ihn als TAMPERED meldet. Ihn still zu überspringen hieße,
/// dass ausgerechnet die Manipulation das Verdikt auf „NOT VERIFIABLE"
/// abschwächte — der Angreifer bekäme das mildere Wort geschenkt.
fn seals_naming(store: &dyn ContextStore, id: SessionId) -> Fallible<Vec<ContentHash>> {
    let mut found = Vec::new();
    for seal_id in store.list_seals()? {
        let text = match store.seal_text(&seal_id) {
            Ok(Some(text)) => text,
            Err(StoreError::SealMismatch { .. }) => {
                // Die behauptete Zuordnung aus den Roh-Bytes lesen — nur zur
                // AUFNAHME in die Prüfmenge, nie als Beleg (das Verdikt
                // spricht check_seals).
                let claims_this = store
                    .seal_bytes(&seal_id)
                    .ok()
                    .flatten()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .and_then(|text| Seal::parse(&text).ok())
                    .is_some_and(|claimed| match &claimed.outcome {
                        SealOutcome::Stored { session } => {
                            session.parse::<SessionId>().ok() == Some(id)
                        }
                        SealOutcome::ObservationsStored { .. } | SealOutcome::Rejected => false,
                    });
                if claims_this {
                    found.push(seal_id);
                }
                continue;
            }
            Ok(None) | Err(_) => continue,
        };
        let Ok(seal) = Seal::parse(&text) else {
            continue;
        };
        if let SealOutcome::Stored { session } = &seal.outcome {
            if session == &id.to_string() {
                found.push(seal_id);
            }
        }
    }
    Ok(found)
}

/// Die Assurance einer Session ohne Ausgabe — für `minds fsck
/// --require-assurance`. Dieselben Fakten wie der Verdikt-Block: dieselben
/// Seals (Rückverweis vereinigt mit dem Namensraum, `session_seal_ids`),
/// dieselbe Kette, dieselbe
/// Signaturprüfung ([`signature_state`], ohne `--identity`) und dieselbe
/// Witness-Prüfung. Was dort `TAMPERED` hieße (veränderte oder unlesbare
/// Seals, eine ungültige Witness-Signatur, eine Nutzlast, die nicht auf
/// ihre Id hasht), ist hier A0 — ein Gate in `fsck` ist nie milder als
/// `verify`. Die Coverage-Gründe des Blocks (Schritt 3) fehlen hier — sie
/// berühren die Stufe nicht (`assess` liest aus der Integrität nur
/// `Tampered`).
pub(crate) fn session_assurance(
    store: &dyn ContextStore,
    trust: &witness_trust::WitnessTrust<'_>,
    signers: Option<&str>,
    root: &Path,
    id: SessionId,
    link: Option<EvidenceSource>,
    commit: Option<&str>,
) -> Fallible<minds_reader::assurance::AssuranceReport> {
    let mut tampered = false;
    let session = match store.get(id) {
        Ok(session) => session,
        Err(StoreError::Corrupt { .. }) => {
            tampered = true;
            None
        }
        Err(StoreError::Forgotten { .. }) => None,
        Err(err) => return Err(err.into()),
    };
    let (seal_ids, _) = session_seal_ids(store, id)?;
    let mut seals: Vec<(ContentHash, Seal, String)> = Vec::new();
    let mut unreadable = false;
    for seal_id in &seal_ids {
        match store.seal_text(seal_id) {
            Ok(Some(text)) => match Seal::parse(&text) {
                Ok(seal) => {
                    // Wie `verify_session`: Eine ungültige Witness-Signatur
                    // ist Manipulation.
                    if signature_state(store, seal_id, &text, &seal.scope, signers, None, root)?
                        .is_invalid()
                    {
                        tampered = true;
                    }
                    seals.push((seal_id.clone(), seal, text));
                }
                Err(_) => tampered = true,
            },
            Ok(None) => unreadable = true,
            Err(StoreError::SealMismatch { .. }) => tampered = true,
            Err(err) => return Err(err.into()),
        }
    }
    let pairs: Vec<(&ContentHash, &Seal)> = seals.iter().map(|(id, seal, _)| (id, seal)).collect();
    let chain = coverage_complete(store, &pairs, &mut Vec::new())?;
    let trusted = |seal_id: &ContentHash, text: &str| {
        matches!(
            trust.signature(seal_id, text),
            minds_reader::assurance::SealSignature::Witness { .. }
        )
    };
    let windows = match &session {
        Some(session) => {
            minds_reader::observations::witness_windows(store, &[(id, session)], &trusted)
        }
        None => Vec::new(),
    };
    let observations = assurance::fs_coverage(store, &windows);
    let intent = assurance::intent_state(trust, id, session.as_ref(), &seals);
    Ok(assurance::report(
        trust,
        &assurance::Facts {
            id,
            session: session.as_ref(),
            seals: &seals,
            legacy: seal_ids.is_empty() && !tampered,
            tampered,
            complete: chain.complete && !unreadable,
            chain_closed: chain.chain_closed && !unreadable,
            link,
            ledger: &LedgerCheck::NotChecked,
            observations: &observations,
            intent: &intent,
            commit,
        },
    ))
}

/// Wie viele **andere** Sessions dieselbe `(agent, local_id)` tragen — der
/// heuristische Epochen-Hinweis.
fn sibling_sessions(
    store: &dyn ContextStore,
    this: SessionId,
    agent: &str,
    local_id: &str,
) -> Fallible<usize> {
    let mut count = 0;
    for id in store.list()? {
        if id == this {
            continue;
        }
        let Ok(Some(session)) = store.get(id) else {
            continue;
        };
        if session.agent.name == agent
            && session
                .lineage
                .as_ref()
                .is_some_and(|l| l.local_id == local_id)
        {
            count += 1;
        }
    }
    Ok(count)
}

fn print_seal_line(c: &CheckedSeal) {
    println!(
        "Seal           {}: seq {}–{}, {} event(s), {} gap(s), {} — {}",
        c.id,
        c.seal.first_seq,
        c.seal.last_seq,
        c.seal.events,
        c.seal.gaps,
        c.seal.outcome.human_word(),
        c.signature.word()
    );
}

// ---------------------------------------------------------------------------
// Ein einzelner Seal (--evidence)
// ---------------------------------------------------------------------------

fn verify_seal(target: &str, signers: Option<&str>, identity: Option<&str>) -> Fallible<Verdict> {
    let id: ContentHash = target
        .parse()
        .map_err(|err| format!("not a valid seal id {target:?}: {err}"))?;
    let ctx = Context::open()?;

    let text = match ctx.store.seal_text(&id) {
        Ok(Some(text)) => text,
        Ok(None) => {
            println!("Seal           {id}: not in the store");
            println!("{}", Verdict::Unverifiable.word());
            return Ok(Verdict::Unverifiable);
        }
        Err(StoreError::SealMismatch { actual, .. }) => {
            report_tampered_seal(ctx.store.as_ref(), &id, &actual, None);
            println!("{}", Verdict::Tampered.word());
            return Ok(Verdict::Tampered);
        }
        Err(err) => return Err(err.into()),
    };
    let seal = match Seal::parse(&text) {
        Ok(seal) => seal,
        Err(err) => {
            println!("Seal           {id}: {err}");
            println!("{}", Verdict::Tampered.word());
            return Ok(Verdict::Tampered);
        }
    };
    let signature = signature_state(
        ctx.store.as_ref(),
        &id,
        &text,
        &seal.scope,
        signers,
        identity,
        &ctx.root,
    )?;
    let checked = CheckedSeal {
        id,
        seal,
        text,
        signature,
    };
    print_seal_line(&checked);

    if checked.signature.is_invalid() {
        println!("{}", Verdict::Tampered.word());
        return Ok(Verdict::Tampered);
    }
    let verdict = match &checked.seal.outcome {
        SealOutcome::Rejected => {
            println!(
                "Note           payload rejected by the storage policy — the seal is the evidence that the range existed"
            );
            Verdict::Incomplete
        }
        SealOutcome::ObservationsStored { observations } => {
            println!("Observations   {observations}");
            let mut reasons = Vec::new();
            let complete = coverage_complete(
                ctx.store.as_ref(),
                &[(&checked.id, &checked.seal)],
                &mut reasons,
            )?
            .complete;
            // Wie bei `stored`: Der Seal sagt „abgelegt" — dann muss das
            // Objekt hier auch liegen und auf seine Id hashen.
            match observations
                .parse::<ContentHash>()
                .map_err(|_| ())
                .and_then(|id| ctx.store.get_observations(&id).map_err(|_| ()))
            {
                Ok(Some(_)) => {}
                Ok(None) => reasons.push("the observation object is not in this store".into()),
                Err(()) => reasons.push("the observation object is unreadable".into()),
            }
            for reason in &reasons {
                println!("Gap            {reason}");
            }
            // Das Verdikt sagt etwas über den Seal. Ob seine Beobachtungen
            // eine Zeile erklären dürfen, entscheidet die strengere
            // Witness-Prüfung — das wird hier dazugesagt, nicht verschwiegen.
            let trusted = witness_trust::observation_trust(ctx.store.as_ref(), signers);
            if !trusted(&checked.id, &checked.text) {
                println!(
                    "Note           not trusted for reconciliation — needs a principal restricted to minds-witness in --signers or ~/.ssh/allowed_signers"
                );
            }
            if complete && reasons.is_empty() {
                Verdict::Verified
            } else {
                Verdict::Incomplete
            }
        }
        SealOutcome::Stored { session } => {
            println!("Session        {session}");
            // Dieselbe Ketten-Logik wie beim Session-Verdikt: ein extern
            // aufgelöster stored-Vorgänger (oder ein Policy-Fix-Block-Seal
            // mit identischem Root) ist keine Lücke.
            let mut reasons = Vec::new();
            let complete = coverage_complete(
                ctx.store.as_ref(),
                &[(&checked.id, &checked.seal)],
                &mut reasons,
            )?
            .complete;
            for reason in &reasons {
                println!("Gap            {reason}");
            }
            if complete {
                Verdict::Verified
            } else {
                Verdict::Incomplete
            }
        }
    };
    println!("{}", verdict.word());
    Ok(verdict)
}

// ---------------------------------------------------------------------------
// Der Attestation-Pfad (unverändert)
// ---------------------------------------------------------------------------

fn verify_attestation(
    target: &str,
    sig_file: &str,
    signers: Option<&str>,
    identity: Option<&str>,
) -> Fallible<bool> {
    if !minds_attest::ssh_keygen_available() {
        return Err("ssh-keygen not found".into());
    }
    let id: SessionId = target
        .parse()
        .map_err(|err| format!("not a valid session id {target:?}: {err}"))?;

    let ctx = Context::open()?;
    let session = ctx
        .store
        .get(id)?
        .ok_or_else(|| format!("session {id} is not in the store"))?;

    let payload = minds_core::attestation_payload(id, &session)?;
    let signature = std::fs::read_to_string(sig_file)
        .map_err(|err| format!("signature file {sig_file:?} unreadable: {err}"))?;
    let signers = resolve_signers(signers, &ctx.root)?;
    let identity = resolve_identity(identity, &ctx.root)?;

    Ok(minds_attest::ssh_verify(
        &payload,
        &signature,
        Path::new(&signers),
        &identity,
    )?)
}

/// Die allowed_signers-Datei: `--signers`, sonst `git config
/// gpg.ssh.allowedSignersFile`, sonst `~/.ssh/allowed_signers`.
fn resolve_signers(signers: Option<&str>, root: &Path) -> Fallible<String> {
    resolve_signers_optional(signers, root).ok_or_else(|| {
        "no allowed_signers file: pass --signers <file> or set \
         `git config gpg.ssh.allowedSignersFile`"
            .into()
    })
}

/// Wie [`resolve_signers`], aber ohne Fehler — für Pfade, auf denen „nicht
/// prüfbar" eine gültige Antwort ist.
fn resolve_signers_optional(signers: Option<&str>, root: &Path) -> Option<String> {
    if let Some(signers) = signers {
        return Some(signers.to_string());
    }
    if let Some(configured) = git_config(root, "gpg.ssh.allowedSignersFile") {
        return Some(configured);
    }
    if let Ok(home) = std::env::var("HOME") {
        let default = format!("{home}/.ssh/allowed_signers");
        if Path::new(&default).exists() {
            return Some(default);
        }
    }
    None
}

/// Die Identität (Principal in allowed_signers): `--identity`, sonst
/// `git config user.email`.
fn resolve_identity(identity: Option<&str>, root: &Path) -> Fallible<String> {
    if let Some(identity) = identity {
        return Ok(identity.to_string());
    }
    git_config(root, "user.email")
        .ok_or_else(|| "no identity: pass --identity <id> or set `git config user.email`".into())
}

fn git_config(root: &Path, key: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", key])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !value.is_empty()).then_some(value)
}

#[cfg(test)]
mod issue_version_tests {
    use super::*;
    use minds_reader::assurance::{IntentSignature, SignerKind};

    /// Ein Befund, der wie eine Bestätigung klingt, nennt seine Grenzen;
    /// ein negativer bleibt, wie er ist.
    #[test]
    fn positive_findings_name_what_they_do_not_say() {
        let signed = IntentSignature::Valid(SignerKind::SecurityKey);
        let current = IssueVersion::Current;
        assert_eq!(issue_version_text(current, &signed, true, false), "current");
        assert_eq!(
            issue_version_text(current, &signed, true, true),
            "current (redacted spans not compared)"
        );
        assert_eq!(
            issue_version_text(current, &IntentSignature::Unsigned, true, false),
            "current (anchor unsigned)"
        );
        // Ein ausgetauschter Snapshot ohne Platzhalter verliert den Hinweis
        // nicht still.
        assert_eq!(
            issue_version_text(current, &IntentSignature::Invalid, false, false),
            "current (stored snapshot does not match the anchor, redaction state unknown, \
             anchor signature invalid)"
        );
        assert_eq!(
            issue_version_text(
                IssueVersion::Changed(IssueHistory::Confirmed),
                &IntentSignature::NotChecked,
                true,
                false
            ),
            "changed since binding — bound version confirmed in description history \
             (anchor signature not checked)"
        );
        for negative in [
            IssueVersion::NotChecked,
            IssueVersion::Changed(IssueHistory::NotFound),
            IssueVersion::Unavailable("unauthorized (HTTP 401)"),
        ] {
            assert_eq!(
                issue_version_text(negative, &IntentSignature::Unsigned, false, true),
                negative.text()
            );
        }
    }
}
