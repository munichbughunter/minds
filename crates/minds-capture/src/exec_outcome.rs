//! Was ein Test- oder Benchmark-Runner gemeldet hat — als Zahlen (EA-18a).
//!
//! Ein [`ToolCall`](minds_core::ToolCall) hielt bisher fest, *dass* ein
//! Kommando lief, nicht *was* es berichtete. Die rohe Tool-Antwort steht nur
//! im Journal, und das wird nach dem Checkpoint verworfen. Ohne gespeichertes
//! Ergebnis hätte ein Replay (EA-18b) nichts, womit es vergleichen könnte.
//!
//! Dieses Modul ist die **reine** Hälfte der Deutung: kein I/O, kein Journal,
//! keine Agent-Spezifika. Den Payload zerlegt der Adapter
//! ([`ToolAdapter::exec_report`](crate::ToolAdapter::exec_report)), den
//! Aufruf mit seinem Post-Event verbindet der Checkpoint.
//!
//! # Drei Schritte, jeder fail-closed
//!
//! 1. **Einfaches Kommando** ([`simple_argv`]): Nur ein einzelnes, einfaches
//!    Kommando wird in ein argv zerlegt — kein `;`, `&&`, `|`, keine
//!    Umleitung, keine Subshell, keine Expansion. Einfache Quotes sind
//!    erlaubt, weil ihre Bedeutung ohne Shell feststeht. `KEY=VALUE`-Präfixe
//!    nur aus [`ENV_ALLOWLIST`] (Variablen, die das Ergebnis nicht ändern);
//!    sie gehören nicht zum argv. So braucht ein Replay nie eine Shell.
//! 2. **Bekannter Runner** ([`recognize`]): `cargo test`, `cargo nextest run`,
//!    `cargo bench` (criterion), `pytest`. Alles andere: kein Ergebnis.
//! 3. **Zusammenfassung parsen** ([`interpret`]): nur die Zahlen der
//!    Zusammenfassungszeilen und die Bench-Namen. Eine Zeile, die wie eine
//!    Zusammenfassung beginnt, aber nicht vollständig passt, ergibt *kein*
//!    Ergebnis — lieber keine Zahl als eine falsche.
//!
//! Die Ausgabeformen sind **aufgezeichnet, nicht geraten**: siehe
//! `tests/fixtures/claude-code/README.md` (cargo 1.9x/libtest, nextest
//! 0.9.146, criterion 0.5, pytest 9.1).
//!
//! # Keine Fließkommazahlen
//!
//! Criterion meldet `[2.8587 µs 2.8631 µs 2.8676 µs]`. Der Median wird ohne
//! `f64` in ganze Nanosekunden umgerechnet ([`to_nanos`]): dezimal geparst,
//! ganzzahlig skaliert, kaufmännisch gerundet. Deterministisch auf jeder
//! Plattform — und das Envelope bleibt frei von Float-Formatierungsfragen.

use std::collections::BTreeMap;

use minds_core::{BenchValue, ExecClass, ExecOutcome, TestCounts};

/// Was der Adapter aus dem Post-Payload eines Shell-Aufrufs zieht: die
/// sichtbare Ausgabe und — falls der Payload ihn trägt — den Exit-Code.
///
/// Der Text verlässt dieses Modul nie: [`interpret`] liest nur Zahlen und
/// Bench-Namen heraus.
#[derive(Clone, PartialEq, Eq)]
pub struct ExecReport {
    /// stdout und stderr, wie der Payload sie trägt (Claude Code führt beide
    /// zusammen in `stdout`).
    pub output: String,
    /// Der Exit-Code, nur wenn der Payload ihn nennt.
    pub exit_code: Option<i32>,
}

/// Bewusst ohne die Ausgabe: Ein späteres `{:?}` in einem Log druckte sonst
/// stdout.
impl std::fmt::Debug for ExecReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecReport")
            .field("output_len", &self.output.len())
            .field("exit_code", &self.exit_code)
            .finish()
    }
}

/// Ein erkannter Runner — bestimmt Klasse, kanonischen Namen und Parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    /// `cargo test` — libtest-Zeilen `test result: …`, über alle Binaries
    /// summiert.
    CargoTest,
    /// `cargo nextest run` — die `Summary`-Zeile.
    CargoNextest,
    /// `cargo bench` mit criterion — `time: [lo mid hi]`, der Median.
    CargoBenchCriterion,
    /// `pytest` — die abschließende Zusammenfassung.
    Pytest,
}

impl Runner {
    /// Der kanonische Name im Envelope.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CargoTest => "cargo-test",
            Self::CargoNextest => "cargo-nextest",
            Self::CargoBenchCriterion => "cargo-bench-criterion",
            Self::Pytest => "pytest",
        }
    }

    /// Test oder Benchmark.
    pub fn class(self) -> ExecClass {
        match self {
            Self::CargoBenchCriterion => ExecClass::Bench,
            _ => ExecClass::Test,
        }
    }
}

/// Umgebungsvariablen, die als `KEY=VALUE`-Präfix erlaubt sind: Sie ändern
/// Logging, Backtraces und Farben — nicht, welche Tests laufen oder was sie
/// ergeben. Alles andere (etwa `RUSTFLAGS=`, `DATABASE_URL=`) macht das
/// Kommando zu einem nicht gedeuteten. Die erlaubten Werte prüft
/// [`env_value_ok`] je Variable.
pub const ENV_ALLOWLIST: &[&str] = &[
    "RUST_LOG",
    "RUST_BACKTRACE",
    "RUST_LIB_BACKTRACE",
    "CARGO_TERM_COLOR",
    "NO_COLOR",
];

/// Ist `value` ein Wert, wie ihn die erlaubte Variable `name` kennt? Je
/// Variable die echte Grammatik, so eng, dass kein erkennbares Geheimnis
/// hineinpasst:
///
/// - `RUST_LOG`: kommagetrennte Direktiven `ziel[=stufe]` oder `stufe`; ein
///   Ziel ist ein Rust-Pfad in Kleinbuchstaben (`minds_capture::adapter`),
///   eine Stufe `trace|debug|info|warn|error|off`. Großbuchstaben,
///   Bindestriche und Segmente über 24 Zeichen kommen nicht vor — Token- und
///   Entropie-Detektoren brauchen 32 und mehr zusammenhängende Zeichen
///   (`ghp_` + 36, `npm_` + 36).
/// - `RUST_BACKTRACE`, `RUST_LIB_BACKTRACE`: `0`, `1`, `full`, `short`.
/// - `CARGO_TERM_COLOR`: `auto`, `always`, `never`.
/// - `NO_COLOR`: leer, `0`, `1`, `true`, `false`.
fn env_value_ok(name: &str, value: &str) -> bool {
    const LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error", "off"];
    let rust_path = |path: &str| {
        !path.is_empty()
            && path.len() <= 128
            && path.split("::").all(|segment| {
                segment.len() <= 24
                    && segment
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            })
    };
    match name {
        "RUST_LOG" => value
            .split(',')
            .all(|directive| match directive.split_once('=') {
                Some((target, level)) => rust_path(target) && LEVELS.contains(&level),
                None => LEVELS.contains(&directive) || rust_path(directive),
            }),
        "RUST_BACKTRACE" | "RUST_LIB_BACKTRACE" => matches!(value, "0" | "1" | "full" | "short"),
        "CARGO_TERM_COLOR" => matches!(value, "auto" | "always" | "never"),
        "NO_COLOR" => matches!(value, "" | "0" | "1" | "true" | "false"),
        _ => false,
    }
}

/// Obergrenze für ein gedeutetes Kommando (Bytes). Ein Runner-Aufruf ist
/// kurz; das Kommando ist fremd und kann bis zur Hook-Grenze (32 MiB)
/// wachsen. Darüber wird nichts zerlegt und nichts gesucht — linear oder
/// nicht, der Checkpoint soll an einem Kommando nicht hängen.
pub const MAX_COMMAND: usize = 64 * 1024;

/// Obergrenze für Benchmarks je Lauf — darüber kein Ergebnis statt eines
/// angeschnittenen.
const MAX_BENCHES: usize = 4096;

/// Obergrenze für die Länge eines Bench-Namens (Bytes).
const MAX_BENCH_NAME: usize = 256;

/// Warum ein Kommando kein einfaches ist. Ohne Inhalt: Der Grund wird nicht
/// gespeichert, nur „nicht gedeutet".
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a single simple command")]
pub struct NotSimple;

