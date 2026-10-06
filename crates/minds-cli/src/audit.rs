//! `minds audit --export` — die Provenienz-Kette als portables Bündel
//! (Schicht 3, R6).
//!
//! Die Frage, die ein Auditor stellt, lautet nicht „habt ihr Reviews?", sondern:
//! **Wer hat diese Zeilen geschrieben, auf welche Anweisung, wer hat sie geprüft,
//! und warum wurde gemerged?** Alle vier Antworten liegen im Repo — verteilt über
//! Trailer, Store, Attribution und Review-Ref. Dieses Kommando legt sie in
//! *einer* Datei nebeneinander:
//!
//! ```text
//! Change ──▶ Commits ──▶ Sessions ──▶ Attribution ──▶ Verdicts (+ Signaturen)
//! ```
//!
//! # Warum ein Bündel und nicht ein Bericht
//!
//! Ein Bericht wäre eine Behauptung über das Repo. Das Bündel enthält die
//! **prüfbaren Bestandteile**: die Session-Ids (Hashes ihres Inhalts), die
//! kanonischen Attestation- und Review-Payloads (byte-genau die Texte, über die
//! signiert wird) und die vorhandenen Signaturen. Wer es bekommt, kann jeden
//! Hash und jede Signatur ohne dieses Werkzeug nachrechnen — mit `blake3` und
//! `ssh-keygen -Y verify`.
//!
//! Was das Bündel **nicht** kann, steht in `docs/verification-guide.md`. Ein
//! Export, dessen Grenzen nicht mitgeliefert werden, lädt zur Überinterpretation
//! ein, und das wäre bei einem Nachweis-Artefakt der schlimmste Fehler.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use minds_core::evidence::{ProofSentence, limits_at, proves_at};
use minds_core::{ChangeId, Review, SessionId, Trailer, attestation_payload, review_payload};
use minds_git::{CommitId, Repo};
use minds_reader::assurance::Assurance;
use minds_store::{ContextStore, ReviewStore};
use serde::Serialize;

use crate::config;
use crate::verify_cmd::witness_trust::WitnessTrust;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Schema-Version des Bündels. Ein Auditor liest sie zuerst.
///
/// v2 (ADR-0011): je Session ihre Evidence-Seals (byte-genauer Text plus
/// Signatur), dazu die sessionlosen Block-Seals unter `rejected_seals` —
/// zurückgehaltene Sessions sind Teil der Kette, nicht ihr blinder Fleck.
///
/// v3 (EA-13): das Bündel nennt seine Assurance-Stufe (`assurance`, die
/// schwächste Session) und je Session deren Stufe; `proves` und
/// `does_not_prove` sind die Sätze **dieser** Stufe, jeder mit stabiler
/// `id` statt als bloßer Text.
const BUNDLE_SCHEMA_VERSION: u32 = 3;

/// Der Zuschnitt des Bündels (Phase 7).
///
/// **`full` gibt es absichtlich nicht:** Der Store hält ausschließlich
/// redigierte Sessions (fail-closed) — ein Modus, der „mehr als redacted"
/// verspräche, wäre ein leeres Versprechen oder ein Leck. `redacted` ist
/// deshalb das Maximum, `proof` das Minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Alles, was der Store hergibt: Intents, Payloads, Verdicts, Kommentare,
    /// Seals. Der bisherige (und Default-)Zuschnitt.
    Redacted,
    /// Nur das Beweisgerüst: Ids, kanonische Payload-Texte, Seals samt
    /// Signaturen, Verdict-Metadaten. Kein Intent, keine Zusammenfassungen,
    /// keine Kommentare — prüfbar, ohne Inhalt weiterzugeben.
    Proof,
}

impl Mode {
    fn word(self) -> &'static str {
        match self {
            Mode::Redacted => "redacted",
            Mode::Proof => "proof",
        }
    }
}

