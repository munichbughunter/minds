//! `minds checkpoint` — der kalte Pfad, den der post-commit-Hook aufruft.
//!
//! Hier schließt sich der Kern-Loop: Das Journal, das `minds hook` heiß und roh
//! gefüllt hat, wird gedeutet, redigiert, gespeichert und über einen
//! Commit-Trailer mit dem Code verbunden.
//!
//! ```text
//!   Journal ──► adapter::checkpoint ──► Redaction ──► Store ──► Trailer an HEAD
//!   (roh)        (Session)              (fail-closed)  (b3-…)    (Minds-Session-Id)
//!      │
//!      └──► chain::chain ──► Seal ──► refs/minds/evidence/<seal_id>   (ADR-0011)
//!           (Root+Coverage)  (signierbar)
//! ```
//!
//! # Der Seal kommt vor dem Discard
//!
//! Das volle [`ReadOutcome`](minds_capture::Journal::read) — Events, Lücken,
//! Beschädigtes — wird zur Kette gefaltet und als Seal abgelegt, **bevor** das
//! Journal verschwindet: Der Seal ist das, was die Journal-Löschung überlebt.
//! Scheitert die Seal-Ablage, bleibt das Journal liegen (vertagt, wie bei
//! jedem anderen Fehler) — die Ablage ist idempotent, der nächste Lauf holt
//! sie nach. Epochen (dieselbe Session über mehrere Checkpoints; die Seqs
//! starten nach dem Discard wieder bei 0) verkettet die `previous`-Zeile über
//! den lokalen [`EpochState`].
//!
//! Weist die Redaction eine Session zurück, entsteht **trotzdem** ein Seal —
//! `outcome=storage_policy_rejected_payload`, `session=-`: Für den Auditor
//! existierte die Session, ihr Bereich ist versiegelt, nur die Nutzlast wurde
//! zurückgewiesen. Der Seal trägt keinen Intent, keine Pfade, keinen
//! Redaction-Feldnamen (ADR-0011, Entscheidung 3).
//!
//! # Der Trailer, nicht die Produced-Kante
//!
//! Der Verweis Commit → Session steht als **Trailer in der Commit-Message** —
//! nicht am Hash. Nur so übersteht er Rebase, Squash und Cherry-Pick
//! (Architektur-Prinzip 1). Ihn nachzurüsten heißt aber, die Message zu ändern,
//! und die Message ist Teil des Commit-Objekts: Der Commit wird umgeschrieben
//! (siehe [`Repo::amend_head_with_sessions`]).
//!
//! Genau deshalb trägt die hier gebaute Session **keine Produced-Kante**. Sie
//! zeigte auf den Commit *vor* dem Nachrüsten — den, den das Amend verwaist und
//! den `git gc` irgendwann einsammelt. Eine Kante, die ins Leere zeigen kann,
//! wäre in einem Record, dessen Wert seine Nachweisbarkeit ist, das Gegenteil
//! von hilfreich. Der Trailer ist die belastbare Richtung (Commit → Session);
//! die Kante Session → Commit bleibt der Adapter-Fähigkeit für Abläufe
//! vorbehalten, in denen der Commit feststeht (Store-Index, späteres `minds
//! link`).
//!
//! # Robust gegen die eine schlechte Session
//!
//! Scheitert eine Session (Redaction bricht ab, der Store nimmt sie nicht), wird
//! *nur sie* übersprungen und ihr Journal **nicht** verworfen — sie bleibt für
//! den nächsten Lauf und für `minds fsck` sichtbar. Die übrigen Sessions laufen
//! trotzdem durch. Eine vergiftete Session darf den Checkpoint der anderen nicht
//! mitreißen.
//!
//! # `--commit` als Wächter
//!
//! Der post-commit-Hook reicht den gerade entstandenen Commit als `--commit`
//! herein. Steht HEAD noch dort, wird nachgerüstet; ist HEAD inzwischen
//! weitergewandert (ein zweiter Commit kam dazwischen), werden die Sessions zwar
//! gespeichert, aber **nicht** an den falschen Commit getrailert — dann fehlt
//! nur der Verweis, und `minds fsck` meldet die Waise, statt dass ein falscher
//! entsteht.

use std::path::Path;
use std::process::ExitCode;

use minds_capture::epoch::EpochState;
use minds_capture::{Checkpoint, Journal, adapter, chain};
use minds_core::SessionId;
use minds_core::evidence::{ChainResult, Seal, SealOutcome};
use minds_git::{CommitId, Repo};
use minds_store::ContextStore;

