//! Die Session als Schrittliste (C11): ein Schritt je Tool-Aufruf, in
//! Session-Reihenfolge, mit Verb, Ziel, Ergebnis und Zeit seit Start.
//!
//! Der Graph ([`crate::graph`]) zeichnet die Session als Baum mit immer
//! derselben Gestalt (Absicht → Agent → Züge → Aufrufe → Änderung → Review).
//! Die Story beantwortet die Frage der Seite direkt: **Was hat der Agent
//! zwischen Prompt und Commit getan — in welcher Reihenfolge, mit welchem
//! Ergebnis?** Verzweigung gibt es nur, wo sie existiert (Sub-Agents).
//!
//! # Was hier gerechnet wird — und was nicht
//!
//! - **Verben** kommen aus Tool und Effekt (`wrote`, `edited`, `read`, …);
//!   für Kommandos aus dem gedeuteten Lauf (`ExecOutcome`) oder dem ersten
//!   Wort des Kommandos (`tests`, `lint`, `build`). Das Verb ist eine
//!   Lesehilfe, kein Beleg.
//! - **Ergebnisse** stehen nur da, wenn der Record sie kennt
//!   ([`minds_core::ExecOutcome`], Exit-Code, [`minds_core::CaptureNote`]) —
//!   sonst bleibt das Feld leer. Nie geraten: `ruff check` ohne gelesenes
//!   Ergebnis ist kein „clean".
//! - **Schreib-Claims** sagen nur, ob ein Schreibzeit-Hash aufgezeichnet ist.
//!   Ob ein Witness den Claim bestätigt, rechnet
//!   [`crate::observations::corroborate`] gegen den Store — nicht hier.
//! - **Pfade** sind repo-relativ: Das Arbeitsverzeichnis der Session
//!   (`Lineage::cwd`, es trägt den Benutzernamen) erscheint weder in Zielen
//!   noch in Kommandos; außerhalb des Repos bleibt `…/<dateiname>`.
//! - Alle Texte sind entschärft ([`crate::text`]); Kommandos und Zugtexte
//!   sind auf [`COMMAND_MAX`] Zeichen gekappt und sagen, wie viel fehlt.

use jiff::Timestamp;
use minds_core::{
    CaptureNote, CaptureStatus, EdgeKind, EffectKind, Endpoint, ExecClass, Role, Session,
    SessionId, WrittenUnavailable,
};

use crate::index::Index;
use crate::text::{sanitize, sanitize_lines, sanitize_path};

/// Auf so viele Zeichen werden Kommandos, Argumente und Zugtexte im Detail
/// gekappt (C8) — danach steht, wie viele fehlen.
pub const COMMAND_MAX: usize = 4_000;

/// Auf so viele Zeichen wird das Ziel eines Schritts in der Liste gekürzt;
/// das Detail trägt es vollständig (bis [`COMMAND_MAX`]).
pub const TARGET_MAX: usize = 200;

/// Was der Agent in einem Schritt getan hat — aus Tool und Effekt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verb {
    /// Ein Kommando (Bash), ohne erkannten Runner.
    Ran,
    /// Ein Testlauf (gedeuteter Runner oder `cargo test`, `pytest`, …).
    Tests,
    /// Ein Benchmark-Lauf.
    Bench,
    /// Ein Lint- oder Format-Check (`cargo clippy`, `ruff`, …).
    Lint,
    /// Ein Build (`cargo build`, `go build`, …).
    Build,
    /// Eine Datei geschrieben (Write).
    Wrote,
    /// Eine Datei geändert (Edit, MultiEdit, Patch).
    Edited,
    /// Eine Datei gelöscht.
    Deleted,
    /// Eine Datei gelesen.
    Read,
    /// Gesucht (Grep, Glob, …).
    Searched,
    /// Von außen geholt (WebFetch, WebSearch).
    Fetched,
    /// An einen Sub-Agent delegiert (Task).
    Delegated,
    /// Ein Tool ohne gedeutete Wirkung — mit Namen.
    Tool,
}

impl Verb {
    /// Das Wort in der Liste (`wrote`, `tests`, …).
    pub const fn word(self) -> &'static str {
        match self {
            Verb::Ran => "ran",
            Verb::Tests => "tests",
            Verb::Bench => "bench",
            Verb::Lint => "lint",
            Verb::Build => "build",
            Verb::Wrote => "wrote",
            Verb::Edited => "edited",
            Verb::Deleted => "deleted",
            Verb::Read => "read",
            Verb::Searched => "searched",
            Verb::Fetched => "fetched",
            Verb::Delegated => "delegated",
            Verb::Tool => "tool",
        }
    }

    /// Ob der Schritt eine Datei schreibt oder ändert — nur solche Schritte
    /// tragen einen Schreib-Claim.
    pub const fn writes(self) -> bool {
        matches!(self, Verb::Wrote | Verb::Edited)
    }

    /// Ob der Schritt ein Kommando ist.
    pub const fn executes(self) -> bool {
        matches!(
            self,
            Verb::Ran | Verb::Tests | Verb::Bench | Verb::Lint | Verb::Build
        )
    }
}

/// Das Ergebnis eines Kommandos — ausschließlich, was der Record kennt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Die Zähler eines gedeuteten Testlaufs.
    Tests {
        /// Bestanden.
        passed: u64,
        /// Gescheitert.
        failed: u64,
        /// Ausgelassen.
        ignored: u64,
    },
    /// So viele Benchmark-Werte eines gedeuteten Laufs.
    Benches(usize),
    /// Nur der Exit-Code ist bekannt (Claude Code nennt ihn bei Fehlschlag).
    Exit(i32),
    /// Ein bekannter Runner lief, aber sein Ergebnis wurde nicht gelesen.
    NotCaptured(CaptureNote),
}

