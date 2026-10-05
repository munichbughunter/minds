//! Wiederverwendbarer Checkpoint: Evidence lesen, redigieren, speichern und
//! versiegeln. Trailer und Commit-Index bleiben Aufgabe des Aufrufers.

use std::collections::BTreeSet;
use std::path::Path;

use minds_capture::epoch::EpochState;
use minds_capture::{Checkpoint, Journal, adapter, chain};
use minds_core::SessionId;
use minds_core::evidence::{ChainResult, Seal, SealOutcome, SealSummary};
use minds_git::Repo;
use minds_redact::RedactionPipeline;
use minds_store::ContextStore;

use crate::hooklog::{self, Source};

pub type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Journal und zugehöriger Epochen-Zustand samt Beobachtungsgrenze.
pub struct EvidenceSource<'a> {
    pub journal: &'a Journal,
    pub epochs: &'a EpochState,
    pub scope: &'static str,
}

/// Auswahl der best-effort-Signatur; fehlgeschlagenes Signieren verhindert
/// weder Seal-Ablage noch Discard. Wiederverwendete Seals behalten ihre Signatur.
#[allow(dead_code)] // Key und None sind für weitere Core-Aufrufer vorgesehen.
pub enum SealSigner<'a> {
    UserConfig,
    Key {
        path: &'a Path,
        namespace: &'static str,
    },
    None,
}

/// Abhängigkeiten des Checkpoints, unabhängig vom Ort des Journals.
pub struct CheckpointEnv<'a> {
    #[allow(dead_code)] // Bestandteil der gemeinsamen Umgebung für weitere Aufrufer.
    pub repo: &'a Repo,
    pub root: &'a Path,
    /// Basis für hook.log, beim CLI-Kommando das Git-Verzeichnis.
    pub log_dir: &'a Path,
    pub store: &'a dyn ContextStore,
    pub pipeline: &'a RedactionPipeline,
    pub tracked: Option<&'a BTreeSet<String>>,
}

/// Erfolgreich gespeicherte und versiegelte Sessions sowie alle Seals,
/// einschließlich Policy-Block-Seals und idempotent wiederverwendeter Seals.
pub struct CheckpointOutcome {
    pub stored: Vec<SessionId>,
    pub sealed: Vec<SealSummary>,
}

/// Witness-Grenzen: Auswahl, Integritätsprüfung und dauerhafte Quittierung
/// müssen vor dem Discard durchlaufen werden. Der lokale Checkpoint nutzt
/// weiterhin den unveränderten Standardpfad.
pub trait CheckpointGuard {
    fn includes(&self, key: &minds_capture::SessionKey) -> bool;
    /// Prüft genau das `ReadOutcome`, das danach gespeichert und versiegelt wird.
    fn validate(
        &self,
        key: &minds_capture::SessionKey,
        read: &minds_capture::ReadOutcome,
        result: &ChainResult,
    ) -> Fallible<()>;
    fn sealed(&self, key: &minds_capture::SessionKey, sealed: &SealSummary) -> Fallible<()>;
    fn path_map(&self) -> &[(std::path::PathBuf, std::path::PathBuf)];
    fn report(&self, message: &str);
}

/// Verarbeitet die Quelle mit derselben Fehlerisolation wie `minds checkpoint`.
/// Nur nach erfolgreicher Speicherung und Versiegelung wird verworfen;
/// abgewiesene oder vertagte Journale bleiben zur Diagnose liegen.
pub fn run_checkpoint(
    env: &CheckpointEnv<'_>,
    src: &EvidenceSource<'_>,
    signer: &SealSigner<'_>,
) -> Fallible<CheckpointOutcome> {
    run_checkpoint_guarded(env, src, signer, None)
}

