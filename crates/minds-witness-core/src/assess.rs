//! Die Regel, nach der die Pre-Phase für **einen** Commit Seal-Zustand,
//! Abdeckung und Urteil bildet (Abschnitt 7, Schritte 6 und 7). Ausgelagert,
//! damit ein Nachrechner (`minds verify --receipt`, Spec S3 §6.3) dieselbe
//! Regel verwendet wie der Server — keine zweite Fassung, die abdriften
//! könnte. Rein: kein I/O, keine Ereignisse.

use std::collections::BTreeSet;

use crate::finding::{Coverage, CoverageStatus, PolicyVerdict, SealState, SealStatus};
use crate::ids::{CommitId, HashStr};
use crate::policy::Policy;
use crate::traits::{SealBlob, SealSignature, SealSummary};

/// Was die Seal-Suche für einen Commit ergab.
#[derive(Clone, Debug)]
pub enum SealLookup {
    /// Kein Seal.
    Absent,
    /// Seal gefunden und vom Inspektor gedeutet.
    Inspected(SealBlob, SealSummary),
    /// Seal gefunden, aber nicht lesbar (`SealError::Malformed`).
    Malformed(SealBlob),
}

/// Seal-Angaben, Abdeckung und Urteil eines Commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assessment {
    /// Seal-Angaben.
    pub seal: SealStatus,
    /// Abdeckung.
    pub coverage: Coverage,
    /// Urteil der Policy.
    pub result: PolicyVerdict,
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Abdeckung aus geänderten Pfaden und (optional) den Pfaden des Seals.
fn coverage(
    status: CoverageStatus,
    paths: &BTreeSet<String>,
    touched: Option<&BTreeSet<String>>,
    cap: u32,
) -> Coverage {
    let uncovered_all: Vec<&String> = paths
        .iter()
        .filter(|p| touched.is_none_or(|t| !t.contains(*p)))
        .collect();
    let cap = usize::try_from(cap).unwrap_or(usize::MAX);
    Coverage {
        paths_covered: len_u32(paths.len() - uncovered_all.len()),
        paths_in_diff: len_u32(paths.len()),
        status,
        uncovered_truncated: uncovered_all.len() > cap,
        uncovered: uncovered_all.into_iter().take(cap).cloned().collect(),
    }
}

/// Verdict, erste zutreffende Regel gewinnt (Abschnitt 7, Schritt 7).
fn verdict(seal: &SealStatus, coverage: &Coverage, threshold: u8) -> PolicyVerdict {
    if coverage.status == CoverageStatus::MergeCommit {
        return PolicyVerdict::Pass;
    }
    if seal.status != SealState::Valid {
        return PolicyVerdict::Fail;
    }
    if coverage.paths_in_diff == 0 {
        return PolicyVerdict::Pass;
    }
    let percent = u64::from(coverage.paths_covered) * 100 / u64::from(coverage.paths_in_diff);
    if percent >= u64::from(threshold) {
        PolicyVerdict::Pass
    } else {
        PolicyVerdict::Fail
    }
}

/// Abdeckung und Urteil zu einem **gegebenen** Seal-Zustand: `status` sagt,
/// wie die Abdeckung entsteht, `touched` sind die belegten Pfade (bei
/// `NoSeal` ohne Bedeutung). Für `MergeCommit` sind `paths` leer.
pub fn judge(
    seal: SealStatus,
    status: CoverageStatus,
    paths: &BTreeSet<String>,
    touched: Option<&BTreeSet<String>>,
    policy: &Policy,
) -> Assessment {
    let touched = match status {
        CoverageStatus::Computed => touched,
        CoverageStatus::NoSeal | CoverageStatus::MergeCommit => None,
    };
    let coverage = coverage(status, paths, touched, policy.uncovered_cap);
    let result = verdict(&seal, &coverage, policy.coverage_threshold_percent);
    Assessment {
        seal,
        coverage,
        result,
    }
}

/// Die vollständige Regel für Commit `c` im Repository `repo`: Ein Merge
/// (mindestens zwei Eltern) ohne eigene Änderungen braucht kein Seal —
/// `lookup` wird dann nicht aufgerufen —, sonst bestimmt das Ergebnis der
/// Seal-Suche Zustand und Abdeckung.
pub fn assess<E>(
    c: &CommitId,
    repo: &str,
    parents: usize,
    paths: &BTreeSet<String>,
    lookup: impl FnOnce() -> Result<SealLookup, E>,
    policy: &Policy,
) -> Result<Assessment, E> {
    let absent = SealStatus {
        hash: None,
        ref_name: None,
        status: SealState::Absent,
    };
    if parents >= 2 && paths.is_empty() {
        return Ok(judge(
            absent,
            CoverageStatus::MergeCommit,
            paths,
            None,
            policy,
        ));
    }
    Ok(match lookup()? {
        SealLookup::Absent => judge(absent, CoverageStatus::NoSeal, paths, None, policy),
        SealLookup::Inspected(blob, summary) => {
            let bound_ok =
                summary.bound_commit == *c && summary.bound_repo.as_ref().is_none_or(|r| r == repo);
            let status = if !bound_ok {
                SealState::Mismatched
            } else {
                match summary.signature {
                    SealSignature::Valid => SealState::Valid,
                    SealSignature::Invalid => SealState::Invalid,
                    SealSignature::Unsigned => SealState::Unsigned,
                }
            };
            judge(
                SealStatus {
                    hash: Some(summary.hash),
                    ref_name: Some(blob.ref_name),
                    status,
                },
                CoverageStatus::Computed,
                paths,
                Some(&summary.touched_paths),
                policy,
            )
        }
        SealLookup::Malformed(blob) => judge(
            SealStatus {
                hash: Some(HashStr::of_bytes(&blob.bytes)),
                ref_name: Some(blob.ref_name),
                status: SealState::Invalid,
            },
            CoverageStatus::NoSeal,
            paths,
            None,
            policy,
        ),
    })
}
