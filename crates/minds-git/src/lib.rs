//! `minds-git` — die dünne Schicht zwischen Minds und Git.
//!
//! Alles, was Minds von Git braucht, ist client-seitig: ein Repository finden,
//! Refs lesen und schreiben, die Historie ablaufen, Objekte lesen und
//! schreiben, Commit-Messages lesen und ergänzen. Genau dafür gibt es dieses
//! Crate — und für nichts sonst. Es kennt weder `Session` noch Store noch CLI;
//! es macht Git-Begriffe zu Rust-Typen.
//!
//! # Was das Crate heute kann
//!
//! - **Repo öffnen** — [`Repo::discover`] (sucht nach oben, wie `git`) und
//!   [`Repo::open`] (genau dieser Pfad).
//! - **HEAD auflösen** — [`Repo::head`] mit allen drei Zuständen als
//!   [`Head`]-Varianten, inklusive des ungeborenen HEAD eines frischen Repos.
//! - **Revwalk** — [`Repo::revwalk`]: jeder erreichbare Commit genau einmal.
//! - **Blobs und Trees** — [`Repo::read_blob_at`], [`Repo::list_blobs_at`],
//!   [`Repo::write_blob`], [`Repo::write_tree`]: der Objekt-Layer, auf dem der
//!   `ContextStore` aus M4 aufsetzt.
//! - **Der Kontext-Ref** — [`Repo::commit_tree_to_ref`] verankert einen Baum
//!   unter [`DEFAULT_CONTEXT_REF`] als Orphan-Historie, mit Compare-and-Swap
//!   gegen parallele Läufe.
//! - **Trailer lesen** — [`Repo::session_ids_of`]: vom Commit zurück zur
//!   Session.
//! - **Trailer nachrüsten** — [`Repo::amend_head_with_sessions`]: von der
//!   Session zurück zum Commit, für den `post-commit`-Weg. Damit ist die
//!   Schleife aus der Vision in beide Richtungen geschlossen.
//!
//! Es fehlt noch: der `BlameProvider`-Trait.
//!
//! # Trailer anhängen: zwei Wege, ein Format
//!
//! Wer den Trailer *vor* dem Commit setzen kann, braucht dieses Crate dafür
//! nicht: [`minds_core::Trailer::append_all`] erweitert die Message als Text,
//! ohne Repository und ohne Historie umzuschreiben — der Weg des
//! `prepare-commit-msg`-Hooks aus M6. [`Repo::amend_head_with_sessions`] ist
//! die Nachrüstung für alles, was schon committet ist; die Absatz- und
//! Idempotenz-Regeln kommen in beiden Fällen aus `minds-core`, damit es nur
//! *eine* Definition davon gibt, wie eine Minds-Message aussieht.
//!
//! # gix bleibt eine Implementierungsentscheidung
//!
//! Kein gix-Typ steht in einer öffentlichen Signatur — weder in [`GitError`]
//! (siehe `error.rs`) noch bei [`CommitId`] (siehe `oid.rs`). Das kostet ein paar
//! Newtypes und spart, dass jeder gix-Bump durch `minds-store`, `minds-cli` und
//! `minds-reader` durchschlägt. Es hält außerdem die Tür für den im Plan
//! vorgesehenen `git`-Shell-Fallback offen (Architektur-Prinzip 5): Wenn Blame
//! vorerst über die Shell läuft, merkt das keine andere Schicht.
//!
//! # I/O lebt hier
//!
//! `minds-core` hat kein I/O, `minds-redact` auch nicht — beide sind reine
//! Funktionen mit Golden-Tests. Dieses Crate ist die Gegenseite: Es tut fast
//! nichts als I/O. Deshalb testet es gegen echte Repositories im
//! Temp-Verzeichnis statt gegen Mocks (siehe `src/fixture.rs`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod amend;
mod blame;
mod diff;
mod error;
mod head;
mod objects;
mod oid;
mod refs;
mod repo;
mod trailer;
mod walk;

#[cfg(test)]
mod fixture;
mod time;

pub use amend::TrailerUpdate;
pub use blame::{AutoBlame, BlameLine, BlameProvider, GixBlame, ShellBlame};
pub use diff::{CommitDiff, DiffFile, DiffKind, DiffLine};
pub use error::{GitError, Result, Source};
pub use head::Head;
pub use oid::{BlobId, CommitId, CommitIdParseError, TreeId};
pub use refs::{DEFAULT_CONTEXT_REF, MINDS_REF_NAMESPACE, RefUpdate};
pub use repo::Repo;
