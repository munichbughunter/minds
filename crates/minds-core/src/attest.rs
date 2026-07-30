//! Der kanonische, signierbare Text einer Attribution.
//!
//! „Wer hat diese Zeilen geschrieben — Mensch oder Maschine, mit welchem Modell?"
//! beantwortet die [`Attribution`](crate::Attribution) *als Behauptung*. Eine
//! Signatur über genau diesen Text macht daraus einen **Nachweis**: Ein
//! Schlüsselinhaber steht dafür ein, dass diese Session (dieser exakte Inhalt,
//! über ihre [`SessionId`]) mit diesem Agenten und Modell entstand.
//!
//! # Warum das reicht
//!
//! Die `SessionId` ist der blake3-Hash der kanonischen Session; Agent und Modell
//! stehen *im* Envelope und damit *im* Hash. Den Payload zu signieren bindet also
//! den vollständigen Session-Inhalt — Agent und Modell stehen zusätzlich im
//! Klartext, damit ein Mensch die Zusage lesen kann, nicht nur ein Verifizierer.
//!
//! Rein und deterministisch: gleiche Session ⇒ byte-gleicher Payload. Signiert
//! und verifiziert wird außerhalb (die CLI ruft `ssh-keygen -Y sign/verify`);
//! `minds-core` hat kein I/O.

use crate::{Session, SessionId};

/// Versions-/Domänen-Präfix des Payloads. Ändert sich das Format, ändert sich
/// die Version — eine alte Signatur verifiziert dann bewusst nicht mehr.
pub const ATTESTATION_VERSION: &str = "minds-attestation-v1";

/// Der kanonische Text, über den signiert wird.
pub fn attestation_payload(id: SessionId, session: &Session) -> String {
    format!(
        "{ATTESTATION_VERSION}\n\
         session={id}\n\
         agent={} {}\n\
         model={}/{}\n",
        session.agent.name, session.agent.version, session.model.provider, session.model.id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Agent, Intent, Model};

    fn sid() -> SessionId {
        format!("b3-{}", "a".repeat(64)).parse().unwrap()
    }

    fn session() -> Session {
        Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.4.2".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent::default(),
        )
    }

    #[test]
    fn payload_binds_session_agent_and_model() {
        let p = attestation_payload(sid(), &session());
        assert!(p.starts_with("minds-attestation-v1\n"));
        assert!(p.contains(&format!("session=b3-{}", "a".repeat(64))));
        assert!(p.contains("agent=claude-code 1.4.2"));
        assert!(p.contains("model=anthropic/claude-opus-4"));
    }

    #[test]
    fn payload_is_deterministic() {
        assert_eq!(
            attestation_payload(sid(), &session()),
            attestation_payload(sid(), &session())
        );
    }

    #[test]
    fn a_different_agent_changes_the_payload() {
        let mut other = session();
        other.agent.name = "codex".into();
        assert_ne!(
            attestation_payload(sid(), &session()),
            attestation_payload(sid(), &other)
        );
    }
}
