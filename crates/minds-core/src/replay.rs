//! Der Replay-Record (EA-18b): was ein CI-Lauf beim Wiederholen der
//! entscheidenden Test- und Benchmark-Befehle einer Session beobachtet hat.
//!
//! # Die Form
//!
//! Kanonisches JSON nach RFC 8785 ([`ReplayRecord::canonical_bytes`]):
//!
//! ```text
//! {"commit":…,"environment":{…},"interpretation_version":1,
//!  "results":[{"argv":[…],"call":…,"expected":{…},"observed":{…},
//!              "reason":…,"turn":…,"verdict":"reproduced"}],
//!  "schema":1,"session":…}
//! ```
//!
//! Abgelegt unter `refs/minds/anchors/replay/<64 hex>`, die Id ist
//! `blake3(kanonische Bytes)` — dieselbe Regel wie beim Observation-Objekt,
//! keine neue Hash-Domäne (W4). Die Signatur (`ssh-sig`, Namespace
//! `minds-anchor`) liegt als `record.sig` daneben, nie darin.
//!
//! # Was der Record **nicht** ist
//!
//! Keine Assurance-Aussage (W2): Das `verdict` je Befehl ist die Beobachtung
//! des CI-Laufs („dieses argv ergab diese Zahlen"), nicht `A3 reproduced`.
//! Ob ein Record für eine Session zählt — signiert, vollständig, ohne
//! `claim not reproduced` —, rechnet der Reader zur Lesezeit
//! (`minds_reader::replay`), gegen die entscheidenden Befehle der Session
//! **selbst**, nie gegen die Liste im Record.
//!
//! # Kein neuer Text aus der Programmausgabe
//!
//! Jeder String im Record stammt aus bereits redigiertem Material (argv und
//! Bench-Namen der gespeicherten Session), aus dem eigenen Vokabular
//! (`verdict`, `reason`) oder aus einer eng geprüften CI-Umgebung
//! ([`ReplayEnvironment`]). Beobachtete Benchmarks erscheinen nur unter
//! Namen, die die Session schon trägt — ein Name aus der Ausgabe des
//! Replays erreicht den Record nie.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{BenchValue, CanonError, ContentHash, ExecClass, TestCounts};

/// Die Schema-Version des Records.
pub const REPLAY_SCHEMA: u32 = 1;

/// Höchstgröße eines Records — so viel liest der Store zurück.
pub const MAX_RECORD: usize = 4 * 1024 * 1024;

/// Die Art des signierten Dokuments. `minds-anchor` signiert auch künftige
/// Gegenzeichnungen (EA-19); ohne dieses Feld könnte ein anders gemeintes,
/// tolerant gelesenes Dokument als Replay-Record durchgehen.
pub const REPLAY_KIND: &str = "replay";

/// Ein Replay-Record — eine Session, ein Commit, ein CI-Lauf.
///
/// Gelesen wird tolerant (unbekannte Felder werden ignoriert, fehlende
/// Sammelfelder sind leer); geschrieben wird kanonisch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayRecord {
    /// [`REPLAY_KIND`] — ein Record ohne zählt nirgends.
    #[serde(default)]
    pub kind: String,
    /// [`REPLAY_SCHEMA`].
    pub schema: u32,
    /// Der Commit, auf dessen Checkout wiederholt wurde (volle Hex-Id).
    pub commit: String,
    /// Die Session, deren Befehle wiederholt wurden (`b3-…`).
    pub session: String,
    /// Die Version der Deutung „entscheidender Befehl" und des Vergleichs
    /// (`minds_reader::replay::INTERPRETATION_VERSION`).
    pub interpretation_version: u32,
    /// Ein Ergebnis je entscheidendem Befehl, in Reihenfolge seines letzten
    /// Auftretens in der Session.
    #[serde(default)]
    pub results: Vec<ReplayResult>,
    /// Die Blob-Id der Policy (`.minds/replay.json`) im Baum des Commits,
    /// unter der verglichen wurde — `None` ohne Policy. Mitsigniert: Wer
    /// den Record später liest, sieht, welche Toleranzen galten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    /// Das Projekt, in dessen Pipeline der Replay lief (`gitlab.example.com/
    /// group/repo`, `github.com/org/repo`) — mitsigniert. Commit- und
    /// Session-Ids sind Inhalts-Hashes und in Forks gleich; ohne Projekt
    /// ließe sich ein in Projekt B signierter Record in Projekt A ablegen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Wo der Replay lief.
    #[serde(default)]
    pub environment: ReplayEnvironment,
}