/// Zerlegt `command` in ein argv — nur, wenn es ein einzelnes, einfaches
/// Kommando ist (siehe Modul-Doku). Erlaubte `KEY=VALUE`-Präfixe werden
/// entfernt.
///
/// Abgelehnt wird jedes Zeichen, dessen Bedeutung eine Shell bräuchte:
/// Steuer- und Umleitungszeichen (`; & | < > ( )`), Expansion (`$`, Backtick,
/// Glob `* ? [ ]`, Klammern `{ }`, `~`, `#` und `^` überall, `=` am
/// Wortanfang — die beiden letzten Regeln für zsh), `\` und `!`, Steuer- und
/// Bidi-Zeichen. In einfachen Quotes ist alles wörtlich (außer Steuer- und
/// Bidi-Zeichen); in doppelten Quotes bleiben `$`, Backtick, `\` und `!`
/// verboten, weil sie dort noch expandieren.
///
/// **Einfach heißt nicht harmlos.** Ein so zerlegtes argv ist eindeutig ohne
/// Shell ausführbar — ob es ausgeführt werden *darf* (`--config`,
/// `--manifest-path`, `pytest -p <plugin>`, `+toolchain`), entscheidet erst
/// die Allowlist eines Replays (EA-18b). Aliase und Shell-Funktionen der
/// Agent-Shell sieht das argv ebenfalls nicht.
pub fn simple_argv(command: &str) -> Result<Vec<String>, NotSimple> {
    if command.len() > MAX_COMMAND {
        return Err(NotSimple);
    }
    #[derive(PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }

    // Ein Wort: der Text und, falls es eine Zuweisung sein kann, die
    // Länge des ungequoteten Namens vor dem ersten `=`.
    struct Word {
        text: String,
        assign: Option<usize>,
        quoted: bool,
        /// Ist `text` bisher ein gültiger Zuweisungsname? Schrittweise
        /// mitgeführt — ein erneutes Prüfen des ganzen Worts bei jedem `=`
        /// wäre bei fremden Kommandos quadratisch.
        name_ok: bool,
    }

    let mut words: Vec<Word> = Vec::new();
    let mut current: Option<Word> = None;
    let mut quote = Quote::None;
    // Hat das vorige Zeichen eine einfache Quote geschlossen?
    let mut closed_single = false;

    for c in command.chars() {
        // Steuer- und unsichtbare Zeichen, auch in Quotes: Zeilenumbrüche
        // trennen Kommandos, ESC/BEL/U+202E würden in jeder späteren Anzeige
        // des argv (show, HTML, Replay-Ausgabe) zur Falle.
        // Der Tab ist Worttrenner wie das Leerzeichen.
        if (c.is_control() && c != '\t') || is_invisible(c) {
            return Err(NotSimple);
        }
        // zsh mit `RC_QUOTES` liest `''` in einfachen Quotes als ein
        // wörtliches `'` (`'it''s'` → `it's`), Bash als zwei Strings
        // (`its`). Eine Quote, die direkt nach dem Schließen einer einfachen
        // wieder öffnet, ist deshalb mehrdeutig.
        if c == '\'' && closed_single {
            return Err(NotSimple);
        }
        closed_single = quote == Quote::Single && c == '\'';
        match quote {
            Quote::Single => {
                if c == '\'' {
                    quote = Quote::None;
                } else {
                    current.as_mut().ok_or(NotSimple)?.text.push(c);
                }
            }
            Quote::Double => match c {
                '"' => quote = Quote::None,
                '$' | '`' | '\\' | '!' => return Err(NotSimple),
                _ => current.as_mut().ok_or(NotSimple)?.text.push(c),
            },
            Quote::None => match c {
                ' ' | '\t' => {
                    if let Some(word) = current.take() {
                        words.push(word);
                    }
                }
                ';' | '&' | '|' | '<' | '>' | '(' | ')' | '$' | '`' | '\\' | '*' | '?' | '['
                | ']' | '{' | '}' | '!' => return Err(NotSimple),
                '\'' | '"' => {
                    let word = current.get_or_insert_with(|| Word {
                        text: String::new(),
                        assign: None,
                        quoted: false,
                        name_ok: true,
                    });
                    word.quoted = true;
                    quote = if c == '\'' {
                        Quote::Single
                    } else {
                        Quote::Double
                    };
                }
                // `~` und `#` ungequotet **überall**: am Wortanfang Tilde und
                // Kommentar, Bash expandiert `~` auch nach `=`/`:`
                // (`FOO=~/x`), und unter zsh mit `EXTENDED_GLOB` (Claude Code
                // übernimmt die Optionen der Nutzer-Shell) sind beide mitten
                // im Wort Glob-Operatoren — ebenso `^` (Negation).
                '~' | '#' | '^' => return Err(NotSimple),
                // Am Wortanfang: `=cmd` expandiert in zsh zum Programmpfad.
                '=' if current.is_none() => return Err(NotSimple),
                _ => {
                    let word = current.get_or_insert_with(|| Word {
                        text: String::new(),
                        assign: None,
                        quoted: false,
                        name_ok: true,
                    });
                    if c == '='
                        && word.assign.is_none()
                        && !word.quoted
                        && word.name_ok
                        && !word.text.is_empty()
                    {
                        word.assign = Some(word.text.len());
                    }
                    word.name_ok = word.name_ok
                        && (c == '_'
                            || c.is_ascii_alphabetic()
                            || (c.is_ascii_digit() && !word.text.is_empty()));
                    word.text.push(c);
                }
            },
        }
    }
    if quote != Quote::None {
        return Err(NotSimple);
    }
    if let Some(word) = current.take() {
        words.push(word);
    }

    // Führende Zuweisungen: nur erlaubte, und sie fallen aus dem argv.
    let mut argv = Vec::with_capacity(words.len());
    let mut leading = true;
    for word in words {
        if leading && let Some(name_len) = word.assign {
            // Der Wert fällt aus dem argv und damit aus dessen Gegenprobe in
            // der Redaction — deshalb nur ungequotet und nur in der Grammatik
            // der jeweiligen Variablen ([`env_value_ok`]). Ein gequoteter
            // Wert könnte den Kontext eines Detektors im Text der `arguments`
            // brechen (`RUST_LOG=--pass''word=hunter2`), ein freier Wert ein
            // Token an der Gegenprobe vorbeitragen (`RUST_LOG=ghp_…`).
            if !word.quoted && env_value_ok(&word.text[..name_len], &word.text[name_len + 1..]) {
                continue;
            }
            return Err(NotSimple);
        }
        leading = false;
        argv.push(word.text);
    }
    if argv.is_empty() {
        return Err(NotSimple);
    }
    Ok(argv)
}

/// Unsichtbare Zeichen — drei Gruppen:
///
/// - alle `Default_Ignorable_Code_Point`s (Unicode 15.1, `DerivedCoreProperties`):
///   Soft Hyphen, CGJ U+034F, Hangul-Füller U+115F/U+1160/U+3164/U+FFA0,
///   Khmer-Vokal-Inhärente U+17B4/U+17B5, mongolische Variation Selectors
///   U+180B–U+180F, Zero-Width-Zeichen, Bidi-Steuerung (Trojan-Source-Klasse),
///   Word Joiner und Invisible Operators U+2060–U+206F, Variation Selectors,
///   BOM, Tag-Zeichen fürs „ASCII smuggling" (U+E0000–U+E0FFF), …;
/// - die übrigen Format-Zeichen der Kategorie Cf (arabische Zahlzeichen,
///   Interlinear-Anker U+FFF9–U+FFFB, …) und die Zeilen-/Absatztrenner
///   U+2028/U+2029;
/// - sichtbar leere Füller, die keine Leerzeichen sind: das Braille-Leerfeld
///   U+2800.
///
/// Abgelehnt im argv und in Bench-Namen: Beides wird später angezeigt, und
/// ein unsichtbares Zeichen mitten in `--pass⁠word` oder zwischen Flag und
/// Wert bräche den Kontext der Redaction-Detektoren.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
}

/// `[A-Za-z_][A-Za-z0-9_]*` — der Name einer Shell-Zuweisung.
fn is_env_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Erkennt einen Runner am argv. `argv[0]` muss der nackte Programmname sein
/// (`cargo`, `pytest`) — ein Pfad wie `.venv/bin/pytest` ist für ein Replay
/// nicht ohne Weiteres auflösbar und bleibt ungedeutet. Eine
/// `+toolchain`-Angabe direkt nach `cargo` ist erlaubt.
pub fn recognize(argv: &[String]) -> Option<Runner> {
    let arg = |i: usize| argv.get(i).map(String::as_str);
    match arg(0)? {
        "cargo" => {
            let mut sub = if arg(1)?.starts_with('+') { 2 } else { 1 };
            // Globale Flags ohne Wert vor dem Subkommando
            // (`cargo --locked test`, `cargo -q test`).
            while arg(sub).is_some_and(is_cargo_global_flag) {
                sub += 1;
            }
            match arg(sub)? {
                "test" => Some(Runner::CargoTest),
                "bench" => Some(Runner::CargoBenchCriterion),
                "nextest" if arg(sub + 1) == Some("run") => Some(Runner::CargoNextest),
                _ => None,
            }
        }
        "pytest" | "py.test" => Some(Runner::Pytest),
        "python" | "python3" if arg(1) == Some("-m") && arg(2) == Some("pytest") => {
            Some(Runner::Pytest)
        }
        _ => None,
    }
}

