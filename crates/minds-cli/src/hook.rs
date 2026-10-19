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
//!
//! # Mit Witness: weitergeben statt schreiben (EA-07)
//!
//! Ist `MINDS_WITNESS_SOCKET` gesetzt, schreibt der Hook nicht selbst, sondern
//! schickt das Event als Frame an den Witness — den einen Schreiber auf dem
//! Host (W1). Die Mauer läuft **davor** und bleibt auf der Agent-Seite: Der
//! Inhalt einer Zugangsdaten-Datei überquert den Socket nicht. Scheitert die
//! Weitergabe, aus welchem Grund auch immer, steht eine Zeile in `hook.log`
//! und das Event geht wie bisher ins lokale Journal. Ohne die Variable ist
//! alles wie vorher. Die Einzelheiten stehen in [`witness`].

pub(crate) mod witness;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use minds_capture::{
    EventKind, Journal, NewEvent, clock, hook_event, secretwall, transcript, witness_proto,
};

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

/// Wie viel vom **Ende** des Transkripts der Hook höchstens liest, um die
/// Modell-Kennung zu finden (K1). Das Modell steht in jeder Assistant-Zeile;
/// ein Megabyte reicht für die letzte, auch bei langen Antworten.
const TRANSCRIPT_TAIL: u64 = 1024 * 1024;

/// Führt den Hook aus. Gibt **immer** [`ExitCode::SUCCESS`] zurück.
///
/// `agent` ist der Name aus der Hook-Registrierung. Fehlt er, ist das ein
/// Konfigurationsfehler — und trotzdem kein Grund für einen Rückgabewert
/// ungleich 0. Ein falsch registrierter Hook darf die Sitzung nicht anders
/// behandeln als ein kaputter; beides landet im Log.
pub fn run(agent: Option<&str>, event_override: Option<&str>) -> ExitCode {
    // Regel 1 und 2 in einer Klammer: [`hooklog::guarded`] fängt den Panic
    // **und** stellt den Standard-Handler still, der sonst vorher schon
    // `thread 'main' panicked at …` auf stderr geschrieben hätte — auf einen
    // Kanal, den Claude Code dem Modell zurückgibt (#54). Der Ort des Panics
    // steht dann im Log, wo er hingehört.
    //
    // Der Rückgabewert von `guarded` (bei Panic `FAILURE`) wird hier bewusst
    // verworfen: Für den heißen Pfad gilt Regel 1 ohne Ausnahme.
    let _ = hooklog::guarded(Source::Hook, || {
        let outcome = match agent {
            Some(agent) => record(agent, event_override),
            None => Err("called without --agent".into()),
        };
        if let Err(err) = outcome {
            hooklog::log(Source::Hook, &format!("{err:#}"));
        }
        ExitCode::SUCCESS
    });

    ExitCode::SUCCESS
}

/// Provoziert einen Panic im heißen Pfad — der einzige Weg, die Zusage aus
/// Regel 1 und 2 gegen den echten Prozess zu prüfen (#54).
///
/// Nur in Debug-Builds vorhanden; im ausgelieferten Release-Binary existiert
/// weder die Variable noch dieser Code. Ein sichtbares Flag wäre der falsche
/// Preis für einen Test — es stünde in `--help` und in der Kommando-Tabelle.
#[cfg(debug_assertions)]
const PANIC_FOR_TEST: &str = "MINDS_PANIC_FOR_TEST";

