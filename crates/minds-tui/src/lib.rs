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

use std::io::IsTerminal;

use minds_git::Repo;
use minds_reader::Inspection;

mod app;
mod filter;
mod input;
mod layout;
mod pipe;
mod term;
mod theme;
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

/// Startet die Oberfläche — oder, wenn stdout kein Terminal ist, schreibt die
/// Zeilen und kehrt zurück.
pub fn run(inspection: Inspection, repo: &Repo, opts: Options) -> Result<(), TuiError> {
    if !std::io::stdout().is_terminal() {
        return print(inspection, repo, opts);
    }
    let mut app = app::App::new(inspection, repo, opts.query);
    if let Start::Why { path, line } = &opts.start {
        app.open_why_line(path, *line)?;
    }
    app.run()?;
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