/// Globale cargo-Flags ohne eigenen Wert, die vor dem Subkommando stehen
/// dürfen.
fn is_cargo_global_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--locked" | "--frozen" | "--offline" | "-q" | "--quiet" | "-v" | "-vv" | "--verbose"
    ) || arg.starts_with("--color=")
        || (arg.starts_with("-Z") && arg.len() > 2)
}

/// Programme, die ein Kommando nur umhüllen — eine **geschlossene** Liste,
/// damit `pip install pytest` oder `which pytest` nicht als Runner-Aufruf
/// gelten. Zwei Wörter für die Projekt-Runner (`uv run`).
const WRAPPERS: &[&[&str]] = &[
    &["timeout"],
    &["env"],
    &["time"],
    &["nice"],
    &["nohup"],
    &["uv", "run"],
    &["poetry", "run"],
    &["pdm", "run"],
];

/// Ruft ein **einfaches** Kommando einen bekannten Runner über einen
/// Wrapper aus [`WRAPPERS`] auf (`timeout 600 cargo test`,
/// `env RUST_LOG=x cargo test`, `uv run pytest`)? Nach dem Wrapper werden
/// nur seine Optionen (`-…`), Zuweisungen (`K=V`) und Zahlen/Dauern (`600`,
/// `10m`) übersprungen; der Runner muss genau an der Stelle danach stehen.
/// `pip install pytest`, `which pytest`, `git commit -m 'fix cargo test'`:
/// kein Wrapper, kein Treffer.
pub fn wraps_runner(argv: &[String]) -> bool {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    WRAPPERS.iter().any(|wrapper| {
        let Some(rest) = words.strip_prefix(*wrapper) else {
            return false;
        };
        let skip = rest
            .iter()
            .take_while(|w| w.starts_with('-') || w.contains('=') || is_duration(w))
            .count();
        recognize(&argv[wrapper.len() + skip..]).is_some()
    })
}

/// `600`, `10m`, `1.5h` — die Dauer- bzw. Zahlargumente von `timeout`/`nice`.
fn is_duration(word: &str) -> bool {
    let digits = word.trim_end_matches(['s', 'm', 'h', 'd']);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Nennt ein Kommando einen bekannten Runner — als Runner selbst oder
/// umhüllt? Für einfache Kommandos am exakten argv, sonst grob
/// ([`mentions_runner`]).
pub fn names_runner(command: &str) -> bool {
    match simple_argv(command) {
        Ok(argv) => recognize(&argv).is_some() || wraps_runner(&argv),
        Err(NotSimple) => mentions_runner(command),
    }
}

/// Nennt ein (nicht einfaches) Kommando einen bekannten Runner? Grundlage
/// des Hinweises „compound command not interpreted": Er steht nur dort, wo
/// ein Ergebnis zu erwarten war — nicht an jedem `ls | head`.
///
/// Grob, aber nicht beliebig: an Shell-Steuerzeichen in Segmente getrennt,
/// je Segment nach führenden Zuweisungen genau eine Stelle — der Runner
/// selbst ([`recognize`]) oder ein Wrapper davor ([`wraps_runner`]).
/// `cd x && cargo test` trifft, `pip install pytest && ls` nicht. Quotes
/// werden nur von Wortenden entfernt, nicht ausgewertet.
pub fn mentions_runner(command: &str) -> bool {
    if command.len() > MAX_COMMAND {
        return false;
    }
    command
        .split(|c: char| ";&|()<>`\n".contains(c))
        .any(|segment| {
            let words: Vec<String> = segment
                .split_whitespace()
                .map(|w| w.trim_matches(|c| c == '\'' || c == '"').to_string())
                .collect();
            // Führende Zuweisungen gehören zum Kommando, nicht davor.
            let start = words
                .iter()
                .take_while(|w| w.split_once('=').is_some_and(|(name, _)| is_env_name(name)))
                .count();
            let words = &words[start..];
            recognize(words).is_some() || wraps_runner(words)
        })
}

/// Deutet die Ausgabe eines erkannten Runners. `None`, wenn weder Zähler
/// noch Benchmarks noch ein Exit-Code vorliegen — ein Ergebnis ohne jede
/// Zahl wäre keine Aussage.
pub fn interpret(runner: Runner, argv: Vec<String>, report: &ExecReport) -> Option<ExecOutcome> {
    let text = strip_ansi(&report.output);
    let (tests, benches) = match runner {
        Runner::CargoTest => (libtest_counts(&text), Vec::new()),
        Runner::CargoNextest => (nextest_counts(&text), Vec::new()),
        Runner::Pytest => (pytest_counts(&text), Vec::new()),
        Runner::CargoBenchCriterion => (None, criterion_benches(&text).unwrap_or_default()),
    };
    if tests.is_none() && benches.is_empty() && report.exit_code.is_none() {
        return None;
    }
    // Ohne einen einzigen criterion-Wert ist nicht belegt, dass criterion
    // lief (libtest-`#[bench]`, Build-Fehler) — das Label
    // `cargo-bench-criterion` behauptete mehr, als bekannt ist.
    if runner == Runner::CargoBenchCriterion && benches.is_empty() {
        return None;
    }
    // Nur ein Exit-Code: Er gilt nur mit einer Lauf-Spur des Runners in der
    // Ausgabe. 126/127 meldet die Shell, `no such command: nextest` cargo,
    // `No module named pytest` Python (Exit 1 — derselbe Code wie „Tests
    // rot") — keiner davon belegt, dass der Runner lief.
    if tests.is_none()
        && benches.is_empty()
        && (matches!(report.exit_code, Some(126 | 127)) || !ran(runner, &text, report.exit_code))
    {
        return None;
    }
    Some(ExecOutcome {
        class: runner.class(),
        runner: runner.as_str().to_string(),
        command: argv,
        // Das Arbeitsverzeichnis kennt nur der Checkpoint (Repo-Wurzel).
        cwd: None,
        exit_code: report.exit_code,
        tests,
        benches,
    })
}

/// Hat der Runner sichtbar gearbeitet? Die Spuren aus den aufgezeichneten
/// Ausgaben: cargo baut (`Compiling`, `error[`) oder startet ein Binary
/// (`Running`), nextest meldet `Starting`/`Summary`, pytest seine
/// `test session starts` — oder einen seiner eigenen Exit-Codes 2–5
/// (abgebrochen, interner Fehler, Aufruffehler, keine Tests).
fn ran(runner: Runner, text: &str, exit_code: Option<i32>) -> bool {
    let has = |needle: &str| text.contains(needle);
    match runner {
        Runner::CargoTest | Runner::CargoBenchCriterion => {
            has("Compiling ") || has("error[") || has("     Running ")
        }
        Runner::CargoNextest => {
            has("Compiling ") || has("error[") || has("    Starting ") || has("     Summary [")
        }
        Runner::Pytest => has("test session starts") || matches!(exit_code, Some(2..=5)),
    }
}

/// Entfernt ANSI-Escape-Sequenzen (Claude Code lässt Farben an: nextest und
/// pytest färben ihre Zusammenfassung) und behandelt `\r` als Zeilenende.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: Parameter und Zwischenbytes bis zum Endbyte 0x40–0x7E.
                Some('[') => {
                    for d in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&d) {
                            break;
                        }
                    }
                }
                // OSC: bis BEL oder ESC \.
                Some(']') => {
                    while let Some(d) = chars.next() {
                        if d == '\u{07}' {
                            break;
                        }
                        if d == '\u{1b}' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                // Zeichensatz-Wahl, drei Zeichen (`ESC ( B` aus `tput sgr0`).
                Some('(' | ')' | '*' | '+') => {
                    chars.next();
                }
                // Ein einzelnes ESC am Zeilenende darf die Zeilen nicht
                // verkleben.
                Some('\n' | '\r') => out.push('\n'),
                // Zwei-Zeichen-Sequenzen (ESC c, ESC 7, …).
                _ => {}
            },
            '\r' => out.push('\n'),
            _ => out.push(c),
        }
    }
    out
}

/// `N label`-Teile einer Zusammenfassung, z. B. `2 passed`, `1 timed out`.
fn count_part(part: &str) -> Option<(u64, &str)> {
    let (number, label) = part.trim().split_once(' ')?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((safe(number.parse().ok()?)?, label.trim()))
}

/// Die größte Zahl, die das kanonische Envelope (RFC 8785, IEEE-754)
/// exakt trägt: 2^53 − 1. Ein größerer Wert ließe die Kanonisierung der
/// **ganzen** Session scheitern — und die Zahl stammt aus Programmausgabe,
/// die der Agent steuert. Ein billiger Weg, den eigenen Record zu
/// verhindern; deshalb: darüber kein Ergebnis.
const MAX_SAFE: u64 = (1 << 53) - 1;