fn record(agent: &str, event_override: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    std::io::stdin().take(MAX_STDIN).read_to_end(&mut bytes)?;

    // Nach dem Lesen von stdin, nicht davor: Sonst schlösse das Kind die Pipe,
    // während der Test noch schreibt (EPIPE), und der Test würde aus einem
    // Grund rot, der mit #54 nichts zu tun hat.
    #[cfg(debug_assertions)]
    match std::env::var(PANIC_FOR_TEST).as_deref() {
        Ok("1") => panic!("deliberate panic for the test"),
        // Der schlimmere Fall, den ein Test bewachen muss: ein Panic, der
        // Payload in seine Meldung einbettet. Er darf nicht im Log landen —
        // `hook.log` wird in Bug-Reports mitgeschickt.
        Ok("payload") => panic!(
            "deliberate panic with payload: {}",
            String::from_utf8_lossy(&bytes)
        ),
        _ => {}
    }

    // Mit Witness braucht die Weitergabe die Rohbytes, `parse` verbraucht sie.
    // Die Kopie entsteht nur, wenn ein Witness da ist (Variable oder
    // Standardpfad dieses Repos), und nur, wenn sie überhaupt in einen Frame
    // passt: Ein Event über `MAX_FRAME` bliebe ohnehin lokal, und eine
    // nutzlose Kopie von bis zu 32 MiB ist auf dem heißen Pfad ein
    // OOM-Risiko, kein Detail.
    //
    // Gesucht wird ab dem Arbeitsverzeichnis des Hooks (vor `parse`); ob das
    // Event zu genau diesem Repo gehört, prüft der Abgleich mit dem `cwd`
    // des Payloads unten.
    let here = std::env::current_dir().unwrap_or_default();
    let witness = witness::discovered_socket(&here).map(|found| {
        let raw = witness::admissible(agent, event_override, bytes.len()).map(|()| bytes.clone());
        (found, raw)
    });

    // Die Uhr liest der Prozess, nicht der Parser: So bleibt `parse` eine reine
    // Funktion und damit gegen Fixtures testbar.
    let mut parsed = hook_event::parse(bytes, agent, event_override, clock::now())?;

    // Fail-closed, noch vor dem ersten Byte auf der Platte und auf dem Socket:
    // Berührt dieses Event eine Zugangsdaten-Datei, wird ihr Inhalt
    // weggelassen, nicht aufgehoben.
    let walled = secretwall::guard(&mut parsed.event).is_some();

    // K1: Beim Ende eines Zugs oder der Session kennt der Hook das Transkript
    // und meldet die Modell-Kennung mit — nur die Kennung, vom Ende der Datei
    // gelesen, damit der heiße Pfad nicht an einem großen Transkript hängt.
    // Der Witness liest das Transkript nicht selbst; so kennt auch er das
    // Modell, und das lokale Journal hat es, falls die Datei später fehlt.
    attach_model(&mut parsed.event);

    // Über den Standardpfad gefunden, aber das Event gehört zu einem anderen
    // Repo (der Agent startet Hooks nicht zwingend im Projekt): Dann geht es
    // nicht an diesen Witness, sondern wie bisher ins Journal seines Repos.
    // Ein Event ohne `cwd` gehört — wie beim Journal — zum Arbeitsverzeichnis
    // des Hooks.
    let witness = witness.filter(|(found, _)| match (&found.root, &parsed.cwd) {
        (Some(root), Some(cwd)) => witness::repo_root(cwd).as_ref() == Some(root),
        _ => true,
    });

    let unreachable = match witness {
        Some((found, raw)) => {
            let peer = found.peer;
            let socket = found.socket;
            // Hat die Mauer gegriffen, gehen nicht die Rohbytes über den
            // Socket, sondern ihr gewallter Neubau — der passt auch dann in
            // einen Frame, wenn der Rohpayload es nicht tat.
            //
            // Ein Payload, der kein JSON war (abgeschnitten, kaputt — `parse`
            // hat ihn als Zeichenkette gerettet), konnte die Mauer nicht
            // prüfen. Lokal ist das der bekannte Stand; über den Socket ginge
            // er womöglich zu einem anderen Nutzer. Also bleibt er hier —
            // gleich welcher Art: Bei kaputtem JSON ist auch die Eventart nur
            // geraten, und ein Tool-Event ohne erkannten Namen wäre sonst
            // durchgerutscht.
            //
            // Hat die Mauer gegriffen, war die Kopie oben umsonst. Das lässt
            // sich nicht vorziehen, weil `parse` die Bytes verbraucht; die
            // Obergrenze `MAX_FRAME` gilt trotzdem.
            let stdin = if walled {
                secretwall::walled_hook_bytes(&parsed.key, &parsed.event)
                    .ok_or("unforwardable payload")
            } else if salvaged(&parsed.event) {
                Err("unforwardable payload")
            } else {
                raw
            };
            let model = parsed
                .event
                .model
                .clone()
                .map(|model| (parsed.key.local_id().to_owned(), model));
            match stdin.and_then(|stdin| {
                witness::forward_with_model(&socket, peer, agent, event_override, stdin, model)
            }) {
                Ok(()) => return Ok(()),
                Err(kind) => Some(kind),
            }
        }
        None => None,
    };

    // Das Arbeitsverzeichnis aus dem Payload schlaegt unser eigenes — Agents
    // starten Hooks nicht zwingend im Projektverzeichnis, und ein Hook, der das
    // falsche Repository findet, schreibt ins falsche Journal.
    let start: PathBuf = match parsed.cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir()?,
    };

    let journal = match Journal::discover(&start) {
        Ok(journal) => journal,
        // Ohne Journal kein Ort neben ihm: Das Event scheitert ohnehin, und
        // `run` loggt den Fehler — der Grund des Rückfalls reist in derselben
        // Zeile mit statt in einer zweiten.
        Err(err) => {
            return Err(match unreachable {
                Some(kind) => format!("witness unreachable: {kind}; {err}").into(),
                None => err.into(),
            });
        }
    };
    // Der Rückfall ist kein Fehler des Events, aber einer der Einrichtung — er
    // gehört ins Log, und zwar neben das Journal, das ihn jetzt auffängt.
    if let Some(kind) = unreachable {
        hooklog::log_static_beside(&journal, Source::Hook, &["witness unreachable: ", kind]);
    }
    journal.append(&parsed.key, parsed.event)?;
    Ok(())
}

