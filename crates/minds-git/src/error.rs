//! Der Fehlertyp dieses Crates — und die Naht zu `gix`.
//!
//! # Warum gix' Fehlertypen nicht durch diese API scheinen
//!
//! `gix` steht bei 0.x und bewegt sich schnell: Fehler-Enums bekommen Varianten,
//! wandern zwischen Modulen, werden umbenannt. Stünden sie in der Signatur von
//! [`GitError`], wäre **jeder gix-Bump ein Breaking Change** für `minds-store`,
//! `minds-cli` und `minds-reader` — obwohl dort niemand gix kennt.
//!
//! Deshalb trägt jede Variante ihre Ursache als [`Source`] (`Box<dyn Error>`).
//! Das kostet die Fähigkeit, auf gix-Interna zu matchen — und genau die will
//! hier niemand: Was die Aufrufer unterscheiden müssen, steht in den *Varianten*
//! dieses Enums, formuliert in Minds-Begriffen. Alles darunter ist Diagnose und
//! erreicht den Nutzer über die Fehlerkette (`{:#}` bzw. `source()`).
//!
//! Das ist dieselbe Linie wie Architektur-Prinzip 5 im Plan: gix ist eine
//! Implementierungsentscheidung, keine öffentliche Zusage. Käme für einen
//! Teilbereich je der `git`-Shell-Fallback (Blame), ändert sich an dieser API
//! nichts.

use std::fmt;
use std::path::PathBuf;

use crate::oid::CommitId;

/// Kurzform für `Result` mit [`GitError`].
pub type Result<T> = std::result::Result<T, GitError>;

