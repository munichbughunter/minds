//! `minds brief [<datei>...] [--hook]` — ein größenbegrenzter Kontext-Block für
//! den Start einer neuen Agent-Session.
//!
//! Vorausschauend statt rückblickend: „Ich fange gleich an *diesen* Dateien an —
//! was muss ein Agent wissen?" Ohne Pfade nimmt es das ganze Repo. Anders als
//! `recall`/`distill` ist die Ausgabe **gedeckelt** (siehe [`CAP`]), damit der
//! Agent-Input klein bleibt und keine Tokens verschwendet.
//!
//! # `--hook`: der Kontext direkt in die Session
//!
//! Mit `--hook` gibt das Kommando statt reinem Markdown das
//! SessionStart-Envelope von Claude Code aus
//! (`hookSpecificOutput.additionalContext`) — genau das, was `minds enable
//! --recall` als SessionStart-Hook registriert, damit jede neue Session den Brief
//! der vorigen automatisch voranstellt (Vision-Problem #3). Der Vertrag ist
//! Claude-spezifisch; andere Agents folgen, sobald ihr Envelope verifiziert ist.

use std::process::ExitCode;

use minds_core::Session;

use crate::context::{self, Context};
use crate::hooklog::{self, Source};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Höchstzahl der Einträge je Abschnitt — klein, damit der Block als Agent-Input
/// taugt (Headroom-Rücksicht).
const CAP: usize = 8;

/// Führt `minds brief` aus. `paths` sind die Dateien, um die es geht; leer =
/// ganzes Repo. `hook` verpackt die Ausgabe ins Claude-SessionStart-Envelope.
///
/// # Zwei Aufrufer, zwei Kanäle
///
/// Ohne `--hook` steht ein Mensch davor: Der Fehler gehört auf stderr, wie bei
/// `recall` und `distill`.
///
/// Mit `--hook` läuft das Kommando aus der Agent-Konfiguration, und die
/// registrierte Zeile lautet `minds brief --hook 2>/dev/null || true`. stderr
/// geht dort ins Nichts, der Rückgabewert wird verschluckt — scheiterte
/// `brief`, startete die Sitzung ohne den Kontext, den minds ihr mitgeben
/// wollte, und niemand erfuhr es (#68). Deshalb hier dieselbe Klammer wie bei
/// den Git-Hooks: der Fehler nach `<git-dir>/minds/hook.log`, der Panic
/// ebenfalls, und **kein Byte** auf stdout — dort steht der injizierte
/// Kontext, ein Fehlertext würde als solcher in die Sitzung gehoben.
///
/// Übersprungene Sessions (#83) folgen derselben Aufteilung: ohne `--hook`
/// eine Zeile auf stderr, mit `--hook` ein Eintrag im Log.
pub fn run(paths: &[String], hook: bool) -> ExitCode {
    if !hook {
        return match brief(paths, false) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("minds brief: {err}");
                ExitCode::FAILURE
            }
        };
    }

    hooklog::guarded(Source::Brief, || match brief(paths, true) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            hooklog::log(Source::Brief, &format!("{err:#}"));
            ExitCode::FAILURE
        }
    })
}

/// Provoziert einen Panic — der einzige Weg, gegen den echten Prozess zu
/// prüfen, dass er weder die Sitzung erreicht noch spurlos verschwindet (#68).
/// Nur in Debug-Builds vorhanden, wie das Pendant in [`crate::hook`].
#[cfg(debug_assertions)]
const PANIC_FOR_TEST: &str = "MINDS_BRIEF_PANIC_FOR_TEST";

fn brief(paths: &[String], hook: bool) -> Fallible<()> {
    #[cfg(debug_assertions)]
    if std::env::var(PANIC_FOR_TEST).as_deref() == Ok("1") {
        panic!("absichtlicher Panic für den Test");
    }

    let ctx = Context::open()?;
    let (all, skipped) = ctx.all_sessions()?;

    let sessions: Vec<Session> = if paths.is_empty() {
        all
    } else {
        all.into_iter()
            .filter(|session| paths.iter().any(|path| context::touches(session, path)))
            .collect()
    };

    let label = if paths.is_empty() {
        "gesamtes Repo".to_string()
    } else {
        paths.join(", ")
    };
    let markdown = minds_reader::brief::render(
        &format!("Kontext für den Agenten — {label}"),
        &sessions,
        Some(CAP),
    );

    // Der Hinweis kommt vor der Ausgabe — auch ein leerer Brief erklärt sich
    // dann (#83). Im Hook-Fall geht er ins Log, stdout trägt nur das Envelope.
    if let Some(note) = skipped.note() {
        if hook {
            hooklog::log(Source::Brief, &note);
        } else {
            eprintln!("minds brief: {note}");
        }
    }

    if hook {
        // Das dokumentierte SessionStart-Envelope: additionalContext wird der
        // neuen Session vorangestellt.
        let doc = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": markdown,
            }
        });
        println!("{}", serde_json::to_string(&doc)?);
    } else {
        print!("{markdown}");
    }
    Ok(())
}
