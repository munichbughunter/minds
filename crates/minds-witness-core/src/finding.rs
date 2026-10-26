//! Befund der Pre-Phase für genau einen Commit innerhalb genau eines
//! `RefUpdate`. Alle Structs deklarieren ihre Felder alphabetisch nach dem
//! serialisierten Namen, weil daraus die kanonische Form der Quittung
//! entsteht. Das Modul enthält nur Daten; wie ein Befund entsteht, steht in
//! der Pre-Phase.

use serde::{Deserialize, Serialize};

use crate::ids::{CommitId, HashStr};
use crate::policy::Mode;
use crate::push::{Pusher, ServerInfo};

/// Zustand des Seals eines Commits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealState {
    /// Gültig signiert und an Commit (und ggf. Repo) gebunden.
    Valid,
    /// Signatur ungültig oder Seal nicht lesbar.
    Invalid,
    /// Ohne Signatur.
    Unsigned,
    /// Gültiges Seal, aber an einen anderen Commit oder ein anderes Repo gebunden.
    Mismatched,
    /// Kein Seal vorhanden.
    Absent,
}

impl SealState {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Unsigned => "unsigned",
            Self::Mismatched => "mismatched",
            Self::Absent => "absent",
        }
    }
}

/// Seal-Angaben im Befund.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealStatus {
    /// Hash des Seal-Inhalts, sofern ein Seal gefunden wurde.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<HashStr>,
    /// Ref des Seals, sofern ein Seal gefunden wurde.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_name: Option<String>,
    /// Zustand.
    pub status: SealState,
}

/// Wie die Abdeckung zustande kam.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// Gegen die Pfade eines Seals berechnet.
    Computed,
    /// Kein (lesbares) Seal; nichts abgedeckt.
    NoSeal,
    /// Merge ohne mergeeigene Änderungen.
    MergeCommit,
}

/// Abdeckung der geänderten Pfade durch das Seal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    /// Anzahl abgedeckter Pfade.
    pub paths_covered: u32,
    /// Anzahl geänderter Pfade.
    pub paths_in_diff: u32,
    /// Herkunft der Zahlen.
    pub status: CoverageStatus,
    /// Nicht abgedeckte Pfade, aufsteigend, höchstens `uncovered_cap` Stück.
    pub uncovered: Vec<String>,
    /// `true`, wenn `uncovered` gekürzt wurde.
    pub uncovered_truncated: bool,
}

/// Urteil der Policy über einen Befund.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyVerdict {
    /// Bestanden.
    Pass,
    /// Nicht bestanden.
    Fail,
}

impl PolicyVerdict {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

/// Policy-Ergebnis im Befund.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyResult {
    /// Modus der Policy zum Prüfzeitpunkt.
    pub mode: Mode,
    /// Urteil.
    pub result: PolicyVerdict,
    /// Version der Policy.
    pub version: u32,
}

/// Angaben zum Ref-Update, in dem der Commit geprüft wurde.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushInfo {
    /// Neuer Stand des Refs.
    pub new: CommitId,
    /// Anzahl der von `new_commits` gelieferten Commits.
    pub new_commits: u32,
    /// Alter Stand des Refs.
    pub old: CommitId,
    /// Ref-Name.
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// `true`, wenn nicht alle neuen Commits bewertet wurden.
    pub truncated: bool,
}

/// Ergebnis der Pre-Phase für genau einen Commit innerhalb genau eines `RefUpdate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Geprüfter Commit.
    pub commit: CommitId,
    /// Abdeckung.
    pub coverage: Coverage,
    /// Policy-Ergebnis.
    pub policy: PolicyResult,
    /// Ref-Update.
    pub push: PushInfo,
    /// Pusher (ggf. gehasht).
    pub pusher: Pusher,
    /// Repository-Kennung.
    pub repo: String,
    /// Seal-Angaben.
    pub seal: SealStatus,
    /// Serverangaben.
    pub server: ServerInfo,
}
