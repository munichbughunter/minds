//! `minds inspect` — die Entstehung einer Änderung, im Terminal.
//!
//! Git beantwortet „was ist passiert?"; Minds beantwortet „warum, durch wen,
//! mit welchen Schritten, mit welchem Beleg, mit welcher Bewertung?". Diese
//! Oberfläche macht die Kette navigierbar: eine **Activity**-Liste der
//! Sessions, der **Graph** einer Session (Absicht → Agent → Effekte →
//! Änderung → Review) und die **Why**-Kette einer Zeile oder eines Commits
//! samt Inspector, der jede Kante erklärt.
//!
//! # Leitplanken
//!
//! - **Nur `minds-reader`.** Kein eigener Ref-, Store- oder Journal-Zugriff;
//!   was die Oberfläche nicht bekommt, bekommt erst der Reader.
//! - **Strikt lesend.** Keine Reviews, kein `forget`, keine Konfiguration.
//! - **Nur gespeicherte, redigierte Daten.** Das Journal bleibt außen vor.
//! - **Fail-soft.** Eine vergessene oder kaputte Session ist eine
//!   degradierte Zeile, kein Absturz; das Terminal wird auch bei Panic
//!   zurückgegeben.
//! - **Pipe-tauglich.** Ist stdout kein Terminal, kommen die Zeilen
//!   tab-separiert und ohne ANSI — dieselbe Liste, dieselbe Suche.
//! - **Live.** Die Oberfläche hält keinen eingefrorenen Stand: Sie fragt ihre
//!   [`Source`] regelmäßig nach einem Fingerabdruck und lädt neu, wenn er
//!   sich ändert (oder auf `r`). Woraus der Fingerabdruck besteht, weiß die
//!   Quelle — die Oberfläche fasst dafür kein Git an.

use std::io::IsTerminal;

use minds_git::Repo;
use minds_reader::Inspection;

mod app;
mod changes;
mod filter;
mod input;
mod layout;
mod pipe;
mod term;
mod theme;
mod verify;
mod view;

pub use layout::Zoom;

/// Womit die Oberfläche beginnt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Die Liste.
    Activity,
    /// Die Herkunftskette einer Zeile.
    Why {
        /// Der Pfad.
        path: String,
        /// Die Zeile, 1-basiert.
        line: u32,
    },
}

/// Die Optionen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Eine Suche, mit der die Liste beginnt.
    pub query: Option<String>,
    /// Womit begonnen wird.
    pub start: Start,
}

/// Was schiefgehen kann.
#[derive(Debug)]
pub enum TuiError {
    /// Das Terminal oder stdout.
    Io(std::io::Error),
    /// Der Reader.
    Reader(minds_reader::ReaderError),
}

impl std::fmt::Display for TuiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TuiError::Io(err) => write!(f, "Terminal: {err}"),
            TuiError::Reader(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for TuiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TuiError::Io(err) => Some(err),
            TuiError::Reader(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for TuiError {
    fn from(err: std::io::Error) -> Self {
        TuiError::Io(err)
    }
}

impl From<minds_reader::ReaderError> for TuiError {
    fn from(err: minds_reader::ReaderError) -> Self {
        TuiError::Reader(err)
    }
}

/// Ein Fingerabdruck des Stands, in zwei Teilen: HEAD für sich, weil nur
/// eine Bewegung von HEAD die teuren, Git-gestützten Teile (Blame, Abgleich)
/// neu rechnen muss — alles andere in `refs`, als Hash, damit der Abdruck bei
/// vielen Refs klein bleibt. Er wird nur verglichen, nie gezeigt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    /// HEAD: Branch und Commit, wie die Quelle sie liest.
    pub head: String,
    /// Alles Übrige, das ein Neuladen auslöst.
    pub refs: String,
}

/// Woher die Oberfläche ihren Stand bekommt — und woran sie merkt, dass er
/// sich geändert hat.
///
/// Die CLI implementiert das über Repo und Store; die Oberfläche selbst
/// bleibt beim Reader.
pub trait Source {
    /// Lädt das Lese-Modell frisch.
    fn load(&self) -> Result<Inspection, minds_reader::ReaderError>;

    /// Ein billiger Fingerabdruck des Stands, den [`load`](Self::load)
    /// lesen würde. Ändert er sich, lädt die Oberfläche neu. `None` heißt:
    /// gerade nicht bestimmbar — dann wird nicht von selbst neu geladen
    /// (`r` lädt trotzdem).
    fn stamp(&self) -> Option<Stamp>;

    /// Die signaturabhängigen Teile des Urteils über `commit` — Assurance
    /// je Session, Intent-Lage, Scope —, wie `minds verify` sie rechnet
    /// (gegen die vertrauenswürdigen Signer). `Err`: nicht bestimmbar, mit
    /// Grund — der Verify-Tab sagt dann nie VERIFIED.
    fn verify(&self, _commit: minds_git::CommitId) -> Result<CommitVerify, String> {
        Err("not available".into())
    }
}

/// Die Assurance einer Session, wie `minds verify` sie ausspricht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAssurance {
    /// Die Session.
    pub session: minds_core::SessionId,
    /// Die erreichte Stufe.
    pub level: minds_reader::assurance::Assurance,
    /// Warum nicht höher — der erste Grund, entschärft.
    pub reason: Option<String>,
    /// Die Intent-Lage (gebunden, verkettet, Signatur).
    pub intent: minds_reader::assurance::IntentState,
    /// Das Seal-Material ist verändert (auch eine ungültige
    /// Witness-Signatur).
    pub tampered: bool,
}

