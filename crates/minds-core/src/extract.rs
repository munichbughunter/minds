//! Extraktoren: aus abgeschlossenen Sessions destillierte, wiederverwendbare
//! Fakten — der reine Kern von `minds recall`, `distill` und `brief` (Track R).
//!
//! # Deterministisch und ohne I/O
//!
//! Dieses Modul rechnet ausschließlich auf bereits geladenen [`Session`]s. Es
//! öffnet kein Repo, ruft keine Uhr, würfelt nicht. Gleiche Eingabe ⇒
//! byte-gleiche Ausgabe — die Voraussetzung dafür, dass die daraus erzeugten
//! Briefs diffbar und in Golden-Tests festnagelbar sind.
//!
//! # Stark vs. heuristisch — ehrlich getrennt
//!
//! Nicht jedes Signal ist gleich verlässlich, und ein Brief soll das zeigen
//! dürfen:
//!
//! - **stark**, weil aus dem normalisierten [`Effect`](crate::Effect) gelesen:
//!   [`Extract::commands`] (Exec), [`Extract::hot_files`],
//!   [`Extract::co_changes`].
//! - **heuristisch**, weil aus Mustern bzw. Freitext geraten:
//!   [`Extract::reworks`] (Write→Delete, Churn) und [`Extract::corrections`]
//!   (Korrektur-Sprache in einem User-Turn nach einer Assistant-Antwort).
//!
//! „Konventionen" als Stilregeln entstehen hier bewusst **nicht** — die
//! bräuchten den Code selbst oder ein LLM. Was herauskommt, sind beobachtete
//! Fakten.
//!
//! # Agent-neutral
//!
//! Alle starken Signale lesen den normalisierten [`Effect`] am Tool-Call, nicht
//! das agent-spezifische `arguments`-JSON. Ein Agent, dessen Adapter noch keine
//! Effekte setzt (Track A), liefert hier schlicht weniger — kein Fehler, nur
//! weniger Fakten. Einzige Ausnahme ist der Kommando-Text selbst, den nur der
//! `arguments`-String trägt; ihn zieht [`command_of`] mit einer klar benannten
//! Heuristik heraus.

use std::collections::{BTreeMap, BTreeSet};

use crate::{EffectKind, Role, Session};

/// Ab wie vielen Write-Effekten auf denselben Pfad in **einer** Session wir von
/// „Churn" sprechen — ein Signal dafür, dass eine Datei schwer zu treffen war.
const CHURN_MIN: u32 = 3;

/// Sessions, die mehr als so viele verschiedene Dateien ändern, tragen **nicht**
/// zu den Co-Change-Paaren bei. Ein Massen-Refactor ist kein Aussage über
/// „diese zwei gehören zusammen" — und die Paarbildung ist O(n²) je Session.
const CO_CHANGE_MAX_FILES: usize = 40;

/// Maximale Zeichenzahl einer zitierten Korrektur.
const CORRECTION_MAX: usize = 200;

/// Maximale Zeichenzahl eines entrauschten Kommandos.
const CMD_MAX: usize = 120;

/// Korrektur-Marker (klein geschrieben, Deutsch und Englisch). Bewusst kurz und
/// als Heuristik markiert: `actually`/`eigentlich` erzeugen gelegentlich einen
/// Fehltreffer. Das ist der Preis für 0 Tokens; der Reader zeigt Korrekturen als
/// „vermutet", nicht als Tatsache.
const CORRECTION_MARKERS: &[&str] = &[
    "nein",
    "stattdessen",
    "rückgängig",
    "revert",
    "undo",
    "instead",
    "nicht so",
    "das ist falsch",
    "war falsch",
    "that's wrong",
    "don't",
    "eigentlich",
    "actually",
];

/// Ein destillierter Fakt-Satz aus einer oder mehreren Sessions.
///
/// Alle Listen sind deterministisch sortiert (die stärksten/häufigsten zuerst,
/// Gleichstand bricht der Name auf). Verdichten oder Kappen ist Sache des
/// Aufrufers (`brief` deckelt, `distill` nicht).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extract {
    /// Wie viele Sessions in diesen Extrakt eingeflossen sind.
    pub sessions_considered: usize,
    /// Ausgeführte Kommandos (Exec), nach Häufigkeit.
    pub commands: Vec<CommandFact>,
    /// Häufig geänderte Dateien.
    pub hot_files: Vec<FileFact>,
    /// Datei-Paare, die in denselben Sessions zusammen geändert wurden.
    pub co_changes: Vec<CoChange>,
    /// Heuristische Sackgassen: Datei angelegt und wieder gelöscht, oder mehrfach
    /// umgeschrieben.
    pub reworks: Vec<Rework>,
    /// Heuristische Korrekturen aus dem Gesprächsverlauf.
    pub corrections: Vec<Correction>,
}

