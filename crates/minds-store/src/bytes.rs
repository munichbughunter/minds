//! Die Nutzlast — und der Nachweis, dass sie gespeichert werden darf.
//!
//! [`SessionBytes`] ist das einzige, was ein Backend je zu schreiben bekommt:
//! die kanonischen JSON-Bytes einer Session und die [`SessionId`], die genau
//! diese Bytes ergeben. Beides zusammen, nie einzeln — ein Backend soll die ID
//! nicht selbst ausrechnen können und erst recht nicht eine andere nehmen.
//!
//! # Warum es dafür einen eigenen Typ gibt
//!
//! Die Schreibmethode des Traits *könnte* `(SessionId, &[u8])` nehmen. Dann wäre
//! sie aber der Weg an [`ContextStore::put`](crate::ContextStore::put) vorbei:
//! Jemand ruft sie mit selbstgebauten Bytes auf, und die fail-closed-Zusage aus
//! `minds-redact` ist ein Kommentar. Weil `SessionBytes` nur aus einer
//! [`RedactedSession`] entstehen kann, trägt der Wert den Beweis mit sich —
//! dieselbe Bauform, mit der `minds-redact` seine eigene Garantie durchsetzt.
//!
//! # Kanonisch heißt: von jedem reproduzierbar
//!
//! Geschrieben wird die Form nach RFC 8785 (siehe `minds_core::canonical`),
//! nicht das, was serde gerade ausgibt. Nur so kann ein Dritter — ein Auditor
//! mit Python, ein zweites Werkzeug in Go — die ID aus dem Inhalt nachrechnen.
//! Der Store hält also nicht „irgendein JSON dieser Session", sondern *das* JSON
//! dieser Session.

use std::fmt;

use minds_core::{SessionId, to_canonical_json};
use minds_redact::RedactedSession;

use crate::error::{Result, StoreError};

/// Eine gespeicherte oder zu speichernde Session: kanonische Bytes plus ihre ID.
///
/// Zu bekommen ausschließlich über [`SessionBytes::of`] — der Typ verbürgt sich
/// dafür, dass `id == blake3(bytes)` gilt und dass die Session die Redaction
/// durchlaufen hat.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionBytes {
    id: SessionId,
    bytes: Vec<u8>,
}

impl SessionBytes {
    /// Kanonisiert eine geprüfte Session und berechnet ihre ID.
    ///
    /// # Fehler
    ///
    /// - [`StoreError::Canonical`] — die Session ließ sich nicht kanonisieren.
    /// - [`StoreError::Unredacted`] — `redaction.applied` ist nicht gesetzt.
    ///   Über [`RedactedSession`] ist das nicht erreichbar; die Prüfung steht
    ///   trotzdem hier, weil das Flag im Envelope die Zusage ist, die
    ///   *gespeichert* wird. Was der Store schreibt, muss sich auch aus dem
    ///   Geschriebenen belegen lassen.
    pub fn of(session: &RedactedSession) -> Result<Self> {
        let envelope = session.session();
        let bytes = to_canonical_json(envelope)?;
        let id = SessionId::from_canonical_bytes(&bytes);

        if !envelope.redaction.applied {
            return Err(StoreError::Unredacted { id });
        }

        Ok(Self { id, bytes })
    }

    /// Die content-adressierte ID — der Hash genau dieser Bytes.
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Die kanonischen JSON-Bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Gibt die Bytes heraus, ohne zu kopieren.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Zeigt ID und Größe statt des vollständigen JSON.
///
/// Ein `{bytes:?}` in einer Fehlermeldung oder einem Log soll lesbar bleiben —
/// und der Session-Inhalt gehört nicht ungefragt in ein Log, auch redigiert
/// nicht.
impl fmt::Debug for SessionBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionBytes")
            .field("id", &self.id)
            .field("len", &self.bytes.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use minds_core::to_canonical_string;

    use super::*;
    use crate::fixture::redacted;

    #[test]
    fn bytes_are_the_canonical_form_of_the_session() {
        let session = redacted("Retry-Test reparieren");
        let stored = SessionBytes::of(&session).unwrap();

        assert_eq!(
            stored.as_bytes(),
            to_canonical_json(session.session()).unwrap()
        );
    }

    #[test]
    fn the_id_is_the_hash_of_exactly_those_bytes() {
        let session = redacted("Retry-Test reparieren");
        let stored = SessionBytes::of(&session).unwrap();

        assert_eq!(stored.id(), session.session().id().unwrap());
        assert_eq!(
            stored.id(),
            SessionId::from_canonical_bytes(stored.as_bytes())
        );
    }

    #[test]
    fn same_session_yields_the_same_bytes_and_id() {
        // Dedup fällt aus dieser Eigenschaft ab — hier ist sie explizit.
        let first = SessionBytes::of(&redacted("gleicher Inhalt")).unwrap();
        let second = SessionBytes::of(&redacted("gleicher Inhalt")).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn different_sessions_yield_different_ids() {
        let first = SessionBytes::of(&redacted("Fall A")).unwrap();
        let second = SessionBytes::of(&redacted("Fall B")).unwrap();

        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn stored_json_is_readable_text() {
        // Der Store hält JSON, keine Blackbox: Wer den Blob mit `git show`
        // ansieht, soll etwas erkennen können.
        let session = redacted("Retry-Test reparieren");
        let stored = SessionBytes::of(&session).unwrap();
        let text = String::from_utf8(stored.into_bytes()).unwrap();

        assert_eq!(text, to_canonical_string(session.session()).unwrap());
        assert!(text.starts_with("{\"agent\":"));
    }

    #[test]
    fn debug_shows_id_and_size_but_not_the_content() {
        let stored = SessionBytes::of(&redacted("streng geheim nicht")).unwrap();
        let text = format!("{stored:?}");

        assert!(text.contains(&stored.id().to_string()));
        assert!(!text.contains("streng geheim"));
    }
}
