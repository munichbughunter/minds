//! B6 · Prüfungen nicht im Event-Loop: ein Worker-Thread prüft, das
//! Ergebnis kommt über einen Kanal und wird beim nächsten Tick übernommen.

use super::*;
use std::time::{Duration, Instant};

/// Eine Prüfung, die Zeit braucht — und nur eigene Daten hält (Send).
struct Slow(CommitVerify);

impl crate::Background for Slow {
    fn verify(&mut self, _commit: CommitId) -> Result<CommitVerify, String> {
        std::thread::sleep(Duration::from_millis(50));
        Ok(self.0.clone())
    }
}

#[test]
fn b6_the_check_runs_in_a_worker_and_the_ui_stays_responsive() {
    let (_dir, repo) = repo();
    let mut app = app(&repo);
    app.reduce(Action::TabTo(2));
    let mut worker = crate::app::Worker::spawn(Box::new(Slow(witnessed(false))));
    assert!(
        app.send_check(&mut worker),
        "the shown commit goes to the worker"
    );
    // Während die Prüfung läuft: „checking", und die Oberfläche reagiert.
    assert_eq!(
        app.verify.as_ref().unwrap().overall(),
        crate::verify::Overall::Checking
    );
    assert!(!app.send_check(&mut worker), "one check at a time");
    app.reduce(Action::Down);
    assert_eq!(app.verify.as_ref().unwrap().cursor, 1);
    // Der nächste Tick übernimmt das Ergebnis.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.take_checks(&mut worker) {
        assert!(Instant::now() < deadline, "the worker answers");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(app.verify.as_ref().unwrap().cli.is_some());
    assert_ne!(
        app.verify.as_ref().unwrap().overall(),
        crate::verify::Overall::Checking
    );
}

/// Ein ASCII-Spinner — er dreht sich je Tick.
#[test]
fn b6_the_spinner_is_ascii_and_turns() {
    let frames: Vec<char> = (0..5).map(crate::app::spinner).collect();
    assert_eq!(frames, ['|', '/', '-', '\\', '|']);
}

/// B6: „Das bestehende Drosseln (`VERIFY_EVERY`) bleibt" — auch mit Worker:
/// Ein Neuladen stößt eine Prüfung erst nach `VERIFY_EVERY` an, ein vom
/// Nutzer verlangter Check sofort; der Worker nimmt nur eine zur Zeit.
#[test]
fn b6_the_throttle_stays_with_the_worker() {
    let (_dir, repo) = repo();
    let mut app = app(&repo);
    app.reduce(Action::TabTo(2));
    let every = crate::app::VERIFY_EVERY;
    let state = app.verify.as_mut().unwrap();
    state.pending = true;
    state.urgent = false;
    assert!(!state.due(every / 2, every), "throttled after a reload");
    assert!(state.due(every, every), "due once VERIFY_EVERY has passed");
    state.urgent = true;
    assert!(
        state.due(Duration::ZERO, every),
        "a user request is not throttled"
    );
    let mut worker = crate::app::Worker::spawn(Box::new(Slow(witnessed(false))));
    assert!(app.send_check(&mut worker));
    assert!(
        !app.verify.as_ref().unwrap().urgent,
        "the request is consumed"
    );
    assert!(!app.send_check(&mut worker), "one check at a time");
}
