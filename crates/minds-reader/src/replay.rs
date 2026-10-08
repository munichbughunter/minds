//! Replay (EA-18b): die **reine** Hälfte — welche Befehle einer Session
//! entscheidend sind, ob die Team-Policy ihre Ausführung erlaubt, in welchem
//! Verzeichnis sie laufen dürften, wie Beobachtung und Bericht verglichen
//! werden und was ein abgelegter Record für die Assurance zählt.
//!
//! Ausgeführt wird hier nichts. Das Starten der Prozesse (ohne Shell, mit
//! geleerter Umgebung, mit Zeitlimit) macht `minds replay` in der CLI; alles,
//! was dort entschieden wird, steht als Funktion hier und ist ohne Prozess
//! testbar (W5: „replay matching" lebt im Reader).
//!
//! # Entscheidende Befehle — Deutung v1
//!
//! [`decisive`]: das **letzte** Auftreten jedes verschiedenen Tripels
//! `(Klasse, normalisiertes argv, cwd)` über alle Turns der Session, in der
//! Reihenfolge dieser letzten Auftreten. Das `cwd` gehört dazu: `cargo
//! test` in `crates/a` und in `crates/b` sind zwei Behauptungen. Nur Aufrufe mit gespeichertem
//! [`ExecOutcome`] (EA-18a) zählen. Ändert sich diese Regel oder der
//! Vergleich, steigt [`INTERPRETATION_VERSION`]; ein Record mit anderer
//! Version zählt dann für keine Session.
//!
//! # Die Allowlist ist die Sicherheitsgrenze
//!
//! Das argv kommt aus dem Envelope — also aus Material, das auch der Agent
//! schreiben kann. Ausgeführt wird es nur, wenn die **reviewte** Policy
//! `.minds/replay.json` (aus dem Baum des geprüften Commits) es erlaubt
//! ([`allows`]): Programmname und Subkommando exakt, jedes
//! Flag aus der Liste, kein Argument mit absolutem Pfad, `..`, `+toolchain`
//! oder `@argsfile`. Keine Policy heißt: nichts ist erlaubt.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use minds_core::replay::{
    Observed, ReplayPolicy, ReplayRecord, ReplayResult, ReplayVerdict, RunnerRule,
};
use minds_core::{BenchValue, ExecClass, ExecOutcome, Session, SessionId};

use crate::assurance::ReplaySummary;

/// Version der Deutung „entscheidender Befehl" und des Vergleichs.
pub const INTERPRETATION_VERSION: u32 = 1;

/// Höchstens so viele Argumente hat ein ausführbarer Befehl.
pub const MAX_ARGS: usize = 256;

/// Höchstens so viele Bytes zählen alle Argumente zusammen.
pub const MAX_ARGV_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Entscheidende Befehle
// ---------------------------------------------------------------------------

/// Ein entscheidender Befehl: wo er zuletzt stand und was er berichtete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decisive<'s> {
    /// Index des Turns.
    pub turn: u32,
    /// Index des Tool-Aufrufs im Turn.
    pub call: u32,
    /// Das gespeicherte Ergebnis.
    pub outcome: &'s ExecOutcome,
}

/// Die entscheidenden Befehle der Session (Deutung v1, siehe Modul-Doku).
/// Deterministisch: dieselbe Session ergibt dieselbe Liste.
pub fn decisive(session: &Session) -> Vec<Decisive<'_>> {
    // Schlüssel: Klasse (als Rang, `ExecClass` ist nicht geordnet), argv
    // und Arbeitsverzeichnis.
    let mut last: BTreeMap<(u8, &[String], Option<&str>), Decisive<'_>> = BTreeMap::new();
    for (turn_index, turn) in session.turns.iter().enumerate() {
        for (call_index, call) in turn.tool_calls.iter().enumerate() {
            let Some(outcome) = &call.outcome else {
                continue;
            };
            let (Ok(turn), Ok(call)) = (u32::try_from(turn_index), u32::try_from(call_index))
            else {
                continue;
            };
            let class = match outcome.class {
                ExecClass::Test => 0,
                ExecClass::Bench => 1,
            };
            last.insert(
                (class, outcome.command.as_slice(), outcome.cwd.as_deref()),
                Decisive {
                    turn,
                    call,
                    outcome,
                },
            );
        }
    }
    let mut out: Vec<Decisive<'_>> = last.into_values().collect();
    out.sort_by_key(|d| (d.turn, d.call));
    out
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// Ob `argv` unter `policy` ausgeführt werden darf — die eine
/// Sicherheitsgrenze (siehe Modul-Doku). Eine Regel trifft, wenn `argv[0]`
/// exakt ihr Programm und `argv[1]` exakt eines ihrer Subkommandos ist;
/// danach muss **jedes** weitere Argument erlaubt sein: ein Flag aus
/// `allow_flags` (auch `--flag=wert`, dann gilt für den Wert dieselbe Regel
/// wie für Positionale) oder ein harmloses Positional ([`plain_argument`]).
pub fn allows(policy: &ReplayPolicy, argv: &[String]) -> bool {
    // Ein argv, das `execve` sprengte (`E2BIG`), wäre kein Befehl, sondern
    // ein Hebel gegen den ganzen Lauf.
    if argv.len() > MAX_ARGS || argv.iter().map(String::len).sum::<usize>() > MAX_ARGV_BYTES {
        return false;
    }
    let [argv0, sub, rest @ ..] = argv else {
        return false;
    };
    policy.runners.values().any(|rule| {
        rule.argv0 == *argv0
            && rule.sub.iter().any(|s| s == sub)
            && rest.iter().all(|arg| rule_allows_arg(rule, arg))
    })
}

