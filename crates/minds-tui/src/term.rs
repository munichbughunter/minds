//! Terminal-Besitz: Raw-Mode und Alternate-Screen als Guard, der beim
//! Verlassen **und** bei einem Panic zurückgibt, was er genommen hat.
//!
//! `ratatui::try_init` installiert bereits einen Panic-Hook, der das
//! Terminal wiederherstellt, bevor der alte Hook die Meldung schreibt. Der
//! Guard hier sorgt für den Normalfall — Rückkehr und `?` — und macht die
//! Wiederherstellung testbar, ohne ein Terminal zu brauchen.

use ratatui::DefaultTerminal;

/// Hält das Terminal und gibt es beim Fallen zurück.
pub struct Guard {
    restore: fn(),
}

impl Guard {
    /// Übernimmt das Terminal (Raw-Mode, Alternate-Screen, Panic-Hook).
    pub fn take() -> std::io::Result<(Self, DefaultTerminal)> {
        let terminal = ratatui::try_init()?;
        Ok((
            Self {
                restore: ratatui::restore,
            },
            terminal,
        ))
    }

    #[cfg(test)]
    fn with(restore: fn()) -> Self {
        Self { restore }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        (self.restore)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static RESTORED: AtomicUsize = AtomicUsize::new(0);

    fn count() {
        RESTORED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn dropping_the_guard_restores_exactly_once() {
        {
            let _guard = Guard::with(count);
        }
        assert_eq!(RESTORED.load(Ordering::SeqCst), 1);
    }
}
