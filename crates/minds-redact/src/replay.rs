//! Der Replay-Record (EA-18b) auf dem Weg in den Store: geprüft wie jede
//! Session, nur durch die Pipeline erreichbar.
//!
//! Der Record trägt argv und Bench-Namen aus dem Envelope — und das kann
//! jeder mit Push-Recht auf `refs/minds/*` ablegen, ohne dass es je durch
//! [`RedactionPipeline::redact_session`] ging. [`ScannedReplayRecord`] hat
//! deshalb keinen öffentlichen Konstruktor: Den einzigen Weg geht
//! [`RedactionPipeline::scan_replay`], und `ContextStore::put_replay`
//! nimmt nur ihn. Die Prüfung zerlegt jedes Feld ohne `..` — ein neues
//! Textfeld kompiliert erst, wenn jemand entschieden hat, wie es geprüft
//! wird.
//!
//! Geschwärzt wird hier nichts: Ein Record mit Fund wird abgelehnt
//! (fail-closed) — ein halb geschwärztes argv taugte für keinen Vergleich.

use minds_core::replay::{Expected, Observed, ReplayEnvironment, ReplayRecord, ReplayResult};
use minds_core::{BenchValue, RedactionCounts, SessionId};

use crate::RedactionPipeline;

/// Ein Replay-Record, dessen sämtliche Texte die Redaction unverändert
/// passiert haben und dessen Ids ihre Form haben.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedReplayRecord(ReplayRecord);

impl ScannedReplayRecord {
    /// Der geprüfte Record.
    pub fn record(&self) -> &ReplayRecord {
        &self.0
    }
}

/// Warum ein Record nicht abgelegt werden darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplayScanError {
    /// Commit, Session oder Policy-Blob haben nicht die Form einer Id.
    #[error("the replay record carries a malformed id")]
    MalformedId,
    /// Ein Text trüge etwas, das die Redaction-Policy entfernt.
    #[error("the replay record would carry text the redaction policy removes")]
    Redactable,
}

