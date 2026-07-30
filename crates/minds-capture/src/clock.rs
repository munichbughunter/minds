//! Zeitstempel — RFC 3339 in UTC, ohne Datums-Dependency.
//!
//! Der Hook liegt auf dem heißen Pfad: Bei jedem Tool-Call startet unser
//! Prozess neu. Eine Datums-Bibliothek dafür einzuziehen wäre nicht falsch,
//! aber sie kostet Compile-Zeit und Binärgröße für eine einzige Zeile Ausgabe.
//! Die Umrechnung „Sekunden seit Epoch → Kalenderdatum" ist ein geschlossenes,
//! seit Jahrzehnten bekanntes Stück Arithmetik (Howard Hinnants
//! `civil_from_days`); sie hier zu haben ist billiger als jede Alternative und
//! vollständig testbar.
//!
//! # Wo Zeit *nicht* herkommt
//!
//! Nur der Hook liest die Uhr, weil nur er dabei ist, wenn etwas passiert. Der
//! Adapter, der später aus dem Journal eine [`Session`](minds_core::Session)
//! baut, ruft **niemals** [`now_rfc3339`] auf: Er würde damit bei jedem Lauf
//! ein anderes Envelope und damit eine andere `SessionId` erzeugen. Derselbe
//! Journal-Inhalt muss immer denselben Hash ergeben, sonst ist die
//! Content-Adressierung eine Lüge und die Fixture-Tests aus M5 sind nicht
//! schreibbar.

use std::time::{SystemTime, UNIX_EPOCH};

/// Jetzt, als RFC-3339-Zeitstempel in UTC mit Millisekunden
/// (`2026-07-23T09:12:04.512Z`), zusammen mit den Nanosekunden seit Epoch.
///
/// Die zweite Zahl ist der Sortierschlüssel; die Zeichenkette ist für Menschen.
/// Beide stammen aus **einer** Ablesung, damit sie nicht auseinanderlaufen.
///
/// Vor 1970 gibt es hier nichts zu tun: Liegt die Systemuhr davor, liefern wir
/// `0`/Epoch statt zu panicken. Ein Hook darf nie die Sitzung des Nutzers
/// abbrechen — auch nicht wegen einer kaputten Uhr.
pub fn now() -> (String, u64) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
    (rfc3339_from_nanos(nanos), nanos)
}

/// Formatiert Nanosekunden seit Epoch als RFC 3339 in UTC mit Millisekunden.
pub fn rfc3339_from_nanos(nanos: u64) -> String {
    let secs = (nanos / 1_000_000_000) as i64;
    let millis = (nanos % 1_000_000_000) / 1_000_000;

    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);

    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60,
    )
}

/// Parst einen RFC-3339-Zeitstempel in UTC (`2026-07-23T09:12:04.512Z`) zu
/// Sekunden seit Epoch.
///
/// Bewusst tolerant und ohne Datums-Dependency (dieselbe Linie wie der Rest
/// dieses Moduls): Gelesen werden nur Datum und Uhrzeit auf Sekunden;
/// Nachkommastellen, `Z` und ein Zeitzonen-Offset werden ignoriert. Die
/// Transkripte, die das hier füttern, sind ausnahmslos UTC. Passt die Form
/// nicht, ergibt es `None` statt eines geratenen Zeitpunkts.
///
/// Die Umkehrung von [`rfc3339_from_nanos`], für das Zuordnen importierter
/// Sessions zu Commits ([`crate::match_commits`]).
pub fn epoch_seconds_from_rfc3339(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T')?;

    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;

    // Nur `HH:MM:SS` — alles ab dem Punkt, `Z` oder `+`/`-` fällt weg.
    let hms = &time[..time.find(['.', 'Z', '+']).unwrap_or(time.len())];
    let mut t = hms.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let min: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next().unwrap_or("0").parse().ok()?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + min * 60 + sec)
}

/// (Jahr, Monat, Tag) → Tage seit 1970-01-01. Die Umkehrung von
/// [`civil_from_days`], nach Howard Hinnant.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // [0, 11], 0 = März
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Tage seit 1970-01-01 → (Jahr, Monat, Tag), proleptischer gregorianischer
/// Kalender. Nach Howard Hinnant, `chrono`-Algorithmus ohne `chrono`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    // Verschiebt den Nullpunkt auf 0000-03-01, damit der Schalttag ans
    // Jahresende rutscht und keine Sonderfaelle mehr erzeugt.
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], 0 = Maerz
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;

    (y + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_epoch() {
        assert_eq!(rfc3339_from_nanos(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn known_instants() {
        // 2000-03-01, direkt hinter dem Schaltjahr-Sonderfall (2000 ist eines,
        // 1900 waere keines).
        assert_eq!(
            rfc3339_from_nanos(951_868_800 * 1_000_000_000),
            "2000-03-01T00:00:00.000Z"
        );
        // 2024-02-29 — Schalttag.
        assert_eq!(
            rfc3339_from_nanos(1_709_164_800 * 1_000_000_000),
            "2024-02-29T00:00:00.000Z"
        );
        // Sekunden, Minuten, Stunden und Millisekunden gemeinsam.
        assert_eq!(
            rfc3339_from_nanos(1_753_261_924_512_000_000),
            "2025-07-23T09:12:04.512Z"
        );
    }

    #[test]
    fn milliseconds_are_truncated_not_rounded() {
        // 999_999 ns sind 0 ms — Aufrunden wuerde einen Zeitpunkt erzeugen,
        // der noch nicht war.
        assert_eq!(rfc3339_from_nanos(999_999), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_from_nanos(1_500_000), "1970-01-01T00:00:00.001Z");
    }

    #[test]
    fn now_is_after_2026_and_self_consistent() {
        let (text, nanos) = now();
        assert_eq!(text, rfc3339_from_nanos(nanos));
        assert!(text.as_str() > "2026-01-01T00:00:00.000Z", "{text}");
        assert!(text.ends_with('Z'));
    }

    #[test]
    fn rfc3339_parses_back_to_epoch() {
        assert_eq!(
            epoch_seconds_from_rfc3339("1970-01-01T00:00:00.000Z"),
            Some(0)
        );
        assert_eq!(
            epoch_seconds_from_rfc3339("2025-07-23T09:12:04.512Z"),
            Some(1_753_261_924)
        );
        // Schalttag.
        assert_eq!(
            epoch_seconds_from_rfc3339("2024-02-29T00:00:00Z"),
            Some(1_709_164_800)
        );
    }

    #[test]
    fn parse_is_the_inverse_of_format() {
        // Round-Trip ueber ein paar volle Sekunden.
        for secs in [0i64, 951_868_800, 1_709_164_800, 1_784_797_924] {
            let text = rfc3339_from_nanos(secs as u64 * 1_000_000_000);
            assert_eq!(epoch_seconds_from_rfc3339(&text), Some(secs), "{text}");
        }
    }

    #[test]
    fn a_malformed_stamp_is_none_not_a_panic() {
        assert_eq!(epoch_seconds_from_rfc3339(""), None);
        assert_eq!(epoch_seconds_from_rfc3339("kein datum"), None);
        assert_eq!(epoch_seconds_from_rfc3339("2026-13-01T00:00:00Z"), None);
        // Fehlende Sekunden sind erlaubt (auf 0).
        assert!(epoch_seconds_from_rfc3339("2026-07-23T09:12:00Z").is_some());
    }
}