/// Führt `minds audit` aus.
pub fn run(
    export: bool,
    out: Option<&str>,
    base: Option<&str>,
    mode: Option<&str>,
    signers: Option<&str>,
) -> ExitCode {
    if !export {
        eprintln!("minds audit: expected --export");
        return ExitCode::FAILURE;
    }
    let mode = match mode {
        None | Some("redacted") => Mode::Redacted,
        Some("proof") => Mode::Proof,
        Some(other) => {
            eprintln!(
                "minds audit: unknown mode {other:?} — redacted or proof \
                 (there is deliberately no full: the store holds redacted content only)"
            );
            return ExitCode::FAILURE;
        }
    };
    match audit(out, base, mode, signers) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds audit: {err}");
            ExitCode::FAILURE
        }
    }
}

// --- Das Bündel -------------------------------------------------------------

#[derive(Debug, Serialize)]
struct Bundle {
    schema_version: u32,
    /// Der Zuschnitt: `redacted` oder `proof`.
    mode: &'static str,
    generated_at: String,
    repository: RepositoryInfo,
    /// Die Stufe des Bündels und was es auf ihr belegt — und was nicht. Im
    /// Artefakt selbst, nicht nur in der Doku: Es wird weitergereicht, die
    /// Doku bleibt zurück.
    #[serde(flatten)]
    proof: ProofSection,
    changes: Vec<ChangeRecord>,
    /// Block-Seals (ADR-0011): Sessions, deren Nutzlast die Speicher-Policy
    /// zurückwies. Es gibt keine Session-Id — der Seal ist der Beweis, dass
    /// der Bereich existierte.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    rejected_seals: Vec<SealRecord>,
}

/// Stufe und Proof-Vokabular des Bündels (EA-13): `proves` und
/// `does_not_prove` sind genau die Sätze, die auf `assurance.level` gelten
/// ([`minds_core::evidence::proves_at`], [`minds_core::evidence::limits_at`])
/// — dasselbe Vokabular wie TUI und `minds verify`.
#[derive(Debug, Serialize)]
struct ProofSection {
    assurance: BundleAssurance,
    proves: Vec<SentenceRecord>,
    does_not_prove: Vec<SentenceRecord>,
}

