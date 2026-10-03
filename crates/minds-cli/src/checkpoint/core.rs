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

/// Verarbeitet die Quelle mit derselben Fehlerisolation wie `minds checkpoint`.
/// Nur nach erfolgreicher Speicherung und Versiegelung wird verworfen;
/// abgewiesene oder vertagte Journale bleiben zur Diagnose liegen.
pub fn run_checkpoint(
    env: &CheckpointEnv<'_>,
    src: &EvidenceSource<'_>,
    signer: &SealSigner<'_>,
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
        hooklog::report_at(
            log_dir,
            Source::Checkpoint,
            &format!(
                "journal directory without a readable key file skipped: {}",
                dir.display()
            ),
        );
    }
    for key in sessions.keys {
        let read = journal.read(&key)?;
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
                hooklog::report_at(
                    log_dir,
                    Source::Checkpoint,
                    &format!("{} skipped: {err}", key.display_redacted(pipeline)),
                );
                continue;
            }
        };
        let result = chain::chain_salted(&salt, &read);

        match store_one(env, &key, &read.events) {
            Ok(id) => {
                let sealed = seal_epoch(
                    env,
                    src,
                    signer,
                    &key,
                    &read.events,
                    &result,
                    SealOutcome::Stored {
                        session: id.to_string(),
                    },
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
                    hooklog::report_at(
                        log_dir,
                        Source::Checkpoint,
                        &format!("seal back-reference for {id} not recorded: {err}"),
                    );
                }
                // Erst nach erfolgreicher Ablage UND Versiegelung verwerfen:
                // Ein Absturz dazwischen darf weder Rohdaten noch Beweis
                // verlieren.
                journal.discard(&key)?;
                println!("  {}: {id}", key.display_redacted(pipeline));
                print_session_sealed(&sealed);
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
                        &key,
                        &read.events,
                        &result,
                        SealOutcome::Rejected,
                    ) {
                        note.push_str(&format!(" — coverage sealed: {}", sealed.seal_id));
                        outcome.sealed.push(sealed);
                    }
                }
                hooklog::report_at(log_dir, Source::Checkpoint, &note);
            }
        }
    }

    Ok(outcome)
}

/// Baut den Seal dieser Epoche, legt ihn ab und schreibt den Epochen-Zustand
/// fort. Gibt die [`SealSummary`] zurück — oder `None`, wenn die Ablage
/// scheiterte (dann bleibt das Journal liegen und der nächste Lauf holt sie
/// idempotent nach).
fn seal_epoch(
    env: &CheckpointEnv<'_>,
    src: &EvidenceSource<'_>,
    signer: &SealSigner<'_>,
    key: &minds_capture::SessionKey,
    events: &[minds_capture::JournalEvent],
    result: &ChainResult,
    outcome: SealOutcome,
) -> Option<SealSummary> {
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
                    _ => false,
                };
                if prev.root == result.root && prev.scope == src.scope && same_outcome {
                    // Signatur-Anwesenheit frisch nachsehen, nicht raten: Der
                    // wiederverwendete Seal kann inzwischen signiert sein.
                    let signed = store.seal_signature(prev_id).ok().flatten().is_some();
                    return Some(SealSummary::new(prev_id.clone(), prev, signed));
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
            hooklog::report_at(
                log_dir,
                Source::Checkpoint,
                &format!("seal could not be built: {err}"),
            );
            return None;
        }
    };
    let seal_id = match store.put_seal(&text) {
        Ok(id) => id,
        Err(err) => {
            hooklog::report_at(
                log_dir,
                Source::Checkpoint,
                &format!("seal not stored: {err}"),
            );
            return None;
        }
    };

    // Best-effort-Signatur (ADR-0011, Entscheidung 5): nur mit konfiguriertem
    // Schlüssel, nie ein Grund zum Abbruch — ein unsignierter Seal bleibt
    // hash-valide, `minds sign --seal` rüstet nach.
    let signed = sign_seal_best_effort(env, signer, &seal_id, &text);
    // Epochen-Zustand best-effort: Fehlt er künftig, ist die Kette offen —
    // sichtbar und ehrlich, kein Grund, den Checkpoint zu kippen.
    if let Err(err) = epochs.record(key, &seal_id) {
        hooklog::report_at(
            log_dir,
            Source::Checkpoint,
            &format!("epoch state not advanced: {err}"),
        );
    }
    Some(SealSummary::new(seal_id, seal, signed))
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
        SealOutcome::Rejected => {
            println!("    ✓ block seal recorded — the payload was rejected, the range is attested");
        }
    }
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
            hooklog::report_at(
                log_dir,
                Source::Checkpoint,
                &format!("seal {seal_id} not signed: {err}"),
            );
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
    let session = adapter::checkpoint(key, events, &ctx);
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
        hooklog::report_at(
            log_dir,
            Source::Checkpoint,
            &format!("branch for {} not created: {err}", put.id()),
        );
    }

    Ok(put.id())
}
