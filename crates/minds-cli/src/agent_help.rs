//! `minds agent-help` — die eigene Kommando-Karte, maschinenlesbar.
//!
//! Gedacht für den Agenten, nicht für den Menschen (`--help` bleibt für den
//! Menschen). Ein Agent, der `minds` fahren soll, parst diese JSON-Karte und
//! weiß, welche Kommandos es gibt und wie sie heißen — ohne die Prosa-Hilfe zu
//! interpretieren. Billig, aber in Agent-Workflows hebelstark: die CLI
//! beschreibt sich selbst.
//!
//! Die Karte ist handgeschrieben (USAGE-Prosa gehört nicht generiert), aber
//! nicht handgepflegt-driftend: Ein Test vergleicht ihre Namen mit
//! [`crate::public_commands`] — wer ein Kommando ergänzt, ohne es hier
//! einzutragen, wird rot. Vor #11 fehlten acht Kommandos, und die Karte nannte
//! sich trotzdem vollständig.
//!
//! Die `usage`-Zeilen sind bewusst **kuratierte Kurzformen**, keine
//! vollständigen Flag-Listen — vollständig ist die USAGE in `main.rs`. Der
//! Test bewacht deshalb nur die Namensmenge, nicht die Flags.

use std::process::ExitCode;

/// Die Kommando-Karte als JSON-Wert — getrennt von [`run`], damit der Test sie
/// lesen kann, statt stdout zu parsen.
fn card() -> serde_json::Value {
    serde_json::json!({
        "tool": "minds",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Dauerhafter Kontext für Agent-Sessions, in Git.",
        "commands": [
            {"name": "enable", "usage": "minds enable [--agent <name>] [--child-repo <pfad>] [--child-remote <url>]", "summary": "Repo Minds-fähig einrichten: Hooks + Store-Config."},
            {"name": "hook", "usage": "minds hook --agent <name> [--event <name>]", "summary": "Agent-Hook-Event von stdin ins lokale Journal. Endet immer mit 0."},
            {"name": "checkpoint", "usage": "minds checkpoint [--commit <id>]", "summary": "Journal deuten, redigieren, Sessions ablegen, Trailer anhängen."},
            {"name": "show", "usage": "minds show [<commit>] [--full]", "summary": "Intent und Attribution hinter einem Commit."},
            {"name": "why", "usage": "minds why <datei>:<zeile> [--full]", "summary": "Die Session hinter einer einzelnen Zeile."},
            {"name": "blame", "usage": "minds blame <datei>", "summary": "Attribution je Zeile, nach Session aggregiert, mit Kontext-Abdeckung."},
            {"name": "recall", "usage": "minds recall <ziel>", "summary": "Kontext-Brief hinter Datei, Zeile oder Commit — Agent-freundlich."},
            {"name": "distill", "usage": "minds distill [--path <dir>] [--out <datei>]", "summary": "AGENTS.md-Entwurf aus der Repo-Historie."},
            {"name": "brief", "usage": "minds brief [<datei>...]", "summary": "Größenbegrenzter Kontext-Block für den Start einer Session."},
            {"name": "recap", "usage": "minds recap [--limit <n>] [--all]", "summary": "Die jüngsten Sessions auf einen Blick."},
            {"name": "search", "usage": "minds search <query>", "summary": "Prompts und Sessions durchsuchen."},
            {"name": "agent-help", "usage": "minds agent-help", "summary": "Diese maschinenlesbare Kommando-Karte."},
            {"name": "metrics", "usage": "minds metrics [--format prometheus|openmetrics|json]", "summary": "Kennzahlen aus dem Store — Prometheus, OpenMetrics oder JSON."},
            {"name": "fsck", "usage": "minds fsck [--require-review]", "summary": "Ist jeder Trailer auflösbar? Journal-Lücken? Mit --require-review: Policy-Gate."},
            {"name": "forget", "usage": "minds forget <session> [--reason <text>]", "summary": "DSGVO-Löschung: Nutzlast wird Tombstone, die Referenz bleibt auflösbar."},
            {"name": "sign", "usage": "minds sign <session> [--key <pfad>]", "summary": "Attribution einer Session signieren (ssh-sig), nach stdout."},
            {"name": "verify", "usage": "minds verify <session> --sig <datei> [--signers <datei>] [--identity <id>]", "summary": "Signierte Attribution prüfen; Rückgabewert ≠ 0 bei ungültig."},
            {"name": "review", "usage": "minds review <change-id|session-id> --approve|--reject|--needs-work [--summary <text>] [--sign]", "summary": "Verdict als Git-Objekt anlegen; --sign macht daraus einen Nachweis."},
            {"name": "reviews", "usage": "minds reviews <subject> [--signers <datei>]", "summary": "Verdicts und Thread zu einem Change; --signers prüft die Signaturen."},
            {"name": "comment", "usage": "minds comment <subject> [--on <datei:zeile|turn:<n>>] \"<text>\"", "summary": "Anmerkung an den Review-Thread — append-only, konfliktfrei mergebar."},
            {"name": "stack", "usage": "minds stack [--base <ref>]", "summary": "Abhängige Changes und ihr Review-Stand; überlebt Rebase und Force-Push."},
            {"name": "gitlab", "usage": "minds gitlab mirror <subject> --mr <nr> | minds gitlab webhook [--write]", "summary": "Verdicts als MR-Note spiegeln bzw. Webhook-Kommentar als Verdict deuten."},
            {"name": "audit", "usage": "minds audit --export [--out <datei>] [--base <ref>]", "summary": "Provenienz-Kette als portables, ohne dieses Werkzeug prüfbares Bündel."},
            {"name": "sync", "usage": "minds sync [--remote <name>]", "summary": "Kontext und Reviews zum Remote — alle Refs in einer Verbindung, nie mit --force, außer für die Übertragung einer DSGVO-Löschung (Tombstone-Ref)."},
            {"name": "render", "usage": "minds render [--out <verzeichnis>]", "summary": "Statische HTML-Seite über den Kontext."}
        ]
    })
}

/// Führt `minds agent-help` aus — schreibt die Kommando-Karte als JSON.
pub fn run() -> ExitCode {
    // `to_string_pretty` über einen festen literalen Wert kann nicht fehlschlagen.
    println!(
        "{}",
        serde_json::to_string_pretty(&card()).expect("statische JSON-Karte serialisiert immer")
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Die Drift-Bremse aus #11: Die Karte nannte sich vollständig, und acht
    /// Kommandos fehlten. Maßstab ist die Parser-Tabelle — **die** Quelle, die
    /// beim Anlegen eines Kommandos ohnehin gepflegt werden muss.
    #[test]
    fn the_card_lists_exactly_the_public_commands() {
        let card = card();
        let listed: BTreeSet<&str> = card["commands"]
            .as_array()
            .expect("commands ist ein Array")
            .iter()
            .map(|entry| {
                entry["name"]
                    .as_str()
                    .expect("jedes Kommando hat einen Namen")
            })
            .collect();
        let public: BTreeSet<&str> = crate::public_commands().collect();

        assert_eq!(
            listed, public,
            "agent-help und die Parser-Tabelle (SPECS) driften auseinander"
        );
    }

    /// Jeder Eintrag braucht die drei Felder, die ein Agent parst.
    #[test]
    fn every_entry_carries_name_usage_and_summary() {
        let card = card();
        for entry in card["commands"].as_array().unwrap() {
            for field in ["name", "usage", "summary"] {
                assert!(
                    entry[field].as_str().is_some_and(|s| !s.is_empty()),
                    "{field} fehlt oder leer: {entry}"
                );
            }
        }
    }
}