/// Die Stufe des Bündels: die schwächste seiner Sessions — ein Bündel
/// verspricht nie mehr als sein schwächster Teil. Ohne Session `A0`, mit
/// ausdrücklichem Grund. Berechnet beim Export, nie gespeichert (W2) — und
/// eine Einschätzung des Exporteurs, die sich aus dem Bündel allein nicht
/// nachrechnen lässt (`bundle_level_self_reported`).
#[derive(Debug, Serialize)]
struct BundleAssurance {
    /// `A0` … `A3`.
    level: &'static str,
    /// `A1 observed` …
    name: &'static str,
    /// Ob für die Witness-Prüfung eine vertrauenswürdige Signer-Datei
    /// (`--signers` oder `~/.ssh/allowed_signers`) samt `ssh-keygen`
    /// **verfügbar** war — nicht, dass eine Witness-Signatur geprüft wurde.
    /// Ohne sie gibt es kein A2; das Feld sagt dem Prüfer, warum.
    trusted_signers_available: bool,
    /// Warum das Bündel keine Session-Stufe trägt (leerer Bereich).
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

/// Der Grund eines Bündels ohne Session.
const NO_SESSION_IN_RANGE: &str = "no agent session in the exported range";

/// Ein Satz des Proof-Vokabulars: stabile Id und voller Text.
#[derive(Debug, Serialize)]
struct SentenceRecord {
    id: &'static str,
    text: &'static str,
}

/// Die Stufe einer Session im Bündel.
#[derive(Debug, Clone, Serialize)]
struct SessionAssurance {
    /// `A0` … `A3`.
    level: &'static str,
    /// Der erste Grund, warum die Stufe nicht höher liegt — derselbe
    /// Wortlaut wie die `Assurance`-Zeile von `minds verify`.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct RepositoryInfo {
    head: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChangeRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    change_id: Option<String>,
    commits: Vec<String>,
    sessions: Vec<SessionRecord>,
    verdicts: Vec<VerdictRecord>,
    comments: Vec<CommentRecord>,
}

#[derive(Debug, Serialize)]
struct SessionRecord {
    id: String,
    agent: String,
    model: String,
    /// Die Anweisung, auf die hin gearbeitet wurde — das „auf welche Anweisung"
    /// aus der Frage, die dieses Bündel beantworten soll.
    #[serde(skip_serializing_if = "String::is_empty")]
    intent: String,
    /// Der kanonische Text, über den `minds sign` signiert. Byte-genau — wer eine
    /// Signatur hat, prüft sie hiergegen.
    attestation_payload: String,
    /// Ob die Nutzlast noch da ist. Eine per `minds forget` getilgte Session
    /// bleibt in der Kette **sichtbar** — das ist der Punkt an einer redigierbaren
    /// Nutzlast: Die Referenz ist auflösbar, der Inhalt weg.
    payload: PayloadState,
    /// Die Assurance-Stufe der Session (EA-11), beim Export berechnet.
    assurance: SessionAssurance,
    /// Die Evidence-Seals der Session (ADR-0011), in Epochen-Reihenfolge.
    /// Byte-genau — `seal_id = derive_key(\"minds/evidence/v1/seal\", text)`
    /// lässt sich extern nachrechnen, eine Signatur dagegen prüfen.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    seals: Vec<SealRecord>,
}

/// Ein Seal, byte-genau, mit seiner Signatur.
#[derive(Debug, Serialize)]
struct SealRecord {
    id: String,
    /// Der Seal-Text, exakt wie abgelegt — die signierten Bytes.
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum PayloadState {
    Present,
    Forgotten,
    Missing,
    /// Die Nutzlast ist da, aber ein Feld könnte im signierbaren Klartext
    /// eine Zeile fälschen oder Text verstecken (#12). Fail-closed ohne
    /// Abbruch: kein fälschbarer Payload im Bündel, aber der Eintrag bleibt
    /// sichtbar — übersprungen wird gezählt, nicht abgebrochen (#83).
    Unsignable,
}

#[derive(Debug, Serialize)]
struct VerdictRecord {
    hash: String,
    decision: String,
    reviewer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    at: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    summary: String,
    /// Der kanonische Text, über den signiert wird. Leer, wenn ein Feld ihn
    /// fälschen könnte — dann benennt `payload_error` das Feld (#12).
    review_payload: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
}

#[derive(Debug, Serialize)]
struct CommentRecord {
    hash: String,
    anchor: String,
    author: String,
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    at: Option<String>,
}

// --- Der Aufbau -------------------------------------------------------------

fn audit(out: Option<&str>, base: Option<&str>, mode: Mode, signers: Option<&str>) -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);
    let store = config::load(&root).open(&root)?;
    let reviews = ReviewStore::new(Repo::open(&root)?);
    // Vertrauen für die Witness-Prüfung: nur `--signers` oder
    // `~/.ssh/allowed_signers`, nie die Repo-Konfiguration (wie `verify`).
    // Ein ausdrücklich genanntes `--signers`, das sich nicht lesen lässt,
    // ist ein Fehler — sonst fiele ein Tippfehler still auf A1 zurück.
    if let Some(path) = signers {
        // Dieselben Grenzen wie `WitnessTrust::new` (reguläre Datei, höchstens
        // `MAX_SIGNERS_BYTES`) — was sie verwürfe, scheitert hier laut.
        let readable = std::fs::metadata(path).is_ok_and(|meta| {
            meta.is_file() && meta.len() <= crate::verify_cmd::witness_trust::MAX_SIGNERS_BYTES
        }) && std::fs::read_to_string(path).is_ok();
        if !readable {
            return Err(format!("--signers {path}: not a readable file").into());
        }
    }
    let trust = WitnessTrust::new(store.as_ref(), signers);
    let mut levels: BTreeMap<SessionId, (Assurance, SessionAssurance)> = BTreeMap::new();

    let head = repo.head()?.commit().ok_or("HEAD has no commit yet")?;