/// Die Toleranz in Prozent für den Benchmark `name`: das erste passende
/// Override, sonst der Default.
pub fn tolerance_for(policy: &ReplayPolicy, name: &str) -> u32 {
    policy
        .tolerance
        .overrides
        .iter()
        .find(|o| glob(o.bench.as_bytes(), name.as_bytes()))
        .map_or(policy.tolerance.default_pct, |o| o.pct)
}

fn rule_allows_arg(rule: &RunnerRule, arg: &str) -> bool {
    if arg.starts_with('-') {
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg, None),
        };
        rule.allow_flags.iter().any(|flag| flag == name)
            && !arg.chars().any(char::is_control)
            && value.is_none_or(plain_argument)
    } else {
        plain_argument(arg)
    }
}

/// Ein Positional (oder Flag-Wert), das nichts außerhalb des Checkouts
/// nennen und keine Indirektion auslösen kann: nicht leer, keine Steuer-
/// zeichen, kein `+toolchain`, kein `@argsfile`, kein `~`/`$`, kein
/// absoluter Pfad (auch nicht Windows-förmig), kein Backslash, keine
/// `..`-Komponente.
pub fn plain_argument(arg: &str) -> bool {
    !arg.is_empty()
        && arg.len() <= 4096
        && !arg.chars().any(char::is_control)
        && !arg.starts_with(['+', '@', '~', '$', '/', '-'])
        && !arg.contains('\\')
        && !(arg.len() >= 2 && arg.as_bytes()[1] == b':')
        && arg.split('/').all(|part| part != "..")
}

/// `*` (beliebig viele Zeichen) und `?` (genau eines) — für Bench-Muster.
///
/// Iterativ mit Rücksprung nur zum letzten `*`: höchstens O(n·m) Schritte,
/// keine Rekursion. Muster (Policy) und Name (Session) können beide vom
/// Agenten stammen; ein rekursiver Matcher liefe bei `*a*a*a…b` gegen
/// `aaaa…` exponentiell — im Prozess, außerhalb jedes Zeitlimits.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == b'?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

// ---------------------------------------------------------------------------
// Arbeitsverzeichnis
// ---------------------------------------------------------------------------

/// Warum ein entscheidender Befehl nicht ausgeführt wird — fester
/// Wortschatz (der `reason` im Record).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Die Policy erlaubt das argv nicht.
    NotAllowlisted,
    /// Die Session nennt kein (repo-relatives) Arbeitsverzeichnis.
    CwdNotRecorded,
    /// Das Arbeitsverzeichnis liegt außerhalb des Checkouts.
    CwdOutsideCheckout,
    /// Das Arbeitsverzeichnis gibt es im Checkout nicht.
    CwdMissing,
    /// Das Arbeitsverzeichnis trägt der Commit nicht (Cache, Build-Ausgabe).
    CwdNotTracked,
    /// Der Runner des argv ist nicht der berichtete.
    RunnerMismatch,
    /// Das Zeitbudget des Laufs ist aufgebraucht.
    TotalTimeout,
    /// Das Programm ließ sich nicht starten (nicht installiert).
    NotStarted,
    /// Das argv passiert die Redaction nicht — es wird nicht ausgeführt.
    Redactable,
    /// Die Ausgabe war zu lang; ihr Mittelteil fehlt.
    OutputTruncated,
    /// Ein Prozess überlebte den Befehl und hielt seine Ausgabe offen.
    ProcessLingered,
}

impl Skip {
    /// Der Grund im Record und in der Ausgabe.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAllowlisted => "not allowlisted",
            Self::CwdNotRecorded => "cwd not recorded",
            Self::CwdOutsideCheckout => "cwd outside the checkout",
            Self::CwdMissing => "cwd not in the checkout",
            Self::CwdNotTracked => "cwd not tracked by the commit",
            Self::RunnerMismatch => "runner differs from the recorded one",
            Self::TotalTimeout => "total time budget exhausted",
            Self::NotStarted => "command could not be run",
            Self::Redactable => "argv carries redactable text",
            Self::OutputTruncated => "output truncated",
            Self::ProcessLingered => "a process outlived the command",
        }
    }
}

/// Das gespeicherte `cwd` als Pfad **relativ** zum Checkout — rein
/// lexikalisch. `.` ist die Wurzel; alles mit `..`, absolut, leer oder mit
/// Backslash bleibt draußen. Ob der Ort existiert und (nach Symlinks)
/// innerhalb des Checkouts liegt, prüft der Aufrufer am Dateisystem.
pub fn confine_cwd(cwd: Option<&str>) -> Result<PathBuf, Skip> {
    match cwd {
        None => Err(Skip::CwdNotRecorded),
        Some(".") => Ok(PathBuf::new()),
        Some(cwd) if minds_core::observation::plain_relative(cwd) => Ok(PathBuf::from(cwd)),
        Some(_) => Err(Skip::CwdOutsideCheckout),
    }
}

// ---------------------------------------------------------------------------
// Vergleich
// ---------------------------------------------------------------------------

/// Was ein Replay-Lauf ergab, bevor es mit dem Bericht verglichen wird.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    /// Der Exit-Code; `None` bei Signal oder Abbruch.
    pub exit_code: Option<i32>,
    /// Die Deutung der Ausgabe (derselbe Parser wie beim Checkpoint),
    /// falls es eine gab.
    pub parsed: Option<ExecOutcome>,
    /// Nach Ablauf des Zeitlimits abgebrochen.
    pub timed_out: bool,
}

/// Die Beobachtung für den Record: Zahlen, und Benchmarks nur unter Namen,
/// die `expected` schon trägt — kein Name aus der Ausgabe des Replays
/// erreicht den Record.
pub fn observed(expected: &ExecOutcome, run: &Run) -> Observed {
    let known: BTreeSet<&str> = expected.benches.iter().map(|b| b.name.as_str()).collect();
    let mut seen = BTreeSet::new();
    let benches = run
        .parsed
        .iter()
        .flat_map(|p| &p.benches)
        .filter(|b| known.contains(b.name.as_str()) && seen.insert(b.name.as_str()))
        .cloned()
        .collect();
    Observed {
        exit_code: run.exit_code,
        tests: run.parsed.as_ref().and_then(|p| p.tests),
        benches,
        timed_out: run.timed_out,
    }
}