impl Outcome {
    /// `true`, wenn das Ergebnis ein Scheitern meldet.
    pub const fn failed(self) -> bool {
        match self {
            Outcome::Tests { failed, .. } => failed > 0,
            Outcome::Exit(code) => code != 0,
            Outcome::Benches(_) | Outcome::NotCaptured(_) => false,
        }
    }
}

/// Was ein Schreib-Schritt über sich selbst sagt: ob der Schreibzeit-Hash
/// ([`minds_core::Effect::written`]) aufgezeichnet ist. Ohne Hash kann kein
/// Witness den Claim bestätigen — die Zeile bleibt „reported".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteClaim {
    /// Der Hash der geschriebenen Bytes liegt vor.
    Hashed,
    /// Kein Hash — mit dem Grund, wenn der Adapter einen nennt.
    Unhashed(Option<WrittenUnavailable>),
}

/// Ein Schritt: ein Tool-Aufruf, in der Sprache der Oberfläche.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Die laufende Nummer, 1-basiert.
    pub number: usize,
    /// Index des Zugs in [`Session::turns`].
    pub turn: usize,
    /// Index des Aufrufs in [`minds_core::Turn::tool_calls`].
    pub call: usize,
    /// Das Verb.
    pub verb: Verb,
    /// Der Name des Tools, entschärft.
    pub tool: String,
    /// Das Ziel — Pfad, Kommando (eine Zeile), Auftrag an den Sub-Agent
    /// oder der Tool-Name; entschärft, ohne Arbeitsverzeichnis, auf
    /// [`TARGET_MAX`] gekürzt.
    pub target: String,
    /// Das Ergebnis, wenn der Record eines kennt.
    pub outcome: Option<Outcome>,
    /// Der Schreib-Claim bei `wrote`/`edited`.
    pub claim: Option<WriteClaim>,
    /// Beobachtet, aber von keinem Adapter gedeutet (ADR-0011) — die
    /// Deutungs-Achse, nie mit einem Verb überdeckt.
    pub uninterpreted: bool,
    /// Der Zeitpunkt des Zugs, RFC 3339, falls erfasst.
    pub at: Option<String>,
    /// Sekunden seit Session-Start, falls beide Zeiten lesbar sind.
    pub offset_secs: Option<i64>,
    /// Bei `delegated`: die Kind-Session, wenn sie sich eindeutig zuordnen
    /// lässt (siehe [`Story::of`]).
    pub child: Option<SessionId>,
    /// Schlüssel/Wert-Details für die Detailbox, entschärft — das Kommando
    /// vollständig, der Pfad, Runner und Ergebnis, der Capture-Status.
    pub detail: Vec<(String, String)>,
}

/// Ein Zug als Trenner zwischen Schritten: Rolle, Text und der erste
/// Schritt, der zu ihm gehört.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryTurn {
    /// Index in [`Session::turns`].
    pub index: usize,
    /// Die Rolle.
    pub role: Role,
    /// Der Text, entschärft, auf [`COMMAND_MAX`] gekappt.
    pub text: String,
    /// Der Zeitpunkt, falls erfasst.
    pub at: Option<String>,
    /// Sekunden seit Session-Start.
    pub offset_secs: Option<i64>,
    /// Index des ersten Schritts dieses Zugs in [`Story::steps`] — `None`
    /// bei einem Zug ohne Tool-Aufrufe.
    pub first_step: Option<usize>,
}

/// Die Story einer Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Story {
    /// Die Session.
    pub id: SessionId,
    /// Der Auftrag des Menschen, vollständig, zeilenweise entschärft.
    pub prompt: String,
    /// Beginn (Lineage, sonst der erste datierte Zug), RFC 3339.
    pub started_at: Option<String>,
    /// Ende (Lineage, sonst der letzte datierte Zug), RFC 3339.
    pub ended_at: Option<String>,
    /// Dauer in Sekunden, falls Beginn und Ende lesbar sind.
    pub duration_secs: Option<i64>,
    /// Die Schritte, in Session-Reihenfolge.
    pub steps: Vec<Step>,
    /// Die Züge, in Session-Reihenfolge.
    pub turns: Vec<StoryTurn>,
    /// Gestartete Sub-Agents, soweit im Index auflösbar, in Kanten-Reihenfolge.
    pub subagents: Vec<SessionId>,
}

/// Ein Lauf gleichartiger Schritte für die Zusammenfassung (Zoom *Summary*):
/// `3 test runs`, `2 edits of src/a.py`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Index des ersten Schritts in [`Story::steps`].
    pub first: usize,
    /// Wie viele Schritte der Lauf zusammenfasst (≥ 1).
    pub count: usize,
    /// Das gemeinsame Verb.
    pub verb: Verb,
    /// Das gemeinsame Ziel — bei Datei-Schritten der Pfad, sonst `None`.
    pub target: Option<String>,
    /// Das Ergebnis des **letzten** Schritts — ein früheres sagt nichts
    /// über den Stand am Ende.
    pub outcome: Option<Outcome>,
}