impl ReplayRecord {
    /// Die kanonischen Bytes (RFC 8785).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonError> {
        crate::to_canonical_json(self)
    }

    /// Die Identität: `blake3(kanonische Bytes)`.
    pub fn id(&self) -> Result<ContentHash, CanonError> {
        Ok(Self::id_of_bytes(&self.canonical_bytes()?))
    }

    /// Die Identität gespeicherter Bytes — der Leser prüft gegen sie, nicht
    /// gegen eine Neu-Serialisierung (Vorwärts-Toleranz wie bei Sessions).
    pub fn id_of_bytes(bytes: &[u8]) -> ContentHash {
        ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
    }
}

/// Das Ergebnis eines entscheidenden Befehls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayResult {
    /// Index des Turns in der Session.
    pub turn: u32,
    /// Index des Tool-Aufrufs im Turn.
    pub call: u32,
    /// Das normalisierte argv, wie die Session es trägt.
    pub argv: Vec<String>,
    /// Was die Session berichtet.
    pub expected: Expected,
    /// Was der Replay beobachtet hat — fehlt bei `skipped`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<Observed>,
    /// Die Beobachtung des CI-Laufs für diesen Befehl.
    pub verdict: ReplayVerdict,
    /// Warum — fester Wortschatz plus Zahlen und Namen aus der Session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Was die Session für einen Befehl berichtet (aus `ToolCall::outcome`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expected {
    /// Test oder Benchmark.
    pub class: ExecClass,
    /// Der kanonische Runner-Name.
    pub runner: String,
    /// Der berichtete Exit-Code, falls vorhanden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Die berichteten Test-Zähler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<TestCounts>,
    /// Die berichteten Benchmark-Werte.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub benches: Vec<BenchValue>,
}

/// Was der Replay beobachtet hat — nur Zahlen und Namen, die die Session
/// schon trägt.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Observed {
    /// Der Exit-Code; `None`, wenn der Prozess durch ein Signal endete oder
    /// abgebrochen wurde.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Die Test-Zähler, falls die Ausgabe eine Zusammenfassung trug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<TestCounts>,
    /// Die Benchmark-Werte — nur für Namen aus [`Expected::benches`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub benches: Vec<BenchValue>,
    /// Der Befehl wurde nach Ablauf seiner Zeit abgebrochen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
}

/// Die Beobachtung je Befehl.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayVerdict {
    /// Ausgeführt; das Ergebnis entspricht dem berichteten.
    Reproduced,
    /// Ausgeführt; das Ergebnis weicht ab — `claim not reproduced`.
    NotReproduced,
    /// Nicht ausgeführt (nicht freigegeben, `cwd` außerhalb, …).
    Skipped,
}

impl ReplayVerdict {
    /// Das Wort im Record.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reproduced => "reproduced",
            Self::NotReproduced => "not_reproduced",
            Self::Skipped => "skipped",
        }
    }
}

/// Wo der Replay lief. Jedes Feld optional; der Aufrufer belegt nur, was
/// er eng geprüft hat (keine freie Umgebungsvariable erreicht den Record).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReplayEnvironment {
    /// Das CI-System (`gitlab`, `github`), `None` außerhalb von CI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<String>,
    /// Die Pipeline- bzw. Run-Id — nur Ziffern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<String>,
    /// Das Container-Image des Jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

// ---------------------------------------------------------------------------
// Die Team-Policy `.minds/replay.json`
// ---------------------------------------------------------------------------

/// Der Ort der Policy im Baum des geprüften Commits.
pub const POLICY_PATH: &str = ".minds/replay.json";

