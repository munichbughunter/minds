//! Der Fehlertyp dieses Crates.
//!
//! Eine Besonderheit vorweg, weil sie das Verhalten des ganzen Crates prägt:
//! **Auf dem Hook-Pfad werden diese Fehler nie zum Abbruch.** `minds hook`
//! protokolliert sie und endet mit 0. Ein Rekorder, der die Sitzung des
//! Nutzers abschießt, ist schlimmer als ein Rekorder, der ein Event verliert —
//! zumal der Verlust über die Sequenznummer sichtbar bleibt.
//!
//! Auf dem Capture-Pfad (`minds capture`, später) gilt das Gegenteil: Dort ist
//! jeder dieser Fehler ein Abbruch, weil dort etwas *gespeichert* werden soll.
//! Derselbe Fehlertyp, zwei Haltungen — die Unterscheidung liegt beim Aufrufer
//! und ist bewusst nicht in den Typ eingebaut.

use std::path::PathBuf;

/// Kurzform für `Result` mit [`CaptureError`].
pub type Result<T> = std::result::Result<T, CaptureError>;

/// Was beim Erfassen schiefgehen kann.
///
/// `#[non_exhaustive]`, weil dieses Crate mit M5 noch wächst (Adapter,
/// Transkript-Leser, Kanten-Ableitung).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CaptureError {
    /// Ein Dateisystem-Zugriff schlug fehl. Trägt die Operation im Klartext,
    /// damit die Meldung ohne Stacktrace verständlich ist.
    #[error("{op} fehlgeschlagen: {path}")]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Ein Bestandteil eines [`SessionKey`](crate::SessionKey) taugt nicht als
    /// Pfadkomponente.
    ///
    /// Das ist die Sicherheitsgrenze des Journals: Der Wert kommt aus dem JSON,
    /// das der Agent auf stdin schickt. Siehe `SessionKey::new`.
    #[error("unzulässiger Wert für {field}: {value:?}")]
    UnsafeKey { field: &'static str, value: String },

    /// Von hier aufwärts liegt kein Git-Repository.
    #[error("kein Git-Repository gefunden, ausgehend von {start}")]
    NoRepository { start: PathBuf },

    /// Nach vielen Versuchen war keine Sequenznummer frei. Praktisch heißt das:
    /// volles Dateisystem oder fehlende Schreibrechte.
    #[error("keine freie Sequenznummer in {dir} nach {probes} Versuchen")]
    SeqExhausted { dir: PathBuf, probes: u64 },

    #[error("Payload nennt weder session_id noch transcript_path")]
    NoSessionKey,

    /// JSON ließ sich nicht lesen oder schreiben.
    #[error("JSON-Fehler")]
    Json(#[from] serde_json::Error),
}

impl CaptureError {
    /// Baut einen [`CaptureError::Io`] mit Kontext.
    pub(crate) fn io(op: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            op,
            path: path.into(),
            source,
        }
    }
}
