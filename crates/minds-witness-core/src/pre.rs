//! Pre-Phase: entscheidet über einen Push und erzeugt je bewertetem Commit
//! ein `Finding`. Die Funktion ist deterministisch, wirft nie einen Fehler
//! nach außen und macht jeden Fehler zu einer Entscheidung plus Ereignis.
//! Sie signiert und speichert nichts; das ist Aufgabe der Post-Phase.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde::{Deserialize, Serialize};

use crate::WITNESS_NAMESPACE;
use crate::events::{LogEvent, LogLevel};
use crate::finding::{
    Coverage, CoverageStatus, Finding, PolicyResult, PolicyVerdict, PushInfo, SealState, SealStatus,
};
use crate::ids::{CommitId, HashStr};
use crate::policy::{Mode, Policy, PusherMode};
use crate::push::{PushContext, Pusher, RefUpdate};
use crate::traits::{
    ObjectAccess, SealBlob, SealError, SealExpectation, SealInspector, SealSignature, SealSummary,
};

/// Grund einer Ablehnung.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// Schreibversuch in `refs/minds/witness/`.
    WitnessNamespace,
    /// Ein erzwungener Ref hat die Prüfung nicht bestanden oder wurde nicht
    /// vollständig geprüft.
    PolicyFail,
    /// Interner Fehler im Enforce-Modus (fail-closed).
    InternalError,
}

/// Entscheidung über einen Push.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Push annehmen.
    Accept,
    /// Push als Ganzes ablehnen.
    Reject {
        /// Grund.
        reason: RejectReason,
        /// Einzeiliger deutscher Satz mit Präfix `minds-witness:`; geht
        /// unverändert an den Client.
        message: String,
    },
}

/// Ergebnis der Pre-Phase.
#[derive(Debug)]
pub struct PreOutcome {
    /// Entscheidung.
    pub decision: Decision,
    /// Alle gebauten Findings in Reihenfolge der Updates und Commits.
    pub findings: Vec<Finding>,
    /// Ereignisse in Entstehungsreihenfolge.
    pub events: Vec<LogEvent>,
}

fn reject(reason: RejectReason, message: impl Into<String>) -> Decision {
    Decision::Reject {
        reason,
        message: message.into(),
    }
}

fn fail_open(mode: &Mode, message: &str) -> Decision {
    match mode {
        Mode::Observe => Decision::Accept,
        Mode::Enforce => reject(RejectReason::InternalError, message),
    }
}

/// Prüft einen Push (Abschnitt 7). Gleiche Eingaben ergeben gleiches
/// `PreOutcome`, einschließlich der Reihenfolge von Findings und Ereignissen.
pub fn pre_receive(
    ctx: &PushContext,
    policy: &Policy,
    objects: &dyn ObjectAccess,
    seals: &dyn SealInspector,
) -> PreOutcome {
    let mut events = Vec::new();
    let mut findings = Vec::new();
    let result = catch_unwind(AssertUnwindSafe(|| {
        run(ctx, policy, objects, seals, &mut events, &mut findings)
    }));
    let decision = match result {
        Ok(decision) => decision,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unbekannt".to_owned());
            // SPEC-GAP: Ereignisse vor dem Panic bleiben erhalten (sie sind
            // passiert); die Findings werden verworfen, wie Abschnitt 7 es
            // für den Panic-Fall verlangt.
            findings.clear();
            events.push(LogEvent::new(
                LogLevel::Error,
                "panic",
                "Die Pre-Phase ist mit einem Panic abgebrochen.",
                &[("message", message)],
            ));
            fail_open(&policy.mode, "minds-witness: interner Fehler")
        }
    };
    PreOutcome {
        decision,
        findings,
        events,
    }
}

