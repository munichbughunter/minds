//! Pre-Phase: entscheidet über einen Push und erzeugt je bewertetem Commit
//! ein `Finding`. Die Funktion ist deterministisch, wirft nie einen Fehler
//! nach außen und macht jeden Fehler zu einer Entscheidung plus Ereignis.
//! Sie signiert und speichert nichts; das ist Aufgabe der Post-Phase.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde::{Deserialize, Serialize};

use crate::WITNESS_NAMESPACE;
use crate::assess::{SealLookup, assess};
use crate::events::{LogEvent, LogLevel};
use crate::finding::{Finding, PolicyResult, PolicyVerdict, PushInfo};
use crate::ids::{CommitId, HashStr};
use crate::policy::{Mode, Policy, PusherMode};
use crate::push::{PushContext, Pusher, RefUpdate, display_ref};
use crate::traits::{ObjectAccess, SealError, SealExpectation, SealInspector};

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
        Err(_payload) => {
            // Spec S1 v1.4 §7: Ereignisse vor dem Panic bleiben erhalten (sie
            // sind passiert); die Findings werden verworfen. Der Payload wird
            // nie übernommen: er ist unkontrollierter Text eines Adapters und
            // kann Objektinhalte tragen (§10). Das Ereignis hat keine Felder.
            findings.clear();
            events.push(LogEvent::new(
                LogLevel::Error,
                "panic",
                "Die Pre-Phase ist mit einem Panic abgebrochen.",
                &[],
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
    if let Some(u) = ctx.updates.iter().find(|u| is_protected_ref(&u.ref_name)) {
        events.push(LogEvent::new(
            LogLevel::Warn,
            "push_rejected_namespace",
            "Push abgelehnt, weil er in den Witness-Namensraum schreibt.",
            &[("ref", u.ref_name.clone())],
        ));
        let message = if u.ref_name.starts_with(WITNESS_NAMESPACE) {
            format!(
                "minds-witness: Refs unter refs/minds/witness/ schreibt nur der Server (Ref: {})",
                display_ref(&u.ref_name)
            )
        } else {
            format!(
                "minds-witness: Ref {} ist reserviert, weil er den Witness-Namensraum refs/minds/witness/ blockieren würde",
                display_ref(&u.ref_name)
            )
        };
        return reject(RejectReason::WitnessNamespace, message);
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

    // Spec S1 v1.4 §5/§7: Abschnitt 5 sagt zu, dass `inspect` höchstens einmal pro
    // Commit gerufen wird; Abschnitt 7 bewertet denselben Commit unter zwei
    // Refs aber zweimal (t22). Beides gilt, weil Seal-Suche und Inspektion je
    // Commit und `pre_receive`-Aufruf genau einmal laufen und ihr Ergebnis
    // (auch ein interner Fehler, ob aus `seal_for_commit` oder `inspect`) für
    // jeden weiteren Ref wiederverwendet wird. Ein transienter Fehler wirkt so
    // auf alle Refs mit diesem Commit: fail-closed und deterministisch.
    let mut seal_cache: BTreeMap<CommitId, Result<SealLookup, String>> = BTreeMap::new();

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
            // Spec S1 v1.4 §7: bereits erzeugte `finding_created`-Ereignisse des
            // verworfenen Refs bleiben stehen; Ereignisse werden in
            // Entstehungsreihenfolge zurückgegeben und nie zurückgenommen.
            events.push(LogEvent::new(
                LogLevel::Error,
                "ref_evaluation_failed",
                "Die Bewertung eines Refs ist an einem internen Fehler gescheitert.",
                &[("ref", update.ref_name.clone()), ("error", error)],
            ));
            if policy.mode == Mode::Enforce && policy.is_enforced(&update.ref_name) {
                // Spec S1 v1.4 §7: Findings bereits vollständig bewerteter Refs
                // bleiben für die Protokollierung erhalten (Schritt 9).
                return reject(
                    RejectReason::InternalError,
                    format!(
                        "minds-witness: interner Fehler bei {}",
                        display_ref(&update.ref_name)
                    ),
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
        // Spec S1 v1.4 §7: `<status>` erscheint wie serialisiert (snake_case, etwa
        // `absent`), analog `result` (`pass`/`fail`) und `truncated`
        // (`true`/`false`) in den Ereignisfeldern.
        // Spec S1 v1.4 §7: Trifft beides zu, gewinnt `truncated`: eine unvollständige
        // Prüfung ist der grundsätzlichere Grund, und die Meldung bleibt wahr.
        let (grund, message) = if f.push.truncated {
            (
                "truncated",
                format!(
                    "minds-witness: Push auf {} abgelehnt: mehr als {} neue Commits, vollständige Prüfung nicht möglich",
                    display_ref(&f.push.ref_name),
                    policy.max_commits_per_push
                ),
            )
        } else if f.policy.result == PolicyVerdict::Fail {
            (
                "fail",
                format!(
                    "minds-witness: Push auf {} abgelehnt: Commit {} ({}/{} Pfade abgedeckt, Seal {})",
                    display_ref(&f.push.ref_name),
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

/// Ref-Namen, die als Datei den Witness-Namensraum als Verzeichnis
/// blockieren würden (D/F-Konflikt bei Ref-Ablage als Dateien): jeder echte
/// Präfix von `refs/minds/witness/` unterhalb von `refs/`.
const RESERVED_REFS: [&str; 2] = ["refs/minds", "refs/minds/witness"];

/// `true`, wenn ein Push diesen Ref weder anlegen, ändern noch löschen darf
/// (Spec S1 v1.4 §7, Schritt 1).
fn is_protected_ref(ref_name: &str) -> bool {
    ref_name.starts_with(WITNESS_NAMESPACE) || RESERVED_REFS.contains(&ref_name)
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

/// Alles, was die Bewertung eines Refs braucht.
struct RefEnv<'a> {
    ctx: &'a PushContext,
    policy: &'a Policy,
    objects: &'a dyn ObjectAccess,
    seals: &'a dyn SealInspector,
    pusher: &'a Pusher,
    events: &'a mut Vec<LogEvent>,
    findings: &'a mut Vec<Finding>,
    seal_cache: &'a mut BTreeMap<CommitId, Result<SealLookup, String>>,
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
    fn seal_lookup(&mut self, c: &CommitId) -> Result<SealLookup, String> {
        if let Some(cached) = self.seal_cache.get(c) {
            return cached.clone();
        }
        let look = match self.objects.seal_for_commit(c) {
            Err(e) => Err(e.to_string()),
            Ok(None) => Ok(SealLookup::Absent),
            Ok(Some(blob)) => {
                let expected = SealExpectation {
                    repo: self.ctx.repo.clone(),
                    commit: c.clone(),
                };
                match self.seals.inspect(&blob, &expected) {
                    Ok(summary) => Ok(SealLookup::Inspected(blob, summary)),
                    Err(SealError::Malformed(_)) => Ok(SealLookup::Malformed(blob)),
                    Err(e @ SealError::Internal(_)) => Err(e.to_string()),
                }
            }
        };
        self.seal_cache.insert(c.clone(), look.clone());
        look
    }

    fn finding(&mut self, c: &CommitId, push: &PushInfo) -> Result<Finding, String> {
        let info = self.objects.commit(c).map_err(|e| e.to_string())?;
        let paths = self.objects.changed_paths(c).map_err(|e| e.to_string())?;
        let repo = self.ctx.repo.clone();
        let policy = self.policy;
        let assessment = assess(
            c,
            &repo,
            info.parents.len(),
            &paths,
            || self.seal_lookup(c),
            policy,
        )?;
        Ok(Finding {
            commit: c.clone(),
            coverage: assessment.coverage,
            policy: PolicyResult {
                mode: self.policy.mode.clone(),
                result: assessment.result,
                version: self.policy.version,
            },
            push: push.clone(),
            pusher: self.pusher.clone(),
            repo,
            seal: assessment.seal,
            server: self.ctx.server.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeSet;

    use super::*;
    use crate::ids::Timestamp;
    use crate::memory::{MemoryAdapter, SealOutcome};
    use crate::push::{ServerInfo, ServerKind};
    use crate::traits::{SealBlob, SealSignature, SealSummary};

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

    /// Ablehnungsmeldungen gehen als `remote:`-Zeilen an den Client: Ein
    /// Ref-Name mit (8-Bit-)Steuerzeichen oder Überlänge erscheint
    /// escaped und begrenzt, in allen vier Meldungen mit Ref-Namen.
    #[test]
    fn reject_messages_escape_and_bound_ref_names() {
        let evil = format!("refs/heads/x\u{9b}[2J\u{1b}]0;{}", "y".repeat(400));
        let ctx_for = |name: &str| PushContext {
            repo: "acme/payments".to_owned(),
            updates: vec![RefUpdate {
                old: cid(1),
                new: cid(2),
                ref_name: name.to_owned(),
            }],
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
        let enforce = Policy {
            mode: Mode::Enforce,
            enforce_refs: vec!["refs/heads/*".to_owned()],
            max_commits_per_push: 1,
            ..Policy::default()
        };
        let check = |out: &PreOutcome, name: &str| {
            let Decision::Reject { message, .. } = &out.decision else {
                panic!("kein Reject: {:?}", out.decision);
            };
            // Keine Steuerzeichen, auch keine C1-Zeichen (U+0080 bis U+009F);
            // der feste deutsche Text darf Umlaute tragen.
            assert!(!message.chars().any(char::is_control), "{message:?}");
            assert!(message.contains(&display_ref(name)), "{message}");
            assert!(message.len() < 200 + MAX_REF_DISPLAY_SLACK, "{message}");
            // Das Log behält den Namen unverändert.
            assert!(
                out.events
                    .iter()
                    .any(|e| e.fields.get("ref").map(String::as_str) == Some(name))
            );
        };

        // Fail ohne Seal.
        let ctx = ctx_for(&evil);
        let mut a = MemoryAdapter::new(ctx.clone());
        a.add_commit(cid(1), vec![], BTreeSet::new()).add_commit(
            cid(2),
            vec![cid(1)],
            ["x".to_owned()].into(),
        );
        check(&pre_receive(&ctx, &enforce, &a, &a), &evil);

        // Truncated: zwei neue Commits bei max_commits_per_push = 1.
        let created = PushContext {
            updates: vec![RefUpdate {
                old: CommitId::null_of_len(40).unwrap(),
                new: cid(2),
                ref_name: evil.clone(),
            }],
            ..ctx.clone()
        };
        let mut a = MemoryAdapter::new(created.clone());
        a.add_commit(cid(1), vec![], BTreeSet::new()).add_commit(
            cid(2),
            vec![cid(1)],
            ["x".to_owned()].into(),
        );
        check(&pre_receive(&created, &enforce, &a, &a), &evil);

        // Interner Fehler auf erzwungenem Ref.
        let mut a = MemoryAdapter::new(ctx.clone());
        a.add_commit(cid(1), vec![], BTreeSet::new()).add_commit(
            cid(2),
            vec![cid(1)],
            ["x".to_owned()].into(),
        );
        a.fault = crate::memory::Fault::SealInternal;
        check(&pre_receive(&ctx, &enforce, &a, &a), &evil);

        // Witness-Namensraum.
        let witness = format!("refs/minds/witness/\u{9b}{}", "z".repeat(400));
        let ctx = ctx_for(&witness);
        let a = MemoryAdapter::new(ctx.clone());
        check(&pre_receive(&ctx, &enforce, &a, &a), &witness);
    }

    /// Längster fester Text einer Ablehnungsmeldung mit Ref-Namen, großzügig.
    const MAX_REF_DISPLAY_SLACK: usize = 200;
}
