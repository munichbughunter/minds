//! Der Fehlertyp dieses Crates.
//!
//! Der Reader ist die einzige Schicht, die *nichts* kaputtmachen kann: Er liest
//! und schreibt eine Handvoll HTML-Dateien in ein Ausgabeverzeichnis. Deshalb
//! gibt es hier keine fail-closed-Sonderregeln wie in `minds-redact` — ein
//! Fehler heißt schlicht „die Seite konnte nicht gebaut werden".

use std::path::PathBuf;

/// Kurzform für `Result` mit [`ReaderError`].
pub type Result<T> = std::result::Result<T, ReaderError>;

/// Was beim Bauen der Seite schiefgehen kann.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReaderError {
    /// Ein Git-Zugriff schlug fehl (Historie lesen, Blame, Blob).
    #[error("Git-Fehler beim Lesen des Repositories")]
    Git(#[from] minds_git::GitError),

    /// Der Store ließ sich nicht lesen.
    #[error("Store-Fehler beim Auflösen einer Session")]
    Store(#[from] minds_store::StoreError),

    /// Eine Datei ließ sich nicht schreiben.
    #[error("{op} fehlgeschlagen: {path}")]
    Io {
        /// Was versucht wurde, im Klartext.
        op: &'static str,
        /// Die betroffene Datei.
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// HEAD hat noch keinen Commit — es gibt nichts zu rendern.
    #[error("HEAD hat noch keinen Commit; es gibt nichts zu rendern")]
    UnbornHead,
}

impl ReaderError {
    /// Baut einen [`ReaderError::Io`] mit Kontext.
    pub(crate) fn io(op: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            op,
            path: path.into(),
            source,
        }
    }
}
