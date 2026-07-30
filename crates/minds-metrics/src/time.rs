//! Minimale, abhängigkeitsfreie Zeitarithmetik für die Kennzahlen.
//!
//! `minds-core` verbietet sich bewusst eine Datums-Dependency (siehe dortige
//! `lineage`-Doku); diese Crate zieht deshalb keine herein, sondern rechnet die
//! zwei Dinge selbst, die sie braucht: **Sekunden seit Epoch** (für
//! Session-Dauern) und die **Tagesnummer** (für den Streak).
//!
//! Gelesen wird der feste Präfix `YYYY-MM-DDTHH:MM:SS`. Nachkommastellen und
//! Zeitzonen-Offset werden ignoriert (UTC angenommen) — Agent-Zeitstempel sind
//! praktisch immer UTC-`Z`, und für Dauern in Sekunden und Tages-Buckets ist der
//! Sekunden-Präfix genau genug. Passt der Präfix nicht, ist die Antwort `None`
//! statt einer geratenen Zahl.

/// Sekunden seit Unix-Epoch (UTC) aus einem RFC-3339-ähnlichen Zeitstempel.
pub fn epoch_seconds(ts: &str) -> Option<i64> {
    let year: i64 = ts.get(0..4)?.parse().ok()?;
    let month: u32 = ts.get(5..7)?.parse().ok()?;
    let day: u32 = ts.get(8..10)?.parse().ok()?;
    let hour: i64 = ts.get(11..13)?.parse().ok()?;
    let minute: i64 = ts.get(14..16)?.parse().ok()?;
    let second: i64 = ts.get(17..19)?.parse().ok()?;
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Tagesnummer (Tage seit 1970-01-01, UTC) aus dem Datumsanteil.
pub fn day_number(ts: &str) -> Option<i64> {
    let year: i64 = ts.get(0..4)?.parse().ok()?;
    let month: u32 = ts.get(5..7)?.parse().ok()?;
    let day: u32 = ts.get(8..10)?.parse().ok()?;
    Some(days_from_civil(year, month, day))
}

/// Howard Hinnants `days_from_civil`: Tage seit 1970-01-01 für ein
/// proleptisch-gregorianisches Datum. Exakt, ganzzahlig, ohne Tabellen.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let m = m as i64;
    let d = d as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_of_the_unix_epoch_is_zero() {
        assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_seconds("1970-01-01T01:00:00Z"), Some(3_600));
        assert_eq!(epoch_seconds("1970-01-02T00:00:00Z"), Some(86_400));
    }

    #[test]
    fn epoch_ignores_fraction_and_offset() {
        // Nachkommastellen und `Z` dürfen nichts am Sekunden-Präfix ändern.
        assert_eq!(
            epoch_seconds("2026-07-25T09:00:10.512Z"),
            epoch_seconds("2026-07-25T09:00:10")
        );
    }

    #[test]
    fn a_one_hour_session_is_3600_seconds() {
        let start = epoch_seconds("2026-07-25T09:00:00Z").unwrap();
        let end = epoch_seconds("2026-07-25T10:00:00Z").unwrap();
        assert_eq!(end - start, 3_600);
    }

    #[test]
    fn consecutive_days_differ_by_one() {
        let a = day_number("2026-07-24").unwrap();
        let b = day_number("2026-07-25").unwrap();
        assert_eq!(b - a, 1);
        // Monatswechsel.
        let end = day_number("2026-07-31").unwrap();
        let next = day_number("2026-08-01").unwrap();
        assert_eq!(next - end, 1);
    }

    #[test]
    fn a_malformed_stamp_is_none() {
        assert_eq!(epoch_seconds("nope"), None);
        assert_eq!(epoch_seconds(""), None);
        assert_eq!(day_number("2026-13"), None);
    }
}
