//! `minds agent-help` — die eigene Kommando-Karte, maschinenlesbar.
//!
//! Gedacht für den Agenten, nicht für den Menschen (`--help` bleibt für den
//! Menschen). Ein Agent, der `minds` fahren soll, parst diese JSON-Karte und
//! weiß, welche Kommandos es gibt und wie sie heißen — ohne die Prosa-Hilfe zu
//! interpretieren. Billig, aber in Agent-Workflows hebelstark: die CLI
//! beschreibt sich selbst.

use std::process::ExitCode;

/// Führt `minds agent-help` aus — schreibt die Kommando-Karte als JSON.
pub fn run() -> ExitCode {
    let doc = serde_json::json!({
        "tool": "minds",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Dauerhafter Kontext für Agent-Sessions, in Git.",
        "commands": [
            {"name": "enable", "usage": "minds enable [--agent <name>] [--child-repo <pfad>] [--child-remote <url>]", "summary": "Repo Minds-fähig einrichten: Hooks + Store-Config."},
            {"name": "show", "usage": "minds show [<commit>] [--full]", "summary": "Intent und Attribution hinter einem Commit."},
            {"name": "why", "usage": "minds why <datei>:<zeile> [--full]", "summary": "Die Session hinter einer einzelnen Zeile."},
            {"name": "recall", "usage": "minds recall <ziel>", "summary": "Kontext-Brief hinter Datei, Zeile oder Commit — Agent-freundlich."},
            {"name": "distill", "usage": "minds distill [--path <dir>] [--out <datei>]", "summary": "AGENTS.md-Entwurf aus der Repo-Historie."},
            {"name": "brief", "usage": "minds brief [<datei>...]", "summary": "Größenbegrenzter Kontext-Block für den Start einer Session."},
            {"name": "recap", "usage": "minds recap [--limit <n>]", "summary": "Die jüngsten Sessions auf einen Blick."},
            {"name": "search", "usage": "minds search <query>", "summary": "Prompts und Sessions durchsuchen."},
            {"name": "fsck", "usage": "minds fsck [--require-review]", "summary": "Ist jeder Trailer auflösbar? Journal-Lücken? Mit --require-review: Policy-Gate."},
            {"name": "review", "usage": "minds review <change-id|session-id> --approve|--reject|--needs-work [--summary <text>] [--sign]", "summary": "Verdict als Git-Objekt anlegen; --sign macht daraus einen Nachweis."},
            {"name": "reviews", "usage": "minds reviews <subject> [--signers <datei>]", "summary": "Verdicts und Thread zu einem Change; --signers prüft die Signaturen."},
            {"name": "comment", "usage": "minds comment <subject> [--on <datei:zeile|turn:<n>>] \"<text>\"", "summary": "Anmerkung an den Review-Thread — append-only, konfliktfrei mergebar."},
            {"name": "stack", "usage": "minds stack [--base <ref>]", "summary": "Abhängige Changes und ihr Review-Stand; überlebt Rebase und Force-Push."},
            {"name": "audit", "usage": "minds audit --export [--out <datei>] [--base <ref>]", "summary": "Provenienz-Kette als portables, ohne dieses Werkzeug prüfbares Bündel."},
            {"name": "sync", "usage": "minds sync [--remote <name>]", "summary": "Kontext und Reviews zum Remote — alle Refs in einer Verbindung, nie mit --force."},
            {"name": "render", "usage": "minds render [--out <verzeichnis>]", "summary": "Statische HTML-Seite über den Kontext."},
            {"name": "agent-help", "usage": "minds agent-help", "summary": "Diese maschinenlesbare Kommando-Karte."}
        ]
    });
    // `to_string_pretty` über einen festen literalen Wert kann nicht fehlschlagen.
    println!(
        "{}",
        serde_json::to_string_pretty(&doc).expect("statische JSON-Karte serialisiert immer")
    );
    ExitCode::SUCCESS
}