pub fn run_checkpoint_guarded(
    env: &CheckpointEnv<'_>,
    src: &EvidenceSource<'_>,
    signer: &SealSigner<'_>,
    guard: Option<&dyn CheckpointGuard>,
) -> Fallible<CheckpointOutcome> {
    let journal = src.journal;
    let epochs = src.epochs;
    let store = env.store;
    let pipeline = env.pipeline;
    let log_dir = env.log_dir;
    let mut outcome = CheckpointOutcome {
        stored: Vec::new(),
        sealed: Vec::new(),
    };
    let sessions = journal.sessions()?;
    // Verzeichnisse ohne auflösbaren Schlüssel bleiben liegen (kein Discard —
    // dort können vollständige Events liegen) und werden gemeldet, nicht
    // verschwiegen. Der Pfad trägt nur Agentname und Hash, nie ein rohes
    // local_id (#95) — er darf ins Log.
    for dir in &sessions.unresolved {
        report(
            log_dir,
            guard,
            &format!(
                "journal directory without a readable key file skipped: {}",
                dir.display()
            ),
        );
    }
    for key in sessions.keys {
        if guard.is_some_and(|guard| !guard.includes(&key)) {
            continue;
        }
        // Beim Witness (EA-06d) wurden vorige Sessions in diesem Lauf womöglich
        // schon versiegelt und verworfen. Ein Lesefehler dieser einen Session
        // darf den Lauf dann nicht abbrechen — sonst bekäme die Agent-Seite
        // ein „bleiben offen" für Sessions, die längst versiegelt sind, und
        // deren Trailer fehlten. Der lokale Pfad bleibt, wie er war.
        let read = match journal.read(&key) {
            Ok(read) => read,
            Err(err) if guard.is_some() => {
                report(
                    log_dir,
                    guard,
                    &format!("{} skipped: {err}", key.display_redacted(pipeline)),
                );
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        if read.events.is_empty() {
            // Nur Beschädigtes ohne ein einziges Event: kein Zeitstempel, kein
            // Bereich — nichts, was ein Seal ehrlich claimen könnte. fsck
            // meldet den Schaden; das Verzeichnis bleibt liegen.
            continue;
        }

        // Die Kette über das VOLLE ReadOutcome: Lücken und Beschädigtes werden
        // Glieder, nicht Schweigen (bis ADR-0011 wurden sie hier verworfen).
        // Gefaltet wird mit dem Session-Salt: Der Root reist im Seal auf die
        // Forge und wäre ungesalzen ein Payload-Orakel für Ein-Event-Epochen.
        let salt = match epochs.salt(&key) {
            Ok(salt) => salt,
            Err(err) => {
                // Ohne Salt kein Seal, ohne Seal kein Discard — vertagen,
                // wie bei jedem anderen Fehler dieser Session.
                report(
                    log_dir,
                    guard,
                    &format!("{} skipped: {err}", key.display_redacted(pipeline)),
                );
                continue;
            }
        };
        let result = chain::chain_salted(&salt, &read);
        if let Some(guard) = guard {
            if guard.validate(&key, &read, &result).is_err() {
                guard.report(
                    "integrity error: live chain differs from journal; checkpoint deferred",
                );
                continue;
            }
        }

        match store_one(env, &key, &read.events, guard) {
            Ok(id) => {
                let sealed = seal_epoch(
                    env,
                    src,
                    signer,
                    SealInput {
                        key: &key,
                        events: &read.events,
                        result: &result,
                        outcome: SealOutcome::Stored {
                            session: id.to_string(),
                        },
                    },
                    guard,
                );
                let Some(sealed) = sealed else {
                    // Ohne Seal kein Discard: Der Seal muss die
                    // Journal-Löschung überleben. Session ist gespeichert
                    // (idempotent), der nächste Lauf versiegelt nach.
                    continue;
                };
                // Rückverweis Session → Seal, best-effort: aus `list_seals`
                // jederzeit rekonstruierbar, darf den Checkpoint nicht kippen.
                if let Err(err) = store.record_session_seal(id, &sealed.seal_id) {
                    report(
                        log_dir,
                        guard,
                        &format!("seal back-reference for {id} not recorded: {err}"),
                    );
                }
                // Erst nach erfolgreicher Ablage UND Versiegelung verwerfen:
                // Ein Absturz dazwischen darf weder Rohdaten noch Beweis
                // verlieren.
                // Scheitert beim Witness nur das Verwerfen, ist die Session
                // trotzdem gespeichert und versiegelt: Sie zählt, ihr Trailer
                // kommt dran, und der nächste Lauf verwendet den Seal
                // idempotent wieder.
                match journal.discard(&key) {
                    Ok(()) => {}
                    Err(err) if guard.is_some() => report(
                        log_dir,
                        guard,
                        &format!("journal not discarded after sealing: {err}"),
                    ),
                    Err(err) => return Err(err.into()),
                }
                if guard.is_none() {
                    println!("  {}: {id}", key.display_redacted(pipeline));
                    print_session_sealed(&sealed);
                }
                outcome.stored.push(id);
                outcome.sealed.push(sealed);
            }
            Err(err) => {
                // Journal bleibt liegen — die Session ist nicht verloren, nur
                // vertagt. fsck macht sie sichtbar. Das local_id läuft durch
                // die Redaktion, bevor es auf stderr und ins hook.log geht:
                // Seit #35 gilt es als fremdbestimmter Wert, der auch ein
                // Token sein kann (#95).
                let mut note = format!("{} skipped: {err}", key.display_redacted(pipeline));

                // Nur der Policy-Fall bekommt einen Block-Seal: Eine
                // zurückgewiesene Nutzlast ist eine Aussage über die Session;
                // ein Store-Schluckauf wäre eine über die Infrastruktur — der
                // versiegelte sonst irreführend „rejected".
                if err.downcast_ref::<minds_redact::RedactionError>().is_some() {
                    if let Some(sealed) = seal_epoch(
                        env,
                        src,
                        signer,
                        SealInput {
                            key: &key,
                            events: &read.events,
                            result: &result,
                            outcome: SealOutcome::Rejected,
                        },
                        guard,
                    ) {
                        note.push_str(&format!(" — coverage sealed: {}", sealed.seal_id));
                        outcome.sealed.push(sealed);
                    }
                }
                report(log_dir, guard, &note);
            }
        }
    }

    Ok(outcome)
}

/// Versiegelt die laufenden Epochen der Witness-eigenen Streams (EA-08):
/// Scope `witness-fs/v1`, Outcome `observations_stored`, die `session=`-Zeile
/// trägt die Id des Observation-Objekts.
///
/// Dieselbe Reihenfolge wie bei Sessions: Kette über das volle Journal,
/// Prüfung gegen den Live-Fold (`guard.validate`), Objekt redigieren und
/// ablegen, versiegeln (Pflichtsignatur), erst dann verwerfen. Lehnt die
/// Redaction das Objekt ab, entsteht ein Block-Seal desselben Scopes. Jeder
/// Fehler vertagt nur diesen Stream.
#[cfg(unix)]
pub fn seal_observation_streams(
    env: &CheckpointEnv<'_>,
    src: &EvidenceSource<'_>,
    signer: &SealSigner<'_>,
    guard: &dyn CheckpointGuard,
    streams: &[minds_capture::SessionKey],
) -> Vec<SealSummary> {
    let mut sealed = Vec::new();
    for key in streams {
        let read = match src.journal.read(key) {
            Ok(read) => read,
            Err(err) => {
                guard.report(&format!("witness stream skipped: {err}"));
                continue;
            }
        };
        if read.events.is_empty() {
            continue;
        }
        let salt = match src.epochs.salt(key) {
            Ok(salt) => salt,
            Err(err) => {
                guard.report(&format!("witness stream skipped: {err}"));
                continue;
            }
        };
        let result = chain::chain_salted(&salt, &read);
        if guard.validate(key, &read, &result).is_err() {
            guard.report("integrity error: live witness stream differs from journal; deferred");
            continue;
        }
        let object = match observations_of(&read.events) {
            Ok(object) => object,
            Err(err) => {
                guard.report(&format!("integrity error: {err}; witness stream deferred"));
                continue;
            }
        };
        let outcome = match redact_each(env.pipeline, object, guard) {
            Ok(redacted) => match env.store.put_observations(&redacted) {
                Ok(id) => SealOutcome::ObservationsStored {
                    observations: id.to_string(),
                },
                // Die Epoche wird trotzdem geschlossen (Block-Seal): Bliebe
                // sie offen, versiegelte der nächste Checkpoint sie zusammen
                // mit der Arbeit nach diesem Commit — und ein Fenster, das
                // hier endet, reichte bis dorthin. Es fehlen dann
                // Beobachtungen (sichere Richtung); die Grenze stimmt.
                Err(err) => {
                    guard.report(&format!(
                        "observations not stored: {err}; epoch closed, observations discarded"
                    ));
                    SealOutcome::Rejected
                }
            },
            Err(err) => {
                guard.report(&format!("observations rejected by redaction: {err}"));
                SealOutcome::Rejected
            }
        };
        let Some(summary) = seal_epoch(
            env,
            src,
            signer,
            SealInput {
                key,
                events: &read.events,
                result: &result,
                outcome,
            },
            Some(guard),
        ) else {
            continue;
        };
        if let Err(err) = src.journal.discard(key) {
            guard.report(&format!(
                "witness stream not discarded after sealing: {err}"
            ));
        }
        sealed.push(summary);
    }
    sealed
}

/// Das Observation-Objekt aus den `fs.observed`-Events einer Epoche. Die
/// übrigen Events des Streams (Lebenszyklus, Lücken des Beobachters) bindet
/// allein die Kette. Ein unlesbares eigenes Event ist ein Integritätsfehler.
#[cfg(unix)]
pub fn observations_of(
    events: &[minds_capture::JournalEvent],
) -> Fallible<minds_core::observation::Observations> {
    use minds_core::observation::{Observation, ObservationReason, Observations};

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Payload {
        path: String,
        content: Option<minds_core::ContentHash>,
        reason: Option<ObservationReason>,
    }
    let mut observations = Vec::new();
    for event in events.iter().filter(|e| e.raw_kind == FS_OBSERVED) {
        let payload: Payload = serde_json::from_str(event.payload.get())
            .map_err(|_| "unreadable fs.observed event")?;
        observations.push(Observation {
            seq: event.seq,
            at: event.at.clone(),
            path: payload.path,
            content: payload.content,
            reason: payload.reason,
        });
    }
    Ok(Observations::new(observations))
}

/// Redigiert das Objekt; lehnt die Pipeline es ab, wird jede Beobachtung
/// einzeln geprüft und nur die abgelehnte verworfen. Ein absichtlich
/// unredigierbarer Dateiname soll nicht die ganze Epoche des Beobachters
/// auslöschen — die verworfene Beobachtung fehlt (sicherer Fall: weniger
/// Beleg, nie mehr), ihre Zahl steht im Log. Scheitert auch das Gerüst
/// (etwa ohne Detektoren), bleibt es beim Fehler.
#[cfg(unix)]
fn redact_each(
    pipeline: &minds_redact::RedactionPipeline,
    object: minds_core::observation::Observations,
    guard: &dyn CheckpointGuard,
) -> Result<minds_redact::RedactedObservations, minds_redact::RedactionError> {
    use minds_core::observation::Observations;
    match pipeline.redact_observations(object.clone()) {
        Ok(redacted) => Ok(redacted),
        Err(err) => {
            let total = object.observations.len();
            let kept: Vec<_> = object
                .observations
                .into_iter()
                .filter(|o| {
                    pipeline
                        .redact_observations(Observations::new(vec![o.clone()]))
                        .is_ok()
                })
                .collect();
            if kept.len() == total {
                return Err(err);
            }
            guard.report(&format!(
                "{} observation(s) dropped: redaction refused them",
                total - kept.len()
            ));
            pipeline.redact_observations(Observations::new(kept))
        }
    }
}

/// `raw_kind` einer Datei-Beobachtung im Witness-Stream.
#[cfg(unix)]
pub const FS_OBSERVED: &str = "fs.observed";

/// Baut den Seal dieser Epoche, legt ihn ab und schreibt den Epochen-Zustand
/// fort. Gibt die [`SealSummary`] zurück — oder `None`, wenn die Ablage
/// scheiterte (dann bleibt das Journal liegen und der nächste Lauf holt sie
/// idempotent nach).
struct SealInput<'a> {
    key: &'a minds_capture::SessionKey,
    events: &'a [minds_capture::JournalEvent],
    result: &'a ChainResult,
    outcome: SealOutcome,
}

fn seal_epoch(
    env: &CheckpointEnv<'_>,
    src: &EvidenceSource<'_>,
    signer: &SealSigner<'_>,
    input: SealInput<'_>,
    guard: Option<&dyn CheckpointGuard>,
) -> Option<SealSummary> {
    let SealInput {
        key,
        events,
        result,
        outcome,
    } = input;
    let store = env.store;
    let log_dir = env.log_dir;
    let epochs = src.epochs;
    let previous = epochs.last_seal(key);

    // Idempotenz über Läufe hinweg: Deckt der letzte Seal bereits genau diese
    // Kette mit demselben Ausgang ab (ein Lauf, dessen Discard scheiterte;
    // ein liegengebliebenes, unverändertes Journal nach einem Redaction-
    // Block), wird er wiederverwendet — sonst verkettete jeder erneute
    // Checkpoint einen inhaltsgleichen Seal auf seinen Vorgänger, und jeder
    // Commit ließe die Kette grundlos wachsen.
    if let Some(prev_id) = &previous {
        if let Ok(Some(prev_text)) = store.seal_text(prev_id) {
            if let Ok(prev) = Seal::parse(&prev_text) {
                let same_outcome = match (&prev.outcome, &outcome) {
                    (SealOutcome::Rejected, SealOutcome::Rejected) => true,
                    (SealOutcome::Stored { session: a }, SealOutcome::Stored { session: b }) => {
                        a == b
                    }
                    (
                        SealOutcome::ObservationsStored { observations: a },
                        SealOutcome::ObservationsStored { observations: b },
                    ) => a == b,
                    _ => false,
                };
                if prev.root == result.root && prev.scope == src.scope && same_outcome {
                    // Signatur-Anwesenheit frisch nachsehen, nicht raten: Der
                    // wiederverwendete Seal kann inzwischen signiert sein.
                    let signed = store.seal_signature(prev_id).ok().flatten().is_some();
                    let sealed = SealSummary::new(prev_id.clone(), prev, signed);
                    if let Some(guard) = guard {
                        if !signed || guard.sealed(key, &sealed).is_err() {
                            return None;
                        }
                    }
                    return Some(sealed);
                }
            }
        }
    }

    let last_event_at = events.last().map(|e| e.at.clone()).unwrap_or_default();
    let seal = Seal {
        root: result.root.clone(),
        agent: key.agent().to_string(),
        // Die Beobachtungsgrenze steht IM Seal: „vollständig" gilt nur
        // innerhalb des vom Aufrufer angegebenen Scopes.
        scope: src.scope.to_string(),
        first_seq: result.coverage.first_seq,
        last_seq: result.coverage.last_seq,
        events: result.coverage.events,
        gaps: result.coverage.gaps.len() as u64,
        pre_chain: result.coverage.pre_chain,
        outcome,
        previous,
        last_event_at,
    };
    let text = match seal.to_text() {
        Ok(text) => text,
        Err(err) => {
            // #12-Fall am Agentnamen — benennt das Feld, zitiert nie den Wert.
            report(log_dir, guard, &format!("seal could not be built: {err}"));
            return None;
        }
    };
    // Beim Witness ist die Signatur Pflicht. Vor der ersten Ablage signieren:
    // ein fehlender Schlüssel darf keinen scheinbar erfolgreichen Seal erzeugen.
    let required_signature = if guard.is_some() {
        let SealSigner::Key { path, namespace } = signer else {
            return None;
        };
        match minds_attest::ssh_sign_ns(&text, path, namespace) {
            Ok(signature) => Some(signature),
            Err(_) => {
                guard?.report("witness signing failed; checkpoint deferred");
                return None;
            }
        }
    } else {
        None
    };
    let seal_id = match store.put_seal(&text) {
        Ok(id) => id,
        Err(err) => {
            report(log_dir, guard, &format!("seal not stored: {err}"));
            return None;
        }
    };

    // Best-effort-Signatur (ADR-0011, Entscheidung 5): nur mit konfiguriertem
    // Schlüssel, nie ein Grund zum Abbruch — ein unsignierter Seal bleibt
    // hash-valide, `minds sign --seal` rüstet nach.
    let signed = if let Some(signature) = required_signature {
        if store.put_seal_signature(&seal_id, &signature).is_err() {
            return None;
        }
        true
    } else {
        sign_seal_best_effort(env, signer, &seal_id, &text)
    };
    // Lokal best-effort: Fehlt der Zustand künftig, ist die Kette offen —
    // sichtbar und ehrlich, kein Grund, den Checkpoint zu kippen. Beim
    // Witness nicht: Blieb der Zustand auf der vorigen Epoche stehen, nennte
    // der nächste Seal **sie** als Vorgänger — die Kette gabelte sich an
    // dieser Epoche vorbei, und der Reader (EA-08) ließe ihre Beobachtungen
    // stillschweigend weg. Dann vertagen: nicht verworfen, der nächste Lauf
    // versiegelt dieselbe Epoche idempotent (gleicher Inhalt, gleiche Id)
    // und schreibt den Zustand erneut.
    if let Err(err) = epochs.record(key, &seal_id) {
        report(log_dir, guard, &format!("epoch state not advanced: {err}"));
        if guard.is_some() {
            return None;
        }
    }
    let sealed = SealSummary::new(seal_id, seal, signed);
    if let Some(guard) = guard {
        if guard.sealed(key, &sealed).is_err() {
            return None;
        }
    }
    Some(sealed)
}

/// Druckt die „SESSION SEALED"-Zusammenfassung eines frisch (oder idempotent
/// wieder-)versiegelten Bereichs.
///
/// Nur für Menschen: Der post-commit-Hook leitet stdout nach `/dev/null`,
/// sichtbar ist der Block allein beim manuellen `minds checkpoint`. Jede
/// ✓-Zeile ist eine **Tatsache über das Schreiben**, nie ein Prüf-Ergebnis —
/// „recorded", nicht „valid": Verifikation ist `minds verify`s Satz, nicht
/// unserer (ADR-0011, „Heuristik bleibt Heuristik" gilt auch für Zusagen).
fn print_session_sealed(sealed: &SealSummary) {
    let seal = &sealed.seal;
    println!("  SESSION SEALED");
    println!("    Seal       {}", sealed.seal_id);
    println!("    Root       {}", seal.root);
    println!(
        "    Events     {} event(s) · seq {}–{}",
        seal.events, seal.first_seq, seal.last_seq
    );
    // Zweite Schicht neben der Parse-Härtung (wie in verify): Auf dem
    // Wiederverwendungs-Pfad stammt der Scope aus dem Repo.
    println!("    Scope      {}", crate::text::sanitize(&seal.scope));
    println!(
        "    Signature  {}",
        if sealed.signed {
            "signed (validity is checked by `minds verify`)"
        } else {
            "unsigned — `minds sign --seal` adds it"
        }
    );
    if seal.gaps == 0 && seal.pre_chain == 0 {
        println!("    ✓ no gaps in the observed range");
    } else {
        if seal.gaps > 0 {
            println!("    ⚠ {} gap(s) in the observed range", seal.gaps);
        }
        if seal.pre_chain > 0 {
            println!(
                "    ⚠ {} event(s) captured before the evidence chain (unbound)",
                seal.pre_chain
            );
        }
    }
    match &seal.previous {
        Some(prev) => println!("    ✓ chained to the previous epoch ({prev})"),
        None => println!("    ✓ first epoch of this session (chain start)"),
    }
    match &seal.outcome {
        SealOutcome::Stored { .. } => {
            println!("    ✓ seal recorded — check it with `minds verify <session-id>`");
        }
        SealOutcome::ObservationsStored { .. } => {
            println!(
                "    ✓ observation seal recorded — check it with `minds verify --evidence <seal>`"
            );
        }
        SealOutcome::Rejected => {
            println!("    ✓ block seal recorded — the payload was rejected, the range is attested");
        }
    }
}

/// Die „SESSION SEALED"-Zusammenfassung als **eine** Zeile — für den Witness,
/// der sie über den Socket zurückgibt (EA-06d). Das Protokoll trägt nur eine
/// Statuszeile ohne Steuerzeichen; der mehrzeilige Block aus
/// [`print_session_sealed`] passt dort nicht hinein.
///
/// Dieselben Tatsachen wie der Block, dasselbe Vokabular („recorded", nie
/// „valid"). Kein Pfad, kein Intent, kein Payload: nur Hashes, Zahlen und der
/// Scope aus dem Seal selbst.
#[cfg(unix)]
pub fn session_sealed_line(sealed: &SealSummary) -> String {
    let seal = &sealed.seal;
    let head = match &seal.outcome {
        SealOutcome::Stored { session } => format!("SESSION SEALED {session}"),
        SealOutcome::ObservationsStored { observations } => {
            format!("OBSERVATIONS SEALED {observations}")
        }
        SealOutcome::Rejected => "RANGE SEALED (payload rejected)".to_owned(),
    };
    let gaps = if seal.gaps == 0 && seal.pre_chain == 0 {
        "no gaps".to_owned()
    } else {
        format!("{} gap(s), {} pre-chain", seal.gaps, seal.pre_chain)
    };
    format!(
        "{head} · seal {} · scope {} · {} event(s) · seq {}–{} · {gaps} · {}",
        sealed.seal_id,
        crate::text::sanitize(&seal.scope),
        seal.events,
        seal.first_seq,
        seal.last_seq,
        if sealed.signed { "signed" } else { "unsigned" },
    )
}

/// Signiert einen frisch abgelegten Seal mit dem gewählten Signer.
/// Best-effort: Jeder Fehlschlag ist eine Log-Zeile, nie ein Abbruch —
/// der Seal bleibt hash-valide, die Signatur ist die Urheber-Bindung obendrauf.
/// Gibt zurück, ob am Ende eine Signatur **liegt** (Anwesenheit, keine
/// Prüfung) — die Summary spricht sie sonst falsch aus.
fn sign_seal_best_effort(
    env: &CheckpointEnv<'_>,
    signer: &SealSigner<'_>,
    seal_id: &minds_core::ContentHash,
    text: &str,
) -> bool {
    let guard = None;
    let configured;
    let (key, namespace) = match signer {
        SealSigner::UserConfig => {
            configured = match crate::sign_cmd::configured_key(env.root) {
                Some(key) => key,
                None => return false,
            };
            (Path::new(&configured), minds_attest::NAMESPACE)
        }
        SealSigner::Key { path, namespace } => (*path, *namespace),
        SealSigner::None => return false,
    };
    let store = env.store;
    let log_dir = env.log_dir;
    if !minds_attest::ssh_keygen_available() {
        return false;
    }
    let outcome = minds_attest::ssh_sign_ns(text, key, namespace)
        .map_err(|err| err.to_string())
        .and_then(|sig| {
            store
                .put_seal_signature(seal_id, &sig)
                .map_err(|err| err.to_string())
        });
    match outcome {
        Ok(()) => true,
        Err(err) => {
            report(log_dir, guard, &format!("seal {seal_id} not signed: {err}"));
            false
        }
    }
}

/// Baut eine Session, redigiert sie und legt sie ab. Gibt ihre [`SessionId`]
/// zurück.
fn store_one(
    env: &CheckpointEnv<'_>,
    key: &minds_capture::SessionKey,
    events: &[minds_capture::JournalEvent],
    guard: Option<&dyn CheckpointGuard>,
) -> Fallible<SessionId> {
    let root = env.root;
    let log_dir = env.log_dir;
    let pipeline = env.pipeline;
    let store = env.store;
    let tracked = env.tracked;
    // Kein Commit im Kontext: die Produced-Kante bliebe sonst am verwaisten
    // Vor-Amend-Commit hängen (siehe Modul-Doku). Der Artefakt-Hash braucht die
    // Repo-Wurzel, um relative Pfade aufzulösen; das tracked-Set ist die
    // Read-Hash-Grenze (nur getrackter, ohnehin sichtbarer Inhalt).
    let ctx = Checkpoint {
        root: Some(root),
        commit: None,
        tracked,
        // Dieselbe Policy, die gleich die Session redigiert, prüft vorher die
        // Bytes hinter jedem Schreib-Hash (EA-01a).
        redaction: Some(pipeline),
    };
    let session = match guard {
        Some(guard) => adapter::checkpoint_witness(key, events, &ctx, guard.path_map()),
        None => adapter::checkpoint(key, events, &ctx),
    };
    let redacted = pipeline.redact_session(session)?;
    let put = store.put(&redacted)?;

    // Wurde die Session vergessen, bleibt sie vergessen (#6): kein Store-Record,
    // und vor allem kein Branch — sonst stünde der Klartext beim nächsten
    // Capture-Lauf wieder als `session.md` browsbar auf der Forge. Der
    // Branch-Schreibweg schützt sich zwar selbst (gestaffelter Guard in
    // `put_session_branch_bytes`), doch wir gehen ihn hier im schon entschiedenen
    // Fall gar nicht erst an.
    if put.was_forgotten() {
        return Ok(put.id());
    }

    // Die Session als eigenen Branch in der Forge sichtbar machen (nur beim
    // Child-Backend; sonst ein No-op). Best-effort: Der maßgebliche Record liegt
    // bereits im Store, und der Browsing-Branch lässt sich daraus jederzeit neu
    // bauen — ein Fehlschlag hier darf den Checkpoint nicht abbrechen und die
    // Session nicht ins Journal zurückwerfen.
    if let Err(err) = store.put_session_branch(&redacted) {
        report(
            log_dir,
            guard,
            &format!("branch for {} not created: {err}", put.id()),
        );
    }

    Ok(put.id())
}

fn report(log_dir: &Path, guard: Option<&dyn CheckpointGuard>, message: &str) {
    match guard {
        Some(guard) => guard.report(message),
        None => hooklog::report_at(log_dir, Source::Checkpoint, message),
    }
}
