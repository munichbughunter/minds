//! `minds hook` — die Prozesshülle um [`minds_capture::hook_event`].
//!
//! Dieses Modul weiß nichts über Agents, Eventnamen oder Payload-Felder. Es
//! weiß etwas über **Prozesse**: dass stdin begrenzt gelesen werden muss, dass
//! ein Panic nicht nach draußen darf, dass der Rückgabewert 0 ist und dass
//! stdout jemand anderem gehört. Das Formatwissen sitzt in `minds-capture` —
//! damit stimmt die Abhängigkeitsrichtung aus dem Plan (`capture ← cli`), und
//! Payload-Fixtures lassen sich testen, ohne einen Prozess zu starten.
//!
//! # Die drei Regeln
//!
//! **1. Immer Exit 0.** Bei Claude Code bedeutet Exit-Code 2 „blockiere diese
//! Aktion", und stderr wird dem Modell zurückgegeben. Ein abstürzender
//! Rekorder, dessen Rückgabewert als Blockade gedeutet wird, macht Arbeit
//! kaputt. Deshalb: `catch_unwind` um alles, jeder Fehler ins Log, Rückgabewert
//! unverändert 0 — auch bei fehlendem `--agent`, auch wenn nichts geschrieben
//! werden konnte.
//!
//! **2. Kein Byte auf stdout.** Mehrere Agents deuten stdout des Hooks als
//! Steuerkanal (JSON-Entscheidungen, injizierter Kontext). Was wir dort
//! ausgäben, würde als Anweisung gelesen. Diagnose geht in eine Datei, nicht
//! auf einen Kanal, der jemandem gehört — siehe [`crate::hooklog`].
//!
//! **3. Nichts Teures.** Kein Repository öffnen, keine Konfiguration lesen, kein
//! Transkript parsen, keine Redaction. Der Hook sucht ein Verzeichnis, schreibt
//! eine Datei und geht. Alles Übrige passiert beim Checkpoint, wo Latenz
//! niemandem wehtut.
//!
//! # Die eine Ausnahme: die Secretfile-Mauer
//!
//! Genau ein Deutungsschritt läuft doch schon hier, weil er *fail-closed* ist
//! und nicht warten darf: [`secretwall::guard`](minds_capture::secretwall::guard)
//! prüft bei einem Tool-Event den Pfad und lässt den Inhalt einer
//! Zugangsdaten-Datei (`​.env`, `id_rsa`, `*.pem`) gar nicht erst ins Journal.
//! Das ist ein Pfad-Test plus, im seltenen Trefferfall, ein kleiner
//! JSON-Neubau — billig genug für den heißen Pfad und die einzige Stelle, an
//! der Weglassen wichtiger ist als Geschwindigkeit.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use minds_capture::{Journal, clock, hook_event, secretwall};

use crate::hooklog::{self, Source};

/// Obergrenze für stdin. Ein `PostToolUse`-Payload trägt das Tool-Ergebnis mit
/// und kann groß sein; unbegrenzt zu lesen hieße, dass ein einzelnes `cat`
/// einer riesigen Datei uns den Speicher füllt.
///
/// Darüber wird abgeschnitten. Das Event geht dabei **nicht** verloren: Der
/// abgeschnittene Rest ist kein gültiges JSON mehr und wird von
/// [`hook_event::parse`] als Zeichenkette abgelegt statt verworfen. Ein
/// unvollständiges Event ist besser als ein OOM im Prozess des Nutzers und
/// besser als gar keines.
const MAX_STDIN: u64 = 32 * 1024 * 1024;

/// Führt den Hook aus. Gibt **immer** [`ExitCode::SUCCESS`] zurück.
///
/// `agent` ist der Name aus der Hook-Registrierung. Fehlt er, ist das ein
/// Konfigurationsfehler — und trotzdem kein Grund für einen Rückgabewert
/// ungleich 0. Ein falsch registrierter Hook darf die Sitzung nicht anders
/// behandeln als ein kaputter; beides landet im Log.
pub fn run(agent: Option<&str>, event_override: Option<&str>) -> ExitCode {
    // Regel 1. Auch ein Panic in unserem eigenen Code darf die Sitzung des
    // Nutzers nicht beenden.
    let outcome = std::panic::catch_unwind(|| match agent {
        Some(agent) => record(agent, event_override),
        None => Err("ohne --agent aufgerufen".into()),
    });

    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(err)) => hooklog::log(Source::Hook, &format!("{err:#}")),
        Err(_) => hooklog::log(Source::Hook, "Panic im Hook — Event verworfen"),
    }

    ExitCode::SUCCESS
}

fn record(agent: &str, event_override: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    std::io::stdin().take(MAX_STDIN).read_to_end(&mut bytes)?;

    // Die Uhr liest der Prozess, nicht der Parser: So bleibt `parse` eine reine
    // Funktion und damit gegen Fixtures testbar.
    let mut parsed = hook_event::parse(bytes, agent, event_override, clock::now())?;

    // Fail-closed, noch vor dem ersten Byte auf der Platte: Berührt dieses Event
    // eine Zugangsdaten-Datei, wird ihr Inhalt weggelassen, nicht aufgehoben.
    secretwall::guard(&mut parsed.event);

    // Das Arbeitsverzeichnis aus dem Payload schlaegt unser eigenes — Agents
    // starten Hooks nicht zwingend im Projektverzeichnis, und ein Hook, der das
    // falsche Repository findet, schreibt ins falsche Journal.
    let start: PathBuf = match parsed.cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir()?,
    };

    Journal::discover(&start)?.append(&parsed.key, parsed.event)?;
    Ok(())
}