/// Vergleicht Bericht und Beobachtung (Deutung v1):
///
/// - Ein Bericht ohne jede Zahl (keine Tests, keine Benchmarks, kein
///   Exit-Code — EA-18a erzeugt ihn nie, ein untergeschobenes Envelope
///   schon): übersprungen, `nothing to compare`.
/// - Zeitüberschreitung oder Ende durch ein Signal: nicht reproduziert.
/// - Exit-Code: Trägt der Bericht einen, muss er gleich sein. Trägt er
///   keinen, heißt das bei Claude Code „Erfolg" (EA-18a: der Code fehlt
///   nur bei Erfolg) — meldet der Bericht dann auch keinen gescheiterten
///   Test, muss der Lauf mit 0 enden (`--cov-fail-under`, ein Absturz nach
///   der Zusammenfassung).
/// - Tests: der Status `failed == 0` und die Zahl `passed` müssen
///   übereinstimmen; abweichende `ignored` stehen im Grund, sind aber keine
///   Abweichung.
/// - Benchmarks: je Name `|beobachtet − berichtet| / berichtet ≤ Toleranz`,
///   ganzzahlig gerechnet; ein fehlender Name ist eine Abweichung.
pub fn compare(
    expected: &ExecOutcome,
    observed: &Observed,
    policy: &ReplayPolicy,
) -> (ReplayVerdict, Option<String>) {
    let not = |reason: String| (ReplayVerdict::NotReproduced, Some(reason));
    // Nichts zu vergleichen: kein Test (auch `0 passed, 0 failed` — ein
    // Filter, der nichts trifft) und kein Benchmark. Ein bloßer Exit-Code
    // belegt keinen Lauf der Tests.
    let no_tests = expected
        .tests
        .is_none_or(|t| t.passed == 0 && t.failed == 0);
    if no_tests && expected.benches.is_empty() {
        return (ReplayVerdict::Skipped, Some("nothing to compare".into()));
    }
    if observed.timed_out {
        return not("timed out".into());
    }
    let Some(exit) = observed.exit_code else {
        return not("terminated by a signal".into());
    };
    let reported_failure = expected.tests.is_some_and(|t| t.failed > 0);
    match expected.exit_code {
        Some(recorded) if exit != recorded => {
            return not(format!("exit code: recorded {recorded}, observed {exit}"));
        }
        None if !reported_failure && exit != 0 => {
            return not(format!("exit code: recorded success, observed {exit}"));
        }
        _ => {}
    }
    let mut note = None;
    if let Some(recorded) = expected.tests {
        let Some(seen) = observed.tests else {
            return not("no test summary in the output".into());
        };
        if (recorded.failed == 0) != (seen.failed == 0) {
            return not(format!(
                "tests: recorded {} failed, observed {} failed",
                recorded.failed, seen.failed
            ));
        }
        if recorded.passed != seen.passed {
            return not(format!(
                "tests: recorded {} passed, observed {} passed",
                recorded.passed, seen.passed
            ));
        }
        if recorded.ignored != seen.ignored {
            note = Some(format!(
                "ignored: recorded {}, observed {}",
                recorded.ignored, seen.ignored
            ));
        }
    }
    // Der Grund nennt den Benchmark über seine Position, nie über seinen
    // Namen: Ein Name aus dem Envelope (`password`) ergäbe neben einem
    // Doppelpunkt ein Schlüssel-Wert-Paar, das die Redaction beim Ablegen
    // verwirft — und mit ihm den ganzen Record samt Befund.
    for (index, bench) in expected.benches.iter().enumerate() {
        let number = index + 1;
        let Some(seen) = observed.benches.iter().find(|b| b.name == bench.name) else {
            return not(format!("bench #{number} missing"));
        };
        let pct = tolerance_for(policy, &bench.name);
        if !within(bench, seen, pct) {
            return not(format!(
                "bench #{number} recorded {}, observed {} (tolerance {pct}%)",
                bench.value, seen.value
            ));
        }
    }
    (ReplayVerdict::Reproduced, note)
}

/// `|beobachtet − berichtet| · 100 ≤ pct · berichtet`, ohne Gleitkomma.
fn within(recorded: &BenchValue, seen: &BenchValue, pct: u32) -> bool {
    if recorded.unit != seen.unit {
        return false;
    }
    let diff = u128::from(recorded.value.abs_diff(seen.value));
    diff * 100 <= u128::from(pct) * u128::from(recorded.value)
}

// ---------------------------------------------------------------------------
// Was ein Record für die Session zählt (EA-11)
// ---------------------------------------------------------------------------

