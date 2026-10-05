//! Die monotone Uhr des Witness (EA-08a): Kein Zeitstempel des Witness liegt
//! je vor einem früheren — auch nicht über einen Neustart hinweg.
//!
//! # Warum
//!
//! Der Leser verankert eine Epochen-Kette an ihrem Beginn (`started_at` im
//! Observation-Objekt) und vergleicht ihn mit dem Fenster einer Session, die
//! derselbe Witness gestempelt hat. Springt die Wanduhr über einen Neustart
//! zurück, läge der Beginn des neuen Laufs sonst **vor** Ereignissen des
//! alten — und eine Kette, die ganz nach einem Commit liegt, sähe aus, als
//! habe sie vorher begonnen.
//!
//! # Regel
//!
//! Jeder Stempel ist `max(jetzt, ⌊hoch⌋ₘₛ + 1 ms)`; `hoch` ist der höchste je
//! vergebene Stempel. Der Schritt ist eine Millisekunde, weil die Zeitstempel
//! im RFC-3339-Text Millisekunden tragen ([`clock::rfc3339_from_nanos`]): So
//! ist jeder Stempel auch **als Text** später als der vorige, nicht nur in
//! `at_nanos`. Der Preis: Kommen mehr als tausend Events je Sekunde, läuft
//! die Uhr der Wanduhr kurz voraus und holt sie wieder ein, sobald es
//! ruhiger wird. Das Format aller Zeitstempel bleibt dabei unverändert.
//!
//! # Persistenz
//!
//! `evidence/clock` im Witness-Home (0600, atomar ersetzt, mit fsync — dieselbe
//! Disziplin wie die Folder-Zustände) hält eine **Reservierung**: eine
//! Obergrenze aller je vergebenen Stempel, als Dezimalzahl in Nanosekunden.
//! Überschreitet ein Stempel sie, wird sie **vor** dem Append des Events auf
//! `Stempel + 1 s` angehoben und geschrieben. So kostet nicht jedes Event
//! einen weiteren fsync-Zyklus (ein Burst von Beobachtungen ließe die Hooks
//! sonst merklich warten), und nach jedem Absturz und Neustart beginnt die
//! Uhr hinter der Reservierung — also hinter jedem Stempel des alten Laufs.
//! Der Preis: Nach einem schnellen Neustart läuft sie der Wanduhr bis zu
//! einer Sekunde voraus (die sichere Richtung — ein späterer Beginn
//! verankert seltener).
//!
//! Die Datei liegt bewusst nicht unter `evidence/state/`: Dort legt die
//! Epochen-Verwaltung je Agent ein Verzeichnis an, und `clock` ist ein
//! gültiger Agent-Name — ein Hook dieses Namens nähme der Uhr den Platz.
//!
//! Fehlt die Datei, ist es der erste Lauf (oder ein Witness von vor EA-08a);
//! der Writer hebt die Marke dann auf das jüngste Event im Journal und den
//! jüngsten Seal im Ledger an. Ist sie unlesbar oder kaputt, startet der
//! Witness **nicht** — ein stilles Zurücksetzen gäbe genau die
//! Rückwärtssprünge frei, die sie verhindern soll. Zur Reparatur wird sie
//! nie gelöscht, sondern mit einem Wert nicht vor der aktuellen Zeit und
//! nicht vor dem letzten versiegelten Stempel neu geschrieben. Rechte und
//! Eigentümer prüft `validate_tree` beim Laden des Homes.

use std::fs;
use std::path::{Path, PathBuf};

use minds_capture::clock;

use super::{Fallible, atomic};

/// Die Uhr-Datei, relativ zum Witness-Home.
pub(super) const CLOCK_FILE: &str = "evidence/clock";

const NANOS_PER_MILLI: u64 = 1_000_000;

/// So weit reicht eine Reservierung über den Stempel hinaus, der sie
/// auslöste.
const LEASE_NANOS: u64 = 1_000_000_000;

/// Die Uhr eines Witness-Laufs samt ihrer Hochwassermarke.
#[derive(Debug)]
pub(super) struct WitnessClock {
    path: PathBuf,
    /// Der höchste vergebene (oder im Journal gesehene) Stempel — nach dem
    /// Öffnen die Reservierung des vorigen Laufs.
    high_water: u64,
    /// Die persistierte Obergrenze; kein Stempel liegt darüber.
    reserved: u64,
}

impl WitnessClock {
    /// Liest die Marke aus `home`. Eine fehlende Datei ist Null; jede andere
    /// Abweichung ist `corrupt witness clock state`.
    pub(super) fn open(home: &Path) -> Fallible<Self> {
        let path = home.join(CLOCK_FILE);
        let high_water = match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Ok(meta) if meta.is_file() => fs::read(&path)
                .ok()
                .and_then(|bytes| parse(&bytes))
                .ok_or(CORRUPT)?,
            _ => return Err(CORRUPT.into()),
        };
        Ok(Self {
            path,
            high_water,
            reserved: high_water,
        })
    }

    /// Hebt die Marke auf einen Stempel an, der schon im Journal steht
    /// (Wiederanlauf). Nur im Speicher; der nächste [`Self::stamp`]
    /// reserviert darüber hinaus.
    pub(super) fn witnessed(&mut self, at_nanos: u64) {
        self.high_water = self.high_water.max(at_nanos);
    }

    /// Der früheste Stempel, den [`Self::stamp`] jetzt noch vergeben darf.
    fn floor(&self) -> Fallible<u64> {
        (self.high_water / NANOS_PER_MILLI)
            .checked_add(1)
            .and_then(|millis| millis.checked_mul(NANOS_PER_MILLI))
            .ok_or_else(|| "witness clock exhausted".into())
    }

    /// Der Stempel, den die Uhr jetzt vergäbe — ohne ihn zu verbrauchen.
    pub(super) fn peek(&self) -> Fallible<u64> {
        Ok(clock::now().1.max(self.floor()?))
    }

    /// Vergibt den Stempel für `candidate` (die Ablesung des Aufrufers):
    /// unverändert, wenn er auch als Text nach der Marke liegt, sonst
    /// `⌊Marke⌋ₘₛ + 1 ms`. Liegt er über der Reservierung, ist die neue
    /// persistiert, bevor er zurückkommt.
    pub(super) fn stamp(&mut self, candidate: (String, u64)) -> Fallible<(String, u64)> {
        let floor = self.floor()?;
        let at = if candidate.1 >= floor {
            candidate
        } else {
            (clock::rfc3339_from_nanos(floor), floor)
        };
        if at.1 > self.reserved {
            let reserved = at.1.saturating_add(LEASE_NANOS);
            atomic(&self.path, format!("{reserved}\n").as_bytes())?;
            self.reserved = reserved;
        }
        self.high_water = at.1;
        Ok(at)
    }
}