/// Das Urteil von `minds verify` — sein Exit-Code, nicht nachgebaut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyVerdict {
    /// Exit 0.
    Verified,
    /// Exit 2.
    Incomplete,
    /// Exit 1.
    Tampered,
    /// Exit 3.
    NotVerifiable,
}

/// Was `minds verify` über einen Commit sagt, soweit es Signaturen und den
/// Store braucht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitVerify {
    /// Das Urteil, von `minds verify` selbst — `Err` mit Grund, wenn es sich
    /// nicht bestimmen ließ (Exit 4, kein Binary).
    pub verdict: Result<VerifyVerdict, String>,
    /// Je verknüpfter Session.
    pub sessions: Vec<SessionAssurance>,
    /// Die geänderten Pfade außerhalb des erklärten Bereichs, entschärft —
    /// `None`, wenn kein Bereich geprüft werden konnte (siehe `scope_note`).
    pub out_of_scope: Option<Vec<String>>,
    /// Dieselben Pfade unentschärft — nur zum Navigieren (Datei im Diff
    /// wählen), nie zur Anzeige.
    pub out_of_scope_paths: Vec<String>,
    /// Warum kein Bereich geprüft wurde (kein Intent gebunden, …).
    pub scope_note: Option<String>,
}

/// Startet die Oberfläche — oder, wenn stdout kein Terminal ist, schreibt die
/// Zeilen und kehrt zurück.
pub fn run(source: &dyn Source, repo: &Repo, opts: Options) -> Result<(), TuiError> {
    // Der Fingerabdruck **vor** dem Laden: Ändert sich der Stand dazwischen,
    // sieht die erste Prüfung einen neuen Abdruck und lädt nach — umgekehrt
    // ginge die Änderung verloren.
    if !std::io::stdout().is_terminal() {
        return print(source.load()?, repo, opts);
    }
    let stamp = source.stamp();
    let inspection = source.load()?;
    let mut app = app::App::new(inspection, repo, opts.query);
    app.live = stamp.is_some();
    app.stamp = stamp;
    if let Start::Why { path, line } = &opts.start {
        app.open_why_line(path, *line)?;
    }
    app.run(source)?;
    Ok(())
}

/// Der Pipe-Weg.
fn print(inspection: Inspection, repo: &Repo, opts: Options) -> Result<(), TuiError> {
    let mut out = std::io::stdout().lock();
    match opts.start {
        Start::Why { path, line } => {
            let chain = inspection.why_line(repo, &path, line)?;
            pipe::why(&mut out, &chain)?;
        }
        Start::Activity => {
            let terms = filter::terms(opts.query.as_deref().unwrap_or(""));
            let cards: Vec<_> = inspection
                .cards()
                .into_iter()
                .filter(|card| filter::matches(card, inspection.index(), &terms))
                .collect();
            pipe::cards(&mut out, &cards)?;
        }
    }
    Ok(())
}