fn run(
    ctx: &PushContext,
    policy: &Policy,
    objects: &dyn ObjectAccess,
    seals: &dyn SealInspector,
    events: &mut Vec<LogEvent>,
    findings: &mut Vec<Finding>,
) -> Decision {
    // Schritt 1: Namensraum schützen, vor jeder anderen Prüfung.
    if let Some(u) = ctx
        .updates
        .iter()
        .find(|u| u.ref_name.starts_with(WITNESS_NAMESPACE))
    {
        events.push(LogEvent::new(
            LogLevel::Warn,
            "push_rejected_namespace",
            "Push abgelehnt, weil er in den Witness-Namensraum schreibt.",
            &[("ref", u.ref_name.clone())],
        ));
        return reject(
            RejectReason::WitnessNamespace,
            format!(
                "minds-witness: Refs unter refs/minds/witness/ schreibt nur der Server (Ref: {})",
                u.ref_name
            ),
        );
    }

    // Schritt 2: Policy validieren.
    if let Err(e) = policy.validate() {
        events.push(LogEvent::new(
            LogLevel::Error,
            "policy_invalid",
            "Die Policy ist ungültig.",
            &[("error", e.to_string())],
        ));
        return fail_open(&policy.mode, "minds-witness: Policy ungültig");
    }

    // Schritt 3: Kontext prüfen (einheitliche Id-Länge).
    let mut lens = ctx
        .updates
        .iter()
        .flat_map(|u| [u.old.as_str().len(), u.new.as_str().len()]);
    if let Some(first) = lens.next() {
        if lens.any(|l| l != first) {
            events.push(LogEvent::new(
                LogLevel::Error,
                "push_context_invalid",
                "Der Push-Kontext ist ungültig, weil die Commit-Ids verschiedene Längen haben.",
                &[],
            ));
            return fail_open(&policy.mode, "minds-witness: Push-Kontext ungültig");
        }
    }

    // Schritt 4: Pusher ableiten.
    let pusher = derive_pusher(&ctx.pusher, policy);

    // SPEC-GAP: Abschnitt 5 sagt zu, dass `inspect` höchstens einmal pro
    // Commit gerufen wird; Abschnitt 7 bewertet denselben Commit unter zwei
    // Refs aber zweimal (t22). Beides gilt, weil Seal-Suche und Inspektion je
    // Commit und `pre_receive`-Aufruf genau einmal laufen und ihr Ergebnis
    // (auch ein interner Fehler, ob aus `seal_for_commit` oder `inspect`) für
    // jeden weiteren Ref wiederverwendet wird. Ein transienter Fehler wirkt so
    // auf alle Refs mit diesem Commit: fail-closed und deterministisch.
    let mut seal_cache: BTreeMap<CommitId, Result<SealLook, String>> = BTreeMap::new();

    // Schritte 5 bis 7: je bewertetem Ref.
    for update in &ctx.updates {
        if update.new.is_null() || !policy.is_evaluated(&update.ref_name) {
            continue;
        }
        let start = findings.len();
        let mut env = RefEnv {
            ctx,
            policy,
            objects,
            seals,
            pusher: &pusher,
            events: &mut *events,
            findings: &mut *findings,
            seal_cache: &mut seal_cache,
        };
        if let Err(error) = env.evaluate(update) {
            findings.truncate(start);
            // SPEC-GAP: bereits erzeugte `finding_created`-Ereignisse des
            // verworfenen Refs bleiben stehen; Ereignisse werden in
            // Entstehungsreihenfolge zurückgegeben und nie zurückgenommen.
            events.push(LogEvent::new(
                LogLevel::Error,
                "ref_evaluation_failed",
                "Die Bewertung eines Refs ist an einem internen Fehler gescheitert.",
                &[("ref", update.ref_name.clone()), ("error", error)],
            ));
            if policy.mode == Mode::Enforce && policy.is_enforced(&update.ref_name) {
                // SPEC-GAP: Findings bereits vollständig bewerteter Refs
                // bleiben für die Protokollierung erhalten (Schritt 9).
                return reject(
                    RejectReason::InternalError,
                    format!("minds-witness: interner Fehler bei {}", update.ref_name),
                );
            }
        }
    }

    // Schritt 8: Entscheiden.
    if policy.mode == Mode::Observe {
        return Decision::Accept;
    }
    for f in findings.iter() {
        if !policy.is_enforced(&f.push.ref_name) {
            continue;
        }
        // SPEC-GAP: `<status>` erscheint wie serialisiert (snake_case, etwa
        // `absent`), analog `result` (`pass`/`fail`) und `truncated`
        // (`true`/`false`) in den Ereignisfeldern.
        // SPEC-GAP: Trifft beides zu, gewinnt `truncated`: eine unvollständige
        // Prüfung ist der grundsätzlichere Grund, und die Meldung bleibt wahr.
        let (grund, message) = if f.push.truncated {
            (
                "truncated",
                format!(
                    "minds-witness: Push auf {} abgelehnt: mehr als {} neue Commits, vollständige Prüfung nicht möglich",
                    f.push.ref_name, policy.max_commits_per_push
                ),
            )
        } else if f.policy.result == PolicyVerdict::Fail {
            (
                "fail",
                format!(
                    "minds-witness: Push auf {} abgelehnt: Commit {} ({}/{} Pfade abgedeckt, Seal {})",
                    f.push.ref_name,
                    f.commit.as_str(),
                    f.coverage.paths_covered,
                    f.coverage.paths_in_diff,
                    f.seal.status.as_str()
                ),
            )
        } else {
            continue;
        };
        events.push(LogEvent::new(
            LogLevel::Warn,
            "push_rejected_policy",
            "Push abgelehnt, weil ein erzwungener Ref die Prüfung nicht bestanden hat.",
            &[
                ("ref", f.push.ref_name.clone()),
                ("commit", f.commit.as_str().to_owned()),
                ("grund", grund.to_owned()),
            ],
        ));
        return reject(RejectReason::PolicyFail, message);
    }
    Decision::Accept
}