impl RedactionPipeline {
    /// Prüft jeden Text des Records — argv (als Zeile und je Element),
    /// Runner, Bench-Namen und -Einheiten (berichtet und beobachtet),
    /// Gründe, Art, Projekt, Umgebung — und die Form der Ids. Ein Fund
    /// lehnt den ganzen Record ab.
    pub fn scan_replay(
        &self,
        record: ReplayRecord,
    ) -> Result<ScannedReplayRecord, ReplayScanError> {
        let ReplayRecord {
            kind,
            schema: _,
            commit,
            session,
            interpretation_version: _,
            results,
            policy,
            project,
            environment:
                ReplayEnvironment {
                    ci,
                    pipeline,
                    image,
                },
        } = &record;
        let hex = |text: &str| {
            matches!(text.len(), 40 | 64)
                && text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if !hex(commit)
            || session.parse::<SessionId>().is_err()
            || policy.as_deref().is_some_and(|p| !hex(p))
        {
            return Err(ReplayScanError::MalformedId);
        }
        let mut texts: Vec<&str> = vec![kind.as_str()];
        texts.extend(
            [project, ci, pipeline, image]
                .into_iter()
                .filter_map(|text| text.as_deref()),
        );
        let mut lines = Vec::new();
        for result in results {
            let ReplayResult {
                turn: _,
                call: _,
                argv,
                expected:
                    Expected {
                        class: _,
                        runner,
                        exit_code: _,
                        tests: _,
                        benches: expected_benches,
                    },
                observed,
                verdict: _,
                reason,
            } = result;
            lines.push(argv.join(" "));
            texts.extend(argv.iter().map(String::as_str));
            texts.push(runner);
            texts.extend(reason.as_deref());
            let observed_benches = observed.iter().flat_map(|o| {
                let Observed {
                    exit_code: _,
                    tests: _,
                    benches,
                    timed_out: _,
                } = o;
                benches
            });
            for BenchValue {
                name,
                value: _,
                unit,
            } in expected_benches.iter().chain(observed_benches)
            {
                texts.push(name);
                texts.push(unit);
            }
        }
        texts.extend(lines.iter().map(String::as_str));
        for text in texts {
            let out = self.redact(text);
            if out.counts != RedactionCounts::default() || out.invalid_findings > 0 {
                return Err(ReplayScanError::Redactable);
            }
        }
        Ok(ScannedReplayRecord(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::replay::{REPLAY_KIND, REPLAY_SCHEMA, ReplayVerdict};
    use minds_core::{ExecClass, TestCounts};

    fn record() -> ReplayRecord {
        let counts = TestCounts {
            passed: 12,
            failed: 0,
            ignored: 0,
        };
        ReplayRecord {
            kind: REPLAY_KIND.into(),
            schema: REPLAY_SCHEMA,
            commit: "ab".repeat(20),
            session: format!("b3-{}", "cd".repeat(32)),
            interpretation_version: 1,
            results: vec![ReplayResult {
                turn: 0,
                call: 0,
                argv: vec![
                    "cargo".into(),
                    "test".into(),
                    "-p".into(),
                    "minds-core".into(),
                    "--".into(),
                    "--exact".into(),
                    "replay::tests::replay_record_golden".into(),
                ],
                expected: Expected {
                    class: ExecClass::Test,
                    runner: "cargo-test".into(),
                    exit_code: None,
                    tests: Some(counts),
                    benches: vec![BenchValue {
                        name: "parse/b3_hash".into(),
                        value: 10,
                        unit: "ns".into(),
                    }],
                },
                observed: Some(Observed {
                    exit_code: Some(0),
                    tests: Some(counts),
                    benches: Vec::new(),
                    timed_out: false,
                }),
                verdict: ReplayVerdict::Reproduced,
                reason: None,
            }],
            policy: Some("3b18e512dba79e4c8300dd08aeb37f8e728b8dad".into()),
            project: Some("gitlab.example.com/group/repo".into()),
            environment: ReplayEnvironment {
                ci: Some("gitlab".into()),
                pipeline: Some("4711".into()),
                image: Some(format!(
                    "registry.gitlab.com/group/rust:1.90@sha256:{}",
                    "0f1e2d3c4b5a6978".repeat(4)
                )),
            },
        }
    }

    fn pipeline() -> RedactionPipeline {
        crate::RedactionConfig::default().pipeline().unwrap()
    }

    #[test]
    fn an_ordinary_record_passes_unchanged() {
        let scanned = pipeline().scan_replay(record()).unwrap();
        assert_eq!(scanned.record(), &record());
    }

    #[test]
    fn any_redactable_text_refuses_the_record() {
        let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        type Mutation = Box<dyn Fn(&mut ReplayRecord)>;
        let mut cases: Vec<Mutation> = vec![
            Box::new(|r| {
                r.results[0]
                    .argv
                    .extend(["--password".into(), "hunter2".into()])
            }),
            Box::new(move |r| r.results[0].expected.runner = token.into()),
            Box::new(move |r| r.results[0].expected.benches[0].name = format!("x/{token}")),
            Box::new(move |r| r.results[0].expected.benches[0].unit = token.into()),
            Box::new(move |r| r.results[0].reason = Some(format!("bench {token}: missing"))),
            Box::new(move |r| r.project = Some(format!("host/{token}"))),
            Box::new(|r| r.environment.image = Some("anna@example.com/rust".into())),
        ];
        cases.push(Box::new(move |r| {
            r.results[0].observed.as_mut().unwrap().benches = vec![BenchValue {
                name: token.into(),
                value: 1,
                unit: "ns".into(),
            }]
        }));
        for (index, case) in cases.iter().enumerate() {
            let mut r = record();
            case(&mut r);
            assert_eq!(
                pipeline().scan_replay(r),
                Err(ReplayScanError::Redactable),
                "case {index}"
            );
        }
    }

    #[test]
    fn malformed_ids_refuse_the_record() {
        for case in [
            |r: &mut ReplayRecord| r.commit = "HEAD; rm -rf /".into(),
            |r: &mut ReplayRecord| r.session = "not-a-session".into(),
            |r: &mut ReplayRecord| r.policy = Some("x".into()),
        ] {
            let mut r = record();
            case(&mut r);
            assert_eq!(pipeline().scan_replay(r), Err(ReplayScanError::MalformedId));
        }
    }
}