/// Die Zählung eines Records gegen die **eigenen** entscheidenden Befehle
/// der Session — `None`, wenn der Record nicht zu dieser Session **und**
/// diesem Commit gehört oder eine andere Art oder Deutung (Schema, Version)
/// trägt.
///
/// Der Commit bindet: Ein Replay belegt nur den Checkout, auf dem er lief.
/// Ein Record von einem anderen Commit derselben Session (ein Wegwerf-Commit
/// mit eigener Policy, ein älterer Stand) zählt für den geprüften nie.
///
/// Ein Ergebnis zählt nur, wenn Turn, Aufruf, argv **und** der Bericht
/// (`expected`) genau dem entscheidenden Befehl der Session entsprechen —
/// ein Record, der Befehle auslässt, erfindet oder ihren Bericht ändert,
/// deckt sie nicht ab (`assess` meldet dann `replay covers …`). Jeder
/// Befehl zählt höchstens einmal. `signed` ist die Prüfung des Aufrufers
/// (gültig unter `minds-anchor`); ein unsignierter Record wird gezeigt,
/// zählt aber nie für A3.
pub fn summarize(
    id: SessionId,
    session: &Session,
    commit: &str,
    record: &ReplayRecord,
    signed: bool,
) -> Option<ReplaySummary> {
    if record.kind != minds_core::replay::REPLAY_KIND
        || record.schema != minds_core::replay::REPLAY_SCHEMA
        || record.interpretation_version != INTERPRETATION_VERSION
        || record.session != id.to_string()
        || record.commit != commit
    {
        return None;
    }
    let decisive = decisive(session);
    let mut summary = ReplaySummary {
        signed,
        decisive: decisive.len(),
        reproduced: 0,
        not_reproduced: 0,
        skipped: 0,
    };
    for d in &decisive {
        let Some(result) = record.results.iter().find(|r| matches(r, d)) else {
            continue;
        };
        match result.verdict {
            ReplayVerdict::Reproduced => summary.reproduced += 1,
            ReplayVerdict::NotReproduced => summary.not_reproduced += 1,
            ReplayVerdict::Skipped => summary.skipped += 1,
        }
    }
    Some(summary)
}

fn matches(result: &ReplayResult, d: &Decisive<'_>) -> bool {
    let e = &result.expected;
    result.turn == d.turn
        && result.call == d.call
        && result.argv == d.outcome.command
        && e.class == d.outcome.class
        && e.runner == d.outcome.runner
        && e.exit_code == d.outcome.exit_code
        && e.tests == d.outcome.tests
        && e.benches == d.outcome.benches
}

/// Der Bericht eines entscheidenden Befehls in Record-Form.
pub fn expected(outcome: &ExecOutcome) -> minds_core::replay::Expected {
    minds_core::replay::Expected {
        class: outcome.class,
        runner: outcome.runner.clone(),
        exit_code: outcome.exit_code,
        tests: outcome.tests,
        benches: outcome.benches.clone(),
    }
}

/// Von mehreren Zählungen derselben Session die **schwächste**: die mit
/// den meisten `claim not reproduced`, dann den meisten übersprungenen,
/// dann den wenigsten reproduzierten. Ein wiederholter CI-Lauf kann einen
/// Fehlschlag nicht überdecken (fail-closed).
pub fn weakest(summaries: impl IntoIterator<Item = ReplaySummary>) -> Option<ReplaySummary> {
    summaries
        .into_iter()
        .max_by_key(|s| (s.not_reproduced, s.skipped, std::cmp::Reverse(s.reproduced)))
}

// ---------------------------------------------------------------------------
// Die abgelegten Records eines Repositorys
// ---------------------------------------------------------------------------

/// Die Replay-Records unter `refs/minds/anchors/replay/`, einmal gelesen:
/// Jeder Record wird gelesen und gegen seine Id geprüft, **behalten** werden
/// nur Id, Session und Commit (der volle Record wird erst für passende
/// erneut gelesen). Wer `refs/minds/*` mit vielen großen Records füllt,
/// verlangsamt `verify` — mehr nicht.
///
/// **Vergiftet** ist der Namensraum, wenn er sich nicht auflisten lässt
/// oder ein Ref mit Record-Namen keinen lesbaren, auf seine Id hashenden
/// Record trägt: Welche Session ein veränderter Record betraf, sagen nur
/// Bytes, die der Angreifer schrieb. Sonst ließe sich ein signierter
/// Fehlschlag durch Austausch seines Blobs still entfernen. Das **Löschen**
/// eines Refs oder sein Zurücksetzen auf den Commit vor `record.sig` fällt
/// so nicht auf — dagegen hilft erst ein Anker außerhalb von `refs/minds`
/// (EA-19).
#[derive(Debug, Default)]
pub struct ReplayIndex {
    entries: Vec<(minds_core::ContentHash, String, String)>,
    tainted: bool,
}

