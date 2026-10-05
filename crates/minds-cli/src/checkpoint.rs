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
//!
//! # Mit Witness: erst delegieren, dann lokal (EA-06d)
//!
//! Ist `MINDS_WITNESS_SOCKET` gesetzt, bittet der Checkpoint zuerst den
//! Witness, seine Sessions zu versiegeln und zu trailern, und druckt dessen
//! Zusammenfassung. Danach läuft der Pfad oben unverändert für das lokale
//! Rückfall-Journal. Der Witness ist best-effort: Antwortet er nicht, steht
//! eine Zeile auf stderr, und der Exit-Code bleibt der des lokalen Pfads.
//! Einzelheiten — auch, warum der Wächter dem Amend des anderen Schreibers
//! folgen darf — in [`delegate`].

use std::path::Path;
use std::process::ExitCode;

use minds_capture::Journal;
use minds_capture::epoch::EpochState;
use minds_core::SessionId;
use minds_git::{CommitId, Repo, TrailerUpdate};
use minds_store::ContextStore;

pub mod core;
pub(crate) mod delegate;

#[cfg(test)]
mod tests;

use self::core::{CheckpointEnv, EvidenceSource, SealSigner, run_checkpoint};
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
    checkpoint_at(&cwd, commit)
}

fn checkpoint_at(cwd: &Path, commit: Option<&str>) -> Fallible<()> {
    let repo = Repo::discover(cwd)?;
    let root = repo_root(&repo);

    // Erst der Witness, dann das lokale Rückfall-Journal (EA-06d). Ohne
    // `MINDS_WITNESS_SOCKET` ist das ein Leerlauf. Hat der Witness HEAD beim
    // Trailern verschoben, folgt `attach_trailers` ihm.
    delegate::delegate(&repo, commit);

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

    let epochs = EpochState::open(git_dir);
    // Die Read-Hash-Grenze einmal je Lauf; bei Fehlern keine Read-Hashes.
    let tracked = tracked_files(&root);
    let env = CheckpointEnv {
        repo: &repo,
        root: &root,
        log_dir: git_dir,
        store: store.as_ref(),
        pipeline: &pipeline,
        tracked: tracked.as_ref(),
    };
    let src = EvidenceSource {
        journal: &journal,
        epochs: &epochs,
        scope: minds_core::evidence::SCOPE_AGENT_HOOKS_V1,
    };
    let outcome = run_checkpoint(&env, &src, &SealSigner::UserConfig)?;
    let stored = outcome.stored;

    let attached = attach_trailers(&repo, commit, &stored, &|note| {
        hooklog::report_at(git_dir, Source::Checkpoint, note);
    })?;
    if let Some(update) = attached.filter(TrailerUpdate::rewrote_head) {
        println!("  Trailer retrofitted to {}", update.commit());
    }

    // Den Verweis Commit → Session zusätzlich in den Store-Index schreiben
    // (beobachtet). Der Trailer ist die verbindliche Quelle, aber er lebt in der
    // Historie des *Code*-Repos; beim Child-Repo-Backend liegen die Sessions in
    // einem eigenen Repo, und der Index reist mit ihnen. So ist der Kontext-Store
    // selbsttragend — wer nur ihn hat (etwa beim Browsen von `minds-child-project`
    // in GitLab), sieht über `index.json`, welche Session zu welchem Commit gehört.
    if let Some(update) = attached {
        index_trailered(&repo, store.as_ref(), commit, update, &stored)?;
    }

    Ok(())
}

/// Schreibt die Index-Kanten für den Commit, an dem die Trailer nun stehen:
/// die eigenen Sessions **und** jede, die seit dem Wächter-Commit per Trailer
/// nachgerüstet wurde.
///
/// Zwei Schreiber (EA-06d) rüsten nacheinander nach, und jeder Amend erzeugt
/// einen neuen Commit. Verknüpfte jeder nur seine eigenen Sessions, hingen die
/// des ersten am Zwischen-Commit, den der zweite Amend verwaist — der Store
/// allein sähe sie nie am Commit des Branches. Was zwischen Wächter und HEAD
/// dazukam, ist per [`Repo::is_trailer_retrofit`] ein reiner Trailer-Nachtrag
/// eines Minds-Schreibers.
pub(crate) fn index_trailered(
    repo: &Repo,
    store: &dyn ContextStore,
    guard: Option<&str>,
    update: TrailerUpdate,
    stored: &[SessionId],
) -> Fallible<()> {
    let mut sessions = stored.to_vec();
    if let Some(guard) = guard.and_then(|raw| raw.parse::<CommitId>().ok()) {
        let before = repo.session_ids_of(guard)?;
        for id in repo.session_ids_of(update.commit())? {
            if !before.contains(&id) && !sessions.contains(&id) {
                sessions.push(id);
            }
        }
    }
    record_index(store, update.commit(), &sessions)
}