/// Die eingepackte Ursache eines [`GitError`] — meist ein gix-Fehler.
///
/// Bewusst `Box<dyn Error>` und kein konkreter Typ: siehe Modul-Doku. `Send +
/// Sync`, damit Fehler über Thread-Grenzen wandern können (die CLI wird
/// Sessions parallel bereinigen).
pub type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Was bei einer Git-Operation schiefgehen kann.
///
/// `#[non_exhaustive]`, weil dieses Crate mit M3 noch wächst (Blobs/Trees,
/// Refs, Blame): Eine neue Variante soll kein Breaking Change sein.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitError {
    /// Ab `start` aufwärts war kein Repository zu finden.
    ///
    /// Der Normalfall, wenn `minds` außerhalb eines Repos aufgerufen wird — für
    /// die CLI der Anlass für einen freundlichen Hinweis, nicht für einen
    /// Stacktrace.
    #[error("kein Git-Repository gefunden — weder in {start} noch darüber")]
    Discover {
        /// Verzeichnis, ab dem nach oben gesucht wurde.
        start: PathBuf,
        /// Ursache aus gix.
        #[source]
        source: Source,
    },

    /// `path` ließ sich nicht als Repository öffnen (kein Repo, kaputtes
    /// `.git`, fehlende Rechte).
    #[error("{path} lässt sich nicht als Git-Repository öffnen")]
    Open {
        /// Der angefragte Pfad.
        path: PathBuf,
        /// Ursache aus gix.
        #[source]
        source: Source,
    },

    /// HEAD ließ sich nicht lesen oder nicht bis auf einen Commit auflösen.
    #[error("HEAD in {path} lässt sich nicht auflösen")]
    Head {
        /// Das Git-Verzeichnis des betroffenen Repositories.
        path: PathBuf,
        /// Ursache aus gix.
        #[source]
        source: Source,
    },

    /// Die Historie ab `tip` ließ sich nicht (vollständig) ablaufen — der
    /// Startpunkt fehlt im Repository oder ein Objekt darunter ist nicht da.
    #[error("Historie ab {tip} lässt sich nicht ablaufen")]
    Revwalk {
        /// Startpunkt des Walks.
        tip: CommitId,
        /// Ursache aus gix.
        #[source]
        source: Source,
    },

    /// Ein Ref ließ sich nicht nachschlagen oder nicht auflösen.
    ///
    /// **Nicht** der Fall „Ref existiert nicht" — der ist regulär und kommt als
    /// `Ok(None)` zurück (siehe `objects.rs`).
    #[error("Ref {name} lässt sich nicht auflösen")]
    Reference {
        /// Der angefragte Ref-Name, z. B. `refs/minds/context`.
        name: String,
        /// Ursache aus gix.
        #[source]
        source: Source,
    },

    /// Ein Objekt ließ sich nicht lesen — es fehlt, ist beschädigt oder hat
    /// nicht den erwarteten Typ.
    #[error("Git-Objekt {id} lässt sich nicht lesen")]
    ReadObject {
        /// Textform des Objekt-Hashes. Bewusst ein `String`: Die Variante
        /// trifft Commits, Trees und Blobs gleichermaßen, und ein
        /// Summen-Typ nur für Fehlermeldungen wäre ein schlechter Tausch.
        id: String,
        /// Ursache aus gix.
        #[source]
        source: Source,
    },

    /// Ein Objekt ließ sich nicht schreiben (Rechte, volle Platte, defekte
    /// Objektdatenbank).
    #[error("Git-Objekt lässt sich nicht schreiben")]
    WriteObject {
        /// Ursache aus gix.
        #[source]
        source: Source,
    },

    /// Ein Commit ließ sich nicht schreiben oder der Ref nicht bewegen.
    #[error("Commit auf {name} lässt sich nicht schreiben")]
    Commit {
        /// Der betroffene Ref-Name.
        name: String,
        /// Ursache aus gix.
        #[source]
        source: Source,
    },

    /// Der Ref hat sich zwischen Lesen und Schreiben bewegt.
    ///
    /// Das ist **kein Defekt, sondern der Schutzmechanismus**: Ein paralleler
    /// `minds capture` war schneller. Nichts ging verloren — der Aufrufer liest
    /// neu und versucht es erneut (siehe `refs.rs`).
    #[error("Ref {name} hat sich bewegt (erwartet: {expected}, gefunden: {actual})")]
    RefRaced {
        /// Der betroffene Ref-Name.
        name: String,
        /// Der Stand, auf dem aufgesetzt wurde.
        expected: String,
        /// Der Stand, der stattdessen vorgefunden wurde.
        actual: String,
    },

    /// Der angefragte Ref liegt außerhalb des Minds-Namensraums.
    ///
    /// Die Leitplanke gegen die eine Klasse Fehler, die man nicht wieder
    /// gutmacht — siehe `refs.rs`.
    #[error("Minds schreibt nur unterhalb von {namespace}, nicht auf {name}")]
    ForbiddenRef {
        /// Der abgewiesene Ref-Name.
        name: String,
        /// Der erlaubte Namensraum.
        namespace: &'static str,
    },

    /// Es ist keine Git-Identität konfiguriert.
    #[error(
        "keine Git-Identität konfiguriert — `git config user.name` und `git config user.email` setzen"
    )]
    Identity,

    /// Ein Pfad taugt nicht als Eintrag in einem Git-Baum.
    ///
    /// Minds baut seine Pfade selbst aus Hashes zusammen — ein ungültiger Pfad
    /// ist deshalb immer ein Programmfehler und nie eine Nutzereingabe. Er wird
    /// abgewiesen, bevor er in einen Baum gerät: Ein krummer Pfad im Store
    /// fiele erst dem Reader auf, und dann ist er schon geschrieben.
    #[error("ungültiger Pfad {path:?}: {reason}")]
    InvalidPath {
        /// Der abgewiesene Pfad.
        path: String,
        /// Was an ihm nicht stimmt.
        reason: &'static str,
    },

    /// HEAD hat noch keinen Commit — es gibt nichts, woran ein Trailer hängen
    /// könnte.
    ///
    /// Anders als der ungeborene HEAD beim *Lesen* (der ist regulär, siehe
    /// `head.rs`) ist er beim Nachrüsten ein Fehler: Der Aufrufer wollte etwas
    /// verlinken, und es gibt nichts zu verlinken.
    #[error("HEAD in {path} hat noch keinen Commit — nichts zum Nachrüsten")]
    NothingToAmend {
        /// Das Git-Verzeichnis des betroffenen Repositories.
        path: PathBuf,
    },

    /// Der Commit ist signiert; ein nachgerüsteter Trailer würde die Signatur
    /// entwerten.
    ///
    /// Minds macht die Signatur eines anderen weder still kaputt noch wirft es
    /// sie weg — siehe `amend.rs`. Der Ausweg ist der `prepare-commit-msg`-Weg,
    /// bei dem der Trailer *vor* der Signatur entsteht.
    #[error(
        "Commit {commit} ist signiert ({header}) — ein nachgerüsteter Trailer würde die Signatur entwerten"
    )]
    SignedCommit {
        /// Der betroffene Commit.
        commit: CommitId,
        /// Der Header, an dem die Signatur erkannt wurde (`gpgsig` bzw.
        /// `gpgsig-sha256`).
        header: String,
    },

    /// Die Commit-Message ist kein gültiges UTF-8 und wird deshalb nicht neu
    /// geschrieben.
    ///
    /// Gelesen wird sie trotzdem — nur verlustbehaftet (siehe
    /// `Repo::message_of`). Beim Zurückschreiben wäre dieser Verlust echter
    /// Datenverlust, also bleibt der Commit, wie er ist.
    #[error("Message von {commit} ist kein gültiges UTF-8 — Minds schreibt sie nicht neu")]
    MessageNotUtf8 {
        /// Der betroffene Commit.
        commit: CommitId,
    },

    /// Blame ließ sich nicht ermitteln — gitoxide liefert nicht, `git` fehlt
    /// oder bricht ab.
    ///
    /// **Nicht** der Fall „Datei gibt es in diesem Commit nicht": Der ist
    /// regulär und kommt als leere Liste zurück (siehe `blame.rs`).
    #[error("Blame für {path} lässt sich nicht ermitteln")]
    Blame {
        /// Der betroffene Pfad, repo-relativ.
        path: String,
        /// Ursache: gix, der `git`-Prozess oder dessen stderr.
        #[source]
        source: Source,
    },

    /// Der Diff eines Commits ließ sich nicht ermitteln — `git` fehlt oder
    /// bricht ab.
    #[error("Diff für Commit {commit} lässt sich nicht ermitteln")]
    Diff {
        /// Der betroffene Commit.
        commit: CommitId,
        /// Ursache: der `git`-Prozess oder dessen stderr.
        #[source]
        source: Source,
    },
}