/// Ein ausgeführtes Kommando und wie oft es vorkam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFact {
    pub command: String,
    pub count: u32,
}

/// Eine geänderte Datei mit Häufigkeit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFact {
    pub path: String,
    /// Zahl der Write-/Delete-Effekte über alle Sessions.
    pub changes: u32,
    /// Zahl der **verschiedenen** Sessions, die die Datei geändert haben.
    pub sessions: u32,
}

/// Zwei Dateien, die zusammen geändert wurden (`a` < `b`, lexikografisch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoChange {
    pub a: String,
    pub b: String,
    /// In wie vielen Sessions beide zusammen geändert wurden.
    pub count: u32,
}

/// Eine heuristische Sackgasse an einer Datei innerhalb einer Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rework {
    pub path: String,
    pub kind: ReworkKind,
}

/// Art einer [`Rework`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReworkKind {
    /// Angelegt/geschrieben und in derselben Session wieder gelöscht.
    WrittenThenDeleted,
    /// In einer Session mehrfach umgeschrieben (≥ [`CHURN_MIN`] Writes).
    Churned { edits: u32 },
}

/// Eine heuristisch als Korrektur erkannte User-Äußerung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub text: String,
}

impl Extract {
    /// Destilliert die Fakten aus einer Menge Sessions.
    ///
    /// Die Reihenfolge der Eingabe beeinflusst das Ergebnis nicht (alle Ausgaben
    /// werden sortiert) — aber der Aufrufer sollte trotzdem eine stabile Menge
    /// übergeben (z. B. `store.list()`-Reihenfolge), damit auch die zitierten
    /// Korrekturen reproduzierbar bleiben.
    pub fn from_sessions(sessions: &[Session]) -> Self {
        let mut commands: BTreeMap<String, u32> = BTreeMap::new();
        let mut file_changes: BTreeMap<String, u32> = BTreeMap::new();
        let mut file_sessions: BTreeMap<String, u32> = BTreeMap::new();
        let mut co: BTreeMap<(String, String), u32> = BTreeMap::new();
        let mut reworks: Vec<Rework> = Vec::new();
        let mut corrections: Vec<Correction> = Vec::new();

        for session in sessions {
            // Alle Write-/Delete-Effekte dieser Session, in Reihenfolge — daraus
            // fallen Hot-Files, Co-Changes und Rework.
            let mut changes: Vec<(String, EffectKind)> = Vec::new();
            for turn in &session.turns {
                for call in &turn.tool_calls {
                    let Some(effect) = &call.effect else { continue };
                    match effect.kind {
                        EffectKind::Exec => {
                            if let Some(cmd) = command_of(&call.arguments) {
                                *commands.entry(cmd).or_default() += 1;
                            }
                        }
                        EffectKind::Write | EffectKind::Delete => {
                            if let Some(path) = &effect.path {
                                changes.push((path.clone(), effect.kind));
                            }
                        }
                        EffectKind::Read | EffectKind::Other => {}
                    }
                }
            }

            // Distinkte geänderte Pfade dieser Session (Erst-Reihenfolge egal,
            // wir sortieren gleich).
            let mut distinct: BTreeSet<String> = BTreeSet::new();
            for (path, _) in &changes {
                *file_changes.entry(path.clone()).or_default() += 1;
                distinct.insert(path.clone());
            }
            for path in &distinct {
                *file_sessions.entry(path.clone()).or_default() += 1;
            }

            // Co-Change-Paare — nur bei überschaubaren Sessions (siehe Konstante).
            if distinct.len() <= CO_CHANGE_MAX_FILES {
                let sorted: Vec<&String> = distinct.iter().collect();
                for i in 0..sorted.len() {
                    for j in (i + 1)..sorted.len() {
                        *co.entry((sorted[i].clone(), sorted[j].clone()))
                            .or_default() += 1;
                    }
                }
            }

            reworks.extend(reworks_in(&changes));
            corrections.extend(corrections_in(session));
        }

        // --- Maps → sortierte Listen -----------------------------------------
        let mut commands: Vec<CommandFact> = commands
            .into_iter()
            .map(|(command, count)| CommandFact { command, count })
            .collect();
        commands.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.command.cmp(&b.command))
        });

        let mut hot_files: Vec<FileFact> = file_changes
            .into_iter()
            .map(|(path, changes)| {
                let sessions = file_sessions.get(&path).copied().unwrap_or(0);
                FileFact {
                    path,
                    changes,
                    sessions,
                }
            })
            .collect();
        hot_files.sort_by(|a, b| {
            b.changes
                .cmp(&a.changes)
                .then_with(|| b.sessions.cmp(&a.sessions))
                .then_with(|| a.path.cmp(&b.path))
        });

        // Co-Change ist erst ab zwei gemeinsamen Sessions ein Signal; ein
        // einmaliges Zusammentreffen ist Zufall.
        let mut co_changes: Vec<CoChange> = co
            .into_iter()
            .filter(|(_, count)| *count >= 2)
            .map(|((a, b), count)| CoChange { a, b, count })
            .collect();
        co_changes.sort_by(|x, y| {
            y.count
                .cmp(&x.count)
                .then_with(|| x.a.cmp(&y.a))
                .then_with(|| x.b.cmp(&y.b))
        });

        reworks.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| rework_ord(&a.kind).cmp(&rework_ord(&b.kind)))
        });
        reworks.dedup();

        // Korrekturen: globale Dedup nach Text, Erst-Reihenfolge bleibt.
        let mut seen = BTreeSet::new();
        corrections.retain(|c| seen.insert(c.text.clone()));

        Extract {
            sessions_considered: sessions.len(),
            commands,
            hot_files,
            co_changes,
            reworks,
            corrections,
        }
    }

    /// `true`, wenn nichts extrahiert wurde — der Aufrufer kann dann einen
    /// ehrlichen Leerzustand zeigen statt einer leeren Überschriftenwüste.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
            && self.hot_files.is_empty()
            && self.co_changes.is_empty()
            && self.reworks.is_empty()
            && self.corrections.is_empty()
    }
}