fn derive_pusher(pusher: &Pusher, policy: &Policy) -> Pusher {
    match policy.pusher_mode {
        PusherMode::Plain => pusher.clone(),
        PusherMode::Hashed => {
            let hash = |value: &str| {
                let mut data = Vec::with_capacity(policy.pusher_salt.len() + 1 + value.len());
                data.extend_from_slice(policy.pusher_salt.as_bytes());
                data.push(0x00);
                data.extend_from_slice(value.as_bytes());
                HashStr::of_bytes(&data).as_str().to_owned()
            };
            Pusher {
                id: hash(&pusher.id),
                username: hash(&pusher.username),
            }
        }
    }
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

/// Alles, was die Bewertung eines Refs braucht.
struct RefEnv<'a> {
    ctx: &'a PushContext,
    policy: &'a Policy,
    objects: &'a dyn ObjectAccess,
    seals: &'a dyn SealInspector,
    pusher: &'a Pusher,
    events: &'a mut Vec<LogEvent>,
    findings: &'a mut Vec<Finding>,
    seal_cache: &'a mut BTreeMap<CommitId, Result<SealLook, String>>,
}

/// Ergebnis von Seal-Suche und Inspektion für einen Commit.
#[derive(Clone)]
enum SealLook {
    /// Kein Seal.
    Absent,
    /// Seal gefunden und gedeutet.
    Inspected(SealBlob, SealSummary),
    /// Seal gefunden, aber nicht lesbar (`SealError::Malformed`).
    Malformed(SealBlob),
}

