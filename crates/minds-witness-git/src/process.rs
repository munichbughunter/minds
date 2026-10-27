//! Prozess-Identität ohne `unsafe` (Spec S3 §3.1, §4.2): PID plus
//! Startzeit. Eine PID allein kann wiederverwendet werden; erst mit der
//! Startzeit ist „derselbe Prozess“ belegbar.
//!
//! - Linux: Feld 22 von `/proc/<pid>/stat` (Startzeit in Ticks seit Boot).
//! - macOS und andere Unix-Systeme: `/bin/ps -o lstart= -p <pid>` mit
//!   `LC_ALL=C` und `TZ=UTC`, als undurchsichtige Zeichenkette verglichen,
//!   Timeout 2 s.
//! - Sonst `None`.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Ob ein Prozess mit dieser Identität lebt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// Läuft, mit derselben Startzeit.
    Alive,
    /// Läuft nicht mehr, oder die PID gehört einem anderen Prozess.
    Dead,
    /// Nicht feststellbar; Aufrufer behandeln das wie `Alive`.
    Unknown,
}

/// Die eigene Prozesskennung.
pub fn own_pid() -> u32 {
    std::process::id()
}

/// Geltungsbereich einer PID: auf Linux Boot-Id und PID-Namensraum dieses
/// Prozesses, sonst leer. PIDs aus einem anderen Bereich (anderer Container,
/// anderer Rechner, Neustart) sind hier nicht prüfbar.
pub fn scope() -> String {
    if cfg!(target_os = "linux") {
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map(|b| b.trim().to_owned())
            .unwrap_or_default();
        let ns = std::fs::read_link("/proc/self/ns/pid")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        return format!("{boot}/{ns}");
    }
    String::new()
}

/// Startzeit des Prozesses `pid` samt Geltungsbereich (`<scope>|<zeit>`),
/// sofern feststellbar; `None` heißt nicht feststellbar oder Prozess nicht
/// vorhanden.
pub fn start_time(pid: u32) -> Option<String> {
    raw_start(pid).map(|t| format!("{}|{t}", scope()))
}

/// Ob eine Startangabe aus diesem Geltungsbereich stammt.
pub fn same_scope(start: &str) -> bool {
    start.split_once('|').is_some_and(|(s, _)| s == scope())
}

/// Ob eine Startangabe nachweislich aus einem früheren Systemstart dieses
/// Rechners stammt: andere Boot-Id **und** die zugehörige Datei wurde vor
/// dem aktuellen Systemstart geschrieben. Alle Prozesse eines früheren
/// Starts sind tot. Eine andere Boot-Id mit jüngerer Datei kann ein anderer
/// Rechner mit gemeinsamem Verzeichnis sein und bleibt nicht prüfbar.
pub fn from_previous_boot(start: &str, written: std::time::SystemTime) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let Some((their, _)) = start.split_once('|') else {
        return false;
    };
    let ours = scope();
    let boot = |s: &str| s.split_once('/').map(|(b, _)| b.to_owned());
    let (Some(their), Some(ours)) = (boot(their), boot(&ours)) else {
        return false;
    };
    if their.is_empty() || ours.is_empty() || their == ours {
        return false;
    }
    boot_time().is_some_and(|b| written < b)
}

/// Zeitpunkt des aktuellen Systemstarts (`btime` aus `/proc/stat`).
fn boot_time() -> Option<std::time::SystemTime> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let secs: u64 = stat
        .lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    Some(std::time::UNIX_EPOCH + Duration::from_secs(secs))
}

fn raw_start(pid: u32) -> Option<String> {
    if cfg!(target_os = "linux") {
        return linux_start(pid);
    }
    if cfg!(unix) {
        return ps_start(pid);
    }
    None
}

/// Ob der Prozess `pid` mit der Startzeit `start` lebt. Ohne bekannte
/// Startzeit (`None`) ist das Ergebnis nie `Dead` allein wegen der PID.
pub fn liveness(pid: u32, start: Option<&str>) -> Liveness {
    // Eine PID aus einem anderen Geltungsbereich ist hier nicht prüfbar.
    if let Some(s) = start
        && !same_scope(s)
    {
        return Liveness::Unknown;
    }
    let start = start.map(|s| s.split_once('|').map_or(s, |(_, t)| t));
    if cfg!(target_os = "linux") {
        let exists = std::path::Path::new(&format!("/proc/{pid}")).exists();
        if !exists {
            return Liveness::Dead;
        }
        return match (start, linux_start(pid)) {
            (Some(a), Some(b)) if a == b => Liveness::Alive,
            (Some(_), Some(_)) => Liveness::Dead,
            _ => Liveness::Unknown,
        };
    }
    if cfg!(unix) {
        return match (start, ps_start(pid)) {
            (_, None) if ps_says_absent(pid) => Liveness::Dead,
            (Some(a), Some(b)) if a == b => Liveness::Alive,
            (Some(_), Some(_)) => Liveness::Dead,
            _ => Liveness::Unknown,
        };
    }
    Liveness::Unknown
}

fn linux_start(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` steht in Klammern und kann Leerzeichen enthalten: ab der
    // letzten `)` zählen. Danach Feld 3 (`state`) … Feld 22 (`starttime`).
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19).map(str::to_owned)
}

fn run_ps(pid: u32) -> Option<(bool, String)> {
    let mut child = Command::new("/bin/ps")
        .env_clear()
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some((status.success(), out.trim().to_owned()))
}

fn ps_start(pid: u32) -> Option<String> {
    match run_ps(pid)? {
        (true, s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn ps_says_absent(pid: u32) -> bool {
    // `ps -p` endet ohne Treffer mit Exit ≠ 0 und leerer Ausgabe; ein
    // Fehler beim Start von `ps` heißt dagegen „unbekannt“.
    matches!(run_ps(pid), Some((false, s)) if s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_alive_and_foreign_start_is_dead() {
        let me = own_pid();
        let start = start_time(me);
        if cfg!(unix) {
            let start = start.expect("eigene Startzeit");
            assert_eq!(liveness(me, Some(&start)), Liveness::Alive);
            let other = format!("{}|0", scope());
            assert_eq!(liveness(me, Some(&other)), Liveness::Dead);
            // Fremder Geltungsbereich: nicht prüfbar, nie „tot“.
            assert_eq!(liveness(me, Some("fremd|1")), Liveness::Unknown);
        }
        // Eine PID, die es nicht gibt (über dem Linux-Maximum).
        let x = format!("{}|x", scope());
        assert_eq!(liveness(4_194_304 + 7, Some(&x)), Liveness::Dead);
        if cfg!(target_os = "linux") {
            let ns = scope().split_once('/').map(|(_, n)| n.to_owned()).unwrap();
            let old = format!("00000000-0000-0000-0000-000000000000/{ns}|1");
            // Früherer Start: vor dem aktuellen Systemstart geschrieben.
            assert!(from_previous_boot(&old, std::time::UNIX_EPOCH));
            // Andere Boot-Id, aber jünger: anderer Rechner möglich.
            assert!(!from_previous_boot(&old, std::time::SystemTime::now()));
            // Eigener Start: nie.
            assert!(!from_previous_boot(
                &format!("{}|1", scope()),
                std::time::UNIX_EPOCH
            ));
        }
    }
}
