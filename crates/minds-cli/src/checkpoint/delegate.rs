//! Checkpoint-Delegation an den Witness (EA-06d).
//!
//! Läuft `minds checkpoint` auf der Agent-Seite (post-commit-Hook im
//! Container) und ist `MINDS_WITNESS_SOCKET` gesetzt, bittet der Checkpoint
//! **zuerst** den Witness, seine Sessions zu versiegeln, und verarbeitet
//! **danach** das lokale Rückfall-Journal wie bisher (`agent-hooks/v1`).
//!
//! ```text
//!   minds checkpoint ──CheckpointRequest{commit}──► witness   (seal witness/v1,
//!        │            ◄──────Ack{status} / Nack──── (Trailer, Ledger)
//!        │
//!        └──► lokaler Pfad, unverändert: Rückfall-Journal ──► seal agent-hooks/v1
//! ```
//!
//! # Die Agent-Seite kennt den Socket, sonst nichts (W1)
//!
//! Kein Witness-Home, kein `witness.json`, kein Ledger, kein Schlüssel: Was
//! der Witness versiegelt hat, erfährt die Agent-Seite allein aus der
//! Statuszeile seiner Antwort. Entdeckt wird er ausschließlich über die
//! Umgebung (W3) — nichts in `.git/config` darf das beeinflussen, denn der
//! Agent kann es ändern.
//!
//! # Fail-open, wie der Hook
//!
//! Antwortet der Witness nicht rechtzeitig, lehnt er ab oder ist er gar nicht
//! erreichbar, steht [`UNAVAILABLE`] auf stderr und eine Zeile mit der
//! Fehlerart in `hook.log` — und der lokale Pfad läuft trotzdem. Der
//! Exit-Code hängt allein am lokalen Pfad, wie vor EA-06d. Die bezeugten
//! Sessions bleiben im Journal des Witness offen und werden beim nächsten
//! erfolgreichen Checkpoint versiegelt: Verloren ist nichts, nur vertagt.
//!
//! # Zwei Schreiber, ein Commit
//!
//! Beide Seiten rüsten Trailer an denselben Commit nach — derselbe
//! Mechanismus, idempotent. Wer zuerst nachrüstet, verschiebt HEAD; der
//! andere hält noch den Wächter-Commit in der Hand. Deshalb gilt in
//! [`super::attach_trailers`] als „HEAD steht noch dort" auch: HEAD ist
//! nachweislich **derselbe Commit mit zusätzlichen Session-Trailern**
//! ([`Repo::is_trailer_retrofit`]). Das trägt in beiden Reihenfolgen — auch
//! wenn der Witness nach Ablauf der Frist erst fertig wird, während der
//! lokale Pfad schon läuft. Ein echter neuer Commit oder eine umformulierte
//! Message lässt den Wächter greifen: kein Trailer am falschen Commit.
//!
//! Der Witness prüft den angefragten Commit gegen sein HEAD **vor** dem
//! Versiegeln. Steht er woanders (anderer Checkout, verlinkter Worktree),
//! lehnt er ab, und seine Sessions bleiben wirklich offen.
//!
//! Dieselbe Agent-Session kann so zwei Session-Ids am Commit tragen: einen
//! bezeugten Bereich (Witness) und einen A1-Bereich (Rückfall-Journal). Das
//! ist gewollt; zusammengeführt wird nichts (EA-11 rechnet je Bereich).

use std::time::{Duration, Instant};

use minds_capture::witness_proto::Frame;
use minds_git::{CommitId, Repo};

use crate::hook::witness;
use crate::hooklog::{self, Source};

/// Frist für die ganze Anfrage in Millisekunden; ohne gültigen Wert
/// [`DEFAULT_TIMEOUT`].
pub(crate) const TIMEOUT_ENV: &str = "MINDS_WITNESS_TIMEOUT_MS";

/// Der Witness versiegelt und signiert synchron; das dauert länger als ein
/// Hook-Frame, aber nie so lange, dass ein Commit spürbar hängen darf.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// Die eine Zeile auf stderr, wenn der Witness nicht versiegelt hat.
pub(crate) const UNAVAILABLE: &str =
    "witness unavailable — witnessed sessions stay open, will be sealed at the next checkpoint";

/// Trenner zwischen den Zeilen der Witness-Statusmeldung. Das Protokoll trägt
/// genau eine Zeile ohne Steuerzeichen; der Witness fügt seine Zeilen damit
/// zusammen, die Agent-Seite trennt sie wieder.
pub(crate) const LINE_SEPARATOR: &str = " | ";

