//! `minds reinterpret <session-id>` — dieselbe Evidence, der heutige Blick.
//!
//! Interpretation ist rekonstruierbar, Evidence nicht veränderbar (ADR-0011):
//! Dieses Kommando liest eine **gespeicherte** Session und deutet ihre
//! erhaltenen Tool-Aufrufe mit dem **aktuellen** Adapter-Stand neu — strikt
//! lesend. Es schreibt nichts: keine neue Session, kein verändertes Envelope,
//! kein angefasster Seal. Die Ausgabe ist je Aufruf ein
//! Interpretations-Protokoll:
//!
//! ```text
//! #0 Read
//!    Evidenz       b3-…#turn0/call0 (unverändert)
//!    gespeichert   claude-code v1 → READ src/retry.rs
//!    aktuell       claude-code v1 → READ src/retry.rs (unverändert)
//! ```
//!
//! Deterministisch: gleiche Session + gleicher Adapter-Stand ⇒ gleiche
//! Ausgabe. Ein Agent ohne Adapter bleibt „beobachtet, nicht gedeutet" — und
//! genau das steht dann da, statt einer erfundenen Wirkung.

use std::process::ExitCode;

use minds_capture::adapter_for;
use minds_core::{CaptureStatus, EffectKind, SessionId};

use crate::context::Context;
use crate::text::{sanitize, sanitize_path};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds reinterpret` aus.
pub fn run(target: Option<&str>) -> ExitCode {
    let Some(target) = target else {
        eprintln!("minds reinterpret: expected <session-id> (b3-…)");
        return ExitCode::FAILURE;
    };
    match reinterpret(target) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Der Fehlertext kann Repo-Inhalte zitieren (Tombstone-Grund) —
            // Terminal-Härtung an der Senke.
            eprintln!(
                "minds reinterpret: {}",
                crate::text::sanitize(&err.to_string())
            );
            ExitCode::FAILURE
        }
    }
}

fn reinterpret(target: &str) -> Fallible<()> {
    let id: SessionId = target
        .parse()
        .map_err(|err| format!("not a valid session id {target:?}: {err}"))?;
    let ctx = Context::open()?;
    let session = ctx
        .store
        .get(id)?
        .ok_or_else(|| format!("session {id} is not in the store"))?;

    println!("Session   {id}");
    println!("Agent     {}", sanitize(&session.agent.name));
    let adapter = adapter_for(&session.agent.name);
    match adapter {
        Some(adapter) => println!(
            "Adapter   {} v{} (current state)",
            adapter.agent(),
            adapter.version()
        ),
        None => println!(
            "Adapter   none for {} — the interpretation stays \"observed, not interpreted\"",
            sanitize(&session.agent.name)
        ),
    }

    let mut calls = 0usize;
    let mut changed = 0usize;
    for (turn_index, turn) in session.turns.iter().enumerate() {
        for (call_index, call) in turn.tool_calls.iter().enumerate() {
            // 0-basiert, passend zur Evidenz-Adresse.
            println!("#{calls} {}", sanitize(&call.name));
            calls += 1;
            // Die Evidenz-Adresse: unveränderlich, das ist der Punkt.
            println!("   Evidence      {id}#turn{turn_index}/call{call_index} (unchanged)");
            println!("   stored        {}", stored_line(call));

            let current = adapter.and_then(|a| a.interpret_stored(&call.name, &call.arguments));
            match current {
                Some(now) => {
                    let same = call.effect.as_ref() == Some(&now.effect)
                        && call.capture.as_ref().map(|c| c.status) == Some(now.status);
                    if !same {
                        changed += 1;
                    }
                    println!(
                        "   current       {} v{} → {}{}",
                        now.adapter,
                        now.adapter_version,
                        effect_line(now.status, Some(&now.effect)),
                        if same {
                            " (unchanged)"
                        } else {
                            " (REINTERPRETED)"
                        }
                    );
                }
                None => {
                    println!("   current       no adapter — interpretation unchanged");
                }
            }
        }
    }

    if calls == 0 {
        println!("no tool calls — nothing to interpret");
    } else {
        println!(
            "{calls} call(s), {changed} with a newer interpretation. The evidence is unchanged — \
             only the view of it."
        );
    }
    Ok(())
}

/// Die gespeicherte Deutung eines Aufrufs, eine Zeile.
fn stored_line(call: &minds_core::ToolCall) -> String {
    match &call.capture {
        Some(capture) => format!(
            "{} v{} → {}",
            sanitize(&capture.adapter),
            capture.adapter_version,
            effect_line(capture.status, call.effect.as_ref())
        ),
        None => format!(
            "captured before the evidence chain → {}",
            effect_line(CaptureStatus::Interpreted, call.effect.as_ref())
        ),
    }
}

/// Wirkung als Wort + Pfad — oder die ehrliche Leerstelle.
fn effect_line(status: CaptureStatus, effect: Option<&minds_core::Effect>) -> String {
    if status == CaptureStatus::Uninterpreted {
        return "◐ observed, not interpreted — effect unknown".to_string();
    }
    match effect {
        Some(effect) => {
            let word = match effect.kind {
                EffectKind::Read => "READ",
                EffectKind::Write => "EDIT",
                EffectKind::Exec => "EXEC",
                EffectKind::Delete => "DELETE",
                EffectKind::Other => "TOOL",
            };
            match &effect.path {
                Some(path) => format!("{word} {}", sanitize_path(path)),
                None => word.to_string(),
            }
        }
        None => "TOOL (no effect)".to_string(),
    }
}