/// Zieht ein entrauschtes Kommando aus dem rohen `arguments`-JSON eines
/// Tool-Calls.
///
/// Zwei Schritte, beide als Heuristik benannt:
///
/// 1. **Rohstring finden** ([`raw_command`]): die gängigen Schlüssel
///    (`command`, `cmd`, `script`, `code`); schlägt das fehl, der getrimmte
///    Rohstring. Der Wert ist bereits redigiert (er kommt aus dem Store),
///    enthält also keine Secrets.
/// 2. **Entrauschen** ([`clean_command`]): führende `cd …`-Zeilen weg, von einer
///    Pipe der Kopf, Trailing-Redirection gekappt, lange Zeilen gekürzt — damit
///    `cargo test … | grep x` und `… | grep y` als **ein** Fakt zählen statt
///    zwei.
pub fn command_of(arguments: &str) -> Option<String> {
    clean_command(&raw_command(arguments)?)
}

/// Der rohe Kommando-String, noch unbereinigt.
fn raw_command(arguments: &str) -> Option<String> {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(arguments)
    {
        for key in ["command", "cmd", "script", "code"] {
            if let Some(serde_json::Value::String(s)) = map.get(key) {
                if !s.trim().is_empty() {
                    return Some(s.clone());
                }
            }
        }
    }
    let trimmed = arguments.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Entrauscht ein rohes Shell-Kommando zu seiner aussagekräftigen Essenz.
fn clean_command(raw: &str) -> Option<String> {
    // Erste bedeutungstragende Zeile — cd-Zeilen und Leerzeilen übersprungen.
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !is_cd(l))?;
    // Von einer Pipe bleibt der Kopf; das ist das eigentliche Kommando.
    let head = line.split(" | ").next().unwrap_or(line).trim();
    // Trailing-Redirection abschneiden (`… 2>&1`).
    let head = head.strip_suffix(" 2>&1").unwrap_or(head).trim();
    (!head.is_empty()).then(|| truncate_chars(head, CMD_MAX))
}

/// `true`, wenn die Zeile nur ins Verzeichnis wechselt.
fn is_cd(line: &str) -> bool {
    line == "cd" || line.starts_with("cd ")
}