/// `Some(n)` nur bis [`MAX_SAFE`].
fn safe(n: u64) -> Option<u64> {
    (n <= MAX_SAFE).then_some(n)
}

/// Addiert Zähler, `None` bei Überlauf oder jenseits von [`MAX_SAFE`].
fn add(a: TestCounts, b: TestCounts) -> Option<TestCounts> {
    Some(TestCounts {
        passed: safe(a.passed.checked_add(b.passed)?)?,
        failed: safe(a.failed.checked_add(b.failed)?)?,
        ignored: safe(a.ignored.checked_add(b.ignored)?)?,
    })
}

const ZERO: TestCounts = TestCounts {
    passed: 0,
    failed: 0,
    ignored: 0,
};

/// libtest: je Test-Binary (Unit, Integration, Doc-Tests) eine Zeile
/// `test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered
/// out; finished in 0.00s` — summiert.
///
/// Der einzige Parser, der mehrere Zeilen summiert, und deshalb an cargos
/// Kopfzeilen verankert (`Running …`, `Doc-tests …`): Je Kopfzeile zählt die
/// **letzte** `test result:`-Zeile am Zeilenanfang bis zur nächsten — die
/// eigene Zusammenfassung von libtest steht immer zuletzt, eine Zeile aus der
/// mitgeschnittenen Ausgabe eines gescheiterten Tests (`failures:`-Block)
/// davor wird so überschrieben. Eingerückte Zeilen zählen nie. Fail-closed,
/// keine Zähler: eine Kopfzeile ohne Ergebniszeile (abgestürztes Binary,
/// abgeschnittene Ausgabe), eine Ergebniszeile vor jeder Kopfzeile, oder
/// eine Ergebniszeile, der eine der drei Zahlen fehlt.
///
/// Ohne `--no-fail-fast` bricht cargo nach dem ersten roten Binary ab: Die
/// Zähler decken dann nur die gelaufenen Binaries — das ist, was berichtet
/// wurde, nicht die ganze Suite.
///
/// Bekannte Lücke: `cargo test -q` druckt keine Kopfzeilen und ergibt
/// deshalb nie Zähler (fail-closed, nur der Exit-Code bleibt). Annahme: Eine
/// Kürzung trifft das **Ende** der Ausgabe (so beobachtet, siehe
/// Fixture-README) — eine Kürzung in der Mitte, die genau eine Kopfzeile
/// samt Ergebnis entfernt, ergäbe zu kleine Summen.
fn libtest_counts(text: &str) -> Option<TestCounts> {
    let mut binaries: Vec<Option<TestCounts>> = Vec::new();
    for line in text.lines() {
        // Verankert an cargos rechtsbündiger Statusspalte (12 Zeichen):
        // Eine Zeile „Running migrations…" aus der mitgeschnittenen Ausgabe
        // eines Tests ist keine Kopfzeile.
        if line.starts_with("     Running ") || line.starts_with("   Doc-tests ") {
            // Die vorige Kopfzeile blieb ohne Ergebnis: Das Ganze ergibt
            // ohnehin keine Zähler — früh aufhören, statt bei einer Flut von
            // Kopfzeilen Speicher zu sammeln.
            if matches!(binaries.last(), Some(None)) {
                return None;
            }
            binaries.push(None);
            continue;
        }
        let Some(rest) = line.strip_prefix("test result: ") else {
            continue;
        };
        *binaries.last_mut()? = Some(libtest_result(rest)?);
    }
    if binaries.is_empty() {
        return None;
    }
    binaries
        .into_iter()
        .try_fold(ZERO, |total, counts| add(total, counts?))
}

/// Die Zahlen einer libtest-Ergebniszeile (ohne den Präfix `test result: `).
fn libtest_result(rest: &str) -> Option<TestCounts> {
    let rest = rest
        .strip_prefix("ok.")
        .or_else(|| rest.strip_prefix("FAILED."))?;
    let (mut passed, mut failed, mut ignored) = (None, None, None);
    for part in rest.split(';') {
        let Some((n, label)) = count_part(part) else {
            continue;
        };
        match label {
            "passed" => passed = Some(n),
            "failed" => failed = Some(n),
            "ignored" => ignored = Some(n),
            _ => {}
        }
    }
    Some(TestCounts {
        passed: passed?,
        failed: failed?,
        ignored: ignored?,
    })
}

/// nextest: die letzte Zeile `Summary [   0.006s] 3 tests run: 2 passed, 1
/// failed, 1 skipped`. `skipped` zählt als ausgelassen, `timed out` und
/// `exec failed` als gescheitert; Klammerzusätze (`passed (1 flaky)`) werden
/// ignoriert. Ein unbekanntes Label oder eine Summe, die nicht zur Zahl der
/// gelaufenen Tests passt: keine Zähler.
fn nextest_counts(text: &str) -> Option<TestCounts> {
    let line = text.lines().rev().find_map(|line| {
        // Verankert an nextests rechtsbündiger Statusspalte (fünf
        // Leerzeichen): Mitgeschnittene Testausgabe ist eingerückt und kann
        // so keine eigene `Summary` unterschieben.
        let rest = line.strip_prefix("     Summary [")?;
        Some(rest.split_once("] ")?.1)
    })?;
    let (run, parts) = line.split_once(" run: ")?;
    let run = run
        .strip_suffix(" tests")
        .or_else(|| run.strip_suffix(" test"))?;
    // Nach fail-fast nennt nextest `2/5 tests run`: gelaufen / geplant.
    let run = run.split_once('/').map_or(run, |(ran, _)| ran);
    let run: u64 = run.parse().ok()?;

    let mut counts = ZERO;
    for part in split_outside_parens(parts) {
        let part = match part.find(" (") {
            Some(at) => &part[..at],
            None => part,
        };
        let (n, label) = count_part(part)?;
        let slot = match label {
            "passed" => &mut counts.passed,
            "failed" | "timed out" | "exec failed" => &mut counts.failed,
            "skipped" => &mut counts.ignored,
            _ => return None,
        };
        *slot = safe(slot.checked_add(n)?)?;
    }
    (counts.passed.checked_add(counts.failed)? == run).then_some(counts)
}