/// Schreibt für jeden gerade abgelegten Session-Verweis eine beobachtete Kante
/// `commit → session` in den Store-Index.
pub(crate) fn record_index(
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
pub(crate) fn tracked_files(root: &Path) -> Option<std::collections::BTreeSet<String>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        // `core.fsmonitor` ist ein Befehl aus der Repo-Konfiguration. Der
        // Witness ruft das hier auf dem Host in einem Repo auf, dessen
        // `.git/config` der Agent schreiben kann (EA-06d) — kein fremder
        // Befehl auf seinem Weg.
        .args(["-c", "core.fsmonitor=false", "ls-files", "-z"])
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

/// Größter Index, den der Witness liest.
#[cfg(unix)]
const MAX_TRACKED_INDEX: u64 = 64 * 1024 * 1024;

/// [`tracked_files`] für den Witness (EA-10): aus dem Index des
/// festgehaltenen, ohne Includes geöffneten Repos statt über einen
/// `git`-Prozess — der folgte einem `include.path` aus der Konfiguration des
/// Agenten, etwa auf ein FIFO. Begrenzt gelesen: Ein Index, den der Agent auf
/// Millionen Einträge aufbläht, füllt nicht den Speicher; über der Grenze
/// heißt die Antwort `None` — Grenze unbekannt, keine Read-Hashes
/// (fail-closed).
#[cfg(unix)]
pub(crate) fn tracked_files_pinned(repo: &Repo) -> Option<std::collections::BTreeSet<String>> {
    Some(repo.tracked_paths(MAX_TRACKED_INDEX)?.into_iter().collect())
}

/// Rüstet die Trailer an HEAD nach — aber nur, wenn HEAD noch auf dem
/// Wächter-Commit steht. Gibt zurück, was an HEAD geschah; der Commit, an dem
/// die Trailer nun stehen, ist [`TrailerUpdate::commit`] (der
/// *nachgerüstete*, also nach dem Amend). `None`, wenn nichts getrailert
/// wurde.
///
/// Idempotent, auch über Schreiber hinweg: Witness und lokaler Pfad rufen
/// beide hierher (EA-06d); was schon in der Message steht, wird nicht
/// wiederholt. Gedruckt wird hier nichts — stdout gehört dem Aufrufer, und
/// beim Witness ist es der `--follow`-Strom. Was zu melden ist, geht an
/// `report`: beim lokalen Pfad `hook.log` im Repo, beim Witness sein eigenes
/// Log — nie eine Datei im Worktree des Agenten, die ein FIFO sein könnte.
pub(crate) fn attach_trailers(
    repo: &Repo,
    commit: Option<&str>,
    sessions: &[SessionId],
    report: &dyn Fn(&str),
) -> Fallible<Option<TrailerUpdate>> {
    attach_with(repo, commit, sessions, report, &|| {})
}

/// [`attach_trailers`] mit einer Naht zwischen Prüfung und Amend — dort, wo
/// im Betrieb der andere Schreiber dazwischenkommen kann. Nur Tests setzen
/// sie.
fn attach_with(
    repo: &Repo,
    commit: Option<&str>,
    sessions: &[SessionId],
    report: &dyn Fn(&str),
    before_amend: &dyn Fn(),
) -> Fallible<Option<TrailerUpdate>> {
    if sessions.is_empty() {
        return Ok(None);
    }

    let expected: Option<CommitId> = commit.map(str::parse).transpose()?;

    // Zwei Schreiber, ein HEAD (EA-06d): Geprüft und getrailert wird gegen
    // denselben beobachteten Commit — der Compare-and-Swap des Amends sichert
    // genau ihn. Rüstet der andere dazwischen nach, meldet er `RefRaced`;
    // dann einmal neu prüfen — ist HEAD nur derselbe Commit mit mehr
    // Trailern, trailern wir dorthin, sonst greift der Wächter.
    let moved = |expected: CommitId| {
        // Die einzige Spur, die dieser Fall hinterlässt: `fsck` sieht einen
        // Trailer ohne Session, aber nicht eine Session ohne Trailer.
        report(&format!(
            "HEAD no longer points at {expected}; {} session(s) stored, but no trailer attached",
            sessions.len()
        ));
    };
    let mut raced = None;
    for _ in 0..2 {
        let Some(head) = repo.head()?.commit() else {
            if let Some(expected) = expected {
                moved(expected);
                return Ok(None);
            }
            // Ungeborenes HEAD ohne Wächter: derselbe benannte Fehler wie bisher.
            return Ok(Some(repo.amend_head_with_sessions(sessions)?));
        };
        if let Some(expected) = expected {
            if !carries(repo, expected, head) {
                moved(expected);
                return Ok(None);
            }
        }
        before_amend();
        match repo.amend_commit_with_sessions(head, sessions) {
            Err(err @ minds_git::GitError::RefRaced { .. }) => raced = Some(err),
            update => return Ok(Some(update?)),
        }
    }
    // Zweimal überholt: der benannte Fehler des letzten Versuchs.
    Err(raced.map_or_else(
        || "HEAD kept moving while session trailers were attached".into(),
        |err| Box::new(err) as Box<dyn std::error::Error>,
    ))
}

/// Ob HEAD auf `expected` steht — oder auf `expected` mit zusätzlichen
/// Session-Trailern, nachgerüstet vom jeweils anderen Schreiber (Witness oder
/// lokaler Pfad, EA-06d).
#[cfg(unix)]
pub(crate) fn head_carries(repo: &Repo, expected: CommitId) -> Fallible<bool> {
    Ok(repo
        .head()?
        .commit()
        .is_some_and(|head| carries(repo, expected, head)))
}

/// `head` ist `expected` oder dessen reiner Trailer-Nachtrag. Alles andere,
/// auch ein nicht prüfbarer Commit, zählt als „weitergewandert": Lieber kein
/// Trailer als einer am falschen Commit.
fn carries(repo: &Repo, expected: CommitId, head: CommitId) -> bool {
    head == expected || repo.is_trailer_retrofit(expected, head).unwrap_or(false)
}

/// Die Repo-Wurzel: das Elternverzeichnis von `.git`. Für ein bares Repo (kein
/// Elternteil) fällt sie auf das Git-Verzeichnis selbst zurück.
fn repo_root(repo: &Repo) -> std::path::PathBuf {
    repo.git_dir()
        .parent()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}
