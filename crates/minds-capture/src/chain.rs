//! Die Brücke vom Journal zur Evidence-Chain: aus einem [`ReadOutcome`] die
//! Kettenglieder in verbindlicher Reihenfolge (ADR-0011).
//!
//! Der Fold selbst lebt in [`minds_core::evidence`] — pur, ohne I/O, extern
//! nachrechenbar. Hier steht nur, **wie** ein gelesenes Journal zu Gliedern
//! wird; diese Regel ist Teil des Vertrags, denn eine andere Reihenfolge
//! ergäbe einen anderen Root:
//!
//! 1. Events in `seq`-Reihenfolge (so liest das Journal). Ein Event mit
//!    gestempeltem Hash wird [`ChainItem::Event`], ein Alt-Event ohne Stempel
//!    [`ChainItem::PreChain`].
//! 2. Jeder **maximale Lauf** fehlender Sequenznummern wird an seiner Stelle
//!    ein [`GapRecord::Missing`] — die Lücke steht dort, wo sie klafft.
//! 3. Beschädigte Dateien ([`ReadOutcome::damaged`]) kommen **ans Ende**,
//!    sortiert nach Sequenznummer (aus dem Dateinamen, soweit lesbar; ohne
//!    Nummer zuletzt, nach Dateiname). Sie haben keinen verlässlichen Platz
//!    in der Folge — eine leere Reservierung kann von jedem Zeitpunkt
//!    stammen —, aber sie gehören in die Geschichte: Wer sie wegließe,
//!    bekäme einen anderen Root.
//!
//! Die Coverage claimt dabei nur, was tatsächlich gelesen wurde
//! ([`ReadOutcome`] kennt nur den Bereich zwischen kleinster und größter
//! vorhandener Nummer) — Crash-Ehrlichkeit, ADR-0011 Entscheidung 2.

use std::fs;
use std::path::Path;

use minds_core::evidence::{self, ChainItem, ChainResult, GapRecord};

use crate::journal::ReadOutcome;

/// Obergrenze für das Hashen beschädigter Dateien: Reste stammen aus einer
/// nicht vertrauenswürdigen Größenquelle (liegengebliebene `.tmp`); mehr als
/// dieses Präfix zu binden lohnt den Speicher nicht.
const DAMAGED_HASH_CAP: u64 = 4 * 1024 * 1024;

/// Baut die Kettenglieder eines gelesenen Journals und faltet sie zum Root.
pub fn chain(outcome: &ReadOutcome) -> ChainResult {
    evidence::chain(&items(outcome))
}

/// Wie [`chain`], aber mit dem Session-Salt gefaltet — die Form, die in einen
/// Seal gehört (Anti-Orakel, siehe [`evidence::chain_salted`]).
pub fn chain_salted(salt: &[u8; 32], outcome: &ReadOutcome) -> ChainResult {
    evidence::chain_salted(salt, &items(outcome))
}

/// Die Glieder in verbindlicher Reihenfolge (siehe Modul-Doku).
pub fn items(outcome: &ReadOutcome) -> Vec<ChainItem> {
    let mut items = Vec::with_capacity(outcome.events.len() + outcome.damaged.len() + 4);

    // 1./2. Events und Luecken, verschraenkt in seq-Reihenfolge. `gaps` ist
    // aufsteigend (das Journal zaehlt aufsteigend durch); Laeufe werden zu
    // einem Glied zusammengefasst.
    let mut gaps = outcome.gaps.iter().copied().peekable();
    for event in &outcome.events {
        while let Some(&from) = gaps.peek() {
            if from > event.seq {
                break;
            }
            let mut to = from;
            gaps.next();
            while gaps.peek() == Some(&(to + 1)) {
                to += 1;
                gaps.next();
            }
            items.push(ChainItem::Gap(GapRecord::Missing { from, to }));
        }
        items.push(match &event.event_hash {
            Some(hash) => ChainItem::Event {
                seq: event.seq,
                hash: hash.clone(),
            },
            None => ChainItem::PreChain { seq: event.seq },
        });
    }
    // Luecken hinter dem letzten Event kann es per Definition nicht geben
    // (`gaps` zaehlt nur *zwischen* vorhandenen Nummern) — falls doch, gehen
    // sie trotzdem nicht verloren.
    while let Some(&from) = gaps.peek() {
        let mut to = from;
        gaps.next();
        while gaps.peek() == Some(&(to + 1)) {
            to += 1;
            gaps.next();
        }
        items.push(ChainItem::Gap(GapRecord::Missing { from, to }));
    }

    // 3. Beschaedigtes ans Ende, deterministisch sortiert.
    let mut damaged: Vec<(Option<u64>, &Path)> = outcome
        .damaged
        .iter()
        .map(|p| (seq_of(p), p.as_path()))
        .collect();
    damaged.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.1.cmp(b.1)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(b.1),
    });
    for (seq, path) in damaged {
        items.push(ChainItem::Gap(GapRecord::Damaged {
            seq,
            bytes: damaged_bytes(path),
        }));
    }

    items
}

/// Die Sequenznummer aus einem Journal-Dateinamen (`0000000042.json`,
/// `0000000042.json.tmp`), soweit lesbar.
fn seq_of(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let digits = name.split('.').next()?;
    if digits.len() != 10 {
        return None;
    }
    digits.parse().ok()
}