/// Trennt an `,` — aber nicht innerhalb von Klammern
/// (`2 passed (1 slow, 1 flaky), 1 failed`).
fn split_outside_parens(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(text[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts
}

/// pytest: die letzte Zusammenfassung `==== 1 failed, 2 passed, 1 skipped in
/// 0.02s ====` (mit `-q` ohne Rahmen). `no tests ran` ergibt Nullen.
///
/// Zuordnung: `passed`/`xpassed` → bestanden, `failed`/`error(s)` →
/// gescheitert, `skipped`/`xfailed` → ausgelassen; `deselected`,
/// `warning(s)` und `rerun` zählen nicht. Ein unbekanntes Label: keine
/// Zähler. `xpassed` erscheint nur im nicht-strikten Fall — unter
/// `xfail_strict` meldet pytest einen unerwarteten Erfolg selbst als
/// `failed`, er landet dann dort.
fn pytest_counts(text: &str) -> Option<TestCounts> {
    // Nur die **letzte** nicht-leere Zeile: pytest schreibt seine
    // Zusammenfassung immer zuletzt (gerahmt wie mit `-q`). Fehlt sie dort
    // (INTERNALERROR, ein Plugin ersetzt sie), zählt keine frühere
    // summary-artige Zeile aus der Testausgabe.
    let last = text.lines().rev().find(|line| !line.trim().is_empty())?;
    let line = last.trim().trim_matches('=').trim();
    let (parts, tail) = line.rsplit_once(" in ")?;
    if !is_pytest_duration(tail) {
        return None;
    }
    if parts == "no tests ran" {
        return Some(ZERO);
    }
    let mut counts = ZERO;
    for part in parts.split(',') {
        let (n, label) = count_part(part)?;
        let slot = match label {
            "passed" | "xpassed" => &mut counts.passed,
            "failed" | "error" | "errors" => &mut counts.failed,
            "skipped" | "xfailed" => &mut counts.ignored,
            "deselected" | "warning" | "warnings" | "rerun" => continue,
            _ => return None,
        };
        *slot = safe(slot.checked_add(n)?)?;
    }
    Some(counts)
}

/// `0.02s`, optional gefolgt von ` (0:00:00)`.
fn is_pytest_duration(tail: &str) -> bool {
    let seconds = match tail.split_once(" (") {
        Some((seconds, clock)) => {
            if !clock.ends_with(')') {
                return false;
            }
            seconds
        }
        None => tail,
    };
    seconds.strip_suffix('s').is_some_and(|n| {
        !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            && n.bytes().filter(|b| *b == b'.').count() <= 1
    })
}

/// criterion: `sort/1k                 time:   [297.88 ns 298.47 ns 299.59
/// ns]`, bei langen Namen der Name auf eigener Zeile und `time:` eingerückt
/// darunter. Der Median (mittlerer Wert) in ganzen Nanosekunden. Doppelte
/// Namen: der letzte Wert, an der Stelle des ersten Auftretens.
///
/// Ein Name gilt nur, wenn criterion ihn **selbst angekündigt** hat
/// (`Benchmarking <name>: Analyzing`, direkt vor der Messung): Sonst könnte
/// jede beliebige Zeile der Programmausgabe vor einer eingerückten
/// `time:`-Zeile als „Name" ins Envelope wandern — ohne den Kontext, den
/// die Redaction bräuchte (`Password:` auf der Zeile davor).
///
/// Die Ankündigung ist **keine Vertrauensgrenze**: Jedes Programm kann sie
/// samt `time:`-Zeile drucken. Sie verhindert versehentliche Übernahmen;
/// was den Envelope schützt, ist allein der Redaction-Scan des Namens — und
/// der sieht ihn ohne Kontext (DOCUMENTED_GAPS `context-free-bench-name`).
///
/// `None` (kein Bench-Ergebnis) bei einer `time:`-Zeile, deren Form nicht
/// passt oder deren Name nicht der angekündigte ist, oder bei mehr als
/// [`MAX_BENCHES`] Benchmarks.
fn criterion_benches(text: &str) -> Option<Vec<BenchValue>> {
    let mut benches: Vec<BenchValue> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut announced: Option<&str> = None;
    let mut previous: Option<&str> = None;
    for line in text.lines() {
        if let Some(name) = line
            .strip_prefix("Benchmarking ")
            .and_then(|rest| rest.strip_suffix(": Analyzing"))
        {
            announced = Some(name);
            previous = None;
            continue;
        }
        let Some((before, after)) = line.rsplit_once("time:") else {
            if !line.trim().is_empty() {
                previous = Some(line);
            }
            continue;
        };
        let after = after.trim();
        if !after.starts_with('[') {
            previous = Some(line);
            continue;
        }
        // Keine offene Ankündigung: keine Messzeile eines Benchmarks, sondern
        // etwa der `change:`-Block eines Folgelaufs mit Throughput
        // (`time: [-1.07% -0.13% +0.79%]`) — oder Programmausgabe. Sie
        // trägt keinen Namen bei und wird übersprungen.
        let Some(expected) = announced else {
            previous = Some(line);
            continue;
        };
        let name = match before.trim() {
            "" => previous?.trim(),
            name => name,
        };
        if name != expected
            || name.is_empty()
            || name.len() > MAX_BENCH_NAME
            || name.chars().any(|c| c.is_control() || is_invisible(c))
        {
            return None;
        }
        let value = criterion_median(after)?;
        match index.get(name) {
            Some(&at) => benches[at].value = value,
            None => {
                if benches.len() == MAX_BENCHES {
                    return None;
                }
                index.insert(name.to_string(), benches.len());
                benches.push(BenchValue {
                    name: name.to_string(),
                    value,
                    unit: "ns".to_string(),
                });
            }
        }
        announced = None;
        previous = None;
    }
    Some(benches)
}

/// `[2.8587 µs 2.8631 µs 2.8676 µs]` → der mittlere Wert in ns.
fn criterion_median(bracket: &str) -> Option<u64> {
    let inner = bracket.strip_prefix('[')?.split_once(']')?.0;
    let tokens: Vec<&str> = inner.split_whitespace().collect();
    let [_, _, number, unit, _, _] = tokens[..] else {
        return None;
    };
    to_nanos(number, unit)
}

/// Rechnet einen Dezimalwert mit Zeiteinheit **ohne Fließkomma** in ganze
/// Nanosekunden um, kaufmännisch gerundet (ab ,5 auf).
///
/// Der Wert wird als ganze Zahl samt Nachkommastellen gelesen und in
/// Pikosekunden × 10^Nachkommastellen skaliert; erst die letzte Division
/// rundet. Unbekannte Einheit, Vorzeichen, Exponent oder Überlauf: `None`.
pub fn to_nanos(number: &str, unit: &str) -> Option<u64> {
    let picos_per_unit: u128 = match unit {
        "ps" => 1,
        "ns" => 1_000,
        // U+00B5 (Mikro-Zeichen, so schreibt criterion) und U+03BC (my).
        "µs" | "μs" | "us" => 1_000_000,
        "ms" => 1_000_000_000,
        "s" => 1_000_000_000_000,
        _ => return None,
    };
    let (int, frac) = number.split_once('.').unwrap_or((number, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if int.is_empty() || !digits(int) || !digits(frac) || int.len() > 20 || frac.len() > 12 {
        return None;
    }
    let scale = 10u128.checked_pow(u32::try_from(frac.len()).ok()?)?;
    let mantissa =
        int.parse::<u128>()
            .ok()?
            .checked_mul(scale)?
            .checked_add(if frac.is_empty() {
                0
            } else {
                frac.parse::<u128>().ok()?
            })?;
    let picos_scaled = mantissa.checked_mul(picos_per_unit)?;
    let divisor = scale.checked_mul(1_000)?;
    let nanos = picos_scaled.checked_add(divisor / 2)? / divisor;
    safe(u64::try_from(nanos).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(command: &str) -> Option<Vec<String>> {
        simple_argv(command).ok()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_simple_command_splits_into_argv() {
        assert_eq!(argv("cargo test"), Some(strings(&["cargo", "test"])));
        assert_eq!(
            argv("  cargo   test -p minds-core\t--lib "),
            Some(strings(&["cargo", "test", "-p", "minds-core", "--lib"]))
        );
        // Quotes ohne Expansion: Ihre Bedeutung steht ohne Shell fest.
        assert_eq!(
            argv(r#"cargo test -- 'mod::a b' "exact name" x'y'"z""#),
            Some(strings(&[
                "cargo",
                "test",
                "--",
                "mod::a b",
                "exact name",
                "xyz"
            ]))
        );
        assert_eq!(
            argv("pytest ''"),
            Some(strings(&["pytest", ""])),
            "ein leeres gequotetes Wort ist ein Wort"
        );
    }

    #[test]
    fn allowlisted_env_prefixes_are_dropped_others_refuse() {
        assert_eq!(
            argv("RUST_LOG=debug RUST_BACKTRACE=1 cargo test"),
            Some(strings(&["cargo", "test"]))
        );
        assert_eq!(
            argv("RUST_LOG=minds_capture=debug,info cargo test"),
            Some(strings(&["cargo", "test"]))
        );
        // Erlaubte Werte nur ungequotet und aus einem engen Zeichensatz: Der
        // Wert fällt aus dem argv und damit aus dessen Gegenprobe.
        for command in [
            "RUST_LOG='a b' cargo test",
            "RUST_LOG=--pass''word=hunter2 cargo test",
            "RUST_LOG='DB_PASSWORD'=hunter2 cargo test",
            "RUST_LOG=a@b cargo test",
            // Ein Token passt nicht in die Grammatik von `RUST_LOG` und
            // könnte sonst an der argv-Gegenprobe vorbei.
            "RUST_LOG=ghp_012345678901234567890123456789012345 cargo test",
            "RUST_LOG=Debug cargo test",
            "RUST_LOG=x=verbose cargo test",
            "RUST_BACKTRACE=yes cargo test",
            "CARGO_TERM_COLOR=hunter2 cargo test",
            "NO_COLOR=hunter2 cargo test",
        ] {
            assert_eq!(argv(command), None, "{command:?}");
        }
        for command in [
            "RUST_LOG=minds_capture::adapter=trace,warn cargo test",
            "RUST_LOG=off RUST_BACKTRACE=full CARGO_TERM_COLOR=never NO_COLOR= cargo test",
            "RUST_LIB_BACKTRACE=0 NO_COLOR=1 cargo test",
        ] {
            assert_eq!(
                argv(command),
                Some(strings(&["cargo", "test"])),
                "{command:?}"
            );
        }
        // Jede erlaubte Variable hat eine Grammatik — Liste und Prüfung
        // bleiben im Gleichschritt.
        for name in ENV_ALLOWLIST {
            assert!(
                ["1", "info", "auto", "full"]
                    .iter()
                    .any(|value| env_value_ok(name, value)),
                "{name} ohne Grammatik"
            );
        }
        assert_eq!(argv("RUSTFLAGS=-Dwarnings cargo test"), None);
        assert_eq!(argv("DATABASE_URL=x cargo test"), None);
        assert_eq!(argv("RUST_LOG=debug"), None, "nur Zuweisung, kein Kommando");
        // Nach dem Programmnamen ist `KEY=VALUE` ein normales Argument.
        assert_eq!(
            argv("cargo test FOO=bar"),
            Some(strings(&["cargo", "test", "FOO=bar"]))
        );
        // Ein gequoteter Name ist keine Zuweisung, sondern das Programm.
        assert_eq!(
            argv("'RUST_LOG=x' cargo"),
            Some(strings(&["RUST_LOG=x", "cargo"]))
        );
    }

    #[test]
    fn anything_a_shell_would_interpret_is_not_simple() {
        for command in [
            "cargo test; ls",
            "cargo test && echo ok",
            "cargo test || true",
            "cargo test | tail -3",
            "cargo test 2>&1",
            "cargo test > out.txt",
            "cargo test < in",
            "(cargo test)",
            "cargo test $(echo -p) x",
            "cargo test `x`",
            "cargo test $FOO",
            "cargo test \"$FOO\"",
            "cargo test \"`x`\"",
            "cargo test *",
            "cargo test foo?",
            "cargo test [ab]",
            "cargo test {a,b}",
            "cargo test ~/x",
            "cargo test # comment",
            // Bash: Tilde nach `=`/`:` in zuweisungsförmigen Argumenten.
            "pytest FOO=~/x",
            "cargo test X=a:~/b",
            // zsh: `=cmd` am Wortanfang expandiert zum Programmpfad.
            "pytest =python",
            "=cargo test",
            "cargo test \\; x",
            "cargo test\nls",
            "cargo test 'open",
            "cargo test \"open",
            "! cargo test",
            "cargo test &",
            "",
            "   ",
        ] {
            assert_eq!(argv(command), None, "{command:?}");
        }
        // zsh mit `EXTENDED_GLOB`: `#`/`~` mitten im Wort und `^` am
        // Wortanfang sind Glob-Operatoren. Gequotet bleiben sie wörtlich.
        for command in [
            "cargo test a#b",
            "cargo test c~d",
            "cargo test ^x",
            "cargo test x^y",
        ] {
            assert_eq!(argv(command), None, "{command:?}");
        }
        assert_eq!(
            argv("cargo test 'a#b' \"c~d\" 'x^y'"),
            Some(strings(&["cargo", "test", "a#b", "c~d", "x^y"]))
        );
        // Steuer- und Bidi-Zeichen, auch in Quotes.
        for command in [
            "cargo test 'a\u{1b}[31mb'",
            "cargo test 'a\u{7}'",
            "cargo test 'x\u{202e}y'",
            "cargo test \"x\u{2066}y\"",
            "cargo test\u{0b}x",
            // Unsichtbare Format-Zeichen: Word Joiner, Tag-Zeichen, Soft
            // Hyphen, Variation Selector, Zeilentrenner.
            "cargo test --pass\u{2060}word hunter2",
            "cargo test 'a\u{e0041}b'",
            "cargo test a\u{00ad}b",
            "cargo test a\u{fe0f}",
            "cargo test a\u{2028}b",
            // Default-Ignorable außerhalb von Cf und sichtbar leere Füller:
            // zwischen Flag und Wert bräche jedes den Detektor-Kontext.
            "pytest --password\u{3164}hunter2",
            "pytest --password\u{2800}hunter2",
            "pytest --password\u{034f}hunter2",
            "pytest --password\u{180b}hunter2",
            "pytest --pass\u{115f}word hunter2",
            "pytest --pass\u{ffa0}word hunter2",
            "pytest a\u{17b4}b",
            "pytest a\u{e0fff}b",
            // zsh `RC_QUOTES`: `''` in einfachen Quotes ist dort ein `'`.
            "cargo test 'it''s'",
        ] {
            assert_eq!(argv(command), None, "{command:?}");
        }
    }

    #[test]
    fn runners_are_recognized_by_exact_argv() {
        let r = |command: &str| recognize(&simple_argv(command).unwrap());
        assert_eq!(r("cargo test"), Some(Runner::CargoTest));
        assert_eq!(r("cargo +nightly test -p x"), Some(Runner::CargoTest));
        assert_eq!(r("cargo nextest run"), Some(Runner::CargoNextest));
        assert_eq!(r("cargo nextest list"), None);
        assert_eq!(
            r("cargo bench --bench sort"),
            Some(Runner::CargoBenchCriterion)
        );
        assert_eq!(r("pytest py"), Some(Runner::Pytest));
        assert_eq!(r("py.test"), Some(Runner::Pytest));
        assert_eq!(r("python3 -m pytest -q"), Some(Runner::Pytest));
        assert_eq!(r("python3 -m pip"), None);
        assert_eq!(r("cargo build"), None);
        assert_eq!(r("cargo"), None);
        assert_eq!(r("ls src"), None);
        assert_eq!(r(".venv/bin/pytest"), None, "Pfade bleiben ungedeutet");
    }

    #[test]
    fn a_runner_inside_a_compound_command_is_mentioned() {
        assert!(mentions_runner("cargo test 2>&1 | tail -3"));
        assert!(mentions_runner("cd crates && cargo +stable test"));
        assert!(mentions_runner("(pytest -x)"));
        assert!(mentions_runner("FOO=1 python -m pytest"));
        assert!(!mentions_runner("ls | head"));
        assert!(!mentions_runner("cargo build && ls"));
        assert!(!mentions_runner("echo test"));
        // Nur am Segmentanfang (oder hinter einem Wrapper): ein Runner-Wort
        // als Argument ist keiner.
        assert!(!mentions_runner(r#"echo "then cargo test" && ls"#));
        assert!(!mentions_runner("pip install pytest && ls"));
        assert!(mentions_runner("cd x && timeout 60 cargo test | tail"));
        assert!(mentions_runner("RUST_LOG=x cargo --locked test; ls"));
    }

    #[test]
    fn nanos_are_integers_and_rounded_half_up() {
        assert_eq!(to_nanos("302.00", "ns"), Some(302));
        assert_eq!(to_nanos("25.631", "ns"), Some(26));
        assert_eq!(to_nanos("25.5", "ns"), Some(26));
        assert_eq!(to_nanos("25.4999", "ns"), Some(25));
        assert_eq!(to_nanos("2.8631", "µs"), Some(2863));
        assert_eq!(to_nanos("2.8631", "μs"), Some(2863));
        assert_eq!(to_nanos("1.5", "ms"), Some(1_500_000));
        assert_eq!(to_nanos("3", "s"), Some(3_000_000_000));
        assert_eq!(to_nanos("499", "ps"), Some(0));
        assert_eq!(to_nanos("500", "ps"), Some(1));
        for (number, unit) in [
            ("1", "min"),
            ("-1", "ns"),
            ("1e3", "ns"),
            ("", "ns"),
            (".5", "ns"),
            ("1.2.3", "ns"),
            ("99999999999999999999999", "s"),
        ] {
            assert_eq!(to_nanos(number, unit), None, "{number} {unit}");
        }
    }

    #[test]
    fn ansi_sequences_and_carriage_returns_are_stripped() {
        assert_eq!(
            strip_ansi("\u{1b}[32;1m     Summary\u{1b}[0m [ 0.006s]"),
            "     Summary [ 0.006s]"
        );
        assert_eq!(strip_ansi("a\rb"), "a\nb");
        assert_eq!(
            strip_ansi("\u{1b}]8;;http://x\u{07}link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
    }

    #[test]
    fn a_malformed_summary_yields_no_counts() {
        // Eine libtest-Zeile ohne `ignored`: lieber keine Zahl als eine
        // falsche.
        const HEAD: &str = "     Running unittests src/lib.rs (target/debug/deps/x)\n";
        assert_eq!(
            libtest_counts(&format!("{HEAD}test result: ok. 2 passed; 0 failed")),
            None
        );
        assert_eq!(
            libtest_counts(&format!("{HEAD}test result: maybe. 2 passed")),
            None
        );
        // nextest: unbekanntes Label, oder Summe passt nicht.
        assert_eq!(
            nextest_counts("     Summary [ 0.1s] 3 tests run: 2 passed, 1 exploded"),
            None
        );
        assert_eq!(
            nextest_counts("     Summary [ 0.1s] 4 tests run: 2 passed, 1 failed"),
            None
        );
        assert_eq!(
            nextest_counts(
                "     Summary [ 0.1s] 3 tests run: 2 passed (1 slow, 1 flaky), 1 timed out, 2 skipped"
            ),
            Some(TestCounts {
                passed: 2,
                failed: 1,
                ignored: 2
            })
        );
        assert_eq!(
            nextest_counts("     Summary [ 0.1s] 1 test run: 1 passed"),
            Some(TestCounts {
                passed: 1,
                failed: 0,
                ignored: 0
            })
        );
        // pytest: unbekanntes Label.
        assert_eq!(pytest_counts("=== 2 passed, 1 exploded in 0.1s ==="), None);
        assert_eq!(
            pytest_counts("2 passed, 1 xfailed, 3 deselected, 1 warning in 1.20s (0:00:01)"),
            Some(TestCounts {
                passed: 2,
                failed: 0,
                ignored: 1
            })
        );
        assert_eq!(pytest_counts("==== no tests ran in 0.01s ===="), Some(ZERO));
        assert_eq!(pytest_counts("passed in a hurry"), None);
    }

    #[test]
    fn libtest_overflow_is_no_count() {
        let max = u64::MAX;
        let text = format!(
            "     Running a\ntest result: ok. {max} passed; 0 failed; 0 ignored\n   Doc-tests b\ntest result: ok. 1 passed; 0 failed; 0 ignored"
        );
        assert_eq!(libtest_counts(&text), None);
    }

    #[test]
    fn libtest_counts_are_anchored_to_cargos_binary_headers() {
        let counts = |passed, failed, ignored| {
            Some(TestCounts {
                passed,
                failed,
                ignored,
            })
        };
        // Eine Ergebniszeile aus der mitgeschnittenen Ausgabe eines
        // gescheiterten Tests steht vor libtests eigener — die letzte zählt.
        let nested = "     Running tests/parser.rs (x)\n\
            ---- parses_summary stdout ----\n\
            test result: ok. 99 passed; 0 failed; 0 ignored\n\
            failures:\n    parses_summary\n\
            test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured\n";
        assert_eq!(libtest_counts(nested), counts(3, 1, 0));
        // Eingerückte Ergebniszeilen zählen nie.
        let indented = "     Running a\n    test result: ok. 9 passed; 0 failed; 0 ignored\n\
            test result: ok. 1 passed; 0 failed; 0 ignored\n";
        assert_eq!(libtest_counts(indented), counts(1, 0, 0));
        // Zwei Kopfzeilen, nur eine Ergebniszeile (abgeschnitten,
        // abgestürzt): keine Teilsumme.
        let truncated = "     Running a\ntest result: ok. 2 passed; 0 failed; 0 ignored\n\
            \x20    Running b\nrunning 40 tests\n";
        assert_eq!(libtest_counts(truncated), None);
        // Ergebniszeile ohne Kopfzeile davor, oder gar keine Kopfzeile.
        assert_eq!(
            libtest_counts("test result: ok. 2 passed; 0 failed; 0 ignored"),
            None
        );
        assert_eq!(libtest_counts("error[E0425]: cannot find value"), None);
    }

    #[test]
    fn criterion_rejects_bad_names_and_keeps_the_last_duplicate() {
        let run = |name: &str, line: &str| format!("Benchmarking {name}: Analyzing\n{line}\n");
        let text = [
            run("a", "a  time:   [1 ns 2 ns 3 ns]"),
            run("b", "b  time:   [1 ns 5 ns 9 ns]"),
            run("a", "a  time:   [1 ns 4 ns 9 ns]"),
        ]
        .concat();
        let benches = criterion_benches(&text).unwrap();
        let got: Vec<(&str, u64)> = benches.iter().map(|b| (b.name.as_str(), b.value)).collect();
        assert_eq!(got, [("a", 4), ("b", 5)]);

        // Eine `time:`-Zeile ohne Namen davor.
        assert_eq!(
            criterion_benches(&run("a", "    time:   [1 ns 2 ns 3 ns]")),
            None
        );
        // Ein überlanger Name.
        let long = "x".repeat(MAX_BENCH_NAME + 1);
        assert_eq!(
            criterion_benches(&run(&long, &format!("{long}  time:   [1 ns 2 ns 3 ns]"))),
            None
        );
        // Falsche Form der Klammer.
        assert_eq!(criterion_benches(&run("a", "a  time:   [1 ns 2 ns]")), None);
        // `time:` ohne Klammer ist keine Messzeile.
        assert_eq!(criterion_benches("elapsed time: 3s"), Some(Vec::new()));
    }

    #[test]
    fn a_bench_name_must_be_announced_by_criterion() {
        // Eine beliebige Zeile der Programmausgabe vor einer eingerückten
        // `time:`-Zeile wird kein Bench-Name — sie stünde sonst ohne ihren
        // Kontext (`Password:`) im Envelope.
        // Ohne Ankündigung trägt eine `time:`-Zeile nichts bei.
        let forged = "Password:\nhunter2\n            time:   [1 ns 2 ns 3 ns]\n";
        assert_eq!(criterion_benches(forged), Some(Vec::new()));
        let inline = "DB_PASSWORD=hunter2   time:   [1 ns 2 ns 3 ns]\n";
        assert_eq!(criterion_benches(inline), Some(Vec::new()));
        // Angekündigt wurde ein anderer Name.
        let other =
            "Benchmarking sort/1k: Analyzing\nhunter2\n            time:   [1 ns 2 ns 3 ns]\n";
        assert_eq!(criterion_benches(other), None);
        // Die echte Langform: angekündigt, Name auf eigener Zeile.
        let long = "Benchmarking sort/reverse_sorted_input/10000: Analyzing\n\
            sort/reverse_sorted_input/10000\n                        time:   [2.8587 µs 2.8631 µs 2.8676 µs]\n";
        let got = criterion_benches(long).unwrap();
        assert_eq!(got[0].name, "sort/reverse_sorted_input/10000");
        assert_eq!(got[0].value, 2863);
    }

    #[test]
    fn a_throughput_change_block_does_not_discard_the_run() {
        // criterion 0.5 mit `Throughput` und Baseline (ab dem zweiten
        // `cargo bench`): Der `change:`-Block trägt eine eingerückte
        // `time:`-Zeile mit Prozenten.
        let text = "Benchmarking parse/1k: Analyzing\n\
            parse/1k                time:   [1.0000 µs 1.2000 µs 1.4000 µs]\n\
            \x20                       thrpt:  [700.00 MiB/s 800.00 MiB/s 900.00 MiB/s]\n\
            \x20                change:\n\
            \x20                       time:   [-1.07% -0.13% +0.79%] (p = 0.78 > 0.05)\n\
            \x20                       thrpt:  [-0.78% +0.13% +1.08%]\n\
            \x20                       No change in performance detected.\n";
        let got = criterion_benches(text).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].name.as_str(), got[0].value), ("parse/1k", 1200));
    }

    #[test]
    fn nextest_fail_fast_reports_ran_of_planned() {
        assert_eq!(
            nextest_counts("     Summary [ 0.1s] 2/5 tests run: 1 passed, 1 failed, 0 skipped"),
            Some(TestCounts {
                passed: 1,
                failed: 1,
                ignored: 0
            })
        );
        assert_eq!(
            nextest_counts("     Summary [ 0.1s] 3/5 tests run: 1 passed, 1 failed"),
            None
        );
    }

    #[test]
    fn numbers_beyond_the_canonical_range_are_no_outcome() {
        // 2^53 ließe die Kanonisierung der ganzen Session scheitern — eine
        // Zahl aus Programmausgabe darf den Record nicht verhindern.
        let max = MAX_SAFE;
        let over = MAX_SAFE + 1;
        let head = "     Running tests/x.rs (x)\n";
        assert_eq!(
            libtest_counts(&format!(
                "{head}test result: ok. {over} passed; 0 failed; 0 ignored"
            )),
            None
        );
        assert_eq!(
            libtest_counts(&format!(
                "{head}test result: ok. {max} passed; 0 failed; 0 ignored"
            ))
            .map(|c| c.passed),
            Some(max)
        );
        // Jede Zahl einzeln im Bereich, die Summe nicht.
        let half = (1u64 << 52) + 1;
        let two = format!(
            "{head}test result: ok. {half} passed; 0 failed; 0 ignored\n{head}test result: ok. {half} passed; 0 failed; 0 ignored"
        );
        assert_eq!(libtest_counts(&two), None);
        assert_eq!(
            nextest_counts(&format!(
                "     Summary [ 0.1s] {over} tests run: {over} passed"
            )),
            None
        );
        assert_eq!(pytest_counts(&format!("{over} passed in 0.1s")), None);
        // criterion: Median jenseits des Bereichs.
        let bench = format!("Benchmarking x: Analyzing\nx  time:   [1 ns {over} ns 1 ns]\n");
        assert_eq!(criterion_benches(&bench), None);
        assert_eq!(to_nanos(&max.to_string(), "ns"), Some(max));
        assert_eq!(to_nanos(&over.to_string(), "ns"), None);
    }

    #[test]
    fn quiet_cargo_test_has_no_headers_and_no_counts() {
        // Bekannte Lücke, festgenagelt: `cargo test -q` druckt keine
        // Kopfzeilen — fail-closed keine Zähler.
        let quiet = "\nrunning 2 tests\n..\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        assert_eq!(libtest_counts(quiet), None);
    }

    #[test]
    fn a_bench_run_without_criterion_values_is_no_outcome() {
        // libtest-`#[bench]` oder ein Build-Fehler: Das Label
        // `cargo-bench-criterion` wäre eine Behauptung ohne Beleg.
        let report = ExecReport {
            output: "test bench_sort ... bench:   1,234 ns/iter (+/- 5)".into(),
            exit_code: Some(101),
        };
        assert_eq!(
            interpret(
                Runner::CargoBenchCriterion,
                strings(&["cargo", "bench"]),
                &report
            ),
            None
        );
    }

    #[test]
    fn a_bench_name_with_invisible_format_characters_is_no_outcome() {
        for name in ["a\u{202e}b", "a\u{200b}b", "a\u{feff}b"] {
            let text =
                format!("Benchmarking {name}: Analyzing\n{name}  time:   [1 ns 2 ns 3 ns]\n");
            assert_eq!(criterion_benches(&text), None, "{name:?}");
        }
    }

    #[test]
    fn a_runner_in_a_quoted_argument_is_not_named() {
        // Am exakten argv: Ein gequotetes Argument ist ein Element.
        assert!(!names_runner("git commit -m 'fix cargo test'"));
        assert!(!names_runner(r#"grep -rn "cargo test" docs"#));
        assert!(names_runner("timeout 600 cargo test"));
        assert!(names_runner("env RUST_LOG=x cargo +nightly test"));
        assert!(names_runner("cargo test"));
        // Nicht einfach: weiterhin die grobe Wortsuche.
        assert!(names_runner("cd x && cargo test"));
    }

    #[test]
    fn splitting_a_hostile_command_stays_linear_and_bounded() {
        // Ein ungültiger Name vor vielen `=`: früher wurde bei jedem `=` das
        // ganze Wort neu geprüft — quadratisch in einem fremden Kommando.
        let half = (MAX_COMMAND - 2) / 2;
        let hostile = format!("{}.{}", "a".repeat(half), "=".repeat(half));
        assert!(hostile.len() <= MAX_COMMAND);
        let started = std::time::Instant::now();
        assert_eq!(
            argv(&hostile),
            Some(strings(&[hostile.as_str()])),
            "ein Wort, keine Zuweisung"
        );
        assert!(!mentions_runner(&hostile));
        // Großzügig für Debug-Builds; quadratisch wären es Sekunden.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        // Über der Grenze wird gar nicht erst zerlegt.
        let too_long = format!("cargo test {}", "x".repeat(MAX_COMMAND));
        assert_eq!(argv(&too_long), None);
        assert!(!mentions_runner(&format!(
            "cargo test; {}",
            "x".repeat(MAX_COMMAND)
        )));
        // Gültige Zuweisungen werden weiter erkannt, ungültige nicht.
        assert_eq!(argv("a.b=c cargo"), Some(strings(&["a.b=c", "cargo"])));
        assert_eq!(argv("1A=x cargo"), Some(strings(&["1A=x", "cargo"])));
        assert_eq!(
            argv("A1=x cargo"),
            None,
            "A1 ist eine Zuweisung, nicht erlaubt"
        );
    }

    #[test]
    fn only_known_wrappers_wrap_a_runner() {
        let wraps = |command: &str| wraps_runner(&simple_argv(command).unwrap());
        assert!(wraps("timeout 600 cargo test"));
        assert!(wraps("timeout -k 5 10m cargo nextest run"));
        assert!(wraps("env RUST_LOG=x cargo test"));
        assert!(wraps("nice -n 10 pytest"));
        assert!(wraps("uv run pytest -q"));
        assert!(wraps("poetry run python -m pytest"));
        // Ein Runner-Name als Argument ist kein Runner-Aufruf.
        for command in [
            "pip install pytest",
            "uv add --dev pytest",
            "python -m pip install pytest",
            "which pytest",
            "echo pytest",
            "git commit -m 'fix cargo test'",
            "uv pip install pytest",
            "timeout 600 echo cargo test",
        ] {
            assert!(!wraps(command), "{command:?}");
            assert!(!names_runner(command), "{command:?}");
        }
    }

    #[test]
    fn global_cargo_flags_before_the_subcommand_are_skipped() {
        let r = |command: &str| recognize(&simple_argv(command).unwrap());
        assert_eq!(r("cargo --locked test"), Some(Runner::CargoTest));
        assert_eq!(r("cargo -q test"), Some(Runner::CargoTest));
        assert_eq!(
            r("cargo +nightly -Zunstable-options test"),
            Some(Runner::CargoTest)
        );
        assert_eq!(
            r("cargo --color=always nextest run"),
            Some(Runner::CargoNextest)
        );
        // Ein Flag mit eigenem Wert ist kein globales Flag ohne Wert.
        assert_eq!(r("cargo --config x test"), None);
    }

    #[test]
    fn an_exit_code_alone_needs_a_trace_of_the_runner() {
        let only_exit = |runner, argv: &[&str], output: &str, code| {
            interpret(
                runner,
                strings(argv),
                &ExecReport {
                    output: output.into(),
                    exit_code: Some(code),
                },
            )
        };
        // Der Runner lief gar nicht.
        assert_eq!(
            only_exit(
                Runner::CargoNextest,
                &["cargo", "nextest", "run"],
                "error: no such command: `nextest`",
                101
            ),
            None
        );
        assert_eq!(
            only_exit(
                Runner::Pytest,
                &["python3", "-m", "pytest"],
                "/usr/bin/python3: No module named pytest",
                1
            ),
            None
        );
        // Er lief: Build-Fehler, pytest-Aufruffehler.
        assert!(
            only_exit(
                Runner::CargoTest,
                &["cargo", "test"],
                "   Compiling x v0.1.0\nerror[E0425]: cannot find value",
                101
            )
            .is_some()
        );
        assert!(
            only_exit(
                Runner::Pytest,
                &["pytest", "--bogus"],
                "ERROR: usage: pytest [options]",
                4
            )
            .is_some()
        );
    }

    #[test]
    fn a_captured_running_line_is_not_a_cargo_header() {
        // „Running migrations…" aus der Ausgabe eines roten Tests darf die
        // Zähler nicht kosten.
        let text = "     Running tests/db.rs (target/debug/deps/db-1)\n\
            \n---- migrates stdout ----\nRunning migrations\n  Running seeds\n\
            failures:\n    migrates\n\
            test result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured\n";
        assert_eq!(
            libtest_counts(text),
            Some(TestCounts {
                passed: 4,
                failed: 1,
                ignored: 0
            })
        );
    }

    #[test]
    fn a_pytest_summary_counts_only_as_the_last_line() {
        // Eine summary-artige Zeile aus der Testausgabe (`-s`, pytester),
        // danach bricht pytest ohne eigene Zusammenfassung ab.
        let text = "=== 5 passed in 0.10s ===\nINTERNALERROR> Traceback (most recent call last):\nINTERNALERROR>   boom\n";
        assert_eq!(pytest_counts(text), None);
        // Leerzeilen hinter der echten Zusammenfassung stören nicht.
        assert_eq!(
            pytest_counts("=== 2 passed in 0.01s ===\n\n"),
            Some(TestCounts {
                passed: 2,
                failed: 0,
                ignored: 0
            })
        );
    }

    #[test]
    fn a_shell_exit_code_alone_is_no_runner_outcome() {
        // 126/127 meldet die Shell — kein Beleg, dass der Runner lief.
        for code in [126, 127] {
            let report = ExecReport {
                output: "zsh: command not found: pytest".into(),
                exit_code: Some(code),
            };
            assert_eq!(
                interpret(Runner::Pytest, strings(&["pytest"]), &report),
                None
            );
        }
    }

    #[test]
    fn an_indented_summary_from_captured_output_is_ignored() {
        // nextest rückt mitgeschnittene Testausgabe ein; mit
        // `failure-output = "final"` steht sie hinter der echten Summary.
        let text = "     Summary [ 0.1s] 3 tests run: 2 passed, 1 failed\n\
            \x20   Summary [ 0.1s] 99 tests run: 99 passed\n";
        assert_eq!(
            nextest_counts(text),
            Some(TestCounts {
                passed: 2,
                failed: 1,
                ignored: 0
            })
        );
    }

    #[test]
    fn report_debug_never_prints_the_output() {
        let report = ExecReport {
            output: "DB_PASSWORD=hunter2".into(),
            exit_code: Some(1),
        };
        let debug = format!("{report:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("output_len: 19"), "{debug}");
    }

    #[test]
    fn interpret_without_any_number_is_none() {
        let report = ExecReport {
            output: "Compiling…".into(),
            exit_code: None,
        };
        assert_eq!(
            interpret(Runner::CargoTest, strings(&["cargo", "test"]), &report),
            None
        );
        // Ein Exit-Code allein ist eine Aussage (z. B. Build-Fehler, 101).
        let report = ExecReport {
            output: "error[E0425]".into(),
            exit_code: Some(101),
        };
        let got = interpret(Runner::CargoTest, strings(&["cargo", "test"]), &report).unwrap();
        assert_eq!(got.exit_code, Some(101));
        assert_eq!(got.tests, None);
    }
}