/// Fragt den Witness, falls konfiguriert, und druckt seine Antwort.
///
/// Scheitert nie: Jede Störung ist eine Zeile auf stderr und in `hook.log`.
/// Dem Trailer-Amend des Witness folgt danach [`super::attach_trailers`]
/// selbst — in beiden Reihenfolgen, auch wenn der Witness erst nach Ablauf
/// der Frist fertig wird.
pub(super) fn delegate(repo: &Repo, commit: Option<&str>) {
    let Some(socket) = witness::socket_path() else {
        return;
    };

    // Der Witness bekommt immer einen konkreten Commit, nie „HEAD": Er prüft
    // ihn gegen sein HEAD, **bevor** er versiegelt. Ohne Commit (ungültiges
    // `--commit`, ungeborenes HEAD) wird er gar nicht gefragt — seine Sessions
    // bleiben offen, und das ist kein Ausfall des Witness, also auch nicht
    // der Satz dafür.
    let target = match commit {
        Some(raw) => raw.parse::<CommitId>().ok(),
        None => repo.head().ok().and_then(|head| head.commit()),
    };
    let Some(target) = target else {
        hooklog::log_at(
            repo.git_dir(),
            Source::Checkpoint,
            "witness not asked: no valid commit to checkpoint",
        );
        return;
    };
    let request = Frame::CheckpointRequest {
        commit: Some(target.to_string()),
        request_id: request_id(),
    };

    match witness::request(&socket, &request, Instant::now() + timeout()) {
        Ok(Frame::Ack { status, .. }) => {
            for line in status.split(LINE_SEPARATOR).filter(|l| !l.is_empty()) {
                // Fremder Text auf das Terminal des Nutzers: Das Protokoll hat
                // Steuerzeichen schon abgewiesen, `sanitize` ist die zweite
                // Schicht (Bidi, Unsichtbares).
                println!("  {}", crate::text::sanitize(line));
            }
        }
        // Der Grund ist fremder Text: nur bekannte Wörter kommen ins Log.
        Ok(Frame::Nack { reason, .. }) => unavailable(
            repo,
            match reason.as_str() {
                "rate limited" => "refused (rate limited)",
                "requester gone" => "refused (requester gone)",
                "commit required" => "refused (commit required)",
                "checkpoint failed" => "refused (checkpoint failed, see the witness log)",
                "status not encodable" => "refused (status not encodable)",
                "not supported" => "refused (not supported)",
                _ => "refused",
            },
        ),
        Ok(_) => unavailable(repo, "unexpected response"),
        // Abgelaufen heißt nicht abgebrochen: Der Witness kann noch
        // versiegeln und trailern. Der Satz auf stderr ist der vereinbarte;
        // das Log sagt genauer, was offen ist.
        Err(witness::TIMEOUT) => unavailable(
            repo,
            "timeout (the witness may still complete this checkpoint)",
        ),
        Err(kind) => unavailable(repo, kind),
    }
}

/// Meldet den Rückfall: der feste Satz auf stderr, die Fehlerart in `hook.log`
/// — der post-commit-Hook schickt stderr nach `/dev/null`, die Datei bleibt.
fn unavailable(repo: &Repo, kind: &'static str) {
    eprintln!("{UNAVAILABLE}");
    hooklog::log_at(
        repo.git_dir(),
        Source::Checkpoint,
        &format!("witness unavailable: {kind}"),
    );
}

/// Obergrenze für [`TIMEOUT_ENV`]: Ein Commit darf nicht beliebig lange am
/// Witness hängen, und `Instant + Duration` darf nicht überlaufen (Panic im
/// post-commit-Hook hieße: auch der lokale Pfad liefe nicht).
const MAX_TIMEOUT: Duration = Duration::from_secs(300);

/// Die Frist aus [`TIMEOUT_ENV`]; ungültig, leer oder `0` heißt Standard,
/// zu groß heißt [`MAX_TIMEOUT`].
fn timeout() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map_or(DEFAULT_TIMEOUT, Duration::from_millis)
        .min(MAX_TIMEOUT)
}

/// Eine Anfrage-Id, die nur eindeutig sein muss, nicht geheim: Sie ordnet die
/// Antwort der Anfrage zu. Prozess-Id und Uhrzeit, durch blake3 verrührt.
fn request_id() -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&std::process::id().to_le_bytes());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    hasher.update(&now.as_nanos().to_le_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    id
}
