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

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds checkpoint` aus. `commit` ist der Wächter-Commit aus dem
/// post-commit-Hook (siehe Modul-Doku); ohne ihn wird HEAD nachgerüstet, sofern
/// vorhanden.
pub fn run(commit: Option<&str>) -> ExitCode {
    match checkpoint(commit) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds checkpoint: {err}");
            ExitCode::FAILURE
        }
    }
}

fn checkpoint(commit: Option<&str>) -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let root = repo_root(&repo);

    let store = config::load(&root).open(&root)?;
    let journal = Journal::open(repo.git_dir());
    // Redaction-Policy: der strenge Default, sofern das Repo unter
    // `.minds/redact.json` nichts anderes vorgibt (fail-closed bei Fehlern).
    let pipeline = config::load_redaction(&root)?.pipeline()?;

    let mut stored: Vec<SessionId> = Vec::new();
    for key in journal.sessions()? {
        let events = journal.read(&key)?.events;
        if events.is_empty() {
            continue;
        }

        match store_one(&key, &events, &root, &pipeline, store.as_ref()) {
            Ok(id) => {
                // Erst nach erfolgreicher Ablage verwerfen: Ein Absturz dazwischen
                // darf Rohdaten nicht verlieren.
                journal.discard(&key)?;
                println!("  {}/{}: {id}", key.agent(), key.local_id());
                stored.push(id);
            }
            Err(err) => {
                // Journal bleibt liegen — die Session ist nicht verloren, nur
                // vertagt. fsck macht sie sichtbar.
                eprintln!("  {}/{}: übersprungen ({err})", key.agent(), key.local_id());
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

    // Die Session als eigenen Branch in der Forge sichtbar machen (nur beim
    // Child-Backend; sonst ein No-op). Best-effort: Der maßgebliche Record liegt
    // bereits im Store, und der Browsing-Branch lässt sich daraus jederzeit neu
    // bauen — ein Fehlschlag hier darf den Checkpoint nicht abbrechen und die
    // Session nicht ins Journal zurückwerfen.
    if let Err(err) = store.put_session_branch(&redacted) {
        eprintln!("  Branch für {} nicht angelegt: {err}", put.id());
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
            eprintln!(
                "  HEAD steht nicht mehr auf {expected}; {} Session(s) gespeichert, aber nicht getrailert",
                sessions.len()
            );
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