use crate::config;
use crate::hooklog::{self, Source};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds checkpoint` aus. `commit` ist der Wächter-Commit aus dem
/// post-commit-Hook (siehe Modul-Doku); ohne ihn wird HEAD nachgerüstet, sofern
/// vorhanden.
pub fn run(commit: Option<&str>) -> ExitCode {
    hooklog::guarded(Source::Checkpoint, || checkpoint_or_report(commit))
}

fn checkpoint_or_report(commit: Option<&str>) -> ExitCode {
    match checkpoint(commit) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Zweimal, weil es zwei Leser gibt: Von Hand aufgerufen liest ein
            // Mensch stderr; aus dem post-commit-Hook liest es niemand, weil
            // der Hook alles nach `/dev/null` schickt. Ohne die Datei bliebe
            // genau der Fall stumm, der am längsten unbemerkt bleibt — ein
            // fail-closed abbrechender Checkpoint checkt nie wieder etwas ein.
            hooklog::report(Source::Checkpoint, &err.to_string());
            ExitCode::FAILURE
        }
    }
}

fn checkpoint(commit: Option<&str>) -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);

    // Das Git-Verzeichnis steht hier fest — die Log-Aufrufe weiter unten müssen
    // es deshalb nicht ein zweites Mal suchen. Das ist nicht nur billiger: Eine
    // abweichende Suche (`GIT_DIR`, ungewöhnliches Layout) schriebe den Eintrag
    // sonst in ein anderes Repo als das gerade bearbeitete.
    let git_dir = repo.git_dir();
    let store = config::load(&root).open(&root)?;
    let journal = Journal::open(git_dir);
    // Redaction-Policy: der strenge Default, sofern das Repo unter
    // `.minds/redact.json` nichts anderes vorgibt (fail-closed bei Fehlern).
    let pipeline = config::load_redaction(&root)?.pipeline()?;

    let mut stored: Vec<SessionId> = Vec::new();
    let outcome = journal.sessions()?;
    // Verzeichnisse ohne auflösbaren Schlüssel bleiben liegen (kein Discard —
    // dort können vollständige Events liegen) und werden gemeldet, nicht
    // verschwiegen. Der Pfad trägt nur Agentname und Hash, nie ein rohes
    // local_id (#95) — er darf ins Log.
    for dir in &outcome.unresolved {
        hooklog::report_at(
            git_dir,
            Source::Checkpoint,
            &format!(
                "Journal-Verzeichnis ohne lesbare Schlüssel-Datei übersprungen: {}",
                dir.display()
            ),
        );
    }
    let epochs = EpochState::open(git_dir);
    // Die Read-Hash-Grenze, einmal je Lauf: die von git getrackten Pfade.
    // Schlägt das fehl, gibt es schlicht keine Read-Hashes (fail-closed in
    // Richtung „weniger Fingerabdruck") — Write-Hashes sind nicht betroffen.
    let tracked = tracked_files(&root);
    for key in outcome.keys {
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
                    git_dir,
                    Source::Checkpoint,
                    &format!("{} übersprungen: {err}", key.display_redacted(&pipeline)),
                );
                continue;
            }
        };
        let result = chain::chain_salted(&salt, &read);

        match store_one(
            &key,
            &read.events,
            &root,
            git_dir,
            &pipeline,
            store.as_ref(),
            tracked.as_ref(),
        ) {
            Ok(id) => {
                let sealed = seal_epoch(
                    &epochs,
                    &key,
                    &read.events,
                    &result,
                    SealOutcome::Stored {
                        session: id.to_string(),
                    },
                    store.as_ref(),
                    &root,
                    git_dir,
                );
                let Some(seal_id) = sealed else {
                    // Ohne Seal kein Discard: Der Seal muss die
                    // Journal-Löschung überleben. Session ist gespeichert
                    // (idempotent), der nächste Lauf versiegelt nach.
                    continue;
                };
                // Rückverweis Session → Seal, best-effort: aus `list_seals`
                // jederzeit rekonstruierbar, darf den Checkpoint nicht kippen.
                if let Err(err) = store.record_session_seal(id, &seal_id) {
                    hooklog::report_at(
                        git_dir,
                        Source::Checkpoint,
                        &format!("Seal-Verweis für {id} nicht eingetragen: {err}"),
                    );
                }
                // Erst nach erfolgreicher Ablage UND Versiegelung verwerfen:
                // Ein Absturz dazwischen darf weder Rohdaten noch Beweis
                // verlieren.
                journal.discard(&key)?;
                println!("  {}: {id}", key.display_redacted(&pipeline));
                stored.push(id);
            }
            Err(err) => {
                // Journal bleibt liegen — die Session ist nicht verloren, nur
                // vertagt. fsck macht sie sichtbar. Das local_id läuft durch
                // die Redaktion, bevor es auf stderr und ins hook.log geht:
                // Seit #35 gilt es als fremdbestimmter Wert, der auch ein
                // Token sein kann (#95).
                let mut note = format!("{} übersprungen: {err}", key.display_redacted(&pipeline));

                // Nur der Policy-Fall bekommt einen Block-Seal: Eine
                // zurückgewiesene Nutzlast ist eine Aussage über die Session;
                // ein Store-Schluckauf wäre eine über die Infrastruktur — der
                // versiegelte sonst irreführend „rejected".
                if err.downcast_ref::<minds_redact::RedactionError>().is_some() {
                    if let Some(seal_id) = seal_epoch(
                        &epochs,
                        &key,
                        &read.events,
                        &result,
                        SealOutcome::Rejected,
                        store.as_ref(),
                        &root,
                        git_dir,
                    ) {
                        note.push_str(&format!(" — Coverage versiegelt: {seal_id}"));
                    }
                }
                hooklog::report_at(git_dir, Source::Checkpoint, &note);
            }
        }
    }

    let attached = attach_trailers(&repo, commit, &stored)?;

    // Den Verweis Commit → Session zusätzlich in den Store-Index schreiben
    // (beobachtet). Der Trailer ist die verbindliche Quelle, aber er lebt in der
    // Historie des *Code*-Repos; beim Child-Repo-Backend liegen die Sessions in
    // einem eigenen Repo, und der Index reist mit ihnen. So ist der Kontext-Store
    // selbsttragend — wer nur ihn hat (etwa beim Browsen von `minds-child-project`
    // in GitLab), sieht über `index.json`, welche Session zu welchem Commit gehört.
    if let Some(commit) = attached {
        record_index(store.as_ref(), commit, &stored)?;
    }

    Ok(())
}

/// Schreibt für jeden gerade abgelegten Session-Verweis eine beobachtete Kante
/// `commit → session` in den Store-Index.
fn record_index(
    store: &dyn ContextStore,
    commit: CommitId,
    sessions: &[SessionId],
) -> Fallible<()> {
    if sessions.is_empty() {
        return Ok(());
    }
    // Je Session eine Kante, an *ihrem* Ref — nicht den ganzen Index lesen und
    // zurückschreiben. Das ist der Unterschied, der eine Agent-Flotte trägt:
    // Zwei gleichzeitige Checkpoints fassen verschiedene Refs an.
    let hex = commit.to_string();
    for id in sessions {
        store.link(
            *id,
            &hex,
            minds_core::EvidenceMark::of(minds_core::EvidenceSource::Observed),
        )?;
    }
    Ok(())
}

/// Die von git getrackten, repo-relativen Pfade — `git ls-files -z`, einmal
/// je Checkpoint-Lauf. `None`, wenn git nicht antwortet.
fn tracked_files(root: &Path) -> Option<std::collections::BTreeSet<String>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        output
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .filter_map(|s| std::str::from_utf8(s).ok())
            .map(str::to_owned)
            .collect(),
    )
}

/// Baut den Seal dieser Epoche, legt ihn ab und schreibt den Epochen-Zustand
/// fort. Gibt die `seal_id` zurück — oder `None`, wenn die Ablage scheiterte
/// (dann bleibt das Journal liegen und der nächste Lauf holt sie idempotent
/// nach).
#[allow(clippy::too_many_arguments)]
fn seal_epoch(
    epochs: &EpochState,
    key: &minds_capture::SessionKey,
    events: &[minds_capture::JournalEvent],
    result: &ChainResult,
    outcome: SealOutcome,
    store: &dyn ContextStore,
    root: &Path,
    git_dir: &Path,
) -> Option<minds_core::ContentHash> {
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
                if prev.root == result.root && same_outcome {
                    return Some(prev_id.clone());
                }
            }
        }
    }

    let last_event_at = events.last().map(|e| e.at.clone()).unwrap_or_default();
    let seal = Seal {
        root: result.root.clone(),
        agent: key.agent().to_string(),
        // Die Beobachtungsgrenze steht IM Seal: „vollständig" heißt
        // vollständig innerhalb der Agent-Hooks, nie „alle Systemaktivität".
        scope: minds_core::evidence::SCOPE_AGENT_HOOKS_V1.to_string(),
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
                git_dir,
                Source::Checkpoint,
                &format!("Seal nicht baubar: {err}"),
            );
            return None;
        }
    };
    let seal_id = match store.put_seal(&text) {
        Ok(id) => id,
        Err(err) => {
            hooklog::report_at(
                git_dir,
                Source::Checkpoint,
                &format!("Seal nicht abgelegt: {err}"),
            );
            return None;
        }
    };

    // Best-effort-Signatur (ADR-0011, Entscheidung 5): nur mit konfiguriertem
    // Schlüssel, nie ein Grund zum Abbruch — ein unsignierter Seal bleibt
    // hash-valide, `minds sign --seal` rüstet nach.
    sign_seal_best_effort(store, &seal_id, &text, root, git_dir);
    // Epochen-Zustand best-effort: Fehlt er künftig, ist die Kette offen —
    // sichtbar und ehrlich, kein Grund, den Checkpoint zu kippen.
    if let Err(err) = epochs.record(key, &seal_id) {
        hooklog::report_at(
            git_dir,
            Source::Checkpoint,
            &format!("Epochen-Zustand nicht fortgeschrieben: {err}"),
        );
    }
    Some(seal_id)
}

/// Signiert einen frisch abgelegten Seal, wenn ein Schlüssel konfiguriert
/// ist. Best-effort: Jeder Fehlschlag ist eine Log-Zeile, nie ein Abbruch —
/// der Seal bleibt hash-valide, die Signatur ist die Urheber-Bindung obendrauf.
fn sign_seal_best_effort(
    store: &dyn ContextStore,
    seal_id: &minds_core::ContentHash,
    text: &str,
    root: &Path,
    git_dir: &Path,
) {
    let Some(key) = crate::sign_cmd::configured_key(root) else {
        return;
    };
    if !minds_attest::ssh_keygen_available() {
        return;
    }
    let outcome = minds_attest::ssh_sign(text, Path::new(&key))
        .map_err(|err| err.to_string())
        .and_then(|sig| {
            store
                .put_seal_signature(seal_id, &sig)
                .map_err(|err| err.to_string())
        });
    if let Err(err) = outcome {
        hooklog::report_at(
            git_dir,
            Source::Checkpoint,
            &format!("Seal {seal_id} nicht signiert: {err}"),
        );
    }
}

/// Baut eine Session, redigiert sie und legt sie ab. Gibt ihre [`SessionId`]
/// zurück.
fn store_one(
    key: &minds_capture::SessionKey,
    events: &[minds_capture::JournalEvent],
    root: &Path,
    git_dir: &Path,
    pipeline: &minds_redact::RedactionPipeline,
    store: &dyn ContextStore,
    tracked: Option<&std::collections::BTreeSet<String>>,
) -> Fallible<SessionId> {
    // Kein Commit im Kontext: die Produced-Kante bliebe sonst am verwaisten
    // Vor-Amend-Commit hängen (siehe Modul-Doku). Der Artefakt-Hash braucht die
    // Repo-Wurzel, um relative Pfade aufzulösen; das tracked-Set ist die
    // Read-Hash-Grenze (nur getrackter, ohnehin sichtbarer Inhalt).
    let ctx = Checkpoint {
        root: Some(root),
        commit: None,
        tracked,
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
            git_dir,
            Source::Checkpoint,
            &format!("Branch für {} nicht angelegt: {err}", put.id()),
        );
    }

    Ok(put.id())
}

/// Rüstet die Trailer an HEAD nach — aber nur, wenn HEAD noch auf dem
/// Wächter-Commit steht. Gibt den Commit zurück, an dem die Trailer nun stehen
/// (der *nachgerüstete*, also nach dem Amend), oder `None`, wenn nichts
/// getrailert wurde.
fn attach_trailers(
    repo: &Repo,
    commit: Option<&str>,
    sessions: &[SessionId],
) -> Fallible<Option<CommitId>> {
    if sessions.is_empty() {
        return Ok(None);
    }

    if let Some(expected) = commit {
        let expected: CommitId = expected.parse()?;
        if repo.head()?.commit() != Some(expected) {
            let note = format!(
                "HEAD steht nicht mehr auf {expected}; {} Session(s) gespeichert, aber nicht getrailert",
                sessions.len()
            );
            // Die einzige Spur, die dieser Fall hinterlässt: `fsck` sieht einen
            // Trailer ohne Session, aber nicht eine Session ohne Trailer.
            hooklog::report_at(repo.git_dir(), Source::Checkpoint, &note);
            return Ok(None);
        }
    }

    let update = repo.amend_head_with_sessions(sessions)?;
    if update.rewrote_head() {
        println!("  Trailer an {} nachgerüstet", update.commit());
    }
    Ok(Some(update.commit()))
}

/// Die Repo-Wurzel: das Elternverzeichnis von `.git`. Für ein bares Repo (kein
/// Elternteil) fällt sie auf das Git-Verzeichnis selbst zurück.
fn repo_root(repo: &Repo) -> std::path::PathBuf {
    repo.git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}