    // Commits einsammeln — ab der Basis, sonst die ganze erreichbare Historie.
    let commits: Vec<CommitId> = match base {
        Some(base) => commits_since(&root, base)?,
        None => repo.revwalk(head)?.collect::<Result<_, _>>()?,
    };

    // Nach Change-Id bündeln. Was keine trägt, kommt unter `None` zusammen —
    // sichtbar, statt weggelassen.
    let mut grouped: BTreeMap<Option<String>, ChangeRecord> = BTreeMap::new();
    for commit in commits {
        let sessions = repo.session_ids_of(commit)?;
        if sessions.is_empty() {
            continue; // nicht agent-authored — kein Teil dieser Kette
        }
        let change = change_id_of(&root, commit).map(|id| id.to_string());
        let record = grouped
            .entry(change.clone())
            .or_insert_with(|| ChangeRecord {
                change_id: change.clone(),
                commits: Vec::new(),
                sessions: Vec::new(),
                verdicts: Vec::new(),
                comments: Vec::new(),
            });
        record.commits.push(commit.to_string());
        for id in sessions {
            if record.sessions.iter().any(|s| s.id == id.to_string()) {
                continue;
            }
            let assurance = session_level(store.as_ref(), &trust, signers, &root, id, &mut levels);
            record
                .sessions
                .push(session_record(store.as_ref(), id, assurance)?);
        }
    }

    // Verdicts und Thread anhängen — an der Change-Id und ersatzweise an jeder
    // Session-Id, weil ein Verdict auch daran hängen darf.
    for (change, record) in grouped.iter_mut() {
        let mut subjects: Vec<String> = change.iter().cloned().collect();
        subjects.extend(record.sessions.iter().map(|s| s.id.clone()));
        for subject in &subjects {
            for review in reviews.for_subject(subject)? {
                record.verdicts.push(verdict_record(&reviews, &review)?);
            }
            for comment in reviews.thread(subject)? {
                record.comments.push(CommentRecord {
                    hash: comment.content_hash()?.to_string(),
                    anchor: comment.anchor.as_text(),
                    author: comment.author.clone(),
                    body: comment.body.clone(),
                    at: comment.at.clone(),
                });
            }
        }
    }

    // Der Proof-Zuschnitt: das Beweisgerüst behalten, den Inhalt entfernen.
    // Bewusst NACH dem vollen Aufbau — ein Filter über fertigen Records kann
    // nichts vergessen, was ein zweiter Aufbau-Pfad vergessen könnte.
    if mode == Mode::Proof {
        for record in grouped.values_mut() {
            for session in &mut record.sessions {
                session.intent.clear();
            }
            for verdict in &mut record.verdicts {
                verdict.summary.clear();
            }
            record.comments.clear();
        }
    }

    let (generated_at, _) = minds_capture::clock::now();
    let bundle = Bundle {
        schema_version: BUNDLE_SCHEMA_VERSION,
        mode: mode.word(),
        generated_at,
        repository: RepositoryInfo {
            head: head.to_string(),
            branch: repo.head()?.branch().map(str::to_owned),
            // Eine Remote-URL kann eingebettete Zugangsdaten tragen
            // (`https://oauth2:glpat-…@…`) — die Senken-Redaktion aus #92
            // greift auch hier; im Proof-Modus entfällt das Feld ganz.
            origin: match mode {
                Mode::Proof => None,
                Mode::Redacted => git(&root, &["remote", "get-url", "origin"])
                    .map(|url| crate::text::without_url_credentials(&url)),
            },
        },
        proof: match levels.values().map(|(level, _)| *level).min() {
            Some(weakest) => proof_section(weakest, trust.trusted(), None),
            None => proof_section(
                Assurance::A0Claimed,
                trust.trusted(),
                Some(NO_SESSION_IN_RANGE),
            ),
        },
        changes: grouped.into_values().collect(),
        rejected_seals: rejected_seal_records(store.as_ref()),
    };