/// Höchstgröße der Policy-Datei.
pub const MAX_POLICY: u64 = 256 * 1024;

/// Die Schema-Version der Policy.
pub const POLICY_SCHEMA: u32 = 1;

/// Standard-Toleranz für Benchmarks in Prozent.
pub const DEFAULT_TOLERANCE_PCT: u32 = 25;

/// Standard-Zeitlimit je Befehl in Sekunden.
pub const DEFAULT_PER_COMMAND_S: u64 = 600;

/// Standard-Zeitlimit für den ganzen Lauf in Sekunden.
pub const DEFAULT_TOTAL_S: u64 = 1800;

/// Obergrenze für jedes Zeitlimit (24 h) — ein Tippfehler soll keinen
/// CI-Runner für Wochen belegen.
const MAX_TIMEOUT_S: u64 = 24 * 60 * 60;

/// Obergrenze für eine Toleranz in Prozent.
const MAX_TOLERANCE_PCT: u32 = 1000;

/// Warum eine Policy nicht gilt. Ohne Werte aus der Datei — nur die Stelle.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// Kein gültiges JSON in der erwarteten Form (auch: unbekanntes Feld).
    #[error("{POLICY_PATH}: invalid replay policy — {category} at line {line}, column {column}")]
    Parse {
        /// `syntax error`, `unexpected value or unknown field`, …
        category: &'static str,
        /// Zeile.
        line: usize,
        /// Spalte.
        column: usize,
    },
    /// Eine Regel ist inhaltlich unzulässig.
    #[error("{POLICY_PATH}: {0}")]
    Invalid(&'static str),
}

/// Ein freigegebener Runner: Programm, Subkommandos, Flags.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerRule {
    /// Der nackte Programmname (`cargo`), nie ein Pfad.
    pub argv0: String,
    /// Erlaubte Werte für `argv[1]` (`test`, `nextest`, `bench`).
    #[serde(default)]
    pub sub: Vec<String>,
    /// Erlaubte Flags, exakt (`--release`) — auch in der Form
    /// `--flag=wert`. `--` muss eigens genannt sein.
    #[serde(default)]
    pub allow_flags: Vec<String>,
}

/// Eine Toleranz für Benchmarks, deren Name auf `bench` passt (`*`, `?`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToleranceOverride {
    /// Das Muster.
    pub bench: String,
    /// Die Toleranz in Prozent.
    pub pct: u32,
}

/// Benchmark-Toleranzen.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tolerance {
    /// Die Toleranz ohne passendes Override.
    #[serde(default = "default_pct")]
    pub default_pct: u32,
    /// Overrides, die erste passende gilt.
    #[serde(default)]
    pub overrides: Vec<ToleranceOverride>,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            default_pct: DEFAULT_TOLERANCE_PCT,
            overrides: Vec::new(),
        }
    }
}

fn default_pct() -> u32 {
    DEFAULT_TOLERANCE_PCT
}

/// Zeitlimits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeouts {
    /// Je Befehl, Sekunden.
    #[serde(default = "default_per_command")]
    pub per_command_s: u64,
    /// Für den ganzen Lauf, Sekunden.
    #[serde(default = "default_total")]
    pub total_s: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            per_command_s: DEFAULT_PER_COMMAND_S,
            total_s: DEFAULT_TOTAL_S,
        }
    }
}

fn default_per_command() -> u64 {
    DEFAULT_PER_COMMAND_S
}

fn default_total() -> u64 {
    DEFAULT_TOTAL_S
}

