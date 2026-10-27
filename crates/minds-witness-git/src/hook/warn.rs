//! Sichere Warnungsausgabe auf dem Push-Kanal (Spec S2 v0.4.1 §11,
//! ADR-PRODUCT-001). Eine Funktion von `PreOutcome` und `PostOutcome`
//! allein: feste Sätze, Ref-Namen (escaped und begrenzt), gekürzte
//! Commit-Ids, Zähler, Prozentwerte, Schwelle und Zustandswörter; nie
//! Pfade, Objektinhalte, Fehlertexte oder Panic-Payloads. Die Ausgabe
//! behauptet nie eine erfolgreiche Git-Annahme.

use minds_witness_core::{
    CoverageStatus, Finding, PolicyVerdict, PostOutcome, PreOutcome, SealState,
};

use crate::guard::shown;

/// Höchstzahl der Finding-Zeilen.
pub const MAX_LINES: usize = 20;

/// Abgeleiteter Produktzustand (ADR-PRODUCT-001), vorläufig für S2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Policy erfüllt (nur als Hinweis mit offener Restmenge).
    Verified,
    /// Gültige Evidence, Coverage unter der Schwelle.
    Partial,
    /// Keine ausreichende Evidence.
    Unverified,
    /// Vorgelegte Evidence oder Bindung ungültig.
    Invalid,
    /// Prüfung aus technischen Gründen nicht abgeschlossen.
    Unknown,
}

impl State {
    /// Das Wort.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Verified => "Verified",
            Self::Partial => "Partial",
            Self::Unverified => "Unverified",
            Self::Invalid => "Invalid",
            Self::Unknown => "Unknown",
        }
    }
}

/// Zustand eines Findings nach der Tabelle in §11.
pub fn state(f: &Finding) -> State {
    match f.seal.status {
        SealState::Invalid | SealState::Unsigned | SealState::Mismatched => State::Invalid,
        _ if f.seal.status == SealState::Absent || f.push.truncated => State::Unverified,
        _ if f.policy.result == PolicyVerdict::Fail && f.coverage.paths_covered > 0 => {
            State::Partial
        }
        _ if f.policy.result == PolicyVerdict::Fail => State::Unverified,
        _ => State::Verified,
    }
}

/// Prozentanzeige mit einer Nachkommastelle (§11, v0.4.1): kaufmännisch
/// gerundet, höchstens 99,9 bei unvollständiger Abdeckung, und bei `fail`
/// nie auf oder über die Schwelle gerundet.
pub fn percent(covered: u32, total: u32, fail: bool, threshold: u8) -> Option<String> {
    if total == 0 {
        return None;
    }
    let (c, d) = (u64::from(covered), u64::from(total));
    let mut tenths = (2000 * c + d) / (2 * d);
    if fail && tenths >= 10 * u64::from(threshold) {
        tenths = 1000 * c / d;
    }
    if c < d {
        tenths = tenths.min(999);
    }
    Some(format!("{},{}", tenths / 10, tenths % 10))
}

fn counts(f: &Finding, threshold: u8) -> String {
    let c = &f.coverage;
    let fail = f.policy.result == PolicyVerdict::Fail;
    match percent(c.paths_covered, c.paths_in_diff, fail, threshold) {
        Some(p) => format!(
            "{}/{} Dateien nachgewiesen ({p} %)",
            c.paths_covered, c.paths_in_diff
        ),
        None => format!(
            "{}/{} Dateien nachgewiesen",
            c.paths_covered, c.paths_in_diff
        ),
    }
}

fn unknown(ref_name: &str) -> String {
    format!(
        "minds-witness: Warnung: {}: Prüfung nicht abgeschlossen (Unknown)",
        shown(ref_name)
    )
}

