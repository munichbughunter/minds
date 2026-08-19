//! `minds checkpoint` — der kalte Pfad, den der post-commit-Hook aufruft.
//!
//! Hier schließt sich der Kern-Loop: Das Journal, das `minds hook` heiß und roh
//! gefüllt hat, wird gedeutet, redigiert, gespeichert und über einen
//! Commit-Trailer mit dem Code verbunden.
//!
//! ```text
//!   Journal ──► adapter::checkpoint ──► Redaction ──► Store ──► Trailer an HEAD
//!   (roh)        (Session)              (fail-closed)  (b3-…)    (Minds-Session-Id)
//! ```
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

use minds_capture::{Checkpoint, Journal, adapter};
use minds_core::SessionId;
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
    for key in outcome.keys {
        let events = journal.read(&key)?.events;
        if events.is_empty() {
            continue;
        }

        match store_one(&key, &events, &root, git_dir, &pipeline, store.as_ref()) {
            Ok(id) => {
                // Erst nach erfolgreicher Ablage verwerfen: Ein Absturz dazwischen
                // darf Rohdaten nicht verlieren.
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
                hooklog::report_at(
                    git_dir,
                    Source::Checkpoint,
                    &format!("{} übersprungen: {err}", key.display_redacted(&pipeline)),
                );
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
        store.link(*id, &hex, minds_core::Evidence::Observed)?;
    }
    Ok(())
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
) -> Fallible<SessionId> {
    // Kein Commit im Kontext: die Produced-Kante bliebe sonst am verwaisten
    // Vor-Amend-Commit hängen (siehe Modul-Doku). Der Artefakt-Hash braucht die
    // Repo-Wurzel, um relative Pfade aufzulösen.
    let ctx = Checkpoint {
        root: Some(root),
        commit: None,
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
