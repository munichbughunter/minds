//! Der **Kommentar** als append-only Operation (Schicht 3, R2).
//!
//! Ein Verdict ([`Review`](crate::Review)) ist die Entscheidung. Die Diskussion,
//! die dahin führt, ist das andere — und sie hat eine Anforderung, die das
//! Verdict nicht hat: **Sie muss mergebar sein.** Zwei Reviewer arbeiten offline
//! am selben Change, beide schreiben, und niemand soll danach einen Konflikt
//! auflösen müssen, der keiner ist.
//!
//! # Warum append-only und content-adressiert
//!
//! Ein Thread ist hier **kein Dokument, das fortgeschrieben wird**, sondern ein
//! Log aus unveränderlichen Operationen. Jeder Kommentar ist ein eigener,
//! content-adressierter Eintrag; sein Hash ist sein Name und sein Platz. Daraus
//! folgt die Eigenschaft, um die es geht:
//!
//! - Zwei Logs zu vereinigen ist eine **Mengenvereinigung**. Sie ist kommutativ
//!   (egal, wer zuerst mergt) und idempotent (egal, wie oft).
//! - Ein Konflikt im Sinne von „zwei Werte an einer Stelle" kann nicht
//!   entstehen: Gleicher Pfad heißt gleicher Inhalt.
//!
//! Das ist das Muster von git-bug, und es ist der Grund, warum ein Review-Thread
//! in Git leben kann, ohne dass jemand einen Merge-Algorithmus dafür schreibt.
//! Die Vereinigung selbst steht in `minds-store`; hier steht nur, was eine
//! Operation ist.
//!
//! # Der Anker
//!
//! Ein Kommentar hängt an einer Stelle: an `datei:zeile`, an einem **Turn** der
//! Session (dem Prompt oder der Antwort, um die es geht) — oder an gar nichts,
//! dann gilt er dem Change als Ganzem. Der Anker ist Teil des Inhalts und damit
//! Teil des Hashes: Derselbe Text an zwei Stellen sind zwei Kommentare, und das
//! ist richtig so.
//!
//! # Kein I/O
//!
//! Datenmodell und Content-Adressierung. Gespeichert (`refs/minds/reviews`) und
//! vereinigt wird außerhalb.

use serde::{Deserialize, Serialize};

use crate::canonical::{CanonError, to_canonical_json};
use crate::lineage::ContentHash;
use crate::review::Subject;

/// Schema-Version des Kommentar-Envelopes.
pub const COMMENT_SCHEMA_VERSION: u32 = 1;

/// Woran ein Kommentar hängt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Anchor {
    /// An einer Zeile einer Datei.
    File {
        /// Pfad, relativ zur Repo-Wurzel.
        path: String,
        /// Zeilennummer, 1-basiert.
        line: u32,
    },
    /// An einem Turn der Session — dem Prompt oder der Antwort, um die es geht.
    Turn {
        /// Index des Turns, 0-basiert (wie im Session-Envelope).
        index: u32,
    },
    /// An nichts Bestimmtem: Der Kommentar gilt dem Change als Ganzem.
    Whole,
}

impl Anchor {
    /// Die kurze Textform für die Anzeige.
    pub fn as_text(&self) -> String {
        match self {
            Anchor::File { path, line } => format!("{path}:{line}"),
            Anchor::Turn { index } => format!("turn:{index}"),
            Anchor::Whole => "gesamt".to_string(),
        }
    }
}

/// Ein Kommentar: eine unveränderliche Operation im Review-Thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub schema_version: u32,
    /// Der Change (oder ersatzweise die Session), um den es geht.
    pub subject: Subject,
    /// Wo im Change der Kommentar hängt.
    pub anchor: Anchor,
    /// Wer geschrieben hat — dieselbe Identität, unter der signiert wird.
    pub author: String,
    /// Der Text.
    pub body: String,
    /// Zeitpunkt, RFC 3339 — vom Aufrufer, nie `now()` im Modell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Der Kommentar, auf den geantwortet wird (dessen Content-Hash).
    ///
    /// Damit wird aus dem flachen Log ein Baum, ohne dass der Log seine
    /// Vereinigbarkeit verliert: Die Antwort nennt ihr Ziel, das Ziel weiß
    /// nichts von der Antwort. Zeigt sie auf etwas, das hier (noch) nicht liegt,
    /// ist das kein Fehler — der andere Log bringt es womöglich mit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
}