impl Story {
    /// Baut die Story einer Session. `index` löst Sub-Agents auf.
    ///
    /// Ein `delegated`-Schritt bekommt seine Kind-Session nur, wenn die
    /// Zuordnung eindeutig ist: genau so viele `Spawned`-Kanten wie
    /// Delegationen, dann der Reihe nach. Sonst bleibt `child` leer und die
    /// Kinder stehen nur in [`Story::subagents`] — keine geratene Zuordnung.
    pub fn of(id: SessionId, session: &Session, index: &Index) -> Self {
        let lineage = session.lineage.as_ref();
        let cwd = lineage.and_then(|l| l.cwd.as_deref());
        let dated = |picker: fn(&[&str]) -> Option<String>| {
            let times: Vec<&str> = session
                .turns
                .iter()
                .filter_map(|t| t.at.as_deref())
                .filter(|at| timestamp(at).is_some())
                .collect();
            picker(&times)
        };
        let started_at = lineage
            .and_then(|l| l.started_at.clone())
            .or_else(|| dated(|t| t.first().map(|s| s.to_string())));
        let ended_at = lineage
            .and_then(|l| l.ended_at.clone())
            .or_else(|| dated(|t| t.last().map(|s| s.to_string())));
        let start = started_at.as_deref().and_then(timestamp);
        let end = ended_at.as_deref().and_then(timestamp);
        let duration_secs = match (start, end) {
            (Some(start), Some(end)) => Some((end - start).get_seconds()),
            _ => None,
        };
        let offset = |at: Option<&str>| -> Option<i64> {
            let at = timestamp(at?)?;
            Some((at - start?).get_seconds())
        };

        // Die Kanten zu Sub-Agents, in Reihenfolge: aufgelöst oder nicht —
        // die Zahl entscheidet, ob eine Zuordnung eindeutig ist.
        let spawned: Vec<Option<SessionId>> = session
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Spawned)
            .map(|edge| match &edge.to {
                Endpoint::Session { agent, local_id } => index.resolve_endpoint(agent, local_id),
                Endpoint::Commit { .. } => None,
            })
            .collect();
        let subagents: Vec<SessionId> = spawned.iter().flatten().copied().collect();

        let mut steps: Vec<Step> = Vec::new();
        let mut turns: Vec<StoryTurn> = Vec::with_capacity(session.turns.len());
        for (turn_index, turn) in session.turns.iter().enumerate() {
            let at = turn.at.clone();
            let offset_secs = offset(at.as_deref());
            let first_step = (!turn.tool_calls.is_empty()).then_some(steps.len());
            turns.push(StoryTurn {
                index: turn_index,
                role: turn.role.clone(),
                text: cap(&sanitize(turn.text.trim())),
                at: at.clone(),
                offset_secs,
                first_step,
            });
            for (call_index, call) in turn.tool_calls.iter().enumerate() {
                let uninterpreted = call
                    .capture
                    .as_ref()
                    .is_some_and(|c| c.status == CaptureStatus::Uninterpreted);
                let effect = call.effect.as_ref();
                let kind = if uninterpreted {
                    None
                } else {
                    effect.map(|e| e.kind)
                };
                let name = sanitize(&call.name);
                let command = (kind == Some(EffectKind::Exec))
                    .then(|| raw_command(&call.arguments).map(|c| strip_cwd(&c, cwd)))
                    .flatten();
                let verb = verb_of(&call.name, kind, command.as_deref(), call.outcome.as_ref());
                let path = effect
                    .and_then(|e| e.path.as_deref())
                    .map(|p| relative_path(p, cwd));
                let (target, mut detail) = match verb {
                    _ if verb.executes() => {
                        let full = command.clone().unwrap_or_else(|| name.clone());
                        let line = first_line(&full);
                        let mut detail = vec![("Command".to_string(), cap(&sanitize(&full)))];
                        if let Some(outcome) = &call.outcome {
                            detail.push(("Runner".into(), sanitize(&outcome.runner)));
                            if let Some(cwd) = &outcome.cwd {
                                detail.push(("Working dir".into(), sanitize_path(cwd)));
                            }
                        }
                        detail.push((
                            "Output".into(),
                            "stdout/stderr not recorded (by design)".into(),
                        ));
                        (clip(&sanitize(line), TARGET_MAX), detail)
                    }
                    Verb::Delegated => {
                        let brief = delegation_brief(&call.arguments).map(|b| strip_cwd(&b, cwd));
                        let mut detail = Vec::new();
                        if let Some(prompt) = prompt_of(&call.arguments) {
                            detail
                                .push(("Prompt".into(), cap(&sanitize(&strip_cwd(&prompt, cwd)))));
                        }
                        let target = brief
                            .map(|b| clip(&sanitize(first_line(&b)), TARGET_MAX))
                            .unwrap_or_else(|| name.clone());
                        (target, detail)
                    }
                    _ => {
                        let target = path
                            .clone()
                            .map(|p| clip(&p, TARGET_MAX))
                            .unwrap_or_else(|| name.clone());
                        let mut detail = Vec::new();
                        if let Some(p) = &path {
                            detail.push(("Path".into(), p.clone()));
                        }
                        (target, detail)
                    }
                };
                detail.insert(0, ("Tool".into(), name.clone()));
                if !call.arguments.trim().is_empty() && !verb.executes() {
                    detail.push((
                        "Arguments".into(),
                        cap(&sanitize(&strip_cwd(&call.arguments, cwd))),
                    ));
                }
                if let Some(capture) = &call.capture {
                    let status = match capture.status {
                        CaptureStatus::Interpreted => "interpreted",
                        CaptureStatus::Uninterpreted => {
                            "observed, not interpreted — effect unknown"
                        }
                    };
                    detail.push((
                        "Interpretation".into(),
                        format!(
                            "{status} ({} v{})",
                            sanitize(&capture.adapter),
                            capture.adapter_version
                        ),
                    ));
                }
                let claim =
                    (verb.writes()).then(|| match effect.and_then(|e| e.written.as_ref()) {
                        Some(_) => WriteClaim::Hashed,
                        None => WriteClaim::Unhashed(effect.and_then(|e| e.written_unavailable)),
                    });
                if let Some(hash) = effect.and_then(|e| e.written.as_ref()) {
                    detail.push(("Written".into(), hash.to_string()));
                }
                let outcome = outcome_of(call);
                if let Some(outcome) = outcome {
                    detail.push(("Result".into(), outcome_words(outcome)));
                }
                steps.push(Step {
                    number: steps.len() + 1,
                    turn: turn_index,
                    call: call_index,
                    verb,
                    tool: name,
                    target,
                    outcome,
                    claim,
                    uninterpreted,
                    at: at.clone(),
                    offset_secs,
                    child: None,
                    detail,
                });
            }
        }