/// Hash über die vorgefundenen Bytes einer beschädigten Datei — damit auch
/// der Schaden selbst adressierbar ist. Leer oder unlesbar ⇒ `None`; das ist
/// ein Zustand, kein Fehler.
fn damaged_bytes(path: &Path) -> Option<minds_core::ContentHash> {
    use std::io::Read;

    // Gedeckelt lesen: Die Groesse stammt aus einer nicht vertrauenswuerdigen
    // Quelle. Bei Uebergroesse bindet der Hash das Praefix — deterministisch,
    // und fuer die Adressierbarkeit des Schadens genug.
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(DAMAGED_HASH_CAP).read_to_end(&mut bytes).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(evidence::payload_hash(&bytes))
}

#[cfg(test)]
mod tests {
    use minds_core::ContentHash;

    use super::*;
    use crate::journal::{EventKind, JournalEvent};

    fn event(seq: u64, stamped: bool) -> JournalEvent {
        JournalEvent {
            seq,
            at: "2026-08-24T10:00:00Z".into(),
            at_nanos: 1_000 + seq,
            kind: EventKind::Other,
            raw_kind: "X".into(),
            cwd: None,
            transcript_path: None,
            payload: serde_json::value::RawValue::from_string("{}".into()).unwrap(),
            payload_hash: None,
            event_hash: stamped.then(|| ContentHash::from_bytes([seq as u8; 32])),
        }
    }

    fn outcome(events: Vec<JournalEvent>, gaps: Vec<u64>) -> ReadOutcome {
        ReadOutcome {
            events,
            gaps,
            damaged: Vec::new(),
        }
    }

    #[test]
    fn a_gapless_run_is_only_events() {
        let got = items(&outcome(vec![event(0, true), event(1, true)], vec![]));
        assert_eq!(
            got,
            vec![
                ChainItem::Event {
                    seq: 0,
                    hash: ContentHash::from_bytes([0; 32])
                },
                ChainItem::Event {
                    seq: 1,
                    hash: ContentHash::from_bytes([1; 32])
                },
            ]
        );
        let result = chain(&outcome(vec![event(0, true), event(1, true)], vec![]));
        assert!(result.coverage.is_gap_free());
        assert_eq!(result.coverage.events, 2);
    }

    #[test]
    fn a_missing_run_becomes_one_gap_link_in_place() {
        // Events 0 und 4, Luecke 1–3: Das Gap-Glied steht ZWISCHEN den Events,
        // nicht irgendwo — die Stelle ist Teil des Roots.
        let got = items(&outcome(
            vec![event(0, true), event(4, true)],
            vec![1, 2, 3],
        ));
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[1],
            ChainItem::Gap(GapRecord::Missing { from: 1, to: 3 })
        );

        // Zwei getrennte Laeufe bleiben zwei Glieder.
        let got = items(&outcome(
            vec![event(0, true), event(2, true), event(4, true)],
            vec![1, 3],
        ));
        assert_eq!(got.len(), 5);
        assert_eq!(
            got[1],
            ChainItem::Gap(GapRecord::Missing { from: 1, to: 1 })
        );
        assert_eq!(
            got[3],
            ChainItem::Gap(GapRecord::Missing { from: 3, to: 3 })
        );
    }

    #[test]
    fn an_unstamped_event_is_a_pre_chain_link() {
        let got = items(&outcome(vec![event(0, false), event(1, true)], vec![]));
        assert_eq!(got[0], ChainItem::PreChain { seq: 0 });
        let result = chain(&outcome(vec![event(0, false), event(1, true)], vec![]));
        assert_eq!(result.coverage.pre_chain, 1);
        assert!(!result.coverage.is_gap_free());
    }

    #[test]
    fn damaged_files_join_the_chain_at_the_end_in_stable_order() {
        let dir = tempfile::tempdir().unwrap();
        let with_bytes = dir.path().join("0000000007.json");
        std::fs::write(&with_bytes, b"truemmer").unwrap();
        let empty = dir.path().join("0000000002.json");
        std::fs::write(&empty, b"").unwrap();
        let nameless = dir.path().join("weird.tmp");
        std::fs::write(&nameless, b"x").unwrap();

        let mut o = outcome(vec![event(0, true)], vec![]);
        // Absichtlich unsortiert uebergeben — die Ordnung stellt die Bruecke her.
        o.damaged = vec![nameless.clone(), with_bytes.clone(), empty.clone()];

        let got = items(&o);
        assert_eq!(got.len(), 4);
        assert_eq!(
            got[1],
            ChainItem::Gap(GapRecord::Damaged {
                seq: Some(2),
                bytes: None, // leer: Crash zwischen create_new und rename
            })
        );
        assert_eq!(
            got[2],
            ChainItem::Gap(GapRecord::Damaged {
                seq: Some(7),
                bytes: Some(evidence::payload_hash(b"truemmer")),
            })
        );
        assert_eq!(
            got[3],
            ChainItem::Gap(GapRecord::Damaged {
                seq: None,
                bytes: Some(evidence::payload_hash(b"x")),
            })
        );

        let result = chain(&o);
        assert_eq!(result.coverage.gaps.len(), 3);
        assert_eq!(result.coverage.events, 1);
    }

    #[test]
    fn the_mixed_case_is_deterministic() {
        // pre-chain + Luecke + damaged gemischt: zweimal gebaut, ein Root.
        let dir = tempfile::tempdir().unwrap();
        let damaged = dir.path().join("0000000009.json");
        std::fs::write(&damaged, b"halb").unwrap();
        let mut o = outcome(
            vec![event(0, false), event(1, true), event(3, true)],
            vec![2],
        );
        o.damaged = vec![damaged];

        let a = chain(&o);
        let b = chain(&o);
        assert_eq!(a.root, b.root);
        assert_eq!(a.coverage.first_seq, 0);
        assert_eq!(a.coverage.last_seq, 3);
        assert_eq!(a.coverage.pre_chain, 1);
        assert_eq!(a.coverage.gaps.len(), 2);
    }
}
