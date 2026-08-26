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
    ///
    /// Der Wert selbst steht bewusst **nicht** im Fehler — nicht einmal im
    /// Struct: Was hier abgelehnt wird, ist per Definition fremdbestimmt, und
    /// gerade die Werte, die die Prüfung reißen (ein JWT über 128 Zeichen, ein
    /// Base64-Secret mit `+`/`=`), sind die, die nie ins hook.log dürfen (#95).
    /// Länge und Regel reichen, um den Fehler zu verstehen.
    #[error(
        "unzulässiger Wert für {field}: {len} Zeichen, erlaubt ist [A-Za-z0-9._-] (1–128, nicht ».« oder »..«)"
    )]
    UnsafeKey { field: &'static str, len: usize },

    /// Von hier aufwärts liegt kein Git-Repository.
    #[error("kein Git-Repository gefunden, ausgehend von {start}")]
    NoRepository { start: PathBuf },

    /// Nach vielen Versuchen war keine Sequenznummer frei. Praktisch heißt das:
    /// volles Dateisystem oder fehlende Schreibrechte.
    #[error("keine freie Sequenznummer in {dir} nach {probes} Versuchen")]
    SeqExhausted { dir: PathBuf, probes: u64 },

    #[error("Payload nennt weder session_id noch transcript_path")]
    NoSessionKey,

    /// Die Schlüssel-Datei eines Session-Verzeichnisses bestätigt nicht den
    /// Schlüssel, unter dem geschrieben werden soll — eine Hash-Kollision,
    /// eine untergeschobene Kennung oder eine beschädigte Datei. In keinem der
    /// drei Fälle darf dort geschrieben werden (#95).
    ///
    /// Der Text nennt bewusst nur das Verzeichnis (dessen Name ein Hash ist)
    /// und keine `local_id`: Diese Meldung wandert über den Hook-Pfad ins
    /// `hook.log`, und dort eine rohe Kennung abzulegen wäre genau das Leck,
    /// das #95 schließt.
    #[error("Schlüssel-Datei bestätigt eine andere Session: {dir}")]
    KeyFileMismatch { dir: PathBuf },

    /// Der Session-Salt fehlt oder ist beschädigt, obwohl bereits eine
    /// versiegelte Epoche existiert. Ein neuer Salt würde für dieselbe
    /// Evidence einen zweiten, abweichenden Root erzeugen — ein Epoch-Fork.
    /// Deshalb wird hier **nicht** regeneriert: Der Verlust ist selbst der
    /// Befund, die Epoche ist ohne den Salt nicht mehr reproduzierbar.
    ///
    /// Der Text nennt nur das Verzeichnis (gehashter Name), nie die rohe
    /// `local_id` — die Meldung wandert ins `hook.log` (#95).
    #[error(
        "Session-Salt fehlt oder ist beschädigt, aber eine versiegelte Epoche existiert: {dir} — der Chain-Root ist nicht mehr reproduzierbar; es wird kein neuer Salt erzeugt (kein zweiter Seal für dieselbe Evidence)"
    )]
    SaltLost { dir: PathBuf },

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
