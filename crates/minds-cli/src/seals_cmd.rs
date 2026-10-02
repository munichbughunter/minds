//! `minds seals` — Evidence-Chain-Seals auffindbar machen, ohne eine Id schon
//! zu kennen (ADR-0011).
//!
//! `minds verify --evidence <seal-id>` und `minds sign --seal <seal-id>`
//! brauchen beide eine Id, die man schon hat. Dieses Kommando ist der
//! Entdeckungspfad davor: `store.list_seals()` (alle) oder
//! `store.seals_of(session)` (gefiltert) — beide existieren und tun schon
//! alles Nötige, hier ist reines Wiring plus Sortierung/Kappung/Anzeige.
//!
//! Reiner Lesepfad: Kein neuer Trait, kein neues Format, kein Schreibzugriff
//! auf `refs/minds/evidence/*`. Ein manipulierter oder unlesbarer Seal
//! bekommt eine kurze eigene Zeile und bricht die übrige Liste nicht ab —
//! „tolerant lesen" gilt auch für eine Übersicht.

use std::fmt::Write as _;
use std::process::ExitCode;

use minds_core::evidence::{Seal, SealOutcome};
use minds_core::{ContentHash, SessionId};
use minds_store::StoreError;

use crate::context::Context;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds seals` aus.
pub fn run(session: Option<&str>, limit: Option<&str>) -> ExitCode {
    match list(session, limit) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds seals: {}", crate::text::sanitize(&err.to_string()));
            ExitCode::FAILURE
        }
    }
}

/// Ein gelesener, hash-valider und geparster Seal — fertig zum Anzeigen.
struct Entry {
    id: ContentHash,
    seal: Seal,
    signed: bool,
}

fn list(session: Option<&str>, limit: Option<&str>) -> Fallible<()> {
    let ctx = Context::open()?;

    // Beide Flags validieren, bevor irgendetwas vom Repo-Zustand abhängt: ein
    // Tippfehler in `--limit` soll auch in einem leeren Repo auffallen, nicht
    // hinter „no seals yet" verschwinden.
    let limit: Option<usize> = limit
        .map(|raw| {
            raw.parse()
                .map_err(|err| format!("not a valid --limit {raw:?}: {err}"))
        })
        .transpose()?;

    let (seal_ids, scope) = match session {
        Some(raw) => {
            let id: SessionId = raw
                .parse()
                .map_err(|err| format!("not a valid session id {raw:?}: {err}"))?;
            (ctx.store.seals_of(id)?, Some(id))
        }
        None => (ctx.store.list_seals()?, None),
    };

    if seal_ids.is_empty() {
        match scope {
            Some(id) => println!("no seals for session {id}"),
            None => println!("no seals yet"),
        }
        return Ok(());
    }

    let mut entries = Vec::new();
    for id in &seal_ids {
        match ctx.store.seal_text(id) {
            Ok(Some(text)) => match Seal::parse(&text) {
                Ok(seal) => {
                    let signed = ctx.store.seal_signature(id)?.is_some();
                    entries.push(Entry {
                        id: id.clone(),
                        seal,
                        signed,
                    });
                }
                Err(err) => println!("▸ {}: TAMPERED — {err}", short_id(id)),
            },
            // Referenziert, aber nicht (mehr) im Store — dieselbe defensive
            // Lücke wie in `verify_cmd::check_seals`; für eine Übersicht
            // reicht Weiterlassen, ohne eigene Zeile (#7).
            Ok(None) => {}
            Err(StoreError::SealMismatch { actual, .. }) => {
                println!(
                    "▸ {}: TAMPERED — the stored text does not hash to this id (found {actual})",
                    short_id(id)
                );
            }
            Err(err) => return Err(err.into()),
        }
    }

    print!("{}", render(sort_and_limit(entries, limit)));
    Ok(())
}

/// Jüngstes zuerst, dann auf `limit` gekappt — **nach** dem Sortieren, sonst
/// zeigte `--limit` beliebige statt die jüngsten Einträge.
///
/// `last_event_at` kommt auf dem einzigen Schreibpfad
/// (`minds_capture::clock::now`, über den Checkpoint) immer als
/// `YYYY-MM-DDTHH:MM:SS.mmmZ` — fest breit, immer drei Nachkommastellen,
/// immer `Z`. Ein reiner String-Vergleich sortiert damit chronologisch
/// richtig, ohne eine Datums-Abhängigkeit einzuziehen. Bricht diese Annahme
/// (ein zweiter Schreibpfad mit anderer Breite), bricht sie lautlos — siehe
/// den Pin-Test in diesem Modul.
fn sort_and_limit(mut entries: Vec<Entry>, limit: Option<usize>) -> Vec<Entry> {
    entries.sort_by(|a, b| b.seal.last_event_at.cmp(&a.seal.last_event_at));
    if let Some(limit) = limit {
        entries.truncate(limit);
    }
    entries
}

/// Baut die Ausgabe aus den sortierten, ggf. gekappten Einträgen.
///
/// Der Kopf zählt, was tatsächlich gedruckt wird — nicht die Zahl vor
/// `--limit`. Ein manipulierter oder unlesbarer Seal bekam seine eigene Zeile
/// schon weiter oben (beim Einsammeln) und zählt hier nicht mit.
fn render(entries: Vec<Entry>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} seal(s):\n", entries.len());
    for entry in &entries {
        let _ = writeln!(
            out,
            "▸ {}  seq {}–{}, {} event(s), {} gap(s), {} — {}",
            short_id(&entry.id),
            entry.seal.first_seq,
            entry.seal.last_seq,
            entry.seal.events,
            entry.seal.gaps,
            entry.seal.outcome.human_word(),
            if entry.signed { "signed" } else { "unsigned" },
        );
        let session = match &entry.seal.outcome {
            SealOutcome::Stored { session } => session.as_str(),
            SealOutcome::Rejected => "-",
        };
        let _ = writeln!(out, "  session  {session}");
        let _ = writeln!(out, "  time     {}", entry.seal.last_event_at);
        let _ = writeln!(out);
    }
    out
}

/// `b3-` plus die ersten zwölf Hex-Zeichen — dieselbe Kürzung wie
/// `blame::short_id` für `SessionId` (kein gemeinsamer Helfer existiert für
/// `ContentHash`; jede Oberfläche im Workspace kopiert ihre eigene).
fn short_id(id: &ContentHash) -> String {
    let s = id.to_string();
    if s.len() <= 15 {
        s
    } else {
        format!("{}…", &s[..15])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
    }

    fn session_id(byte: u8) -> String {
        format!("b3-{}", format!("{:x}", byte % 16).repeat(64))
    }

    fn seal(outcome: SealOutcome, last_event_at: &str) -> Seal {
        Seal {
            root: hash(0xaa),
            agent: "claude-code".into(),
            scope: minds_core::evidence::SCOPE_AGENT_HOOKS_V1.into(),
            first_seq: 1,
            last_seq: 7,
            events: 7,
            gaps: 0,
            pre_chain: 0,
            outcome,
            previous: None,
            last_event_at: last_event_at.into(),
        }
    }

    fn stored_entry(id_byte: u8, session_byte: u8, last_event_at: &str, signed: bool) -> Entry {
        Entry {
            id: hash(id_byte),
            seal: seal(
                SealOutcome::Stored {
                    session: session_id(session_byte),
                },
                last_event_at,
            ),
            signed,
        }
    }

    #[test]
    fn short_id_truncates_after_twelve_hex_chars() {
        let full = short_id(&hash(0x11));
        assert!(full.starts_with("b3-111111111111"));
        assert!(full.ends_with('…'));
        assert_eq!(full.chars().count(), 16); // "b3-" + 12 hex + "…"
    }

    #[test]
    fn an_empty_list_still_prints_a_zero_header() {
        assert_eq!(render(Vec::new()), "0 seal(s):\n\n");
    }

    #[test]
    fn a_stored_seal_shows_its_session_and_signed_word() {
        let entry = stored_entry(0x11, 0x22, "2026-09-17T14:32:05Z", true);
        let out = render(vec![entry]);
        assert!(out.starts_with("1 seal(s):\n\n"), "{out}");
        assert!(out.contains(&short_id(&hash(0x11))), "{out}");
        assert!(out.contains("stored — signed"), "{out}");
        assert!(
            out.contains(&format!("session  {}", session_id(0x22))),
            "{out}"
        );
        assert!(out.contains("time     2026-09-17T14:32:05Z"), "{out}");
    }

    #[test]
    fn an_unsigned_seal_says_so() {
        let entry = stored_entry(0x11, 0x22, "2026-09-17T14:32:05Z", false);
        assert!(render(vec![entry]).contains("stored — unsigned"));
    }

    #[test]
    fn a_rejected_seal_shows_a_dash_for_its_session() {
        let entry = Entry {
            id: hash(0x33),
            seal: seal(SealOutcome::Rejected, "2026-09-15T18:02:12Z"),
            signed: false,
        };
        let out = render(vec![entry]);
        assert!(out.contains("rejected (payload) — unsigned"), "{out}");
        assert!(out.contains("session  -\n"), "{out}");
    }

    #[test]
    fn sorting_is_most_recent_first_and_limit_applies_after() {
        let oldest = stored_entry(0x01, 0x01, "2026-09-15T18:02:12Z", false);
        let middle = stored_entry(0x02, 0x02, "2026-09-16T09:11:40Z", false);
        let newest = stored_entry(0x03, 0x03, "2026-09-17T14:32:05Z", false);

        let sorted = sort_and_limit(vec![oldest, middle, newest], None);
        let ids: Vec<ContentHash> = sorted.iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids, vec![hash(0x03), hash(0x02), hash(0x01)]);
    }

    #[test]
    fn limit_keeps_the_newest_entries_not_arbitrary_ones() {
        let oldest = stored_entry(0x01, 0x01, "2026-09-15T18:02:12Z", false);
        let middle = stored_entry(0x02, 0x02, "2026-09-16T09:11:40Z", false);
        let newest = stored_entry(0x03, 0x03, "2026-09-17T14:32:05Z", false);

        let limited = sort_and_limit(vec![oldest, middle, newest], Some(1));
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].id, hash(0x03));
    }

    #[test]
    fn limit_zero_keeps_the_header_accurate() {
        let entry = stored_entry(0x11, 0x22, "2026-09-17T14:32:05Z", false);
        let limited = sort_and_limit(vec![entry], Some(0));
        assert_eq!(render(limited), "0 seal(s):\n\n");
    }

    /// Pin-Test für die Sortier-Annahme oben: Bricht der einzige Schreibpfad
    /// (`clock::rfc3339_from_nanos`) sein Format, soll das hier auffallen —
    /// nicht erst als stille Fehlsortierung in `minds seals`.
    #[test]
    fn clock_timestamps_have_the_fixed_width_the_sort_relies_on() {
        let now = minds_capture::clock::now();
        assert!(
            now.0.len() == "2026-09-17T14:32:05.123Z".len(),
            "last_event_at-Zeitstempel haben ihre Breite geändert: {}",
            now.0
        );
        assert!(now.0.ends_with('Z'), "{}", now.0);
    }
}