/// Die Zeilen für stderr nach `Accept`. `evaluated` sind die bewerteten
/// Refs des Pushes (für die `Unknown`-Zeilen nach einem Panic).
/// `Unknown`-Zeilen stehen vorn und fallen nie unter den Deckel.
pub fn lines(
    pre: &PreOutcome,
    post: Option<&PostOutcome>,
    threshold: u8,
    limit: u32,
    evaluated: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    let mut unknown_lines = Vec::new();
    for e in &pre.events {
        match e.code.as_str() {
            "ref_evaluation_failed" => {
                unknown_lines.push(unknown(e.fields.get("ref").map_or("", String::as_str)));
            }
            "panic" if evaluated.is_empty() => unknown_lines
                .push("minds-witness: Warnung: Prüfung nicht abgeschlossen (Unknown)".to_owned()),
            "panic" => unknown_lines.extend(evaluated.iter().map(|r| unknown(r))),
            _ => {}
        }
    }
    unknown_lines.dedup();
    for f in &pre.findings {
        let pass = f.policy.result == PolicyVerdict::Pass;
        let full = f.coverage.paths_covered >= f.coverage.paths_in_diff
            || f.coverage.status == CoverageStatus::MergeCommit;
        if pass && full && !f.push.truncated {
            continue;
        }
        let head = format!("{} {}", shown(&f.push.ref_name), &f.commit.as_str()[..12]);
        let policy = if pass && !f.push.truncated {
            format!("Evidence-Policy erfüllt (Schwelle {threshold} %)")
        } else {
            format!("Evidence-Policy nicht erfüllt (Schwelle {threshold} %)")
        };
        let line = if f.push.truncated {
            format!(
                "minds-witness: Warnung: {head}: {}, unvollständige Prüfung (mehr als {limit} neue Commits), {policy}",
                state(f).word()
            )
        } else if pass {
            format!(
                "minds-witness: Hinweis: {head}: {policy}, {}, {} nicht nachgewiesen",
                counts(f, threshold),
                f.coverage.paths_in_diff - f.coverage.paths_covered
            )
        } else {
            format!(
                "minds-witness: Warnung: {head}: {}, {}, {policy}",
                state(f).word(),
                counts(f, threshold)
            )
        };
        out.push(line);
    }
    let room = MAX_LINES.saturating_sub(unknown_lines.len());
    if out.len() > room {
        let more = out.len() - room;
        out.truncate(room);
        out.push(format!("minds-witness: … und {more} weitere Befunde"));
    }
    unknown_lines.append(&mut out);
    let mut out = unknown_lines;
    if !out.is_empty() {
        out.push(
            "minds-witness: Push wird von minds nicht blockiert. Nachweis unvollständig; Details in der Quittung."
                .to_owned(),
        );
    }
    if let Some(post) = post
        && post.failed > 0
    {
        out.push(format!(
            "minds-witness: Warnung: {} Quittung(en) nicht abgelegt.",
            post.failed
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_display_never_claims_more_than_the_result() {
        assert_eq!(percent(2, 3, true, 100).unwrap(), "66,7");
        assert_eq!(percent(2, 3, false, 66).unwrap(), "66,7");
        assert_eq!(percent(2, 3, true, 67).unwrap(), "66,7");
        assert_eq!(percent(199, 200, true, 100).unwrap(), "99,5");
        assert_eq!(percent(1999, 2000, true, 100).unwrap(), "99,9");
        assert_eq!(percent(1999, 2000, false, 99).unwrap(), "99,9");
        assert_eq!(percent(6696, 10000, true, 67).unwrap(), "66,9");
        assert_eq!(percent(2, 2, false, 100).unwrap(), "100,0");
        assert_eq!(percent(0, 3, true, 100).unwrap(), "0,0");
        assert_eq!(percent(0, 0, true, 100), None);
    }

    use minds_witness_core::{
        CommitId, Coverage, Decision, LogEvent, Mode, PolicyResult, PushInfo, Pusher, SealStatus,
        ServerInfo, ServerKind, Timestamp,
    };

    const C: &str = "c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2";

    fn finding(
        ref_name: &str,
        seal: SealState,
        covered: u32,
        total: u32,
        result: PolicyVerdict,
        truncated: bool,
    ) -> Finding {
        Finding {
            commit: CommitId::parse(C).unwrap(),
            coverage: Coverage {
                paths_covered: covered,
                paths_in_diff: total,
                status: if seal == SealState::Absent {
                    CoverageStatus::NoSeal
                } else {
                    CoverageStatus::Computed
                },
                uncovered: vec!["src/geheim.rs".to_owned()],
                uncovered_truncated: false,
            },
            policy: PolicyResult {
                mode: Mode::Observe,
                result,
                version: 1,
            },
            push: PushInfo {
                new: CommitId::parse(C).unwrap(),
                new_commits: 1,
                old: CommitId::null_of_len(40).unwrap(),
                ref_name: ref_name.to_owned(),
                truncated,
            },
            pusher: Pusher {
                id: "local".to_owned(),
                username: "local".to_owned(),
            },
            repo: "git.test/project-42".to_owned(),
            seal: SealStatus {
                hash: None,
                ref_name: None,
                status: seal,
            },
            server: ServerInfo {
                host: "git.test".to_owned(),
                kind: ServerKind::Memory,
                time: Timestamp::parse("2026-10-26T10:00:00Z").unwrap(),
            },
        }
    }

    fn pre(findings: Vec<Finding>) -> PreOutcome {
        PreOutcome {
            decision: Decision::Accept,
            findings,
            events: Vec::new(),
        }
    }

    const CLOSE: &str = "minds-witness: Push wird von minds nicht blockiert. Nachweis unvollständig; Details in der Quittung.";

    #[test]
    fn partial_line_matches_scenario_b_and_never_claims_acceptance() {
        let out = lines(
            &pre(vec![finding(
                "refs/heads/main",
                SealState::Valid,
                2,
                3,
                PolicyVerdict::Fail,
                false,
            )]),
            None,
            100,
            200,
            &[],
        );
        assert_eq!(
            out,
            vec![
                "minds-witness: Warnung: refs/heads/main c2c2c2c2c2c2: Partial, 2/3 Dateien nachgewiesen (66,7 %), Evidence-Policy nicht erfüllt (Schwelle 100 %)".to_owned(),
                CLOSE.to_owned(),
            ]
        );
        for l in &out {
            assert!(
                !l.contains("angenommen") && !l.contains("erfolgreich"),
                "{l}"
            );
            assert!(!l.contains("geheim"), "{l}");
        }
    }

    #[test]
    fn hint_line_when_policy_passes_below_full_coverage() {
        let out = lines(
            &pre(vec![finding(
                "refs/heads/main",
                SealState::Valid,
                2,
                3,
                PolicyVerdict::Pass,
                false,
            )]),
            None,
            66,
            200,
            &[],
        );
        assert_eq!(
            out[0],
            "minds-witness: Hinweis: refs/heads/main c2c2c2c2c2c2: Evidence-Policy erfüllt (Schwelle 66 %), 2/3 Dateien nachgewiesen (66,7 %), 1 nicht nachgewiesen"
        );
        assert_eq!(out.len(), 2);
        // Volle Abdeckung: keine Zeile, auch keine Abschlusszeile.
        let full = lines(
            &pre(vec![finding(
                "refs/heads/main",
                SealState::Valid,
                3,
                3,
                PolicyVerdict::Pass,
                false,
            )]),
            None,
            100,
            200,
            &[],
        );
        assert!(full.is_empty());
    }

    #[test]
    fn states_truncation_sanitizing_cap_and_failed_receipts() {
        let fs = vec![
            finding(
                "refs/heads/a",
                SealState::Absent,
                0,
                2,
                PolicyVerdict::Fail,
                false,
            ),
            finding(
                "refs/heads/b",
                SealState::Unsigned,
                0,
                2,
                PolicyVerdict::Fail,
                false,
            ),
            finding(
                "refs/heads/c",
                SealState::Valid,
                0,
                2,
                PolicyVerdict::Fail,
                false,
            ),
            finding(
                "refs/heads/d",
                SealState::Absent,
                0,
                0,
                PolicyVerdict::Fail,
                true,
            ),
            finding(
                "refs/heads/\u{1b}[31mx",
                SealState::Absent,
                0,
                1,
                PolicyVerdict::Fail,
                false,
            ),
        ];
        let post = PostOutcome {
            stored: 3,
            skipped_existing: 0,
            failed: 2,
            events: Vec::new(),
        };
        let out = lines(&pre(fs), Some(&post), 100, 200, &[]);
        assert!(
            out[0].contains(
                "refs/heads/a c2c2c2c2c2c2: Unverified, 0/2 Dateien nachgewiesen (0,0 %)"
            )
        );
        assert!(out[1].contains(": Invalid, "));
        assert!(out[2].contains(": Unverified, 0/2"));
        assert_eq!(
            out[3],
            "minds-witness: Warnung: refs/heads/d c2c2c2c2c2c2: Unverified, unvollständige Prüfung (mehr als 200 neue Commits), Evidence-Policy nicht erfüllt (Schwelle 100 %)"
        );
        assert!(out[4].starts_with("minds-witness: Warnung: refs/heads/\\x1b[31mx c2c2c2c2c2c2: "));
        assert_eq!(out[5], CLOSE);
        assert_eq!(
            out[6],
            "minds-witness: Warnung: 2 Quittung(en) nicht abgelegt."
        );
        for l in &out {
            assert!(l.bytes().all(|b| b >= 0x20), "{l:?}");
        }

        let many: Vec<Finding> = (0..25)
            .map(|_| {
                finding(
                    "refs/heads/m",
                    SealState::Absent,
                    0,
                    1,
                    PolicyVerdict::Fail,
                    false,
                )
            })
            .collect();
        let out = lines(&pre(many), None, 100, 200, &[]);
        assert_eq!(out.len(), 22);
        assert_eq!(out[20], "minds-witness: … und 5 weitere Befunde");
        assert_eq!(out[21], CLOSE);
    }

    #[test]
    fn unknown_lines_lead_and_truncation_never_claims_the_policy() {
        let mut many: Vec<Finding> = (0..25)
            .map(|_| {
                finding(
                    "refs/heads/m",
                    SealState::Absent,
                    0,
                    1,
                    PolicyVerdict::Fail,
                    false,
                )
            })
            .collect();
        many.push(finding(
            "refs/heads/t",
            SealState::Valid,
            1,
            1,
            PolicyVerdict::Pass,
            true,
        ));
        let mut p = pre(many);
        p.events.push(LogEvent {
            level: minds_witness_core::LogLevel::Error,
            code: "panic".to_owned(),
            message: String::new(),
            fields: Default::default(),
        });
        let out = lines(
            &p,
            None,
            100,
            200,
            &["refs/heads/a".to_owned(), "refs/heads/\u{7}b".to_owned()],
        );
        assert_eq!(
            out[0],
            "minds-witness: Warnung: refs/heads/a: Prüfung nicht abgeschlossen (Unknown)"
        );
        assert_eq!(
            out[1],
            "minds-witness: Warnung: refs/heads/\\x07b: Prüfung nicht abgeschlossen (Unknown)"
        );
        assert_eq!(out.len(), 22);
        assert_eq!(out[20], "minds-witness: … und 8 weitere Befunde");
        let t = lines(
            &pre(vec![finding(
                "refs/heads/t",
                SealState::Valid,
                1,
                1,
                PolicyVerdict::Pass,
                true,
            )]),
            None,
            100,
            2,
            &[],
        );
        assert!(
            t[0].ends_with("Evidence-Policy nicht erfüllt (Schwelle 100 %)"),
            "{t:?}"
        );
    }
}