impl Comment {
    /// Baut einen Kommentar mit aktueller [`COMMENT_SCHEMA_VERSION`].
    pub fn new(
        subject: Subject,
        anchor: Anchor,
        author: impl Into<String>,
        body: impl Into<String>,
        at: Option<String>,
    ) -> Self {
        Self {
            schema_version: COMMENT_SCHEMA_VERSION,
            subject,
            anchor,
            author: author.into(),
            body: body.into(),
            at,
            in_reply_to: None,
        }
    }

    /// Macht daraus eine Antwort auf `parent`.
    pub fn in_reply_to(self, parent: &ContentHash) -> Self {
        Self {
            in_reply_to: Some(parent.to_string()),
            ..self
        }
    }

    /// Die content-adressierte Id — der blake3 der kanonischen Form.
    pub fn content_hash(&self) -> Result<ContentHash, CanonError> {
        let bytes = to_canonical_json(self)?;
        Ok(ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes()))
    }
}

/// Der Sortierschlüssel eines Kommentars: Zeit, dann Hash.
///
/// Der Log ist eine **Menge** — er trägt keine Reihenfolge. Angezeigt werden
/// muss er trotzdem in einer, und die darf nicht davon abhängen, in welcher
/// Reihenfolge zwei Logs zusammengefunden haben. Zeit ordnet, was ein Mensch
/// ordnen würde; der Hash entscheidet den Rest, damit die Ordnung **total** ist
/// und auf jeder Maschine dieselbe.
pub fn order_key(comment: &Comment) -> (String, String) {
    (
        comment.at.clone().unwrap_or_default(),
        comment
            .content_hash()
            .map(|hash| hash.to_string())
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject() -> Subject {
        Subject::Change(format!("I{}", "ab".repeat(20)))
    }

    fn comment() -> Comment {
        Comment::new(
            subject(),
            Anchor::File {
                path: "src/retry.rs".into(),
                line: 42,
            },
            "anna@example.org",
            "Der Backoff verdoppelt hier zu früh.",
            Some("2026-07-28T10:00:00Z".into()),
        )
    }

    #[test]
    fn new_sets_the_schema_version() {
        assert_eq!(comment().schema_version, COMMENT_SCHEMA_VERSION);
    }

    #[test]
    fn json_roundtrips() {
        let c = comment();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Comment>(&json).unwrap(), c);
    }

    #[test]
    fn the_anchor_is_part_of_the_identity() {
        // Derselbe Text an zwei Stellen sind zwei Kommentare — sonst
        // verschluckte der Store den zweiten als Dublette.
        let mut elsewhere = comment();
        elsewhere.anchor = Anchor::Turn { index: 3 };
        assert_ne!(
            comment().content_hash().unwrap(),
            elsewhere.content_hash().unwrap()
        );
    }

    #[test]
    fn the_hash_is_stable() {
        assert_eq!(
            comment().content_hash().unwrap(),
            comment().content_hash().unwrap()
        );
        assert!(
            comment()
                .content_hash()
                .unwrap()
                .as_str()
                .starts_with("b3-")
        );
    }

    #[test]
    fn a_reply_names_its_parent_and_gets_its_own_identity() {
        let parent = comment();
        let hash = parent.content_hash().unwrap();
        let reply = Comment::new(
            subject(),
            Anchor::Whole,
            "bea@example.org",
            "Stimmt, ich ziehe das gleich.",
            Some("2026-07-28T10:05:00Z".into()),
        )
        .in_reply_to(&hash);

        assert_eq!(reply.in_reply_to.as_deref(), Some(hash.as_str()));
        assert_ne!(reply.content_hash().unwrap(), hash);
    }

    #[test]
    fn anchors_render_readably() {
        assert_eq!(comment().anchor.as_text(), "src/retry.rs:42");
        assert_eq!(Anchor::Turn { index: 3 }.as_text(), "turn:3");
        assert_eq!(Anchor::Whole.as_text(), "gesamt");
    }

    #[test]
    fn the_order_is_total_and_independent_of_arrival() {
        // Zwei Kommentare zur selben Zeit: Der Hash entscheidet — und zwar auf
        // jeder Maschine gleich. Ohne diesen zweiten Schlüssel hinge die
        // Anzeige davon ab, wer zuerst gemergt hat.
        let mut a = comment();
        let mut b = comment();
        a.body = "erster".into();
        b.body = "zweiter".into();

        let mut forward = vec![a.clone(), b.clone()];
        let mut backward = vec![b, a];
        forward.sort_by_key(order_key);
        backward.sort_by_key(order_key);

        assert_eq!(forward, backward);
    }
}