        // Delegationen ↔ Kinder: nur bei gleicher Zahl, der Reihe nach.
        let delegated: Vec<usize> = steps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.verb == Verb::Delegated)
            .map(|(i, _)| i)
            .collect();
        if !delegated.is_empty() && delegated.len() == spawned.len() {
            for (i, child) in delegated.into_iter().zip(spawned) {
                steps[i].child = child;
            }
        }

        Self {
            id,
            prompt: sanitize_lines(session.intent.request.split('\n')),
            started_at,
            ended_at,
            duration_secs,
            steps,
            turns,
            subagents,
        }
    }
}

/// Fasst aufeinanderfolgende Schritte gleicher Art zusammen: dasselbe Verb
/// und — bei Datei-Schritten — dasselbe Ziel. Das Ergebnis eines Laufs ist
/// das seines letzten Schritts.
pub fn summarize(steps: &[Step]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let target =
            (!step.verb.executes() && step.verb != Verb::Delegated).then(|| step.target.clone());
        match runs.last_mut() {
            Some(run) if run.verb == step.verb && run.target == target => {
                run.count += 1;
                run.outcome = step.outcome;
            }
            _ => runs.push(Run {
                first: i,
                count: 1,
                verb: step.verb,
                target,
                outcome: step.outcome,
            }),
        }
    }
    runs
}

/// Ein Pfad ohne das Arbeitsverzeichnis der Session: `cwd/src/a.rs` →
/// `src/a.rs`; ein absoluter Pfad außerhalb → `…/<dateiname>`; relative
/// Pfade bleiben. Entschärft wie ein Pfad.
pub fn relative_path(path: &str, cwd: Option<&str>) -> String {
    let stripped = cwd.filter(|cwd| !cwd.is_empty()).and_then(|cwd| {
        let cwd = cwd.trim_end_matches('/');
        path.strip_prefix(cwd)
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|rest| !rest.is_empty())
    });
    let shown = match stripped {
        Some(rest) => rest.to_string(),
        None if path.starts_with('/') || path.starts_with('~') || path.get(1..3) == Some(":\\") => {
            let base = path
                .rsplit(['/', '\\'])
                .find(|part| !part.is_empty())
                .unwrap_or(path);
            format!("…/{base}")
        }
        None => path.to_string(),
    };
    sanitize_path(&shown)
}

/// Nimmt das Arbeitsverzeichnis aus einem Text (Kommando, Argumente):
/// `cwd/` fällt weg, ein alleinstehendes `cwd` wird `.`.
fn strip_cwd(text: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd
        .map(|c| c.trim_end_matches('/'))
        .filter(|c| !c.is_empty())
    else {
        return text.to_string();
    };
    text.replace(&format!("{cwd}/"), "").replace(cwd, ".")
}

/// Das Verb aus Tool-Name, Effekt, Kommando und gedeutetem Lauf.
fn verb_of(
    name: &str,
    kind: Option<EffectKind>,
    command: Option<&str>,
    outcome: Option<&minds_core::ExecOutcome>,
) -> Verb {
    let lower = name.to_ascii_lowercase();
    match kind {
        Some(EffectKind::Exec) => match outcome.map(|o| o.class) {
            Some(ExecClass::Test) if outcome.is_some_and(|o| o.tests.is_some()) => Verb::Tests,
            Some(ExecClass::Bench) => Verb::Bench,
            _ => command.map(command_verb).unwrap_or(Verb::Ran),
        },
        Some(EffectKind::Write) => {
            if lower.contains("edit") || lower.contains("patch") {
                Verb::Edited
            } else {
                Verb::Wrote
            }
        }
        Some(EffectKind::Read) => Verb::Read,
        Some(EffectKind::Delete) => Verb::Deleted,
        Some(EffectKind::Other) | None => {
            if lower.contains("grep") || lower.contains("glob") || lower.contains("search") {
                if lower.contains("web") {
                    Verb::Fetched
                } else {
                    Verb::Searched
                }
            } else if lower.contains("fetch") || lower.contains("web") {
                Verb::Fetched
            } else if lower == "task" || lower.contains("agent") || lower.contains("delegat") {
                Verb::Delegated
            } else {
                Verb::Tool
            }
        }
    }
}

