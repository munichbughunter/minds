//! Das **Review** als versioniertes, content-adressiertes Git-Objekt (Schicht 3).
//!
//! Das Projektgedächtnis von GitLab — Reviews, Approvals, Diskussion — liegt in
//! Postgres, nicht im Repo. Migriert man weg, verliert man es. Radicle und
//! git-bug zeigen den anderen Weg: **als Git-Objekt.** Minds tut dasselbe für
//! Agent-Reviews: das Verdict zu einer Änderung liegt content-adressiert unter
//! `refs/minds/reviews/`, wandert mit dem Repo und überlebt jede Plattform.
//!
//! # An der Change-Id, nicht am Commit
//!
//! Ein Review hängt an einer [`Change-Id`](crate::ChangeId) (Schicht 2) oder
//! ersatzweise an einer [`SessionId`], **nicht** an einem Commit-Hash — sonst
//! verfiele es beim ersten Rebase. Genau dafür gibt es die Change-Id.
//!
//! # Kein I/O
//!
//! Dieses Modul ist Datenmodell + Content-Adressierung. Signiert (über die
//! ssh-sig-Naht aus Schicht 2) und gespeichert (`refs/minds/reviews/`) wird
//! außerhalb.

use serde::{Deserialize, Serialize};

use crate::canonical::{CanonError, to_canonical_json};
use crate::lineage::ContentHash;

/// Schema-Version des Review-Envelopes.
pub const REVIEW_SCHEMA_VERSION: u32 = 1;

/// Das Verdict eines Reviews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Decision {
    /// Angenommen.
    Approve,
    /// Abgelehnt.
    Reject,
    /// Nacharbeit nötig.
    NeedsWork,
}

impl Decision {
    /// Die kanonische Kurzform (für Trailer/Anzeige).
    pub fn as_str(&self) -> &'static str {
        match self {
            Decision::Approve => "approve",
            Decision::Reject => "reject",
            Decision::NeedsWork => "needs-work",
        }
    }
}

/// Woran ein Review hängt — bevorzugt an der stabilen Change-Id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "id")]
pub enum Subject {
    /// Eine stabile Änderungs-Identität (`I<40 hex>`).
    Change(String),
    /// Ersatzweise eine einzelne Session (`b3-<64 hex>`).
    Session(String),
}

impl Subject {
    /// Der Id-Text des Subjekts (ohne die Art).
    pub fn id(&self) -> &str {
        match self {
            Subject::Change(id) | Subject::Session(id) => id,
        }
    }
}

/// Ein Review: das signierbare, content-adressierte Verdict zu einer Änderung.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub schema_version: u32,
    pub subject: Subject,
    pub decision: Decision,
    /// Wer reviewt hat (Identität — dieselbe, unter der signiert wird).
    pub reviewer: String,
    #[serde(default)]
    pub summary: String,
    /// Zeitpunkt, RFC 3339 — vom Aufrufer, nie `now()` im Modell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

impl Review {
    /// Baut ein Review mit aktueller [`REVIEW_SCHEMA_VERSION`].
    pub fn new(
        subject: Subject,
        decision: Decision,
        reviewer: impl Into<String>,
        summary: impl Into<String>,
        at: Option<String>,
    ) -> Self {
        Self {
            schema_version: REVIEW_SCHEMA_VERSION,
            subject,
            decision,
            reviewer: reviewer.into(),
            summary: summary.into(),
            at,
        }
    }

    /// Die content-adressierte Id — der blake3 der kanonischen Form, in derselben
    /// Textform wie [`SessionId`](crate::SessionId)/[`ContentHash`].
    pub fn content_hash(&self) -> Result<ContentHash, CanonError> {
        let bytes = to_canonical_json(self)?;
        Ok(ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes()))
    }
}

/// Versions-/Domänen-Präfix des signierbaren Review-Payloads. Ändert sich das
/// Format, ändert sich die Version — eine alte Signatur verifiziert dann bewusst
/// nicht mehr.
pub const REVIEW_ATTESTATION_VERSION: &str = "minds-review-v1";