/// Die Team-Policy `.minds/replay.json` — Konfigurations-Vokabular, nur
/// gelesen, nie geschrieben (deshalb nur `Deserialize`). Was sie erlaubt,
/// entscheidet `minds_reader::replay::allows` (W5).
///
/// Gelesen mit `deny_unknown_fields`, wie `.minds/redact.json`: Ein
/// Tippfehler (`"default_pc": 5`) fiele sonst still auf den Default zurück
/// — hier hieße das eine **weitere** Toleranz als beschlossen.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayPolicy {
    /// [`POLICY_SCHEMA`].
    pub schema: u32,
    /// Freigegebene Runner, je mit einem Namen (`cargo-test`).
    #[serde(default)]
    pub runners: BTreeMap<String, RunnerRule>,
    /// Benchmark-Toleranzen.
    #[serde(default)]
    pub tolerance: Tolerance,
    /// Zeitlimits.
    #[serde(default)]
    pub timeouts: Timeouts,
    /// Zusätzliche Umgebungsvariablen, die an den Prozess durchgereicht
    /// werden (exakte Namen), über `PATH`, `HOME`, `CARGO_*`, `RUSTUP_*`
    /// hinaus.
    #[serde(default)]
    pub env: Vec<String>,
}

impl ReplayPolicy {
    /// Keine Policy im Commit: nichts ist freigegeben, Standard-Limits.
    pub fn none() -> Self {
        Self {
            schema: POLICY_SCHEMA,
            ..Self::default()
        }
    }

    /// Liest und prüft die Policy. Fehlerhaft heißt Abbruch, nie „keine".
    /// Die Fehlermeldung nennt nur Kategorie und Stelle, nie einen Wert
    /// (wie bei `.minds/redact.json`).
    pub fn parse(bytes: &[u8]) -> Result<Self, PolicyError> {
        let policy: Self = serde_json::from_slice(bytes).map_err(|err| PolicyError::Parse {
            category: match err.classify() {
                serde_json::error::Category::Syntax => "syntax error",
                serde_json::error::Category::Data => "unexpected value or unknown field",
                serde_json::error::Category::Eof => "unexpected end of file",
                serde_json::error::Category::Io => "read error",
            },
            line: err.line(),
            column: err.column(),
        })?;
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), PolicyError> {
        if self.schema != POLICY_SCHEMA {
            return Err(PolicyError::Invalid("unsupported schema (expected 1)"));
        }
        for rule in self.runners.values() {
            // Windows startet `.bat`/`.cmd` über `cmd.exe` — eine Shell.
            // Windows ignoriert Punkte und Leerzeichen am Ende (`npm.cmd.`).
            let lower = rule.argv0.trim_end_matches(['.', ' ']).to_ascii_lowercase();
            if lower.ends_with(".bat") || lower.ends_with(".cmd") {
                return Err(PolicyError::Invalid(
                    "runner argv0 must not be a batch file",
                ));
            }
            if !bare_word(&rule.argv0) {
                return Err(PolicyError::Invalid(
                    "runner argv0 must be a bare program name, not a path",
                ));
            }
            if rule.sub.is_empty() {
                return Err(PolicyError::Invalid("runner sub must name a subcommand"));
            }
            if !rule
                .sub
                .iter()
                .all(|sub| bare_word(sub) && !sub.starts_with('-'))
            {
                return Err(PolicyError::Invalid(
                    "runner sub entries must be plain words",
                ));
            }
            if !rule.allow_flags.iter().all(|flag| {
                flag.starts_with('-')
                    && flag.len() <= 64
                    && !flag.contains('=')
                    && !flag.chars().any(char::is_control)
            }) {
                return Err(PolicyError::Invalid(
                    "allow_flags entries must be flags without a value",
                ));
            }
        }
        let pcts = std::iter::once(self.tolerance.default_pct)
            .chain(self.tolerance.overrides.iter().map(|o| o.pct));
        if pcts.into_iter().any(|pct| pct > MAX_TOLERANCE_PCT) {
            return Err(PolicyError::Invalid("tolerance above 1000%"));
        }
        if self
            .tolerance
            .overrides
            .iter()
            .any(|o| o.bench.is_empty() || o.bench.len() > 256)
        {
            return Err(PolicyError::Invalid("tolerance override without a pattern"));
        }
        let Timeouts {
            per_command_s,
            total_s,
        } = self.timeouts;
        if per_command_s == 0 || total_s == 0 || per_command_s.max(total_s) > MAX_TIMEOUT_S {
            return Err(PolicyError::Invalid(
                "timeouts must be between 1 second and 24 hours",
            ));
        }
        if !self.env.iter().all(|name| env_name(name)) {
            return Err(PolicyError::Invalid(
                "env entries must be variable names ([A-Z_][A-Z0-9_]*)",
            ));
        }
        // Die Policy kommt aus dem geprüften Commit — in einer
        // Merge-Request-Pipeline also womöglich vom Agenten. Zugangsdaten
        // darf sie deshalb nie freigeben.
        if self.env.iter().any(|name| sensitive_env_name(name)) {
            return Err(PolicyError::Invalid(
                "env must not forward credentials, CI or minds variables",
            ));
        }
        Ok(())
    }
}