    let json = serde_json::to_string_pretty(&bundle)?;
    match out {
        Some(path) => {
            std::fs::write(path, format!("{json}\n"))?;
            println!(
                "{} change(s) exported → {path}",
                bundle_len(&bundle.changes)
            );
        }
        None => println!("{json}"),
    }
    Ok(())
}

fn bundle_len(changes: &[ChangeRecord]) -> usize {
    changes.len()
}

/// Stufe und Vokabular des Bündels: was es auf `level` belegt und was
/// nicht — das kanonische Vokabular aus `minds-core`, dieselben Sätze wie
/// in TUI und `minds verify`. Die Grenzen gehören ins Artefakt, nicht nur
/// in die Doku.
fn proof_section(
    level: Assurance,
    trusted_signers_available: bool,
    reason: Option<&'static str>,
) -> ProofSection {
    let record = |sentence: &'static ProofSentence| SentenceRecord {
        id: sentence.id,
        text: sentence.text,
    };
    ProofSection {
        assurance: BundleAssurance {
            level: level.code(),
            name: level.word(),
            trusted_signers_available,
            reason,
        },
        proves: proves_at(level.level()).map(record).collect(),
        does_not_prove: limits_at(level.level()).map(record).collect(),
    }
}

/// Die Stufe einer Session — derselbe Pfad wie `minds verify` und `minds
/// fsck --require-assurance` ([`crate::verify_cmd::session_assurance`]),
/// je Session einmal gerechnet. Lässt sie sich nicht rechnen, steht `A0`
/// da (fail-closed: die niedrigste Stufe, die meisten Grenzen) — das Bündel
/// bricht daran nicht ab.
fn session_level(
    store: &dyn ContextStore,
    trust: &WitnessTrust<'_>,
    signers: Option<&str>,
    root: &Path,
    id: SessionId,
    levels: &mut BTreeMap<SessionId, (Assurance, SessionAssurance)>,
) -> SessionAssurance {
    if let Some((_, known)) = levels.get(&id) {
        return known.clone();
    }
    let (level, reason) = match crate::verify_cmd::session_assurance(
        store,
        trust,
        signers,
        root,
        id,
        Some(minds_core::EvidenceSource::Observed),
    ) {
        Ok(report) => (
            report.overall,
            crate::verify_cmd::assurance::first_reason(&report),
        ),
        Err(err) => {
            // Die Ursache nur auf stderr — sie kann Pfade tragen und gehört
            // nicht ins Bündel.
            eprintln!("minds audit: assurance of {id} not assessed: {err}");
            (
                Assurance::A0Claimed,
                Some("assurance not assessed — evidence unreadable".to_owned()),
            )
        }
    };
    let record = SessionAssurance {
        level: level.code(),
        reason,
    };
    levels.insert(id, (level, record.clone()));
    record
}

fn session_record(
    store: &dyn ContextStore,
    id: SessionId,
    assurance: SessionAssurance,
) -> Fallible<SessionRecord> {
    match store.get(id) {
        Ok(Some(session)) => {
            // Ein manipuliertes Feld legt nicht den ganzen Audit lahm — genau
            // dieses Bündel bräuchte man, um den Eintrag zu untersuchen. Der
            // Payload bleibt dann leer (nichts Fälschbares), der Zustand
            // benennt es.
            let (payload_text, payload) = match attestation_payload(id, &session) {
                Ok(payload) => (payload, PayloadState::Present),
                Err(_) => (String::new(), PayloadState::Unsignable),
            };
            Ok(SessionRecord {
                id: id.to_string(),
                agent: format!("{} {}", session.agent.name, session.agent.version),
                model: format!("{}/{}", session.model.provider, session.model.id),
                intent: session.intent.request.clone(),
                attestation_payload: payload_text,
                payload,
                assurance,
                seals: seal_records_of(store, id),
            })
        }
        // Getilgt: Die Referenz bleibt in der Kette, der Inhalt fehlt. Genau das
        // soll ein Auditor sehen können.
        Err(minds_store::StoreError::Forgotten { .. }) => {
            Ok(forgotten(store, id, PayloadState::Forgotten, assurance))
        }
        Ok(None) => Ok(forgotten(store, id, PayloadState::Missing, assurance)),
        Err(err) => Err(err.into()),
    }
}

