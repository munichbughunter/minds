//! Der Reader — die Oberfläche für v0.1: eine statische Seite über den Kontext.
//!
//! Das ist der Moment, um den es in der Vision geht: **auf eine Zeile klicken
//! und den Prompt dahinter sehen.** Alles in diesem Crate dient diesem einen
//! Satz.
//!
//! # Zustandslos, mit Absicht
//!
//! Der Reader hält keinen Zustand: Er liest bei jedem Lauf Git und Store, baut
//! einen [`Index`] und schreibt HTML. Kein Dienst, keine Datenbank, kein
//! Betrieb — „Ref fetchen, JSON parsen, rendern" (Architektur-Prinzip 6). Damit
//! ist die Seite trivial deploybar: ein Verzeichnis, das jeder Webserver
//! ausliefert, oder eine `file://`-URL im Browser.
//!
//! # Der Schnitt: I/O dünn, Logik rein
//!
//! Nur [`Index::build`] fasst Git und Store an. Alles Weitere — der Join von
//! Blame und Sessions, die Zusammenfassung, das Rendern — sind reine Funktionen
//! über einfache Daten und damit ohne Repository testbar, genau wie in
//! `minds-core`.

mod error;
pub use error::{ReaderError, Result};

mod index;
pub use index::{ContentLink, Degradation, Degraded, Index};

mod file;
pub use file::{FileView, Line};

pub mod summary;
pub use summary::Summary;

pub mod brief;

pub mod html;

mod render;
pub use render::{Site, render};

pub mod text;
pub use text::{sanitize, sanitize_path};

pub mod model;

pub mod graph;

pub mod evidence;

mod query;
pub use query::{Inspection, touches};