/// Der kanonische Text, über den ein Verdict signiert wird.
///
/// # Warum der Hash reicht
///
/// [`Review::content_hash`] ist der blake3 der kanonischen Form; Subjekt,
/// Verdict, Reviewer und Zusammenfassung stehen also *im* Hash. Ihn zu signieren
/// bindet das vollständige Review. Dass sie zusätzlich im Klartext danebenstehen,
/// ist für den Menschen, der die Zusage lesen soll — nicht für den Verifizierer.
///
/// Dieselbe Bauform wie [`attestation_payload`](crate::attestation_payload): Wer
/// eines von beiden prüfen kann, kann auch das andere. Und dieselbe Härtung
/// (#12): Felder mit Zeilen- oder Steuerzeichen erzeugen keinen Payload,
/// sondern einen Fehler — sonst wäre über `reviewer` oder die Subjekt-Id eine
/// zweite `decision=`-Zeile fälschbar. Die Zeilenzahl (5) ist eine Invariante.
///
/// Rein und deterministisch. Signiert und verifiziert wird außerhalb (die CLI
/// ruft `ssh-keygen -Y sign/verify`); `minds-core` hat kein I/O.
pub fn review_payload(hash: &ContentHash, review: &Review) -> Result<String, crate::PayloadError> {
    let (kind, id) = match &review.subject {
        Subject::Change(id) => ("change", id),
        Subject::Session(id) => ("session", id),
    };
    crate::attest::check_single_line("subject.id", id)?;
    crate::attest::check_single_line("reviewer", &review.reviewer)?;
    Ok(format!(
        "{REVIEW_ATTESTATION_VERSION}\n\
         review={hash}\n\
         subject={kind}:{id}\n\
         decision={}\n\
         reviewer={}\n",
        review.decision.as_str(),
        review.reviewer,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review() -> Review {
        Review::new(
            Subject::Change(format!("I{}", "ab".repeat(20))),
            Decision::Approve,
            "anna@example.org",
            "sieht gut aus, Backoff ist jetzt korrekt",
            Some("2026-07-28T10:00:00Z".into()),
        )
    }

    #[test]
    fn new_sets_the_schema_version() {
        assert_eq!(review().schema_version, REVIEW_SCHEMA_VERSION);
    }

    #[test]
    fn json_roundtrips() {
        let r = review();
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Review>(&json).unwrap(), r);
    }

    #[test]
    fn subject_serializes_tagged() {
        let json = serde_json::to_string(&review().subject).unwrap();
        assert!(json.contains(r#""type":"change""#), "{json}");
        assert!(json.contains(&format!(r#""id":"I{}""#, "ab".repeat(20))));
    }

    #[test]
    fn decision_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&Decision::NeedsWork).unwrap(),
            "\"needs-work\""
        );
        assert_eq!(Decision::NeedsWork.as_str(), "needs-work");
    }

    #[test]
    fn content_hash_is_stable_and_content_addressed() {
        // Gleiches Review ⇒ gleicher Hash; ein anderes Verdict ⇒ anderer Hash.
        assert_eq!(
            review().content_hash().unwrap(),
            review().content_hash().unwrap()
        );

        let mut rejected = review();
        rejected.decision = Decision::Reject;
        assert_ne!(
            review().content_hash().unwrap(),
            rejected.content_hash().unwrap()
        );
    }

    #[test]
    fn the_hash_has_the_b3_textform() {
        assert!(review().content_hash().unwrap().as_str().starts_with("b3-"));
    }

    // --- Der signierbare Payload ---------------------------------------------

    #[test]
    fn the_payload_binds_the_hash_and_names_the_verdict() {
        let review = review();
        let hash = review.content_hash().unwrap();
        let payload = review_payload(&hash, &review).unwrap();

        assert!(payload.starts_with("minds-review-v1\n"));
        assert!(payload.contains(&format!("review={hash}")));
        assert!(payload.contains(&format!("subject=change:I{}", "ab".repeat(20))));
        assert!(payload.contains("decision=approve"));
        assert!(payload.contains("reviewer=anna@example.org"));
    }

    #[test]
    fn a_different_verdict_changes_the_payload() {
        // Der Kern: Eine Signatur über das eine Verdict darf nicht auf ein
        // anderes passen. Beides ändert schon den Hash — und der steht im
        // Payload, also fällt es doppelt auf.
        let approved = review();
        let mut rejected = review();
        rejected.decision = Decision::Reject;

        assert_ne!(
            review_payload(&approved.content_hash().unwrap(), &approved).unwrap(),
            review_payload(&rejected.content_hash().unwrap(), &rejected).unwrap()
        );
    }

    #[test]
    fn the_payload_is_deterministic() {
        let hash = review().content_hash().unwrap();
        assert_eq!(
            review_payload(&hash, &review()).unwrap(),
            review_payload(&hash, &review()).unwrap()
        );
    }

    #[test]
    fn a_session_subject_is_told_apart_from_a_change() {
        let mut on_session = review();
        on_session.subject = Subject::Session(format!("b3-{}", "cd".repeat(32)));
        let payload = review_payload(&on_session.content_hash().unwrap(), &on_session).unwrap();
        assert!(payload.contains("subject=session:b3-"), "{payload}");
    }

    // --- Fail-closed gegen Zeilen-Fälschung (#12) ---------------------------

    #[test]
    fn the_line_count_is_an_invariant() {
        let review = review();
        let payload = review_payload(&review.content_hash().unwrap(), &review).unwrap();
        assert_eq!(payload.lines().count(), 5, "{payload:?}");
        assert!(payload.ends_with('\n'), "{payload:?}");
    }

    #[test]
    fn a_newline_in_the_reviewer_yields_no_payload() {
        // Der Angriff aus #12: eine zweite decision=-Zeile über den Reviewer.
        let mut forged = review();
        forged.reviewer = "anna@example.org\ndecision=approve".into();
        let hash = forged.content_hash().unwrap();
        let err = review_payload(&hash, &forged).unwrap_err();
        assert!(err.to_string().contains("reviewer"), "{err}");
        // Der Fehler benennt das Feld, zitiert aber nie den Wert.
        assert!(!err.to_string().contains("anna"), "{err}");
    }

    #[test]
    fn a_newline_in_the_subject_id_yields_no_payload() {
        // Subject-Ids kommen auch über den Webhook — untrusted.
        let mut forged = review();
        forged.subject = Subject::Change("I123\ndecision=approve".into());
        let hash = forged.content_hash().unwrap();
        let err = review_payload(&hash, &forged).unwrap_err();
        assert!(err.to_string().contains("subject"), "{err}");
    }
}