/// Ein Event, dessen Payload als JSON-Zeichenkette gerettet wurde — die Mauer
/// hatte darin keinen Pfad zu sehen.
fn salvaged(event: &NewEvent) -> bool {
    event.payload.get().starts_with('"')
}

/// Hängt beim `Stop`/`SessionEnd` die Modell-Kennung aus dem Ende des
/// Transkripts an das Event (K1). Andere Events bleiben ohne — das Transkript
/// wird nicht bei jedem Tool-Aufruf angefasst. Fehlt die Datei oder das
/// Modell, bleibt das Feld leer; der Hook scheitert daran nie.
///
/// Die Kennung kommt aus einer Datei, die der Agent schreibt: Nur was die
/// Regel des Protokolls erfüllt ([`witness_proto::check_model`]: nicht leer,
/// höchstens `MAX_MODEL` Bytes, druckbares ASCII) wird angehängt — alles
/// andere ist kein Modell, und nie ein Grund, das Event zu opfern.
fn attach_model(event: &mut NewEvent) {
    if !matches!(event.kind, EventKind::TurnEnd | EventKind::SessionEnd) {
        return;
    }
    let Some(path) = event.transcript_path.as_deref() else {
        return;
    };
    event.model = transcript::model_in_tail(Path::new(path), TRANSCRIPT_TAIL)
        .filter(|model| witness_proto::check_model(model).is_ok());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: EventKind, path: Option<&Path>) -> NewEvent {
        NewEvent {
            at: "2026-10-19T10:00:00Z".into(),
            at_nanos: 0,
            kind,
            raw_kind: format!("{kind:?}"),
            cwd: None,
            transcript_path: path.map(|p| p.to_string_lossy().into_owned()),
            model: None,
            payload: serde_json::value::RawValue::from_string("{}".into()).unwrap(),
        }
    }

    /// K1: Nur `Stop` und `SessionEnd` lesen das Ende des Transkripts; ein
    /// Tool-Event bleibt ohne Modell, ebenso ein Event ohne Pfad.
    #[test]
    fn the_model_is_attached_at_stop_and_session_end_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(
            &path,
            br#"{"type":"assistant","message":{"model":"claude-opus-4-1","content":[]}}"#,
        )
        .unwrap();
        for kind in [EventKind::TurnEnd, EventKind::SessionEnd] {
            let mut e = event(kind, Some(&path));
            attach_model(&mut e);
            assert_eq!(e.model.as_deref(), Some("claude-opus-4-1"), "{kind:?}");
        }
        let mut e = event(EventKind::ToolPost, Some(&path));
        attach_model(&mut e);
        assert_eq!(e.model, None);
        let mut e = event(EventKind::TurnEnd, None);
        attach_model(&mut e);
        assert_eq!(e.model, None);
        let mut e = event(EventKind::TurnEnd, Some(&dir.path().join("missing.jsonl")));
        attach_model(&mut e);
        assert_eq!(e.model, None, "fehlende Datei: kein Modell, kein Fehler");
    }

    /// M2: Was im Transkript als Modell steht, prüft der Hook wie der
    /// Witness — zu lang, Steuerzeichen, Leerraum: kein Modell, Event bleibt.
    #[test]
    fn an_unusable_model_id_is_dropped_and_the_event_kept() {
        let dir = tempfile::tempdir().unwrap();
        for (name, bad) in [
            ("long", "m".repeat(witness_proto::MAX_MODEL + 1)),
            ("control", "a\u{1b}b".to_string()),
            ("space", "claude opus".to_string()),
        ] {
            let path = dir.path().join(format!("{name}.jsonl"));
            let line = serde_json::json!({
                "type": "assistant",
                "message": {"model": bad, "content": []}
            });
            std::fs::write(&path, format!("{line}\n")).unwrap();
            let mut e = event(EventKind::TurnEnd, Some(&path));
            attach_model(&mut e);
            assert_eq!(e.model, None, "{name}");
            assert_eq!(e.kind, EventKind::TurnEnd, "{name}: das Event bleibt");
        }
    }
}