impl ReplayIndex {
    /// Liest den Namensraum.
    pub fn load(store: &dyn minds_store::ContextStore) -> Self {
        let mut index = Self::default();
        let Ok(ids) = store.list_replays() else {
            index.tainted = true;
            return index;
        };
        for id in ids {
            let Ok(Some(bytes)) = store.replay_bytes(&id) else {
                index.tainted = true;
                continue;
            };
            if ReplayRecord::id_of_bytes(&bytes) != id {
                index.tainted = true;
                continue;
            }
            // Nur der Kopf; der geparste Wert lebt nur hier.
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                index.tainted = true;
                continue;
            };
            let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_owned);
            match (text("kind"), text("session"), text("commit")) {
                (Some(kind), Some(session), Some(commit))
                    if kind == minds_core::replay::REPLAY_KIND =>
                {
                    index.entries.push((id, session, commit));
                }
                // Ein unversehrtes Dokument anderer Art: kein Replay.
                (Some(kind), _, _) if kind != minds_core::replay::REPLAY_KIND => {}
                // Ohne Art, oder ein Replay ohne Session/Commit: vergiftet.
                _ => index.tainted = true,
            }
        }
        index
    }

    /// Das Replay-Ergebnis der Session für genau `commit`.
    ///
    /// - Nur Records dieses Commits und dieser Session zählen, gezählt gegen
    ///   ihre **eigenen** entscheidenden Befehle ([`summarize`]).
    /// - `check(id, gespeicherte Bytes, Signatur)` ist die Prüfung des
    ///   Aufrufers (gültig unter `minds-anchor`, von einem darauf
    ///   beschränkten Principal).
    /// - Gültig signierte Records gehen vor; von mehreren gilt der
    ///   schwächste ([`weakest`]).
    /// - Eine vorhandene, aber ungültige Signatur, ein nicht mehr lesbarer
    ///   passender Record oder ein vergifteter Namensraum: Keiner zählt als
    ///   signiert (fail-closed) — sonst schöbe eine überschriebene
    ///   `record.sig` einen signierten Fehlschlag in den Topf der
    ///   unsignierten, und ein älterer Erfolg gälte wieder.
    /// - Ohne signierten wird der schwächste unsignierte gezeigt (`signed:
    ///   false`, zählt nie für A3).
    pub fn summary(
        &self,
        store: &dyn minds_store::ContextStore,
        id: SessionId,
        session: &Session,
        commit: &str,
        check: &dyn Fn(&minds_core::ContentHash, &str, &str) -> bool,
    ) -> Option<ReplaySummary> {
        let wanted = id.to_string();
        let mut invalid = self.tainted;
        let mut summaries = Vec::new();
        for (record_id, _, _) in self
            .entries
            .iter()
            .filter(|(_, s, c)| *s == wanted && c == commit)
        {
            let Ok(Some((record, bytes))) = store.get_replay(record_id) else {
                invalid = true;
                continue;
            };
            let signed = match store.replay_signature(record_id) {
                Ok(None) => false,
                Ok(Some(signature)) => {
                    let valid = String::from_utf8(bytes)
                        .is_ok_and(|text| check(record_id, &text, &signature));
                    invalid |= !valid;
                    valid
                }
                Err(_) => {
                    invalid = true;
                    false
                }
            };
            summaries.extend(summarize(id, session, commit, &record, signed));
        }
        if invalid {
            return weakest(summaries.into_iter().map(|summary| ReplaySummary {
                signed: false,
                ..summary
            }));
        }
        let (signed, unsigned): (Vec<_>, Vec<_>) =
            summaries.into_iter().partition(|summary| summary.signed);
        weakest(signed).or_else(|| weakest(unsigned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{Agent, Intent, Model, Role, TestCounts, ToolCall, Turn};

    const SPEC_POLICY: &str = r#"{"schema":1,"runners":{"cargo-test":{"argv0":"cargo","sub":["test","nextest"],"allow_flags":["-p","--package","--release","--lib","--test","--","--exact"]}},"tolerance":{"default_pct":25,"overrides":[{"bench":"sort/*","pct":15}]},"timeouts":{"per_command_s":600,"total_s":1800}}"#;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn policy() -> ReplayPolicy {
        ReplayPolicy::parse(SPEC_POLICY.as_bytes()).unwrap()
    }

    fn test_outcome(command: &[&str], passed: u64) -> ExecOutcome {
        ExecOutcome {
            class: ExecClass::Test,
            runner: "cargo-test".into(),
            command: argv(command),
            cwd: Some(".".into()),
            exit_code: None,
            tests: Some(TestCounts {
                passed,
                failed: 0,
                ignored: 0,
            }),
            benches: Vec::new(),
        }
    }

    fn bench(name: &str, value: u64) -> BenchValue {
        BenchValue {
            name: name.into(),
            value,
            unit: "ns".into(),
        }
    }

    fn session_with(outcomes: Vec<Option<ExecOutcome>>) -> Session {
        let mut session = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "m".into(),
            },
            Intent::default(),
        );
        for outcome in outcomes {
            session.turns.push(Turn {
                role: Role::Assistant,
                text: String::new(),
                tool_calls: vec![ToolCall {
                    name: "Bash".into(),
                    arguments: String::new(),
                    capture: None,
                    effect: None,
                    outcome,
                }],
                parent: None,
                at: None,
            });
        }
        session
    }

    #[test]
    fn decisive_is_the_last_occurrence_of_each_class_and_argv() {
        let session = session_with(vec![
            Some(test_outcome(&["cargo", "test"], 10)),
            None,
            Some(test_outcome(&["cargo", "test", "-p", "sort"], 3)),
            Some(test_outcome(&["cargo", "test"], 12)),
        ]);
        let got = decisive(&session);
        let positions: Vec<(u32, u32, u64)> = got
            .iter()
            .map(|d| (d.turn, d.call, d.outcome.tests.unwrap().passed))
            .collect();
        // `cargo test` zählt mit seinem letzten Auftreten (Turn 3), die
        // Reihenfolge folgt diesen letzten Auftreten.
        assert_eq!(positions, vec![(2, 0, 3), (3, 0, 12)]);
        // Deterministisch.
        assert_eq!(decisive(&session), got);

        // Dasselbe argv in zwei Verzeichnissen: zwei entscheidende Befehle.
        let mut a = test_outcome(&["cargo", "test"], 12);
        a.cwd = Some("crates/a".into());
        let mut b = test_outcome(&["cargo", "test"], 3);
        b.cwd = Some("crates/b".into());
        let session = session_with(vec![Some(a), Some(b)]);
        assert_eq!(decisive(&session).len(), 2);
    }

    #[test]
    fn tolerance_follows_the_first_matching_override() {
        let policy = policy();
        assert_eq!(tolerance_for(&policy, "sort/10k"), 15);
        assert_eq!(tolerance_for(&policy, "search/1k"), 25);
    }

    #[test]
    fn the_allowlist_admits_only_listed_runners_and_flags() {
        let policy = policy();
        for ok in [
            &["cargo", "test"][..],
            &["cargo", "test", "-p", "sort"],
            &["cargo", "test", "--package=sort", "--release"],
            &["cargo", "test", "--", "--exact", "sort::tests::stable"],
            &["cargo", "nextest", "run", "-p", "sort"],
        ] {
            assert!(allows(&policy, &argv(ok)), "{ok:?}");
        }
        for refused in [
            &["rm", "-rf", "/"][..],
            &["sh", "-c", "cargo test"],
            &["cargo"],
            &["cargo", "bench"],
            &["cargo", "+nightly", "test"],
            &["cargo", "test", "--manifest-path", "/tmp/evil/Cargo.toml"],
            &["cargo", "test", "--config", "build.rustc-wrapper='x'"],
            &["cargo", "test", "-p", "../outside"],
            &["cargo", "test", "-p", "/abs"],
            &["cargo", "test", "--package=../x"],
            &["cargo", "test", "@args.txt"],
            &["cargo", "test", "~/x"],
            &["cargo", "test", "C:\\x"],
            &["cargo", "test", "a\nb"],
            &["/usr/bin/cargo", "test"],
            &["python", "bench.py"],
        ] {
            assert!(!allows(&policy, &argv(refused)), "{refused:?}");
        }
        // Zu viele oder zu lange Argumente: nie.
        let mut huge = argv(&["cargo", "test"]);
        huge.extend(std::iter::repeat_n("a".repeat(4096), 600));
        assert!(!allows(&policy, &huge));
        let mut many = argv(&["cargo", "test"]);
        many.extend(std::iter::repeat_n("a".to_owned(), MAX_ARGS));
        assert!(!allows(&policy, &many));
        // Ohne Policy ist nichts erlaubt.
        assert!(!allows(&ReplayPolicy::none(), &argv(&["cargo", "test"])));
    }

    #[test]
    fn cwd_is_confined_lexically() {
        assert_eq!(confine_cwd(Some(".")), Ok(PathBuf::new()));
        assert_eq!(
            confine_cwd(Some("crates/sort")),
            Ok(PathBuf::from("crates/sort"))
        );
        assert_eq!(confine_cwd(None), Err(Skip::CwdNotRecorded));
        for escape in [
            "..",
            "../x",
            "crates/../../x",
            "/etc",
            "/home/anna/project",
            "",
            "a\\b",
            "./x",
        ] {
            assert_eq!(
                confine_cwd(Some(escape)),
                Err(Skip::CwdOutsideCheckout),
                "{escape:?}"
            );
        }
    }

    #[test]
    fn comparison_follows_the_spec() {
        let policy = policy();
        let expected = test_outcome(&["cargo", "test"], 12);
        let observed = |passed, failed, ignored, exit| Observed {
            exit_code: Some(exit),
            tests: Some(TestCounts {
                passed,
                failed,
                ignored,
            }),
            ..Observed::default()
        };
        assert_eq!(
            compare(&expected, &observed(12, 0, 0, 0), &policy),
            (ReplayVerdict::Reproduced, None)
        );
        // `ignored` wird genannt, ist aber keine Abweichung.
        assert_eq!(
            compare(&expected, &observed(12, 0, 2, 0), &policy),
            (
                ReplayVerdict::Reproduced,
                Some("ignored: recorded 0, observed 2".into())
            )
        );
        assert_eq!(
            compare(&expected, &observed(11, 0, 0, 0), &policy),
            (
                ReplayVerdict::NotReproduced,
                Some("tests: recorded 12 passed, observed 11 passed".into())
            )
        );
        assert_eq!(
            compare(&expected, &observed(12, 1, 0, 101), &policy).0,
            ReplayVerdict::NotReproduced
        );
        // Kein Exit-Code im Bericht heißt Erfolg: Ein Lauf, der die Zahlen
        // trifft, aber nicht mit 0 endet, ist keine Wiederholung.
        assert_eq!(
            compare(&expected, &observed(12, 0, 0, 1), &policy),
            (
                ReplayVerdict::NotReproduced,
                Some("exit code: recorded success, observed 1".into())
            )
        );
        let signal = Observed {
            exit_code: None,
            ..observed(12, 0, 0, 0)
        };
        assert_eq!(
            compare(&expected, &signal, &policy),
            (
                ReplayVerdict::NotReproduced,
                Some("terminated by a signal".into())
            )
        );
        // Ein Bericht ohne jede Zahl: nichts zu vergleichen.
        let empty = ExecOutcome {
            tests: None,
            ..expected.clone()
        };
        assert_eq!(
            compare(&empty, &observed(1, 0, 0, 0), &policy),
            (ReplayVerdict::Skipped, Some("nothing to compare".into()))
        );
        // Ein berichteter Fehlschlag ohne Exit-Code: Der Code wird nicht
        // verglichen, nur die Zahlen.
        let mut failing = expected.clone();
        failing.tests = Some(TestCounts {
            passed: 2,
            failed: 1,
            ignored: 0,
        });
        assert_eq!(
            compare(&failing, &observed(2, 1, 0, 101), &policy).0,
            ReplayVerdict::Reproduced
        );
        // Mit Exit-Code im Bericht: verglichen.
        failing.exit_code = Some(101);
        assert_eq!(
            compare(&failing, &observed(2, 1, 0, 1), &policy),
            (
                ReplayVerdict::NotReproduced,
                Some("exit code: recorded 101, observed 1".into())
            )
        );
        let no_summary = Observed {
            exit_code: Some(0),
            ..Observed::default()
        };
        assert_eq!(
            compare(&expected, &no_summary, &policy).1.as_deref(),
            Some("no test summary in the output")
        );
        let timed_out = Observed {
            timed_out: true,
            ..observed(12, 0, 0, 0)
        };
        assert_eq!(
            compare(&expected, &timed_out, &policy).0,
            ReplayVerdict::NotReproduced
        );
    }

    #[test]
    fn replay_bench_tolerance() {
        let policy = policy();
        let expected = ExecOutcome {
            class: ExecClass::Bench,
            runner: "cargo-bench-criterion".into(),
            command: argv(&["cargo", "bench"]),
            cwd: Some(".".into()),
            exit_code: None,
            tests: None,
            benches: vec![bench("sort/10k", 1000), bench("search/1k", 1000)],
        };
        let run = |sort, search| Observed {
            exit_code: Some(0),
            benches: vec![bench("sort/10k", sort), bench("search/1k", search)],
            ..Observed::default()
        };
        // Grenzen inklusive: sort/* 15 %, sonst 25 %.
        assert_eq!(
            compare(&expected, &run(1150, 1250), &policy).0,
            ReplayVerdict::Reproduced
        );
        assert_eq!(
            compare(&expected, &run(850, 750), &policy).0,
            ReplayVerdict::Reproduced
        );
        assert_eq!(
            compare(&expected, &run(1151, 1000), &policy),
            (
                ReplayVerdict::NotReproduced,
                Some("bench #1 recorded 1000, observed 1151 (tolerance 15%)".into())
            )
        );
        assert_eq!(
            compare(&expected, &run(1000, 1251), &policy).0,
            ReplayVerdict::NotReproduced
        );
        // Ein fehlender Benchmark ist eine Abweichung.
        let missing = Observed {
            benches: vec![bench("sort/10k", 1000)],
            ..run(0, 0)
        };
        assert_eq!(
            compare(&expected, &missing, &policy).1.as_deref(),
            Some("bench #2 missing")
        );
        // Ein berichteter Nullwert verlangt eine beobachtete Null.
        let zero = ExecOutcome {
            benches: vec![bench("noop", 0)],
            ..expected.clone()
        };
        let seen = |v| Observed {
            exit_code: Some(0),
            benches: vec![bench("noop", v)],
            ..Observed::default()
        };
        assert_eq!(
            compare(&zero, &seen(0), &policy).0,
            ReplayVerdict::Reproduced
        );
        assert_eq!(
            compare(&zero, &seen(1), &policy).0,
            ReplayVerdict::NotReproduced
        );
    }

    #[test]
    fn observed_carries_only_bench_names_the_session_already_has() {
        let expected = ExecOutcome {
            class: ExecClass::Bench,
            runner: "cargo-bench-criterion".into(),
            command: argv(&["cargo", "bench"]),
            cwd: None,
            exit_code: None,
            tests: None,
            benches: vec![bench("sort/10k", 1000)],
        };
        let parsed = ExecOutcome {
            benches: vec![
                bench("sort/10k", 1010),
                bench("ghp_leaked-from-output", 1),
                bench("sort/10k", 9999),
            ],
            ..expected.clone()
        };
        let got = observed(
            &expected,
            &Run {
                exit_code: Some(0),
                parsed: Some(parsed),
                timed_out: false,
            },
        );
        assert_eq!(got.benches, vec![bench("sort/10k", 1010)]);
    }

    /// Ein Record, wie ihn die Pipeline zum Ablegen freigibt.
    fn scanned(record: &minds_core::replay::ReplayRecord) -> minds_redact::ScannedReplayRecord {
        minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .scan_replay(record.clone())
            .unwrap()
    }

    const COMMIT: &str = "abababababababababababababababababababab";

    fn record_for(session: &Session, id: SessionId) -> ReplayRecord {
        let results = decisive(session)
            .iter()
            .map(|d| ReplayResult {
                turn: d.turn,
                call: d.call,
                argv: d.outcome.command.clone(),
                expected: expected(d.outcome),
                observed: None,
                verdict: ReplayVerdict::Reproduced,
                reason: None,
            })
            .collect();
        ReplayRecord {
            kind: minds_core::replay::REPLAY_KIND.into(),
            policy: None,
            project: None,
            schema: minds_core::replay::REPLAY_SCHEMA,
            commit: COMMIT.into(),
            session: id.to_string(),
            interpretation_version: INTERPRETATION_VERSION,
            results,
            environment: Default::default(),
        }
    }

    #[test]
    fn a_record_counts_only_against_the_sessions_own_decisive_commands() {
        let session = session_with(vec![
            Some(test_outcome(&["cargo", "test"], 12)),
            Some(test_outcome(&["cargo", "test", "-p", "sort"], 3)),
        ]);
        let id = SessionId::of(&session).unwrap();
        let record = record_for(&session, id);
        let full = summarize(id, &session, COMMIT, &record, true).unwrap();
        assert_eq!(
            (
                full.decisive,
                full.reproduced,
                full.not_reproduced,
                full.skipped
            ),
            (2, 2, 0, 0)
        );

        // Ein geänderter Bericht (12 → 13 passed) deckt den Befehl nicht ab.
        let mut forged = record.clone();
        forged.results[0].expected.tests.as_mut().unwrap().passed = 13;
        let s = summarize(id, &session, COMMIT, &forged, true).unwrap();
        assert_eq!((s.decisive, s.reproduced), (2, 1));

        // Ein ausgelassener Befehl ebenso; ein doppelter zählt einmal.
        let mut short = record.clone();
        short.results[1] = short.results[0].clone();
        let s = summarize(id, &session, COMMIT, &short, true).unwrap();
        assert_eq!((s.decisive, s.reproduced), (2, 1));

        // Fremde Session, andere Deutung: zählt gar nicht.
        let mut other = record.clone();
        other.session = format!("b3-{}", "00".repeat(32));
        assert_eq!(summarize(id, &session, COMMIT, &other, true), None);
        let mut newer = record.clone();
        newer.interpretation_version = INTERPRETATION_VERSION + 1;
        assert_eq!(summarize(id, &session, COMMIT, &newer, true), None);

        // Ein Record von einem anderen Commit zählt für diesen nie.
        assert_eq!(
            summarize(id, &session, &"cd".repeat(20), &record, true),
            None
        );
        // Ein Dokument einer anderen Art ebenso wenig.
        let mut foreign = record.clone();
        foreign.kind = "first-sight".into();
        assert_eq!(summarize(id, &session, COMMIT, &foreign, true), None);
    }

    #[test]
    fn the_weakest_summary_wins() {
        let summary = |reproduced, not_reproduced, skipped| ReplaySummary {
            signed: true,
            decisive: 3,
            reproduced,
            not_reproduced,
            skipped,
        };
        assert_eq!(
            weakest([summary(3, 0, 0), summary(2, 1, 0), summary(2, 0, 1)]),
            Some(summary(2, 1, 0))
        );
        assert_eq!(
            weakest([summary(3, 0, 0), summary(2, 0, 1)]),
            Some(summary(2, 0, 1))
        );
        assert_eq!(weakest([]), None);
    }

    #[test]
    fn glob_matches_bench_patterns() {
        assert!(glob(b"sort/*", b"sort/10k"));
        assert!(glob(b"sort/*", b"sort/"));
        assert!(!glob(b"sort/*", b"search/1k"));
        assert!(glob(b"*/1?k", b"sort/10k"));
        assert!(glob(b"*", b""));
        assert!(!glob(b"a", b"ab"));
        assert!(glob(b"a*b*c", b"axxbyyc"));
        assert!(!glob(b"a*b*c", b"axxbyy"));
        assert!(glob(b"**", b"x"));
    }

    #[test]
    fn a_pathological_glob_stays_linear() {
        // Muster und Name können beide vom Agenten stammen.
        let pattern = format!("{}b", "*a".repeat(20));
        let name = "a".repeat(4000);
        let started = std::time::Instant::now();
        assert!(!glob(pattern.as_bytes(), name.as_bytes()));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    // --- ReplayIndex: Bindung, Vorrang, Vergiftung --------------------------

    const VALID: &str = "-----BEGIN SSH SIGNATURE-----\nVkFMSUQ=\n-----END SSH SIGNATURE-----\n";
    const FORGED: &str = "-----BEGIN SSH SIGNATURE-----\nRk9SR0VE\n-----END SSH SIGNATURE-----\n";

    /// Die Prüfung des Aufrufers, gefälscht: gültig ist genau [`VALID`].
    fn check(_: &minds_core::ContentHash, _: &str, signature: &str) -> bool {
        signature == VALID
    }

    fn git(root: &std::path::Path, args: &[&str], input: Option<&str>) -> String {
        use std::io::Write as _;
        let mut child = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        if let Some(input) = input {
            stdin.write_all(input.as_bytes()).unwrap();
        }
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// Ein Ref im Namensraum, dessen `record` `bytes` trägt.
    fn plant(root: &std::path::Path, name: &str, bytes: &str) {
        let blob = git(root, &["hash-object", "-w", "--stdin"], Some(bytes));
        let tree = git(
            root,
            &["mktree"],
            Some(&format!("100644 blob {blob}\trecord\n")),
        );
        let commit = git(root, &["commit-tree", &tree, "-m", "planted"], None);
        git(
            root,
            &[
                "update-ref",
                &format!("refs/minds/anchors/replay/{name}"),
                &commit,
            ],
            None,
        );
    }

    #[test]
    fn the_replay_index_binds_prefers_signed_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "--template="], None);
        let store = minds_store::InRepoStore::open(root).unwrap();
        let session = session_with(vec![Some(test_outcome(&["cargo", "test"], 12))]);
        let id = SessionId::of(&session).unwrap();
        let summary = |store: &minds_store::InRepoStore, commit: &str| {
            ReplayIndex::load(store).summary(store, id, &session, commit, &check)
        };

        // Ein signierter Erfolg.
        let pass = record_for(&session, id);
        let pass_id = minds_store::ContextStore::put_replay(&store, &scanned(&pass)).unwrap();
        minds_store::ContextStore::put_replay_signature(&store, &pass_id, VALID).unwrap();
        let s = summary(&store, COMMIT).unwrap();
        assert!(s.signed);
        assert_eq!((s.reproduced, s.not_reproduced), (1, 0));
        // Gebunden an den Commit.
        assert_eq!(summary(&store, &"cd".repeat(20)), None);

        // Ein unsignierter Fehlschlag ändert nichts: Signierte gehen vor.
        let mut fail = pass.clone();
        fail.results[0].verdict = ReplayVerdict::NotReproduced;
        let fail_id = minds_store::ContextStore::put_replay(&store, &scanned(&fail)).unwrap();
        let s = summary(&store, COMMIT).unwrap();
        assert!(s.signed);
        assert_eq!(s.not_reproduced, 0);

        // Signiert, gilt der schwächste.
        minds_store::ContextStore::put_replay_signature(&store, &fail_id, VALID).unwrap();
        let s = summary(&store, COMMIT).unwrap();
        assert!(s.signed);
        assert_eq!(s.not_reproduced, 1);

        // Eine überschriebene Signatur am Fehlschlag: keiner signiert.
        minds_store::ContextStore::put_replay_signature(&store, &fail_id, FORGED).unwrap();
        assert!(!summary(&store, COMMIT).unwrap().signed);
        minds_store::ContextStore::put_replay_signature(&store, &fail_id, VALID).unwrap();
        assert!(summary(&store, COMMIT).unwrap().signed);

        // Der Blob des Fehlschlags ausgetauscht — auch gegen einen, der eine
        // fremde Session nennt: keiner signiert, für keine Session.
        let elsewhere = format!(
            r#"{{"kind":"replay","session":"b3-{}","commit":"{}"}}"#,
            "00".repeat(32),
            "00".repeat(20)
        );
        plant(root, fail_id.hex(), &elsewhere);
        assert!(!summary(&store, COMMIT).unwrap().signed);
    }

    #[test]
    fn garbage_or_empty_record_refs_taint_the_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "--template="], None);
        let store = minds_store::InRepoStore::open(root).unwrap();
        let session = session_with(vec![Some(test_outcome(&["cargo", "test"], 12))]);
        let id = SessionId::of(&session).unwrap();
        let pass = record_for(&session, id);
        let pass_id = minds_store::ContextStore::put_replay(&store, &scanned(&pass)).unwrap();
        minds_store::ContextStore::put_replay_signature(&store, &pass_id, VALID).unwrap();
        let signed = |store: &minds_store::InRepoStore| {
            ReplayIndex::load(store)
                .summary(store, id, &session, COMMIT, &check)
                .unwrap()
                .signed
        };
        assert!(signed(&store));
        plant(root, &"ef".repeat(32), "garbage");
        assert!(!signed(&store));
    }
}
