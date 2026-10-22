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
//!   noch in Kommandos; außerhalb des Repos bleibt `…/<dateiname>`. In
//!   Kommandos, Argumenten, Prompts und Zugtexten wird jedes
//!   Home-Verzeichnis (`/Users/<name>`, `/home/<name>`, `C:\Users\<name>`)
//!   zu `~` — auch ohne bekanntes cwd. Windows-Trenner und Laufwerke werden
//!   vor dem Vergleich vereinheitlicht.
//! - Alle Texte sind entschärft ([`crate::text`]); Kommandos und Zugtexte
//!   sind auf [`COMMAND_MAX`] Zeichen gekappt und sagen, wie viel fehlt.

use jiff::Timestamp;
use minds_core::ContentHash;
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
    /// Der Anfang **dieses Aufrufs** (`PreToolUse`), RFC 3339, falls erfasst
    /// — das Zeitfenster einer Entscheidung beginnt hier, nicht am Zug.
    pub call_at: Option<String>,
    /// Sekunden seit Session-Start, falls beide Zeiten lesbar sind.
    pub offset_secs: Option<i64>,
    /// Bei `delegated`: die Kind-Session, wenn sie sich eindeutig zuordnen
    /// lässt (siehe [`Story::of`]).
    pub child: Option<SessionId>,
    /// Die Entscheidungen, deren Id die Antwort dieses Aufrufs nannte
    /// (`Session::decision_events` mit `turn`/`call`). Ob der Record im
    /// Zeitfenster des Aufrufs (`at` bis `ended_at`) entstand, prüft der
    /// Leser mit dem Record. Leer für jeden anderen Schritt.
    pub decisions: Vec<ContentHash>,
    /// Das Ende des Aufrufs (`PostToolUse`), RFC 3339, falls erfasst.
    pub ended_at: Option<String>,
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
    /// Die Entscheidungen, die die Session nennt (`Session::decision_events`),
    /// in Journal-Reihenfolge — auch die ohne Aufruf (ältere Records). Was
    /// davon im Zeitfenster seines Aufrufs liegt, sagt der Leser mit dem
    /// Record.
    pub decisions: Vec<ContentHash>,
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
        // Ohne Lineage: der früheste und der späteste datierte Zug — richtig
        // geparst, nie nach Reihenfolge oder String-Vergleich.
        let dated: Vec<(Timestamp, &str)> = session
            .turns
            .iter()
            .filter_map(|t| t.at.as_deref())
            .filter_map(|at| timestamp(at).map(|ts| (ts, at)))
            .collect();
        let started_at = lineage.and_then(|l| l.started_at.clone()).or_else(|| {
            dated
                .iter()
                .min_by_key(|(ts, _)| *ts)
                .map(|(_, at)| at.to_string())
        });
        let ended_at = lineage.and_then(|l| l.ended_at.clone()).or_else(|| {
            dated
                .iter()
                .max_by_key(|(ts, _)| *ts)
                .map(|(_, at)| at.to_string())
        });
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

        // Die Entscheidungen der Session: alle genannten Ids, und je
        // Aufruf (`turn`/`call` am Event) die, die seine Antwort nannte.
        // Ob der Record im Zeitfenster des Aufrufs entstand, prüft, wer den
        // Record hat (`decisions::named_within_call`) — die Story kennt nur
        // die Session.
        let decisions: Vec<ContentHash> = session
            .decision_events
            .iter()
            .map(|e| e.decision_id.clone())
            .collect();
        let mut by_call: std::collections::BTreeMap<(usize, usize), Vec<ContentHash>> =
            std::collections::BTreeMap::new();
        for event in &session.decision_events {
            if let (Some(turn), Some(call)) = (event.turn, event.call) {
                by_call
                    .entry((turn, call))
                    .or_default()
                    .push(event.decision_id.clone());
            }
        }
        let mut steps: Vec<Step> = Vec::new();
        let mut turns: Vec<StoryTurn> = Vec::with_capacity(session.turns.len());
        for (turn_index, turn) in session.turns.iter().enumerate() {
            let at = turn.at.clone();
            let offset_secs = offset(at.as_deref());
            let first_step = (!turn.tool_calls.is_empty()).then_some(steps.len());
            turns.push(StoryTurn {
                index: turn_index,
                role: turn.role.clone(),
                text: cap(&sanitize_lines(
                    hide_paths(turn.text.trim(), cwd).split('\n'),
                )),
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
                    .then(|| raw_command(&call.arguments).map(|c| hide_paths(&c, cwd)))
                    .flatten();
                let verb = verb_of(&call.name, kind, command.as_deref(), call.outcome.as_ref());
                let path = effect
                    .and_then(|e| e.path.as_deref())
                    .map(|p| relative_path(p, cwd));
                let (target, mut detail) = match verb {
                    _ if verb.executes() => {
                        let full = command.clone().unwrap_or_else(|| name.clone());
                        let line = first_line(&full);
                        let mut detail = vec![(
                            "Command".to_string(),
                            cap(&sanitize_lines(full.split('\n'))),
                        )];
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
                        let brief = delegation_brief(&call.arguments).map(|b| hide_paths(&b, cwd));
                        let mut detail = Vec::new();
                        if let Some(prompt) = prompt_of(&call.arguments) {
                            let prompt = hide_paths(&prompt, cwd);
                            detail
                                .push(("Prompt".into(), cap(&sanitize_lines(prompt.split('\n')))));
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
                    let arguments = hide_paths(&call.arguments, cwd);
                    detail.push((
                        "Arguments".into(),
                        cap(&sanitize_lines(arguments.split('\n'))),
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
                let step_decisions: Vec<ContentHash> = by_call
                    .remove(&(turn_index, call_index))
                    .unwrap_or_default();
                for id in &step_decisions {
                    detail.push(("Decision".into(), id.to_string()));
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
                    call_at: call.at.clone(),
                    ended_at: call.ended_at.clone(),
                    offset_secs,
                    child: None,
                    decisions: step_decisions,
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
            prompt: sanitize_lines(
                hide_paths(
                    &minds_core::without_pasted_wrapper(&session.intent.request),
                    cwd,
                )
                .split('\n'),
            ),
            started_at,
            ended_at,
            duration_secs,
            steps,
            turns,
            subagents,
            decisions,
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
/// Pfade bleiben. Trenner werden vor dem Vergleich vereinheitlicht (`\` →
/// `/`); Laufwerk (`C:/…`), UNC (`//server/…`) und `~` gelten als absolut.
/// Entschärft wie ein Pfad.
pub fn relative_path(path: &str, cwd: Option<&str>) -> String {
    let normalized = path.replace('\\', "/");
    let stripped = cwd
        .map(|c| c.replace('\\', "/"))
        .filter(|c| !c.trim_end_matches('/').is_empty())
        .and_then(|cwd| {
            let cwd = cwd.trim_end_matches('/').to_string();
            normalized
                .strip_prefix(cwd.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .filter(|rest| !rest.is_empty())
                .map(str::to_string)
        });
    let shown = match stripped {
        Some(rest) => rest,
        None if is_absolute(&normalized) => {
            let base = normalized
                .rsplit('/')
                .find(|part| !part.is_empty())
                .unwrap_or(&normalized);
            format!("…/{base}")
        }
        None => normalized,
    };
    sanitize_path(&shown)
}

/// Ob ein (vereinheitlichter) Pfad absolut ist: `/…` (auch UNC `//…`),
/// `~…` oder ein Laufwerk `C:/…`.
fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('~') || has_drive(path)
}

/// Ob `path` mit einem Laufwerk `X:/` beginnt (Trenner schon vereinheitlicht).
fn has_drive(path: &str) -> bool {
    let mut chars = path.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(d), Some(':'), Some('/')) if d.is_ascii_alphabetic()
    )
}

/// Nimmt Benutzer-Pfade aus einem Text (Kommando, Argumente, Prompt): das
/// Arbeitsverzeichnis der Session fällt weg (`cwd/` → ``, ein
/// alleinstehendes `cwd` → `.`, nur an Pfadgrenzen — `cwd-2/x` bleibt), ein
/// Home-Verzeichnis (`/Users/<name>`, `/home/<name>`, `C:\Users\<name>`)
/// wird `~` — auch ohne bekanntes cwd. Andere absolute Pfade
/// (`/usr/bin/env`) tragen keinen Namen und bleiben.
pub(crate) fn hide_paths(text: &str, cwd: Option<&str>) -> String {
    let mut out = text.to_string();
    if let Some(cwd) = cwd
        .map(|c| c.trim_end_matches(['/', '\\']))
        .filter(|c| !c.is_empty())
    {
        out = replace_at_boundaries(&out, cwd, ".");
        // Auch die andere Trenner-Schreibweise desselben Verzeichnisses.
        let alt = if cwd.contains('\\') {
            cwd.replace('\\', "/")
        } else {
            cwd.replace('/', "\\")
        };
        if alt != cwd {
            out = replace_at_boundaries(&out, &alt, ".");
        }
    }
    hide_home(&out)
}

/// Zeichen, die einen Pfad in einem Text beenden.
fn ends_path(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ',' | ';' | ')' | ']' | '}' | ':')
}

/// Ersetzt `needle` durch `with` nur dort, wo danach das Ende oder ein
/// Pfad-Ende folgt; `needle/` (oder `needle\`) fällt ganz weg.
fn replace_at_boundaries(text: &str, needle: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(needle) {
        out.push_str(&rest[..at]);
        let after = &rest[at + needle.len()..];
        match after.chars().next() {
            Some('/') | Some('\\') => rest = &after[1..],
            Some(c) if ends_path(c) => {
                out.push_str(with);
                rest = after;
            }
            None => {
                out.push_str(with);
                rest = after;
            }
            Some(_) => {
                out.push_str(needle);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Ersetzt Home-Verzeichnisse durch `~`: `/Users/<name>`, `/home/<name>`,
/// `X:/Users/<name>` und `X:\Users\<name>` — der Name ist genau eine
/// Pfadkomponente. Ohne Laufwerk zählt das Präfix nur am Textanfang oder
/// nach Leerraum, Trenner oder Anführungszeichen (`foo/Users/x` ist keins).
fn hide_home(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some((at, prefix_len)) = find_home(rest) else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..at]);
        let after = &rest[at + prefix_len..];
        let name_len = after
            .find(|c: char| matches!(c, '/' | '\\') || ends_path(c))
            .unwrap_or(after.len());
        if name_len == 0 {
            out.push_str(&rest[at..at + prefix_len]);
            rest = after;
            continue;
        }
        out.push('~');
        rest = &after[name_len..];
    }
}

/// Anfang und Länge des ersten Home-Präfixes in `text` (ohne den Namen).
fn find_home(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut best: Option<(usize, usize)> = None;
    for marker in ["/Users/", "/home/", "\\Users\\"] {
        let mut from = 0;
        while let Some(i) = text[from..].find(marker) {
            let at = from + i;
            let drive = at >= 2 && bytes[at - 1] == b':' && bytes[at - 2].is_ascii_alphabetic();
            let (start, len) = if drive {
                (at - 2, marker.len() + 2)
            } else {
                (at, marker.len())
            };
            let bounded = start == 0
                || text[..start].chars().last().is_some_and(|c| {
                    c.is_whitespace()
                        || matches!(c, '"' | '\'' | '`' | '=' | ':' | '(' | '[' | '{' | ',')
                });
            if bounded && best.is_none_or(|(b, _)| start < b) {
                best = Some((start, len));
            }
            from = at + 1;
        }
    }
    best
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
            } else if matches!(lower.as_str(), "task" | "agent" | "subagent" | "delegate") {
                // Genau diese Namen — `mcp__x__agent_status` ist keine
                // Delegation; eine falsche zählte in die Kind-Zuordnung.
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

    const CWD: &str = "/Users/alice/dev/minds-demo";

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
            at: None,
            ended_at: None,
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

    /// Entscheidungen hängen über `turn`/`call` am Schritt, mit dem Fenster
    /// des Aufrufs (`call_at`, `ended_at`); ein Event ohne Indizes steht in
    /// `Story.decisions`, aber an keinem Schritt.
    #[test]
    fn decisions_hang_on_their_call_with_its_window() {
        let mut s = base("decide");
        let mut first = exec("cargo test");
        first.at = Some("2026-07-25T14:11:30Z".into());
        first.ended_at = Some("2026-07-25T14:11:40Z".into());
        let mut second = exec("minds decide < d.json");
        second.at = Some("2026-07-25T14:12:00Z".into());
        second.ended_at = Some("2026-07-25T14:12:05Z".into());
        let t = s.turns.len();
        s.turns.push(turn(
            Role::Assistant,
            "",
            "2026-07-25T14:11:30Z",
            vec![first, second],
        ));
        let event = |seq, n, turn, call| minds_core::decision::DecisionEvent {
            seq,
            decision_id: hash(n),
            turn,
            call,
        };
        s.decision_events.push(event(2, 1, Some(t), Some(1)));
        s.decision_events.push(event(4, 2, Some(t), Some(0)));
        s.decision_events.push(event(6, 3, None, None));
        s.decision_events.push(event(8, 4, Some(99), Some(0)));
        let story = Story::of(sid('a'), &s, &index_with_child(&s));
        assert_eq!(story.decisions.len(), 4);
        let steps: Vec<&Step> = story.steps.iter().filter(|st| st.turn == t).collect();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].decisions, vec![hash(2)]);
        assert_eq!(steps[0].call_at.as_deref(), Some("2026-07-25T14:11:30Z"));
        assert_eq!(steps[0].ended_at.as_deref(), Some("2026-07-25T14:11:40Z"));
        assert_eq!(steps[1].decisions, vec![hash(1)]);
        assert_eq!(steps[1].call_at.as_deref(), Some("2026-07-25T14:12:00Z"));
        assert_eq!(steps[1].ended_at.as_deref(), Some("2026-07-25T14:12:05Z"));
        // Der Zug trägt die Zeit des Zugs, der Aufruf seine eigene.
        assert_eq!(steps[1].at.as_deref(), Some("2026-07-25T14:11:30Z"));
        let placed: usize = story.steps.iter().map(|st| st.decisions.len()).sum();
        assert_eq!(placed, 2, "ohne Indizes oder außerhalb: an keinem Schritt");
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
                written_at: None,
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
                written_at: None,
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
                        written_at: None,
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

    /// Benutzer-Pfade außerhalb des cwd — und ohne cwd — werden `~/…`:
    /// in Kommandos, Prompts an Sub-Agents und Argumenten; ein Nachbar-Repo
    /// (`minds-demo-2`) wird nicht zum cwd gekürzt.
    #[test]
    fn home_paths_become_tilde_in_commands_prompts_and_arguments() {
        let mut s = base("x");
        s.turns.push(turn(
            Role::Assistant,
            "",
            "2026-10-16T10:37:12Z",
            vec![
                exec("/Users/alice/.cargo/bin/cargo test"),
                exec(&format!("cat {CWD}-2/x.rs {CWD}/y.rs")),
                call(
                    "Task",
                    "{\"description\":\"Notes\",\"prompt\":\"Lies /Users/alice/notes.md und /home/bob/a.txt\"}",
                    None,
                ),
                call("Foo", "{\"file_path\":\"/Users/alice/other/x\"}", None),
            ],
        ));
        for lineage in [s.lineage.clone(), None] {
            let mut s = s.clone();
            s.lineage = lineage;
            let story = Story::of(
                sid('a'),
                &s,
                &Index::from_parts(BTreeMap::new(), BTreeMap::new()),
            );
            assert_eq!(story.steps[0].target, "~/.cargo/bin/cargo test");
            let detail = |i: usize, key: &str| {
                story.steps[i]
                    .detail
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            };
            assert!(
                detail(2, "Prompt").contains("~/notes.md und ~/a.txt"),
                "{}",
                detail(2, "Prompt")
            );
            assert!(
                detail(3, "Arguments").contains("~/other/x"),
                "{}",
                detail(3, "Arguments")
            );
            for step in &story.steps {
                assert!(
                    !step.target.contains("/Users/") && !step.target.contains("/home/"),
                    "{}",
                    step.target
                );
                for (k, v) in &step.detail {
                    assert!(!v.contains("/Users/") && !v.contains("/home/"), "{k}: {v}");
                }
            }
        }
        // Mit cwd: das Repo selbst wird `.`, der Nachbar bleibt erkennbar.
        let story = Story::of(
            sid('a'),
            &s,
            &Index::from_parts(BTreeMap::new(), BTreeMap::new()),
        );
        assert_eq!(story.steps[1].target, "cat ~/dev/minds-demo-2/x.rs y.rs");
        assert!(
            !story.steps[1].target.contains(".-2"),
            "{}",
            story.steps[1].target
        );
    }

    /// Windows-Schreibweisen: Laufwerk mit `/` oder `\\`, cwd mit `\\` —
    /// beides wird relativ; ein anderes Laufwerk bleibt `…/<datei>`.
    #[test]
    fn windows_paths_are_relative_in_both_spellings() {
        let cwd = Some("C:\\Users\\alice\\repo");
        assert_eq!(
            relative_path("C:/Users/alice/repo/src/a.rs", cwd),
            "src/a.rs"
        );
        assert_eq!(
            relative_path("C:\\Users\\alice\\repo\\src\\a.rs", cwd),
            "src/a.rs"
        );
        assert_eq!(relative_path("D:\\data\\x.csv", cwd), "…/x.csv");
        assert_eq!(
            relative_path("\\\\server\\share\\Users\\alice\\f.txt", cwd),
            "…/f.txt"
        );
        assert_eq!(relative_path("src\\a.rs", cwd), "src/a.rs");
        // In Texten: das cwd in beiden Schreibweisen, Home mit Laufwerk.
        assert_eq!(
            hide_paths("type C:/Users/alice/repo/a.rs", cwd),
            "type a.rs"
        );
        assert_eq!(
            hide_paths("type C:\\Users\\alice\\repo\\a.rs", cwd),
            "type a.rs"
        );
        assert_eq!(
            hide_paths("cd C:\\Users\\alice\\other", None),
            "cd ~\\other"
        );
        assert_eq!(
            hide_paths("cat /usr/bin/env foo/Users/x", None),
            "cat /usr/bin/env foo/Users/x"
        );
    }

    /// Ein Tool mit „agent“ im Namen ist keine Delegation — sonst hinge das
    /// Kind am falschen Schritt.
    #[test]
    fn only_task_like_tools_are_delegations() {
        let mut s = base("x");
        s.turns.push(turn(
            Role::Assistant,
            "",
            "2026-10-16T10:37:12Z",
            vec![
                call("mcp__x__agent_status", "{}", None),
                call("Task", "{\"description\":\"Review\"}", None),
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
        let story = Story::of(sid('a'), &s, &index_with_child(&s));
        assert_eq!(story.steps[0].verb, Verb::Tool);
        assert_eq!(story.steps[0].child, None);
        assert_eq!(story.steps[1].verb, Verb::Delegated);
        assert_eq!(story.steps[1].child, Some(sid('b')));
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