/// Das Verb eines Kommandos aus seinen ersten Wörtern — eine Lesehilfe,
/// kein Ergebnis: `tests`, `lint`, `build`, sonst `ran`.
fn command_verb(command: &str) -> Verb {
    let line = first_line(command);
    let words: Vec<&str> = line
        .split_whitespace()
        .skip_while(|w| w.contains('=') || *w == "sudo" || *w == "env" || *w == "time")
        .take(4)
        .collect();
    let [first, rest @ ..] = words.as_slice() else {
        return Verb::Ran;
    };
    let first = first.rsplit('/').next().unwrap_or(first);
    let second = rest.first().copied().unwrap_or("");
    let third = rest.get(1).copied().unwrap_or("");
    match first {
        "cargo" => match second {
            "test" | "nextest" => Verb::Tests,
            "bench" => Verb::Bench,
            "clippy" | "fmt" | "audit" | "deny" => Verb::Lint,
            "build" | "check" => Verb::Build,
            _ => Verb::Ran,
        },
        "pytest" | "jest" | "vitest" | "mocha" | "ctest" | "rspec" => Verb::Tests,
        "ruff" | "eslint" | "flake8" | "pylint" | "mypy" | "golangci-lint" | "shellcheck"
        | "prettier" | "black" | "isort" | "clippy-driver" | "rustfmt" | "gofmt" | "biome" => {
            Verb::Lint
        }
        "make" | "cmake" | "ninja" | "gradle" | "gradlew" | "./gradlew" | "mvn" | "tsc" => {
            Verb::Build
        }
        "go" => match second {
            "test" => Verb::Tests,
            "vet" | "fmt" => Verb::Lint,
            "build" => Verb::Build,
            _ => Verb::Ran,
        },
        "python" | "python3" | "uv" => {
            let module = if second == "run" { third } else { second };
            let module = if module == "-m" {
                rest.iter()
                    .skip_while(|w| **w != "-m")
                    .nth(1)
                    .copied()
                    .unwrap_or("")
            } else {
                module
            };
            match module {
                "pytest" | "unittest" => Verb::Tests,
                "ruff" | "mypy" | "flake8" | "black" | "pylint" => Verb::Lint,
                _ => Verb::Ran,
            }
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            let script = if second == "run" { third } else { second };
            match script {
                "test" => Verb::Tests,
                "lint" | "format" => Verb::Lint,
                "build" => Verb::Build,
                _ => Verb::Ran,
            }
        }
        _ => Verb::Ran,
    }
}

/// Das Ergebnis eines Aufrufs — nur aus dem Record.
fn outcome_of(call: &minds_core::ToolCall) -> Option<Outcome> {
    if let Some(outcome) = &call.outcome {
        if let Some(tests) = outcome.tests {
            return Some(Outcome::Tests {
                passed: tests.passed,
                failed: tests.failed,
                ignored: tests.ignored,
            });
        }
        if !outcome.benches.is_empty() {
            return Some(Outcome::Benches(outcome.benches.len()));
        }
        if let Some(code) = outcome.exit_code {
            return Some(Outcome::Exit(code));
        }
    }
    call.capture
        .as_ref()
        .and_then(|c| c.note)
        .map(Outcome::NotCaptured)
}

/// Das Ergebnis in Worten, für die Detailbox.
fn outcome_words(outcome: Outcome) -> String {
    match outcome {
        Outcome::Tests {
            passed,
            failed,
            ignored,
        } => {
            let mut words = format!("{passed} passed");
            if failed > 0 {
                words.push_str(&format!(", {failed} failed"));
            }
            if ignored > 0 {
                words.push_str(&format!(", {ignored} ignored"));
            }
            words
        }
        Outcome::Benches(n) => format!("{n} benchmark values"),
        Outcome::Exit(code) => format!("exit {code}"),
        Outcome::NotCaptured(note) => note.as_str().to_string(),
    }
}