/// Teile von Variablennamen, die nach Zugangsdaten aussehen.
const CREDENTIAL_PARTS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASS",
    "CREDENTIAL",
    "AUTH",
    "COOKIE",
    "_KEY",
    "APIKEY",
    "ACCESSKEY",
    "PRIVATE",
    "DSN",
    "NETRC",
    "PWD",
];

/// Endungen von Variablennamen, die nach Zugangsdaten aussehen (`GH_PAT`) —
/// als Teil wären sie zu breit (`_PATH`).
const CREDENTIAL_SUFFIXES: &[&str] = &["_PAT", "KEY"];

/// Präfixe von Variablen, die zur CI, zu SSH oder zu Minds selbst gehören.
const SENSITIVE_PREFIXES: &[&str] = &[
    "MINDS_",
    "SSH_",
    "CI_",
    "GITLAB_",
    "GITHUB_",
    "ACTIONS_",
    "RUNNER_",
    "GPG_",
    // Verweise auf Zugangsdaten-Dateien und -Konfiguration.
    "GIT_",
    "DOCKER_",
    "NPM_CONFIG_",
    "KUBE",
    "PIP_",
    "TWINE_",
    "AWS_",
    "AZURE_",
    "GOOGLE_",
];

/// Ob eine Umgebungsvariable nie an einen Replay-Prozess gehen darf — auch
/// nicht, wenn die Policy sie nennt: Zugangsdaten-förmige Namen, CI-, SSH-
/// und Minds-Variablen (der Pfad des Signaturschlüssels).
pub fn sensitive_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    CREDENTIAL_PARTS.iter().any(|part| upper.contains(part))
        || CREDENTIAL_SUFFIXES
            .iter()
            .any(|suffix| upper.ends_with(suffix))
        || SENSITIVE_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

/// Ein Programm- oder Subkommando-Name: kurz, ohne Pfadtrenner.
fn bare_word(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= 64
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && word != "."
        && word != ".."
}