const CORRUPT: &str = "corrupt witness clock state";

/// Genau `<Ziffern>\n` ohne führende Null (außer `0`) — alles andere ist
/// kaputt, nicht „ungefähr richtig".
fn parse(bytes: &[u8]) -> Option<u64> {
    let digits = bytes.strip_suffix(b"\n")?;
    if digits.is_empty()
        || !digits.iter().all(u8::is_ascii_digit)
        || (digits.len() > 1 && digits[0] == b'0')
    {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = NANOS_PER_MILLI;

    /// Ein Home mit `evidence/` und einer Uhr ohne Datei.
    fn fresh() -> (tempfile::TempDir, WitnessClock) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("evidence")).unwrap();
        let clock = WitnessClock::open(dir.path()).unwrap();
        (dir, clock)
    }

    fn at(nanos: u64) -> (String, u64) {
        (clock::rfc3339_from_nanos(nanos), nanos)
    }

    fn file(dir: &tempfile::TempDir) -> String {
        fs::read_to_string(dir.path().join(CLOCK_FILE)).unwrap()
    }

    #[test]
    fn stamps_round_up_to_the_next_millisecond_and_keep_later_candidates() {
        let (_dir, mut clock) = fresh();
        clock.witnessed(5 * MS + 123);
        // Darunter oder in derselben Millisekunde: nächste volle Millisekunde.
        assert_eq!(clock.stamp(at(MS)).unwrap(), at(6 * MS));
        assert_eq!(clock.stamp(at(6 * MS + 5)).unwrap(), at(7 * MS));
        // Genau auf der Untergrenze und darüber: unverändert, Text inklusive.
        assert_eq!(clock.stamp(at(8 * MS)).unwrap(), at(8 * MS));
        let later = ("frei gewählter Text".to_owned(), 20 * MS + 7);
        assert_eq!(clock.stamp(later.clone()).unwrap(), later);
    }

    #[test]
    fn the_reservation_is_written_only_when_a_stamp_exceeds_it() {
        let (dir, mut clock) = fresh();
        assert!(!dir.path().join(CLOCK_FILE).exists());
        clock.stamp(at(1_000 * MS)).unwrap();
        assert_eq!(file(&dir), format!("{}\n", 1_000 * MS + LEASE_NANOS));
        // Innerhalb der Reservierung: kein Schreiben (der Grund für sie).
        fs::write(dir.path().join(CLOCK_FILE), "sentinel").unwrap();
        clock.stamp(at(1_500 * MS)).unwrap();
        clock.stamp(at(2_000 * MS)).unwrap();
        assert_eq!(file(&dir), "sentinel");
        // Darüber: neu reserviert, bevor der Stempel zurückkommt.
        clock.stamp(at(3_000 * MS)).unwrap();
        assert_eq!(file(&dir), format!("{}\n", 3_000 * MS + LEASE_NANOS));
    }

    #[test]
    fn after_a_restart_every_stamp_lies_beyond_the_old_reservation() {
        let (dir, mut clock) = fresh();
        let ahead = clock::now().1 + 3_600_000 * MS;
        clock.stamp(at(ahead)).unwrap();
        drop(clock);
        let mut clock = WitnessClock::open(dir.path()).unwrap();
        let reserved = ahead + LEASE_NANOS;
        assert!(clock.peek().unwrap() > reserved);
        let stamp = clock.stamp(clock::now()).unwrap();
        assert!(stamp.1 > reserved);
        assert!(
            stamp.0.parse::<jiff::Timestamp>().unwrap()
                > clock::rfc3339_from_nanos(reserved)
                    .parse::<jiff::Timestamp>()
                    .unwrap()
        );
    }

    #[test]
    fn an_exhausted_clock_is_an_error_not_an_overflow() {
        let (_dir, mut clock) = fresh();
        clock.witnessed(u64::MAX - 1);
        let err = clock.stamp(at(0)).unwrap_err();
        assert_eq!(err.to_string(), "witness clock exhausted");
        assert!(clock.peek().is_err());
    }

    #[test]
    fn the_clock_file_is_strict() {
        assert_eq!(parse(b"0\n"), Some(0));
        assert_eq!(
            parse(b"1791108000123456789\n"),
            Some(1_791_108_000_123_456_789)
        );
        for bad in [
            &b""[..],
            b"\n",
            b"12",
            b"012\n",
            b" 12\n",
            b"12 \n",
            b"-1\n",
            b"1.5\n",
            b"99999999999999999999\n",
            b"12\n\n",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }
}