/// Rework-Erkennung innerhalb **einer** Session, aus deren geordneten
/// Write-/Delete-Effekten.
fn reworks_in(changes: &[(String, EffectKind)]) -> Vec<Rework> {
    let mut per: BTreeMap<String, Vec<EffectKind>> = BTreeMap::new();
    for (path, kind) in changes {
        per.entry(path.clone()).or_default().push(*kind);
    }
    let mut out = Vec::new();
    for (path, kinds) in per {
        let writes = kinds.iter().filter(|k| **k == EffectKind::Write).count() as u32;
        let deleted = kinds.contains(&EffectKind::Delete);
        if writes > 0 && deleted {
            out.push(Rework {
                path,
                kind: ReworkKind::WrittenThenDeleted,
            });
        } else if writes >= CHURN_MIN {
            out.push(Rework {
                path,
                kind: ReworkKind::Churned { edits: writes },
            });
        }
    }
    out
}

/// Korrekturen aus dem Verlauf einer Session: ein User-Turn nach mindestens
/// einer Assistant-Antwort, dessen Text nach einer Korrektur klingt.
fn corrections_in(session: &Session) -> Vec<Correction> {
    let mut out = Vec::new();
    let mut seen_assistant = false;
    for turn in &session.turns {
        match turn.role {
            Role::Assistant => seen_assistant = true,
            Role::User if seen_assistant && looks_corrective(&turn.text) => {
                let text = first_line_trunc(&turn.text, CORRECTION_MAX);
                if !text.is_empty() {
                    out.push(Correction { text });
                }
            }
            _ => {}
        }
    }
    out
}

fn looks_corrective(text: &str) -> bool {
    let lower = text.to_lowercase();
    CORRECTION_MARKERS
        .iter()
        .any(|marker| contains_word(&lower, marker))
}

/// `true`, wenn `needle` in `haystack` steht, ohne in einen längeren
/// alphanumerischen Lauf eingebettet zu sein — damit „no" nicht in „another"
/// zündet. Mehrwort-Marker matchen als Phrase.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let i = start + pos;
        let before_ok = haystack[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after = i + needle.len();
        let after_ok = haystack[after..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        start = i + needle.len();
    }
    false
}

/// Die erste nicht-leere Zeile, auf `max` Zeichen an der Zeichengrenze gekürzt.
fn first_line_trunc(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    truncate_chars(line, max)
}