/// Der rohe Kommando-String aus den Argumenten: die gängigen Schlüssel,
/// sonst der getrimmte Rohstring. Vollständig — gekürzt wird erst für die
/// Anzeige.
fn raw_command(arguments: &str) -> Option<String> {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(arguments)
    {
        for key in ["command", "cmd", "script", "code"] {
            if let Some(serde_json::Value::String(s)) = map.get(key)
                && !s.trim().is_empty()
            {
                return Some(s.trim().to_string());
            }
        }
    }
    let trimmed = arguments.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Ein JSON-Feld der Argumente als Text.
fn field_of(arguments: &str, key: &str) -> Option<String> {
    let serde_json::Value::Object(map) =
        serde_json::from_str::<serde_json::Value>(arguments).ok()?
    else {
        return None;
    };
    match map.get(key)? {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

/// Der Auftrag an einen Sub-Agent, kurz: `description`, sonst der Prompt.
fn delegation_brief(arguments: &str) -> Option<String> {
    field_of(arguments, "description").or_else(|| prompt_of(arguments))
}

/// Der Prompt an einen Sub-Agent.
fn prompt_of(arguments: &str) -> Option<String> {
    field_of(arguments, "prompt")
}

/// Die erste bedeutungstragende Zeile eines Kommandos — `cd …`-Vorspann
/// und Leerzeilen übersprungen; bei `cd … && rest` der Rest.
fn first_line(text: &str) -> &str {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("cd ") && *l != "cd")
        .or_else(|| text.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    line.strip_prefix("cd ")
        .and_then(|rest| rest.split_once("&& "))
        .map(|(_, rest)| rest.trim())
        .unwrap_or(line)
}

/// Kürzt auf `max` Zeichen, mit `…`.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}…")
}

/// Kappt auf [`COMMAND_MAX`] Zeichen und sagt, wie viele fehlen.
fn cap(text: &str) -> String {
    let total = text.chars().count();
    if total <= COMMAND_MAX {
        return text.to_string();
    }
    let cut: String = text.chars().take(COMMAND_MAX).collect();
    format!("{cut} … {} more", total - COMMAND_MAX)
}

/// Ein Zeitstempel, richtig geparst — nie per String-Vergleich geordnet.
fn timestamp(at: &str) -> Option<Timestamp> {
    at.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::{
        Agent, Capture, CaptureNote, CaptureStatus, Edge, EdgeKind, Effect, EffectKind, Endpoint,
        EvidenceMark, EvidenceSource, ExecClass, ExecOutcome, Intent, Lineage, Model, Role,
        Session, SessionId, TestCounts, ToolCall, Turn, WrittenUnavailable,
    };
    use std::collections::BTreeMap;

    use crate::Index;

    const CWD: &str = "/Users/pd/dev/minds-demo";

    fn sid(c: char) -> SessionId {
        format!("b3-{}", c.to_string().repeat(64)).parse().unwrap()
    }

    fn hash(b: u8) -> minds_core::ContentHash {
        minds_core::ContentHash::from_bytes([b; 32])
    }

    fn base(request: &str) -> Session {
        let mut s = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "2.0".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4-1".into(),
            },
            Intent {
                request: request.into(),
                ..Intent::default()
            },
        );
        let mut lineage = Lineage::new("local");
        lineage.cwd = Some(CWD.into());
        lineage.started_at = Some("2026-10-16T10:37:12Z".into());
        lineage.ended_at = Some("2026-10-16T10:40:12Z".into());
        s.lineage = Some(lineage);
        s
    }

    fn turn(role: Role, text: &str, at: &str, calls: Vec<ToolCall>) -> Turn {
        Turn {
            role,
            text: text.into(),
            tool_calls: calls,
            parent: None,
            at: Some(at.into()),
        }
    }

    fn call(name: &str, args: &str, effect: Option<Effect>) -> ToolCall {
        ToolCall {
            name: name.into(),
            arguments: args.into(),
            capture: Some(Capture {
                status: CaptureStatus::Interpreted,
                adapter: "claude-code".into(),
                adapter_version: 3,
                note: None,
            }),
            effect,
            outcome: None,
        }
    }

    fn exec(command: &str) -> ToolCall {
        call(
            "Bash",
            &serde_json::json!({ "command": command }).to_string(),
            Some(Effect {
                kind: EffectKind::Exec,
                path: None,
                content: None,
                written: None,
                written_unavailable: None,
            }),
        )
    }

    fn write(name: &str, path: &str, written: Option<u8>) -> ToolCall {
        call(
            name,
            &serde_json::json!({ "file_path": path }).to_string(),
            Some(Effect {
                kind: EffectKind::Write,
                path: Some(path.into()),
                content: None,
                written: written.map(hash),
                written_unavailable: written
                    .is_none()
                    .then_some(WrittenUnavailable::PayloadWithoutContent),
            }),
        )
    }

    fn tests_run(command: &str, passed: u64, failed: u64) -> ToolCall {
        let mut c = exec(command);
        c.outcome = Some(ExecOutcome {
            class: ExecClass::Test,
            runner: "pytest".into(),
            command: command.split(' ').map(str::to_string).collect(),
            cwd: Some(".".into()),
            exit_code: (failed > 0).then_some(1),
            tests: Some(TestCounts {
                passed,
                failed,
                ignored: 0,
            }),
            benches: Vec::new(),
        });
        c
    }

    /// Die Demo-Session des Zielbilds: lesen, schreiben, testen, linten,
    /// nachbessern, bauen, delegieren, suchen, holen — und ein beobachteter,
    /// nicht gedeuteter Aufruf.
    fn demo() -> Session {
        let mut s = base("Implementiere die Anforderung aus anforderung.md.\nNicht committen.");
        s.turns.push(turn(
            Role::User,
            "Implementiere die Anforderung aus anforderung.md.",
            "2026-10-16T10:37:12Z",
            vec![],
        ));
        s.turns.push(turn(
            Role::Assistant,
            "Ich lese zuerst.",
            "2026-10-16T10:37:12Z",
            vec![exec(&format!("cd {CWD} && cat anforderung.md && ls -la"))],
        ));
        s.turns.push(turn(
            Role::Assistant,
            "Ich lege die Dateien an.",
            "2026-10-16T10:38:40Z",
            vec![
                write("Write", &format!("{CWD}/src/sort.py"), Some(7)),
                write("Write", "src/test_sort.py", None),
            ],
        ));
        let mut build = exec("cargo build");
        build.outcome = Some(ExecOutcome {
            class: ExecClass::Test,
            runner: "cargo-test".into(),
            command: vec!["cargo".into(), "build".into()],
            cwd: None,
            exit_code: Some(1),
            tests: None,
            benches: Vec::new(),
        });
        let mut not_captured = exec("pytest -q src");
        not_captured.capture.as_mut().unwrap().note = Some(CaptureNote::ResultNotCaptured);
        let mut foo = call("Foo", "{\"x\":1}", None);
        foo.capture.as_mut().unwrap().status = CaptureStatus::Uninterpreted;
        s.turns.push(turn(
            Role::Assistant,
            "Ich teste und bessere nach.",
            "2026-10-16T10:39:15Z",
            vec![
                tests_run("python -m unittest discover -s src", 8, 0),
                exec("ruff check src && ruff format --check src"),
                write("Edit", "src/test_sort.py", None),
                build,
                not_captured,
                call(
                    "Task",
                    "{\"description\":\"Review the tests\",\"prompt\":\"Lies src/test_sort.py und …\"}",
                    None,
                ),
                call("Grep", "{\"pattern\":\"stable\"}", None),
                call("WebFetch", "{\"url\":\"https://example.org\"}", None),
                call(
                    "Read",
                    "{}",
                    Some(Effect {
                        kind: EffectKind::Read,
                        path: Some("/etc/hosts".into()),
                        content: None,
                        written: None,
                        written_unavailable: None,
                    }),
                ),
                foo,
            ],
        ));
        s.edges.push(Edge {
            kind: EdgeKind::Spawned,
            to: Endpoint::Session {
                agent: "claude-code".into(),
                local_id: "child".into(),
            },
            evidence: EvidenceMark::of(EvidenceSource::Observed),
        });
        s
    }

    fn index_with_child(parent: &Session) -> Index {
        let mut child = base("Lies src/test_sort.py und …");
        child.lineage = Some(Lineage::new("child"));
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('a'), parent.clone());
        sessions.insert(sid('b'), child);
        Index::from_parts(sessions, BTreeMap::new())
    }

    fn story() -> Story {
        let s = demo();
        Story::of(sid('a'), &s, &index_with_child(&s))
    }

    #[test]
    fn steps_are_numbered_in_session_order_with_verbs_from_tool_and_effect() {
        let story = story();
        let verbs: Vec<&str> = story.steps.iter().map(|s| s.verb.word()).collect();
        assert_eq!(
            verbs,
            vec![
                "ran",
                "wrote",
                "wrote",
                "tests",
                "lint",
                "edited",
                "build",
                "tests",
                "delegated",
                "searched",
                "fetched",
                "read",
                "tool",
            ]
        );
        let numbers: Vec<usize> = story.steps.iter().map(|s| s.number).collect();
        assert_eq!(numbers, (1..=13).collect::<Vec<_>>());
        // Herkunft je Schritt: Turn- und Aufruf-Index wie `LineSource`.
        assert_eq!((story.steps[0].turn, story.steps[0].call), (1, 0));
        assert_eq!((story.steps[2].turn, story.steps[2].call), (2, 1));
        assert_eq!((story.steps[5].turn, story.steps[5].call), (3, 2));
    }

    #[test]
    fn paths_are_repo_relative_and_never_carry_the_cwd() {
        let story = story();
        assert_eq!(story.steps[1].target, "src/sort.py");
        assert_eq!(story.steps[2].target, "src/test_sort.py");
        // Außerhalb des Repos: nur der Dateiname, mit Auslassung davor.
        assert_eq!(story.steps[11].target, "…/hosts");
        // Auch im Kommando steht das Arbeitsverzeichnis nicht mehr.
        assert_eq!(story.steps[0].target, "cat anforderung.md && ls -la");
        for step in &story.steps {
            assert!(!step.target.contains("/Users/"), "{}", step.target);
            for (k, v) in &step.detail {
                assert!(!v.contains("/Users/"), "{k}: {v}");
            }
        }
        assert_eq!(relative_path("src/a.rs", Some(CWD)), "src/a.rs");
        assert_eq!(relative_path("/tmp/x/y.txt", None), "…/y.txt");
        assert_eq!(relative_path(&format!("{CWD}/a b.rs"), Some(CWD)), "a b.rs");
        // Ein Präfix, das nur zufällig gleich beginnt, ist kein cwd.
        assert_eq!(
            relative_path("/Users/pd/dev/minds-demo-2/a.rs", Some(CWD)),
            "…/a.rs"
        );
    }

    #[test]
    fn outcomes_come_only_from_exec_outcome_never_guessed() {
        let story = story();
        assert_eq!(
            story.steps[3].outcome,
            Some(Outcome::Tests {
                passed: 8,
                failed: 0,
                ignored: 0
            })
        );
        // `ruff check` ohne ExecOutcome: kein Ergebnis — nicht „clean" raten.
        assert_eq!(story.steps[4].outcome, None);
        // Ein Exit-Code ohne Zähler ist das Ergebnis.
        assert_eq!(story.steps[6].outcome, Some(Outcome::Exit(1)));
        // Ein bekannter Runner ohne gelesenes Ergebnis sagt das.
        assert_eq!(
            story.steps[7].outcome,
            Some(Outcome::NotCaptured(CaptureNote::ResultNotCaptured))
        );
        // Schreiben und Suchen haben kein Ergebnis.
        assert_eq!(story.steps[1].outcome, None);
        assert_eq!(story.steps[9].outcome, None);
    }

    #[test]
    fn relative_time_counts_from_the_session_start() {
        let story = story();
        assert_eq!(story.started_at.as_deref(), Some("2026-10-16T10:37:12Z"));
        assert_eq!(story.duration_secs, Some(180));
        let offsets: Vec<Option<i64>> = story.steps.iter().map(|s| s.offset_secs).collect();
        assert_eq!(offsets[0], Some(0));
        assert_eq!(offsets[1], Some(88));
        assert_eq!(offsets[3], Some(123));
        // Ohne Lineage zählt der erste Zug als Start; ohne Zeit bleibt es leer.
        let mut s = demo();
        s.lineage = None;
        let story = Story::of(
            sid('a'),
            &s,
            &Index::from_parts(BTreeMap::new(), BTreeMap::new()),
        );
        assert_eq!(story.started_at.as_deref(), Some("2026-10-16T10:37:12Z"));
        assert_eq!(story.duration_secs, Some(123));
        let mut s = demo();
        for t in &mut s.turns {
            t.at = None;
        }
        s.lineage = None;
        let story = Story::of(
            sid('a'),
            &s,
            &Index::from_parts(BTreeMap::new(), BTreeMap::new()),
        );
        assert!(story.steps.iter().all(|s| s.offset_secs.is_none()));
        assert_eq!(story.duration_secs, None);
    }

    #[test]
    fn a_delegated_step_resolves_its_subagent_when_the_counts_match() {
        let story = story();
        assert_eq!(story.steps[8].child, Some(sid('b')));
        assert_eq!(story.steps[8].target, "Review the tests");
        assert_eq!(story.subagents, vec![sid('b')]);
        // Zwei gestartete Sub-Agents, ein Task-Aufruf: keine Zuordnung
        // raten — die Kinder bleiben in der Liste.
        let mut s = demo();
        s.edges.push(Edge {
            kind: EdgeKind::Spawned,
            to: Endpoint::Session {
                agent: "claude-code".into(),
                local_id: "other".into(),
            },
            evidence: EvidenceMark::of(EvidenceSource::Observed),
        });
        let story = Story::of(sid('a'), &s, &index_with_child(&s));
        assert_eq!(story.steps[8].child, None);
        assert_eq!(story.subagents, vec![sid('b')]);
    }

    #[test]
    fn write_claims_say_whether_a_hash_was_recorded() {
        let story = story();
        assert_eq!(story.steps[1].claim, Some(WriteClaim::Hashed));
        assert_eq!(
            story.steps[2].claim,
            Some(WriteClaim::Unhashed(Some(
                WrittenUnavailable::PayloadWithoutContent
            )))
        );
        assert_eq!(story.steps[0].claim, None);
        assert_eq!(story.steps[11].claim, None);
    }

    #[test]
    fn an_uninterpreted_call_stays_a_tool_step_marked_uninterpreted() {
        let story = story();
        let last = story.steps.last().unwrap();
        assert_eq!(last.verb, Verb::Tool);
        assert!(last.uninterpreted);
        assert_eq!(last.tool, "Foo");
        assert_eq!(last.target, "Foo");
        assert!(!story.steps[0].uninterpreted);
    }

    #[test]
    fn turns_carry_text_and_point_to_their_first_step() {
        let story = story();
        assert_eq!(story.turns.len(), 4);
        assert_eq!(story.turns[0].role, Role::User);
        assert_eq!(story.turns[0].first_step, None);
        assert_eq!(story.turns[1].first_step, Some(0));
        assert_eq!(story.turns[2].first_step, Some(1));
        assert_eq!(story.turns[3].first_step, Some(3));
        assert_eq!(story.turns[1].text, "Ich lese zuerst.");
        assert_eq!(story.turns[1].offset_secs, Some(0));
        assert_eq!(story.turns[3].offset_secs, Some(123));
    }

    #[test]
    fn foreign_text_in_commands_is_sanitized_and_long_commands_are_capped() {
        let long = "a".repeat(COMMAND_MAX + 500);
        let mut s = base("x");
        s.turns.push(turn(
            Role::Assistant,
            "",
            "2026-10-16T10:37:12Z",
            vec![exec(&format!("\u{1b}[2Kecho {long}\nsecond line"))],
        ));
        let story = Story::of(
            sid('a'),
            &s,
            &Index::from_parts(BTreeMap::new(), BTreeMap::new()),
        );
        let step = &story.steps[0];
        assert!(!step.target.contains('\u{1b}'));
        // Die Liste zeigt eine Zeile, gekürzt mit Auslassung.
        assert!(!step.target.contains('\n'));
        assert!(
            step.target.chars().count() <= TARGET_MAX + 1,
            "{}",
            step.target.len()
        );
        assert!(step.target.ends_with('…'));
        // Das Detail trägt das Kommando vollständig bis zur Grenze, dann
        // sagt es, wie viel fehlt.
        let command = &step.detail.iter().find(|(k, _)| k == "Command").unwrap().1;
        assert!(!command.contains('\u{1b}'));
        assert!(
            command.contains("… ") && command.ends_with(" more"),
            "{command}"
        );
        assert!(command.chars().count() <= COMMAND_MAX + 20);
        // Und sagt, was nicht aufgezeichnet ist.
        assert!(
            step.detail
                .iter()
                .any(|(_, v)| v == "stdout/stderr not recorded (by design)")
        );
    }

    #[test]
    fn summary_folds_consecutive_steps_of_the_same_kind() {
        let mut s = base("x");
        s.turns.push(turn(
            Role::Assistant,
            "",
            "2026-10-16T10:37:12Z",
            vec![
                tests_run("pytest -q", 1, 0),
                tests_run("pytest -q", 2, 0),
                tests_run("pytest -q", 3, 0),
                write("Edit", "src/a.py", None),
                write("Edit", "src/a.py", None),
                write("Edit", "src/b.py", None),
                exec("ls"),
            ],
        ));
        let story = Story::of(
            sid('a'),
            &s,
            &Index::from_parts(BTreeMap::new(), BTreeMap::new()),
        );
        let runs = summarize(&story.steps);
        let folded: Vec<(usize, usize, &str)> = runs
            .iter()
            .map(|r| (r.first, r.count, r.verb.word()))
            .collect();
        assert_eq!(
            folded,
            vec![
                (0, 3, "tests"),
                (3, 2, "edited"),
                (5, 1, "edited"),
                (6, 1, "ran")
            ]
        );
        // Der Lauf trägt das letzte Ergebnis — nie ein früheres.
        assert_eq!(
            runs[0].outcome,
            Some(Outcome::Tests {
                passed: 3,
                failed: 0,
                ignored: 0
            })
        );
        assert_eq!(runs[1].target.as_deref(), Some("src/a.py"));
        assert_eq!(summarize(&[]), Vec::<Run>::new());
    }

    #[test]
    fn the_prompt_is_the_full_request_sanitized() {
        let story = story();
        assert_eq!(
            story.prompt,
            "Implementiere die Anforderung aus anforderung.md.\nNicht committen."
        );
        let mut s = base("Zeile\u{2028}eins \u{1b}[2K");
        s.turns.clear();
        let story = Story::of(
            sid('a'),
            &s,
            &Index::from_parts(BTreeMap::new(), BTreeMap::new()),
        );
        assert!(!story.prompt.contains('\u{2028}'));
        assert!(!story.prompt.contains('\u{1b}'));
    }

    #[test]
    fn an_empty_session_has_no_steps_and_no_times() {
        let mut s = base("x");
        s.lineage = None;
        let story = Story::of(
            sid('a'),
            &s,
            &Index::from_parts(BTreeMap::new(), BTreeMap::new()),
        );
        assert!(story.steps.is_empty());
        assert!(story.turns.is_empty());
        assert_eq!(story.started_at, None);
        assert_eq!(story.duration_secs, None);
        assert!(story.subagents.is_empty());
    }

    #[test]
    fn the_inspection_tells_the_story_of_a_readable_session_only() {
        let s = demo();
        let index = index_with_child(&s);
        let inspection = crate::Inspection::from_index(index, Vec::new(), "repo");
        let story = inspection.story(sid('a')).expect("lesbare Session");
        assert_eq!(story.steps.len(), 13);
        assert_eq!(story.id, sid('a'));
        assert!(inspection.story(sid('f')).is_none());
    }
}
