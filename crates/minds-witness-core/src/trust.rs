//! Trust-Snapshot (S1 v1.6, Spec S3 §4.3): genau die Konfiguration, mit der
//! der Server einen Push geprüft hat. Der Hash seiner kanonischen Bytes steht
//! signiert in jeder Quittung mit Schema `/2`.
//!
//! Die Byte-Form ist eingefroren: eigene Typen mit alphabetisch deklarierten
//! Feldern (RFC 8785 für dieses Schema), unabhängig von `Policy`. Ändert sich
//! `Policy`, bleibt `minds.witness.trust/1` lesbar. Der Snapshot enthält
//! keine Geheimnisse: Statt `pusher_salt` trägt er nur dessen Hash.

use serde::{Deserialize, Serialize};

use crate::TRUST_SCHEMA;
use crate::ids::HashStr;
use crate::policy::{Mode, Policy, PusherMode};
use crate::receipt::{VerifyError, canonical_bytes};

/// Die Policy im Snapshot, eingefroren für `minds.witness.trust/1`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustPolicy {
    /// `Policy::coverage_threshold_percent`.
    pub coverage_threshold_percent: u8,
    /// `Policy::enforce_refs`.
    pub enforce_refs: Vec<String>,
    /// `Policy::evaluate_refs`.
    pub evaluate_refs: Vec<String>,
    /// `Policy::max_commits_per_push`.
    pub max_commits_per_push: u32,
    /// `Policy::mode`.
    pub mode: Mode,
    /// `Policy::pusher_mode`.
    pub pusher_mode: PusherMode,
    /// Hash von `Policy::pusher_salt` bei `PusherMode::Hashed`, sonst fehlt
    /// das Feld. Das Salz selbst wird nie veröffentlicht: Mit ihm ließe sich
    /// die Pseudonymisierung der Pusher umkehren.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pusher_salt_hash: Option<HashStr>,
    /// `Policy::uncovered_cap`.
    pub uncovered_cap: u32,
    /// `Policy::version`.
    pub version: u32,
}

impl TrustPolicy {
    /// Die Snapshot-Form einer Policy.
    pub fn of(policy: &Policy) -> Self {
        Self {
            coverage_threshold_percent: policy.coverage_threshold_percent,
            enforce_refs: policy.enforce_refs.clone(),
            evaluate_refs: policy.evaluate_refs.clone(),
            max_commits_per_push: policy.max_commits_per_push,
            mode: policy.mode.clone(),
            pusher_mode: policy.pusher_mode.clone(),
            pusher_salt_hash: (policy.pusher_mode == PusherMode::Hashed)
                .then(|| HashStr::of_bytes(policy.pusher_salt.as_bytes())),
            uncovered_cap: policy.uncovered_cap,
            version: policy.version,
        }
    }

    /// Eine `Policy` für die Nachrechnung von Abdeckung und Verdict. Den
    /// Pusher rechnet niemand nach; sie trägt daher `PusherMode::Plain` und
    /// kein Salz.
    pub fn evaluation_policy(&self) -> Policy {
        Policy {
            mode: self.mode.clone(),
            evaluate_refs: self.evaluate_refs.clone(),
            enforce_refs: self.enforce_refs.clone(),
            max_commits_per_push: self.max_commits_per_push,
            uncovered_cap: self.uncovered_cap,
            coverage_threshold_percent: self.coverage_threshold_percent,
            pusher_mode: PusherMode::Plain,
            pusher_salt: String::new(),
            version: self.version,
        }
    }

    fn validate(&self) -> Result<(), String> {
        self.evaluation_policy()
            .validate()
            .map_err(|e| e.to_string())?;
        match (&self.pusher_mode, &self.pusher_salt_hash) {
            (PusherMode::Hashed, Some(_)) | (PusherMode::Plain, None) => Ok(()),
            _ => Err("pusher_salt_hash passt nicht zu pusher_mode".to_owned()),
        }
    }
}

/// Der Trust-Snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustSnapshot {
    /// Erlaubte Witness-Principals, streng aufsteigend sortiert.
    pub allowed_witness_principals: Vec<String>,
    /// Die verwendete Policy (eingefrorene Form).
    pub policy: TrustPolicy,
    /// Projekt-Identität.
    pub project: String,
    /// Ob nur getrennte Witness-Principals anerkannt werden.
    pub require_separated_witness: bool,
    /// Immer `TRUST_SCHEMA`.
    pub schema: String,
    /// Bytes des Signer-Snapshots, den die Signaturprüfung verwendete.
    pub signers: String,
}

impl TrustSnapshot {
    /// Ein Snapshot aus der geprüften Konfiguration; die Principals werden
    /// sortiert und entdoppelt.
    pub fn new(
        mut allowed_witness_principals: Vec<String>,
        policy: &Policy,
        project: &str,
        require_separated_witness: bool,
        signers: &str,
    ) -> Self {
        allowed_witness_principals.sort();
        allowed_witness_principals.dedup();
        Self {
            allowed_witness_principals,
            policy: TrustPolicy::of(policy),
            project: project.to_owned(),
            require_separated_witness,
            schema: TRUST_SCHEMA.to_owned(),
            signers: signers.to_owned(),
        }
    }

    /// Kanonische Bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        canonical_bytes(self)
    }

    /// Hash der kanonischen Bytes; der Wert im Feld `trust` der Quittung.
    pub fn hash(&self) -> HashStr {
        HashStr::of_bytes(&self.to_bytes())
    }

    /// Liest einen Snapshot streng: Form, Schema, streng sortierte
    /// Principals, gültige Policy und kanonische Bytes. Fehlertexte sind
    /// feste Sätze; Inhalte des Snapshots erscheinen darin nie.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, VerifyError> {
        let bad = |why: &str| VerifyError::TrustSnapshotMalformed(why.to_owned());
        let snapshot: Self =
            serde_json::from_slice(bytes).map_err(|_| bad("Snapshot nicht lesbar"))?;
        if snapshot.schema != TRUST_SCHEMA {
            return Err(bad("unbekanntes Snapshot-Schema"));
        }
        if !snapshot
            .allowed_witness_principals
            .windows(2)
            .all(|w| w[0] < w[1])
        {
            return Err(bad("Principals nicht streng sortiert"));
        }
        if snapshot.policy.validate().is_err() {
            return Err(bad("Policy ungültig"));
        }
        if snapshot.to_bytes() != bytes {
            return Err(bad("Snapshot nicht kanonisch"));
        }
        Ok(snapshot)
    }
}