/// Kürzt auf höchstens `max` Zeichen (nicht Bytes) mit „…" als Marke.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Stabile Ordnung der Rework-Arten für die Sortierung.
fn rework_ord(kind: &ReworkKind) -> u8 {
    match kind {
        ReworkKind::WrittenThenDeleted => 0,
        ReworkKind::Churned { .. } => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Agent, Effect, Intent, Model, Produced, Redaction, Session, ToolCall, Turn, Usage,
    };

    fn session(request: &str) -> Session {
        Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent {
                request: request.into(),
                ..Default::default()
            },
        )
    }

    fn user(text: &str) -> Turn {
        Turn {
            role: Role::User,
            text: text.into(),
            tool_calls: Vec::new(),
            parent: None,
            at: None,
        }
    }

    fn assistant(calls: Vec<ToolCall>) -> Turn {
        Turn {
            role: Role::Assistant,
            text: String::new(),
            tool_calls: calls,
            parent: None,
            at: None,
        }
    }

    fn call(name: &str, arguments: &str, kind: EffectKind, path: Option<&str>) -> ToolCall {
        ToolCall {
            name: name.into(),
            arguments: arguments.into(),
            effect: Some(Effect {
                kind,
                path: path.map(str::to_string),
                content: None,
            }),
        }
    }

    #[test]
    fn command_of_reads_common_keys_then_falls_back() {
        assert_eq!(
            command_of(r#"{"command":"cargo test"}"#).as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            command_of(r#"{"cmd":"  ls -la  "}"#).as_deref(),
            Some("ls -la")
        );
        // Kein bekannter Schlüssel → Rohstring.
        assert_eq!(
            command_of("just some text").as_deref(),
            Some("just some text")
        );
        assert_eq!(command_of("   ").as_deref(), None);
    }

    #[test]
    fn command_of_denoises_cd_prefix_pipe_and_redirection() {
        // Genau das Rauschen aus echten Agent-Bash-Aufrufen.
        let raw = "cd /Users/x/repo\ncargo clippy -p minds-cli --all-targets 2>&1 | grep error";
        assert_eq!(
            command_of(raw).as_deref(),
            Some("cargo clippy -p minds-cli --all-targets")
        );
        // Auch aus dem JSON-Pfad heraus.
        let args = serde_json::json!({"command": "cd /repo\nmake build"}).to_string();
        assert_eq!(command_of(&args).as_deref(), Some("make build"));
    }

    #[test]
    fn command_of_truncates_very_long_commands() {
        let out = command_of(&format!("run {}", "x".repeat(300))).unwrap();
        assert!(out.chars().count() <= 120, "{out}");
        assert!(out.ends_with('…'));
    }

    #[test]
    fn denoising_groups_pipe_variants_into_one_command() {
        // Der eigentliche Gewinn: zwei Aufrufe mit unterschiedlichem grep-Schwanz
        // zählen als ein Befehl.
        let mut s = session("lint");
        s.turns.push(assistant(vec![
            call(
                "Bash",
                r#"{"command":"cargo clippy 2>&1 | grep error"}"#,
                EffectKind::Exec,
                None,
            ),
            call(
                "Bash",
                r#"{"command":"cargo clippy 2>&1 | grep warning"}"#,
                EffectKind::Exec,
                None,
            ),
        ]));
        let x = Extract::from_sessions(&[s]);
        assert_eq!(x.commands.len(), 1);
        assert_eq!(x.commands[0].command, "cargo clippy");
        assert_eq!(x.commands[0].count, 2);
    }

    #[test]
    fn exec_commands_are_counted_and_ranked() {
        let mut a = session("build stuff");
        a.turns.push(assistant(vec![
            call(
                "Bash",
                r#"{"command":"cargo test"}"#,
                EffectKind::Exec,
                None,
            ),
            call(
                "Bash",
                r#"{"command":"cargo test"}"#,
                EffectKind::Exec,
                None,
            ),
            call("Bash", r#"{"command":"cargo fmt"}"#, EffectKind::Exec, None),
        ]));

        let x = Extract::from_sessions(&[a]);
        assert_eq!(x.commands.len(), 2);
        // Häufigstes zuerst.
        assert_eq!(x.commands[0].command, "cargo test");
        assert_eq!(x.commands[0].count, 2);
        assert_eq!(x.commands[1].command, "cargo fmt");
    }

    #[test]
    fn hot_files_count_changes_and_distinct_sessions() {
        let mut a = session("s1");
        a.turns.push(assistant(vec![
            call("Edit", "{}", EffectKind::Write, Some("src/retry.rs")),
            call("Edit", "{}", EffectKind::Write, Some("src/retry.rs")),
        ]));
        let mut b = session("s2");
        b.turns.push(assistant(vec![call(
            "Edit",
            "{}",
            EffectKind::Write,
            Some("src/retry.rs"),
        )]));

        let x = Extract::from_sessions(&[a, b]);
        assert_eq!(x.hot_files.len(), 1);
        assert_eq!(x.hot_files[0].path, "src/retry.rs");
        assert_eq!(x.hot_files[0].changes, 3);
        assert_eq!(x.hot_files[0].sessions, 2);
    }

    #[test]
    fn co_change_needs_two_sessions() {
        let pair = |req| {
            let mut s = session(req);
            s.turns.push(assistant(vec![
                call("Edit", "{}", EffectKind::Write, Some("a.rs")),
                call("Edit", "{}", EffectKind::Write, Some("b.rs")),
            ]));
            s
        };
        // Nur eine Session mit dem Paar → kein Signal.
        assert!(
            Extract::from_sessions(&[pair("once")])
                .co_changes
                .is_empty()
        );
        // Zwei → das Paar erscheint.
        let x = Extract::from_sessions(&[pair("one"), pair("two")]);
        assert_eq!(x.co_changes.len(), 1);
        assert_eq!(
            (x.co_changes[0].a.as_str(), x.co_changes[0].b.as_str()),
            ("a.rs", "b.rs")
        );
        assert_eq!(x.co_changes[0].count, 2);
    }

    #[test]
    fn rework_detects_write_then_delete_and_churn() {
        let mut dead = session("dead end");
        dead.turns.push(assistant(vec![
            call("Write", "{}", EffectKind::Write, Some("scratch.rs")),
            call("Bash", "{}", EffectKind::Delete, Some("scratch.rs")),
        ]));
        let mut churn = session("hard file");
        churn.turns.push(assistant(vec![
            call("Edit", "{}", EffectKind::Write, Some("tricky.rs")),
            call("Edit", "{}", EffectKind::Write, Some("tricky.rs")),
            call("Edit", "{}", EffectKind::Write, Some("tricky.rs")),
        ]));

        let x = Extract::from_sessions(&[dead, churn]);
        assert!(x.reworks.contains(&Rework {
            path: "scratch.rs".into(),
            kind: ReworkKind::WrittenThenDeleted,
        }));
        assert!(x.reworks.contains(&Rework {
            path: "tricky.rs".into(),
            kind: ReworkKind::Churned { edits: 3 },
        }));
    }

    #[test]
    fn corrections_need_a_preceding_assistant_turn() {
        let mut s = session("do a thing");
        // Erster User-Turn ist die Aufgabe, keine Korrektur — auch wenn er ein
        // Marker-Wort enthielte.
        s.turns.push(user("nein, mach es anders")); // vor jeder Antwort
        s.turns.push(assistant(vec![]));
        s.turns.push(user("Nein, benutze stattdessen ein HashSet."));
        s.turns.push(user("noch eine Zeile"));

        let x = Extract::from_sessions(&[s]);
        assert_eq!(x.corrections.len(), 1);
        assert_eq!(
            x.corrections[0].text,
            "Nein, benutze stattdessen ein HashSet."
        );
    }

    #[test]
    fn contains_word_respects_boundaries() {
        assert!(contains_word("nein, das nicht", "nein"));
        assert!(!contains_word("meine antwort", "nein")); // in „meine" eingebettet
        assert!(contains_word("use a set instead", "instead"));
    }

    #[test]
    fn output_is_deterministic_regardless_of_input_order() {
        let build = || {
            let mut a = session("a");
            a.turns.push(assistant(vec![
                call(
                    "Bash",
                    r#"{"command":"cargo build"}"#,
                    EffectKind::Exec,
                    None,
                ),
                call("Edit", "{}", EffectKind::Write, Some("x.rs")),
                call("Edit", "{}", EffectKind::Write, Some("y.rs")),
            ]));
            let mut b = session("b");
            b.turns.push(assistant(vec![
                call(
                    "Bash",
                    r#"{"command":"cargo build"}"#,
                    EffectKind::Exec,
                    None,
                ),
                call("Edit", "{}", EffectKind::Write, Some("x.rs")),
                call("Edit", "{}", EffectKind::Write, Some("y.rs")),
            ]));
            (a, b)
        };
        let (a, b) = build();
        let forward = Extract::from_sessions(&[a.clone(), b.clone()]);
        let backward = Extract::from_sessions(&[b, a]);
        assert_eq!(forward, backward);
    }

    #[test]
    fn empty_input_is_empty() {
        let x = Extract::from_sessions(&[]);
        assert!(x.is_empty());
        assert_eq!(x.sessions_considered, 0);
    }

    #[test]
    fn tool_calls_without_effect_contribute_nothing() {
        // Ein Agent ohne Effekt-Adapter (Track A offen) liefert weniger — nicht
        // einen Fehler.
        let mut s = session("no effects");
        s.turns.push(assistant(vec![ToolCall {
            name: "Bash".into(),
            arguments: r#"{"command":"cargo test"}"#.into(),
            effect: None,
        }]));
        let x = Extract::from_sessions(&[s]);
        assert!(x.is_empty());
    }

    // Deckt zugleich `Produced`/`Usage`/`Redaction`-Importe ab, damit der Test
    // repräsentativ für eine echte Session bleibt.
    #[test]
    fn a_realistic_session_yields_a_mixed_extract() {
        let mut s = session("Retry-Test reparieren");
        s.usage = Usage {
            input_tokens: 10,
            output_tokens: 20,
        };
        s.produced = Produced {
            commit_hint: None,
            files: vec!["src/retry.rs".into()],
        };
        s.redaction = Redaction {
            applied: true,
            ..Default::default()
        };
        s.turns.push(user("Der Retry-Test flackert."));
        s.turns.push(assistant(vec![
            call(
                "Bash",
                r#"{"command":"cargo test retry"}"#,
                EffectKind::Exec,
                None,
            ),
            call("Edit", "{}", EffectKind::Write, Some("src/retry.rs")),
        ]));
        s.turns.push(user("Nein, das behebt die Ursache nicht."));

        let x = Extract::from_sessions(&[s]);
        assert_eq!(x.commands[0].command, "cargo test retry");
        assert_eq!(x.hot_files[0].path, "src/retry.rs");
        assert_eq!(x.corrections.len(), 1);
    }
}