fn forgotten(
    store: &dyn ContextStore,
    id: SessionId,
    payload: PayloadState,
    assurance: SessionAssurance,
) -> SessionRecord {
    SessionRecord {
        id: id.to_string(),
        agent: String::new(),
        model: String::new(),
        intent: String::new(),
        attestation_payload: String::new(),
        payload,
        assurance,
        // Auch eine getilgte Session behält ihre Seals — der payload-freie
        // Beweis überlebt das forget (ADR-0011, Entscheidung 4).
        seals: seal_records_of(store, id),
    }
}

/// Die Seals einer Session als Bündel-Einträge, best-effort: Was nicht lesbar
/// ist, fehlt — das Bündel bricht an einem kaputten Seal nicht ab.
fn seal_records_of(store: &dyn ContextStore, id: SessionId) -> Vec<SealRecord> {
    let Ok(seal_ids) = store.seals_of(id) else {
        return Vec::new();
    };
    seal_ids
        .iter()
        .filter_map(|seal_id| {
            let text = store.seal_text(seal_id).ok().flatten()?;
            let signature = store.seal_signature(seal_id).ok().flatten();
            Some(SealRecord {
                id: seal_id.to_string(),
                text,
                signature,
            })
        })
        .collect()
}

/// Die sessionlosen Block-Seals des Repos.
fn rejected_seal_records(store: &dyn ContextStore) -> Vec<SealRecord> {
    let Ok(all) = store.list_seals() else {
        return Vec::new();
    };
    all.iter()
        .filter_map(|seal_id| {
            let text = store.seal_text(seal_id).ok().flatten()?;
            let seal = minds_core::evidence::Seal::parse(&text).ok()?;
            if !matches!(seal.outcome, minds_core::evidence::SealOutcome::Rejected) {
                return None;
            }
            let signature = store.seal_signature(seal_id).ok().flatten();
            Some(SealRecord {
                id: seal_id.to_string(),
                text,
                signature,
            })
        })
        .collect()
}

fn verdict_record(store: &ReviewStore, review: &Review) -> Fallible<VerdictRecord> {
    let hash = review.content_hash()?;
    // Wie bei den Sessions: degradieren statt abbrechen. Der Fehlertext
    // benennt nur das Feld, nie den Wert.
    let (payload_text, payload_error) = match review_payload(&hash, review) {
        Ok(payload) => (payload, None),
        Err(err) => (String::new(), Some(err.to_string())),
    };
    Ok(VerdictRecord {
        hash: hash.to_string(),
        decision: review.decision.as_str().to_string(),
        reviewer: review.reviewer.clone(),
        at: review.at.clone(),
        summary: review.summary.clone(),
        review_payload: payload_text,
        payload_error,
        signature: store.signature(&hash)?,
    })
}

// --- Kleinkram --------------------------------------------------------------

fn commits_since(root: &Path, base: &str) -> Fallible<Vec<CommitId>> {
    let range = format!("{base}..HEAD");
    let out = git(root, &["rev-list", "--end-of-options", &range]).unwrap_or_default();
    out.lines()
        .map(|line| line.parse::<CommitId>().map_err(Into::into))
        .collect()
}

