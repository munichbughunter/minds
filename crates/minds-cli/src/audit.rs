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
//! Was das Bündel **nicht** kann, steht in `docs/nachweis-leitfaden.md`. Ein
//! Export, dessen Grenzen nicht mitgeliefert werden, lädt zur Überinterpretation
//! ein, und das wäre bei einem Nachweis-Artefakt der schlimmste Fehler.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use minds_core::{ChangeId, Review, SessionId, Trailer, attestation_payload, review_payload};
use minds_git::{CommitId, Repo};
use minds_store::{ContextStore, ReviewStore};
use serde::Serialize;

use crate::config;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Schema-Version des Bündels. Ein Auditor liest sie zuerst.
const BUNDLE_SCHEMA_VERSION: u32 = 1;

/// Führt `minds audit` aus.
pub fn run(export: bool, out: Option<&str>, base: Option<&str>) -> ExitCode {
    if !export {
        eprintln!("minds audit: erwartet --export");
        return ExitCode::FAILURE;
    }
    match audit(out, base) {
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
    generated_at: String,
    repository: RepositoryInfo,
    /// Was dieses Bündel belegt — und was nicht. Im Artefakt selbst, nicht nur
    /// in der Doku: Es wird weitergereicht, die Doku bleibt zurück.
    proves: Vec<String>,
    does_not_prove: Vec<String>,
    changes: Vec<ChangeRecord>,
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

fn audit(out: Option<&str>, base: Option<&str>) -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);
    let store = config::load(&root).open(&root)?;
    let reviews = ReviewStore::new(Repo::open(&root)?);

    let head = repo.head()?.commit().ok_or("HEAD hat noch keinen Commit")?;

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
            record.sessions.push(session_record(store.as_ref(), id)?);
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

    let (generated_at, _) = minds_capture::clock::now();
    let bundle = Bundle {
        schema_version: BUNDLE_SCHEMA_VERSION,
        generated_at,
        repository: RepositoryInfo {
            head: head.to_string(),
            branch: repo.head()?.branch().map(str::to_owned),
            origin: git(&root, &["remote", "get-url", "origin"]),
        },
        proves: proves(),
        does_not_prove: does_not_prove(),
        changes: grouped.into_values().collect(),
    };

    let json = serde_json::to_string_pretty(&bundle)?;
    match out {
        Some(path) => {
            std::fs::write(path, format!("{json}\n"))?;
            println!(
                "{} Change(s) exportiert → {path}",
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

/// Was das Bündel belegt.
fn proves() -> Vec<String> {
    [
        "Jede Session-Id ist der blake3-Hash ihres kanonischen Inhalts — der Inhalt lässt sich gegen sie nachrechnen.",
        "Der attestation_payload ist byte-genau der Text, über den `minds sign` signiert; eine mitgelieferte Signatur ist dagegen prüfbar.",
        "Der review_payload bindet den Hash des Verdicts; eine gültige Signatur darüber weist aus, wer geprüft hat.",
        "Verdicts hängen an der Change-Id und überleben damit Rebase und Force-Push.",
        "Eine getilgte Session bleibt als Referenz sichtbar (payload: forgotten) — Löschung ist nachweisbar, nicht spurlos.",
    ]
    .iter()
    .map(|line| line.to_string())
    .collect()
}

/// Was es nicht belegt. Gehört ins Artefakt, nicht nur in die Doku.
fn does_not_prove() -> Vec<String> {
    [
        "Nicht, dass der Record vollständig ist: Der heiße Pfad ist fail-open, ein verlorenes Event fehlt hier stillschweigend (`minds fsck` macht Lücken sichtbar).",
        "Nicht, dass eine Session tatsächlich die genannten Zeilen erzeugt hat — die Zuordnung stammt aus Trailern (beobachtet) und Heuristik (vermutet); die Herkunft steht an jeder Kante.",
        "Nicht, dass ein Modell das getan hat, was im Transkript steht — aufgezeichnet ist, was der Agent gemeldet hat.",
        "Nicht, wer die Signaturschlüssel kontrolliert. Ohne eine allowed_signers-Datei aus vertrauenswürdiger Quelle ist eine Signatur nur eine Selbstauskunft.",
        "Nicht, dass unsignierte Einträge echt sind: Sie sind content-adressiert, aber niemand steht mit einem Schlüssel dafür ein.",
    ]
    .iter()
    .map(|line| line.to_string())
    .collect()
}

fn session_record(store: &dyn ContextStore, id: SessionId) -> Fallible<SessionRecord> {
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
            })
        }
        // Getilgt: Die Referenz bleibt in der Kette, der Inhalt fehlt. Genau das
        // soll ein Auditor sehen können.
        Err(minds_store::StoreError::Forgotten { .. }) => {
            Ok(forgotten(id, PayloadState::Forgotten))
        }
        Ok(None) => Ok(forgotten(id, PayloadState::Missing)),
        Err(err) => Err(err.into()),
    }
}

fn forgotten(id: SessionId, payload: PayloadState) -> SessionRecord {
    SessionRecord {
        id: id.to_string(),
        agent: String::new(),
        model: String::new(),
        intent: String::new(),
        attestation_payload: String::new(),
        payload,
    }
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