/// Ein Umgebungsvariablen-Name: `[A-Z_][A-Z0-9_]*`.
fn env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && name.len() <= 128
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_POLICY: &str = r#"{"schema":1,"runners":{"cargo-test":{"argv0":"cargo","sub":["test","nextest"],"allow_flags":["-p","--package","--release","--lib","--test","--","--exact"]}},"tolerance":{"default_pct":25,"overrides":[{"bench":"sort/*","pct":15}]},"timeouts":{"per_command_s":600,"total_s":1800}}"#;

    #[test]
    fn the_spec_policy_parses_and_typos_fail_closed() {
        let policy = ReplayPolicy::parse(SPEC_POLICY.as_bytes()).unwrap();
        assert_eq!(policy.timeouts.per_command_s, 600);
        assert_eq!(policy.tolerance.overrides[0].pct, 15);
        assert_eq!(policy.runners["cargo-test"].argv0, "cargo");
        // Ein Tippfehler fällt nicht still auf den Default zurück.
        assert!(matches!(
            ReplayPolicy::parse(br#"{"schema":1,"tolerance":{"default_pc":5}}"#),
            Err(PolicyError::Parse { .. })
        ));
        for invalid in [
            r#"{"schema":2}"#,
            r#"{"schema":1,"runners":{"x":{"argv0":"/usr/bin/cargo","sub":["test"]}}}"#,
            r#"{"schema":1,"runners":{"x":{"argv0":"cargo","sub":[]}}}"#,
            r#"{"schema":1,"runners":{"x":{"argv0":"cargo","sub":["test"],"allow_flags":["--manifest-path=x"]}}}"#,
            r#"{"schema":1,"runners":{"x":{"argv0":"cargo","sub":["test"],"allow_flags":["release"]}}}"#,
            r#"{"schema":1,"timeouts":{"per_command_s":0}}"#,
            r#"{"schema":1,"timeouts":{"total_s":999999999}}"#,
            r#"{"schema":1,"env":["lower"]}"#,
            r#"{"schema":1,"env":["CI_JOB_TOKEN"]}"#,
            r#"{"schema":1,"env":["MINDS_ANCHOR_KEY_FILE"]}"#,
            r#"{"schema":1,"env":["SSH_AUTH_SOCK"]}"#,
            r#"{"schema":1,"env":["NPM_TOKEN"]}"#,
            r#"{"schema":1,"env":["AWS_SECRET_ACCESS_KEY"]}"#,
            r#"{"schema":1,"env":["OPENAI_APIKEY"]}"#,
            r#"{"schema":1,"env":["GH_PAT"]}"#,
            r#"{"schema":1,"env":["DATABASE_DSN"]}"#,
            r#"{"schema":1,"runners":{"x":{"argv0":"npm.cmd","sub":["test"]}}}"#,
            r#"{"schema":1,"runners":{"x":{"argv0":"npm.cmd.","sub":["test"]}}}"#,
            r#"{"schema":1,"tolerance":{"default_pct":5000}}"#,
        ] {
            assert!(
                matches!(
                    ReplayPolicy::parse(invalid.as_bytes()),
                    Err(PolicyError::Invalid(_))
                ),
                "{invalid}"
            );
        }
        // Die Fehlermeldung zitiert keinen Wert aus der Datei.
        let err = ReplayPolicy::parse(br#"{"schema":"glpat-SECRET"}"#).unwrap_err();
        assert!(!err.to_string().contains("glpat"), "{err}");
        assert!(ReplayPolicy::parse(br#"{"schema":1,"env":["RUST_LOG"]}"#).is_ok());
        // Keine Policy: nichts freigegeben, Standard-Limits.
        let none = ReplayPolicy::none();
        assert!(none.runners.is_empty());
        assert_eq!(none.timeouts, Timeouts::default());
    }

    fn sample() -> ReplayRecord {
        ReplayRecord {
            kind: REPLAY_KIND.into(),
            policy: None,
            project: None,
            schema: REPLAY_SCHEMA,
            commit: "ab".repeat(20),
            session: format!("b3-{}", "cd".repeat(32)),
            interpretation_version: 1,
            results: vec![ReplayResult {
                turn: 3,
                call: 0,
                argv: vec!["cargo".into(), "test".into()],
                expected: Expected {
                    class: ExecClass::Test,
                    runner: "cargo-test".into(),
                    exit_code: None,
                    tests: Some(TestCounts {
                        passed: 12,
                        failed: 0,
                        ignored: 0,
                    }),
                    benches: Vec::new(),
                },
                observed: Some(Observed {
                    exit_code: Some(0),
                    tests: Some(TestCounts {
                        passed: 12,
                        failed: 0,
                        ignored: 0,
                    }),
                    ..Observed::default()
                }),
                verdict: ReplayVerdict::Reproduced,
                reason: None,
            }],
            environment: ReplayEnvironment::default(),
        }
    }

    #[test]
    fn the_record_round_trips_and_reads_tolerantly() {
        let record = sample();
        let bytes = record.canonical_bytes().unwrap();
        let back: ReplayRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, record);
        assert_eq!(ReplayRecord::id_of_bytes(&bytes), record.id().unwrap());

        // Tolerant: unbekannte Felder brechen das Lesen nicht.
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["future"] = serde_json::json!({"x": 1});
        value["results"][0]["future"] = serde_json::json!(true);
        let back: ReplayRecord = serde_json::from_value(value).unwrap();
        assert_eq!(back, record);
    }

    /// Golden: die kanonische Form von [`golden_record`].
    const RECORD_CANONICAL: &str = r#"{"commit":"0123456789abcdef0123456789abcdef01234567","environment":{"ci":"gitlab","image":"rust:1.90","pipeline":"4711"},"interpretation_version":1,"kind":"replay","policy":"3b18e512dba79e4c8300dd08aeb37f8e728b8dad","results":[{"argv":["cargo","test","-p","sort"],"call":0,"expected":{"class":"test","runner":"cargo-test","tests":{"failed":0,"ignored":0,"passed":12}},"observed":{"exit_code":0,"tests":{"failed":0,"ignored":0,"passed":11}},"reason":"tests: recorded 12 passed, observed 11 passed","turn":2,"verdict":"not_reproduced"},{"argv":["python","bench.py"],"call":1,"expected":{"benches":[{"name":"sort/10k","unit":"ns","value":1000}],"class":"bench","runner":"cargo-bench-criterion"},"reason":"not allowlisted","turn":3,"verdict":"skipped"}],"schema":1,"session":"b3-abababababababababababababababababababababababababababababababab"}"#;

    /// Golden: blake3 dieser Bytes — die Id unter
    /// `refs/minds/anchors/replay/` (unabhängig nachgerechnet mit Pythons
    /// `blake3`).
    const RECORD_BLAKE3: &str = "6c6c8bb874d758bd95f4d3a016c6d5ba2f6465284c9472b86c7882dab2806cf1";

    fn golden_record() -> ReplayRecord {
        let counts = |passed| TestCounts {
            passed,
            failed: 0,
            ignored: 0,
        };
        ReplayRecord {
            kind: REPLAY_KIND.into(),
            schema: REPLAY_SCHEMA,
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            session: format!("b3-{}", "ab".repeat(32)),
            interpretation_version: 1,
            results: vec![
                ReplayResult {
                    turn: 2,
                    call: 0,
                    argv: vec!["cargo".into(), "test".into(), "-p".into(), "sort".into()],
                    expected: Expected {
                        class: ExecClass::Test,
                        runner: "cargo-test".into(),
                        exit_code: None,
                        tests: Some(counts(12)),
                        benches: Vec::new(),
                    },
                    observed: Some(Observed {
                        exit_code: Some(0),
                        tests: Some(counts(11)),
                        ..Observed::default()
                    }),
                    verdict: ReplayVerdict::NotReproduced,
                    reason: Some("tests: recorded 12 passed, observed 11 passed".into()),
                },
                ReplayResult {
                    turn: 3,
                    call: 1,
                    argv: vec!["python".into(), "bench.py".into()],
                    expected: Expected {
                        class: ExecClass::Bench,
                        runner: "cargo-bench-criterion".into(),
                        exit_code: None,
                        tests: None,
                        benches: vec![BenchValue {
                            name: "sort/10k".into(),
                            value: 1000,
                            unit: "ns".into(),
                        }],
                    },
                    observed: None,
                    verdict: ReplayVerdict::Skipped,
                    reason: Some("not allowlisted".into()),
                },
            ],
            policy: Some("3b18e512dba79e4c8300dd08aeb37f8e728b8dad".into()),
            project: None,
            environment: ReplayEnvironment {
                ci: Some("gitlab".into()),
                pipeline: Some("4711".into()),
                image: Some("rust:1.90".into()),
            },
        }
    }

    #[test]
    fn replay_record_golden() {
        let record = golden_record();
        let bytes = record.canonical_bytes().unwrap();
        assert_eq!(String::from_utf8(bytes.clone()).unwrap(), RECORD_CANONICAL);
        assert_eq!(record.id().unwrap().hex(), RECORD_BLAKE3);
        // Kanonisch heißt auch: Was gelesen wird, schreibt sich byte-gleich
        // zurück.
        let back: ReplayRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.canonical_bytes().unwrap(), bytes);
    }

    #[test]
    fn verdict_words_match_the_serialized_form() {
        for verdict in [
            ReplayVerdict::Reproduced,
            ReplayVerdict::NotReproduced,
            ReplayVerdict::Skipped,
        ] {
            assert_eq!(
                serde_json::to_string(&verdict).unwrap(),
                format!("\"{}\"", verdict.as_str())
            );
        }
    }
}