impl RefEnv<'_> {
    /// Schritte 6 und 7 für einen bewerteten Ref. `Err` trägt den
    /// `Display`-Text des internen Fehlers.
    fn evaluate(&mut self, update: &RefUpdate) -> Result<(), String> {
        let old = if update.old.is_null() {
            None
        } else {
            Some(&update.old)
        };
        let limit = self.policy.max_commits_per_push;
        let range = self
            .objects
            .new_commits(old, &update.new, limit)
            .map_err(|e| e.to_string())?;
        let new_commits = len_u32(range.commits.len());
        let to_check = if range.truncated {
            self.events.push(LogEvent::new(
                LogLevel::Warn,
                "range_truncated",
                "Mehr neue Commits als erlaubt; nur der neue Stand des Refs wird bewertet.",
                &[
                    ("ref", update.ref_name.clone()),
                    ("limit", limit.to_string()),
                ],
            ));
            vec![update.new.clone()]
        } else if range.commits.is_empty() {
            self.events.push(LogEvent::new(
                LogLevel::Info,
                "no_new_commits",
                "Der Ref bringt keine neuen Commits.",
                &[("ref", update.ref_name.clone())],
            ));
            return Ok(());
        } else {
            range.commits
        };
        let push = PushInfo {
            new: update.new.clone(),
            new_commits,
            old: update.old.clone(),
            ref_name: update.ref_name.clone(),
            truncated: range.truncated,
        };
        for c in &to_check {
            let finding = self.finding(c, &push)?;
            self.events.push(LogEvent::new(
                LogLevel::Info,
                "finding_created",
                "Befund für einen Commit erstellt.",
                &[
                    ("ref", push.ref_name.clone()),
                    ("commit", c.as_str().to_owned()),
                    ("result", finding.policy.result.as_str().to_owned()),
                    ("paths_covered", finding.coverage.paths_covered.to_string()),
                    ("paths_in_diff", finding.coverage.paths_in_diff.to_string()),
                    ("truncated", push.truncated.to_string()),
                ],
            ));
            self.findings.push(finding);
        }
        Ok(())
    }

    /// Seal-Suche und Inspektion, je Commit höchstens einmal pro Push.
    fn seal_lookup(&mut self, c: &CommitId) -> Result<SealLook, String> {
        if let Some(cached) = self.seal_cache.get(c) {
            return cached.clone();
        }
        let look = match self.objects.seal_for_commit(c) {
            Err(e) => Err(e.to_string()),
            Ok(None) => Ok(SealLook::Absent),
            Ok(Some(blob)) => {
                let expected = SealExpectation {
                    repo: self.ctx.repo.clone(),
                    commit: c.clone(),
                };
                match self.seals.inspect(&blob, &expected) {
                    Ok(summary) => Ok(SealLook::Inspected(blob, summary)),
                    Err(SealError::Malformed(_)) => Ok(SealLook::Malformed(blob)),
                    Err(e @ SealError::Internal(_)) => Err(e.to_string()),
                }
            }
        };
        self.seal_cache.insert(c.clone(), look.clone());
        look
    }

    fn finding(&mut self, c: &CommitId, push: &PushInfo) -> Result<Finding, String> {
        let cap = self.policy.uncovered_cap;
        let info = self.objects.commit(c).map_err(|e| e.to_string())?;
        let paths = self.objects.changed_paths(c).map_err(|e| e.to_string())?;
        let absent = SealStatus {
            hash: None,
            ref_name: None,
            status: SealState::Absent,
        };
        let (seal, coverage) = if info.parents.len() >= 2 && paths.is_empty() {
            (
                absent,
                Coverage {
                    paths_covered: 0,
                    paths_in_diff: 0,
                    status: CoverageStatus::MergeCommit,
                    uncovered: Vec::new(),
                    uncovered_truncated: false,
                },
            )
        } else {
            match self.seal_lookup(c)? {
                SealLook::Absent => (absent, coverage(CoverageStatus::NoSeal, &paths, None, cap)),
                SealLook::Inspected(blob, summary) => {
                    let bound_ok = summary.bound_commit == *c
                        && summary
                            .bound_repo
                            .as_ref()
                            .is_none_or(|r| *r == self.ctx.repo);
                    let status = if !bound_ok {
                        SealState::Mismatched
                    } else {
                        match summary.signature {
                            SealSignature::Valid => SealState::Valid,
                            SealSignature::Invalid => SealState::Invalid,
                            SealSignature::Unsigned => SealState::Unsigned,
                        }
                    };
                    let cov = coverage(
                        CoverageStatus::Computed,
                        &paths,
                        Some(&summary.touched_paths),
                        cap,
                    );
                    (
                        SealStatus {
                            hash: Some(summary.hash),
                            ref_name: Some(blob.ref_name),
                            status,
                        },
                        cov,
                    )
                }
                SealLook::Malformed(blob) => (
                    SealStatus {
                        hash: Some(HashStr::of_bytes(&blob.bytes)),
                        ref_name: Some(blob.ref_name),
                        status: SealState::Invalid,
                    },
                    coverage(CoverageStatus::NoSeal, &paths, None, cap),
                ),
            }
        };
        let result = verdict(&seal, &coverage, self.policy.coverage_threshold_percent);
        Ok(Finding {
            commit: c.clone(),
            coverage,
            policy: PolicyResult {
                mode: self.policy.mode.clone(),
                result,
                version: self.policy.version,
            },
            push: push.clone(),
            pusher: self.pusher.clone(),
            repo: self.ctx.repo.clone(),
            seal,
            server: self.ctx.server.clone(),
        })
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::ids::Timestamp;
    use crate::memory::{MemoryAdapter, SealOutcome};
    use crate::push::{ServerInfo, ServerKind};

    /// Zählt `inspect`-Aufrufe und delegiert an den `MemoryAdapter`.
    struct CountingInspector<'a> {
        inner: &'a MemoryAdapter,
        calls: Cell<u32>,
    }

    impl SealInspector for CountingInspector<'_> {
        fn inspect(
            &self,
            blob: &SealBlob,
            expected: &SealExpectation,
        ) -> Result<SealSummary, SealError> {
            self.calls.set(self.calls.get() + 1);
            self.inner.inspect(blob, expected)
        }
    }

    fn cid(n: u8) -> CommitId {
        CommitId::parse(&format!("{n:040x}")).unwrap()
    }

    #[test]
    fn inspect_at_most_once_per_commit() {
        let ctx = PushContext {
            repo: "acme/payments".to_owned(),
            updates: ["refs/heads/main", "refs/heads/feature"]
                .iter()
                .map(|r| RefUpdate {
                    old: cid(1),
                    new: cid(2),
                    ref_name: (*r).to_owned(),
                })
                .collect(),
            pusher: Pusher {
                id: "u".to_owned(),
                username: "u".to_owned(),
            },
            server: ServerInfo {
                host: "h".to_owned(),
                kind: ServerKind::Memory,
                time: Timestamp::parse("2026-10-25T10:00:00Z").unwrap(),
            },
        };
        let mut a = MemoryAdapter::new(ctx.clone());
        let touched: BTreeSet<String> = ["x".to_owned()].into();
        a.add_commit(cid(1), vec![], BTreeSet::new())
            .add_commit(cid(2), vec![cid(1)], touched.clone())
            .add_seal(
                cid(2),
                "refs/minds/seals/s",
                b"s".to_vec(),
                SealOutcome::Summary(SealSummary {
                    hash: HashStr::of_bytes(b"s"),
                    signature: SealSignature::Valid,
                    bound_commit: cid(2),
                    bound_repo: None,
                    touched_paths: touched,
                }),
            );
        let spy = CountingInspector {
            inner: &a,
            calls: Cell::new(0),
        };
        let out = pre_receive(&ctx, &Policy::default(), &a, &spy);
        assert_eq!(out.findings.len(), 2);
        assert_eq!(out.findings[0].seal, out.findings[1].seal);
        assert_eq!(spy.calls.get(), 1);

        // Gecachter interner Fehler: beide Refs werden verworfen, `inspect`
        // läuft trotzdem nur einmal, der erzwungene Ref führt zu Reject.
        a.fault = crate::memory::Fault::SealInternal;
        let spy = CountingInspector {
            inner: &a,
            calls: Cell::new(0),
        };
        let policy = Policy {
            mode: Mode::Enforce,
            enforce_refs: vec!["refs/heads/feature".to_owned()],
            ..Policy::default()
        };
        let out = pre_receive(&ctx, &policy, &a, &spy);
        assert_eq!(spy.calls.get(), 1);
        assert!(out.findings.is_empty());
        let failed = out
            .events
            .iter()
            .filter(|e| e.code == "ref_evaluation_failed")
            .count();
        assert_eq!(failed, 2);
        assert!(matches!(
            out.decision,
            Decision::Reject {
                reason: RejectReason::InternalError,
                ..
            }
        ));
    }
}