impl GitError {
    pub(crate) fn discover(start: impl Into<PathBuf>, source: impl Into<Source>) -> Self {
        Self::Discover {
            start: start.into(),
            source: source.into(),
        }
    }

    pub(crate) fn open(path: impl Into<PathBuf>, source: impl Into<Source>) -> Self {
        Self::Open {
            path: path.into(),
            source: source.into(),
        }
    }

    pub(crate) fn head(path: impl Into<PathBuf>, source: impl Into<Source>) -> Self {
        Self::Head {
            path: path.into(),
            source: source.into(),
        }
    }

    pub(crate) fn revwalk(tip: CommitId, source: impl Into<Source>) -> Self {
        Self::Revwalk {
            tip,
            source: source.into(),
        }
    }

    pub(crate) fn reference(name: impl Into<String>, source: impl Into<Source>) -> Self {
        Self::Reference {
            name: name.into(),
            source: source.into(),
        }
    }

    /// `id` ist alles, was sich als Hash anzeigen lässt — `CommitId`, `TreeId`,
    /// `BlobId` oder gix' eigener `ObjectId`.
    pub(crate) fn read_object(id: impl fmt::Display, source: impl Into<Source>) -> Self {
        Self::ReadObject {
            id: id.to_string(),
            source: source.into(),
        }
    }

    pub(crate) fn write_object(source: impl Into<Source>) -> Self {
        Self::WriteObject {
            source: source.into(),
        }
    }

    pub(crate) fn commit(name: impl Into<String>, source: impl Into<Source>) -> Self {
        Self::Commit {
            name: name.into(),
            source: source.into(),
        }
    }

    pub(crate) fn ref_raced(
        name: impl Into<String>,
        expected: Option<CommitId>,
        actual: Option<CommitId>,
    ) -> Self {
        fn show(commit: Option<CommitId>) -> String {
            commit.map_or_else(|| "kein Ref".to_owned(), |c| c.to_string())
        }
        Self::RefRaced {
            name: name.into(),
            expected: show(expected),
            actual: show(actual),
        }
    }

    pub(crate) fn forbidden_ref(name: impl Into<String>) -> Self {
        Self::ForbiddenRef {
            name: name.into(),
            namespace: crate::refs::MINDS_REF_NAMESPACE,
        }
    }

    pub(crate) fn invalid_path(path: impl Into<String>, reason: &'static str) -> Self {
        Self::InvalidPath {
            path: path.into(),
            reason,
        }
    }

    pub(crate) fn nothing_to_amend(path: impl Into<PathBuf>) -> Self {
        Self::NothingToAmend { path: path.into() }
    }

    pub(crate) fn signed_commit(commit: CommitId, header: impl Into<String>) -> Self {
        Self::SignedCommit {
            commit,
            header: header.into(),
        }
    }

    pub(crate) fn message_not_utf8(commit: CommitId) -> Self {
        Self::MessageNotUtf8 { commit }
    }

    pub(crate) fn blame(path: impl Into<String>, source: impl Into<Source>) -> Self {
        Self::Blame {
            path: path.into(),
            source: source.into(),
        }
    }

    pub(crate) fn diff(commit: CommitId, source: impl Into<Source>) -> Self {
        Self::Diff {
            commit,
            source: source.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn message_names_the_path() {
        let err = GitError::open("/tmp/kein-repo", "Testursache");
        assert!(err.to_string().contains("/tmp/kein-repo"));
    }

    #[test]
    fn cause_survives_in_the_error_chain() {
        // Die Fassade darf die Diagnose nicht verschlucken: Was gix gemeldet
        // hat, muss über `source()` erreichbar bleiben.
        let err = GitError::head("/tmp/repo/.git", "kaputte Referenz");
        assert_eq!(err.source().unwrap().to_string(), "kaputte Referenz");
    }

    #[test]
    fn a_refused_amend_names_the_commit() {
        // Die Fehlermeldung muss den Commit benennen, sonst weiß niemand, wo
        // von Hand nachzusehen ist.
        let commit: CommitId = "1e4f0b6a8c2d3e5f7a9b0c1d2e3f4a5b6c7d8e9f".parse().unwrap();
        let signed = GitError::signed_commit(commit, "gpgsig");
        let not_utf8 = GitError::message_not_utf8(commit);

        assert!(signed.to_string().contains(&commit.to_string()));
        assert!(signed.to_string().contains("gpgsig"));
        assert!(not_utf8.to_string().contains(&commit.to_string()));
    }
}