fn change_id_of(root: &Path, commit: CommitId) -> Option<ChangeId> {
    let message = git(
        root,
        &[
            "show",
            "-s",
            "--format=%B",
            "--end-of-options",
            &commit.to_string(),
        ],
    )?;
    Trailer::change_id(&message)
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn repo_root(repo: &Repo) -> PathBuf {
    repo.git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section_json(level: Assurance, trusted_signers_available: bool) -> String {
        format!(
            "{}\n",
            serde_json::to_string_pretty(&proof_section(level, trusted_signers_available, None))
                .unwrap()
        )
    }

    // --- Golden: der Proof-Abschnitt des Bündels je Stufe -------------------
    //
    // Eingefroren als Datei (lesbar im Review) **und** als Hash (fängt jede
    // Byte-Änderung, auch eine, die beim Lesen untergeht). Neu erzeugen mit:
    //   cargo test -p minds-cli --bin minds -- --ignored --nocapture audit_proof_reference

    const GOLDEN_A1: &str = include_str!("../tests/fixtures/audit-proof/a1.json");
    const GOLDEN_A2: &str = include_str!("../tests/fixtures/audit-proof/a2.json");
    const GOLDEN_A1_HASH: &str = "2efb2b2ba207272e5cc755a1e85b1bf8213d84d8848936499433a6565b2aadf2";
    const GOLDEN_A2_HASH: &str = "174a69ffd3db901d8badfb51f1339093249e8b7c1fcee513fcf88bc7cdf24bf8";

    #[test]
    fn audit_bundle_carries_level_golden() {
        let a1 = section_json(Assurance::A1Observed, false);
        let a2 = section_json(Assurance::A2Witnessed, true);
        assert_eq!(a1, GOLDEN_A1);
        assert_eq!(a2, GOLDEN_A2);
        assert_eq!(
            blake3::hash(a1.as_bytes()).to_hex().as_str(),
            GOLDEN_A1_HASH
        );
        assert_eq!(
            blake3::hash(a2.as_bytes()).to_hex().as_str(),
            GOLDEN_A2_HASH
        );
    }

    /// Ein A1-Bündel nennt die append→seal-Lücke, ein A2-Bündel nicht —
    /// dafür die A2-Grenzen und die A2-Zusagen.
    #[test]
    fn the_bundle_states_exactly_the_sentences_of_its_level() {
        let ids = |records: &[SentenceRecord]| records.iter().map(|r| r.id).collect::<Vec<_>>();
        let a1 = proof_section(Assurance::A1Observed, false, None);
        assert_eq!(a1.assurance.level, "A1");
        assert_eq!(a1.assurance.name, "A1 observed");
        assert!(ids(&a1.does_not_prove).contains(&"append_to_seal_window"));
        assert!(!ids(&a1.proves).contains(&"witness_chaining"));

        let a2 = proof_section(Assurance::A2Witnessed, true, None);
        assert_eq!(a2.assurance.level, "A2");
        let limits = ids(&a2.does_not_prove);
        assert!(!limits.contains(&"append_to_seal_window"));
        assert!(limits.contains(&"who_controls_keys_witnessed"));
        assert!(limits.contains(&"only_actor_witnessed"));
        assert!(ids(&a2.proves).contains(&"witness_chaining"));
        // Die Stufe trägt den Abgleich mit dem Commit nicht.
        assert!(limits.contains(&"lines_attributed"));
        // Jedes Bündel sagt, dass es seine Stufe nur behauptet.
        assert!(ids(&a1.does_not_prove).contains(&"bundle_level_self_reported"));
        assert!(limits.contains(&"bundle_level_self_reported"));

        // Jede Stufe: genau die Sätze aus `minds-core`, in derselben Folge.
        for level in Assurance::ALL {
            let section = proof_section(level, false, None);
            let proves: Vec<&str> = proves_at(level.level()).map(|s| s.id).collect();
            let limits: Vec<&str> = limits_at(level.level()).map(|s| s.id).collect();
            assert_eq!(ids(&section.proves), proves);
            assert_eq!(ids(&section.does_not_prove), limits);
        }
    }

    #[test]
    #[ignore = "Golden-Dateien neu erzeugen: --ignored --nocapture"]
    fn audit_proof_reference() {
        for (name, level, signers) in [
            ("a1", Assurance::A1Observed, false),
            ("a2", Assurance::A2Witnessed, true),
        ] {
            let json = section_json(level, signers);
            println!(
                "--- {name}.json  blake3 {}",
                blake3::hash(json.as_bytes()).to_hex()
            );
            print!("{json}");
        }
    }
}
