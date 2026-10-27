//! Push-weite Budgets eines Hook-Aufrufs (Spec S2 §9): Prozessstarts,
//! gelesene Evidence-Bytes und eine Wanduhr-Deadline. Vorprüfung,
//! `GitObjectAccess` und `GitSealInspector` teilen ein `Budget`. Ein
//! erschöpftes Budget ist immer ein technischer Fehler (im Kern
//! `Internal`), nie ein Urteil über Evidence; die Deadline ist eine harte
//! Obergrenze, kein erwarteter Normalbetrieb.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// Welches Budget erschöpft ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BudgetError {
    /// `max_ssh_keygen_processes` erreicht.
    #[error("process budget exhausted")]
    Processes,
    /// `max_evidence_bytes_per_push` überschritten.
    #[error("byte budget exhausted")]
    Bytes,
    /// Deadline überschritten.
    #[error("deadline exceeded")]
    Deadline,
}

/// Zähler und Grenzen eines Hook-Aufrufs.
#[derive(Debug)]
pub struct Budget {
    max_processes: u32,
    processes: Cell<u32>,
    max_bytes: u64,
    bytes: Cell<u64>,
    /// Objekt-Oids, deren Bytes schon gezählt sind (§9: je Oid einmal,
    /// gemeinsam für Vorprüfung, Kandidatenauswahl und Inspektor).
    charged: RefCell<HashSet<String>>,
    deadline: Option<Instant>,
    /// Laufzeiten der gestarteten Prozesse (Messung, Spec S3 §3.4).
    durations: RefCell<Vec<Duration>>,
    /// Zeitpunkt der Konstruktion (für den Deadline-Verbrauch).
    started: Instant,
}

impl Budget {
    /// Ein Budget mit den Grenzen; `deadline` ist der späteste Zeitpunkt,
    /// `None` heißt ohne Deadline (nur für Tests).
    pub fn new(max_processes: u32, max_bytes: u64, deadline: Option<Instant>) -> Self {
        Self {
            max_processes,
            processes: Cell::new(0),
            max_bytes,
            bytes: Cell::new(0),
            charged: RefCell::new(HashSet::new()),
            deadline,
            durations: RefCell::new(Vec::new()),
            started: Instant::now(),
        }
    }

    /// Ein Budget ohne praktische Grenzen, für Tests ohne Budgetbezug.
    pub fn unlimited() -> Self {
        Self::new(u32::MAX, u64::MAX, None)
    }

    /// Prüft die Deadline.
    pub fn check_deadline(&self) -> Result<(), BudgetError> {
        match self.deadline {
            Some(deadline) if Instant::now() >= deadline => Err(BudgetError::Deadline),
            _ => Ok(()),
        }
    }

    /// Verbraucht einen Prozessstart und liefert die Zeit, die dem Prozess
    /// höchstens bleibt: `timeout`, begrenzt durch die Restzeit bis zur
    /// Deadline.
    pub fn take_process(&self, timeout: Duration) -> Result<Duration, BudgetError> {
        self.check_deadline()?;
        let used = self.processes.get();
        if used >= self.max_processes {
            return Err(BudgetError::Processes);
        }
        self.processes.set(used + 1);
        Ok(match self.deadline {
            Some(deadline) => timeout.min(deadline.saturating_duration_since(Instant::now())),
            None => timeout,
        })
    }

    /// Verbraucht `n` gelesene Bytes.
    pub fn take_bytes(&self, n: u64) -> Result<(), BudgetError> {
        self.check_deadline()?;
        let total = self.bytes.get().saturating_add(n);
        if total > self.max_bytes {
            return Err(BudgetError::Bytes);
        }
        self.bytes.set(total);
        Ok(())
    }

    /// Verbraucht die `n` Bytes des Objekts `oid`, je Oid und Budget
    /// höchstens einmal (Spec S2 §9).
    pub fn take_object_bytes(&self, oid: &str, n: u64) -> Result<(), BudgetError> {
        self.check_deadline()?;
        if self.charged.borrow().contains(oid) {
            return Ok(());
        }
        self.take_bytes(n)?;
        self.charged.borrow_mut().insert(oid.to_owned());
        Ok(())
    }

    /// Hält die Laufzeit eines gestarteten Prozesses fest.
    pub fn record_process_time(&self, d: Duration) {
        self.durations.borrow_mut().push(d);
    }

    /// Laufzeiten aller festgehaltenen Prozesse in Startreihenfolge.
    pub fn process_times(&self) -> Vec<Duration> {
        self.durations.borrow().clone()
    }

    /// Verbrauchter Anteil der Deadline in Promille (`None` ohne Deadline).
    pub fn deadline_used_permille(&self) -> Option<u32> {
        let deadline = self.deadline?;
        let total = deadline
            .saturating_duration_since(self.started)
            .as_millis()
            .max(1);
        let used = self.started.elapsed().as_millis();
        Some(u32::try_from(used * 1000 / total).unwrap_or(u32::MAX))
    }

    /// Restzeit bis zur Deadline, `None` ohne Deadline.
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// Bisher gestartete Prozesse.
    pub fn processes(&self) -> u32 {
        self.processes.get()
    }

    /// Bisher gezählte Bytes.
    pub fn bytes(&self) -> u64 {
        self.bytes.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_bytes_count_once_per_oid() {
        let b = Budget::new(0, 10, None);
        b.take_object_bytes("a", 6).unwrap();
        b.take_object_bytes("a", 6).unwrap();
        assert_eq!(b.bytes(), 6);
        assert_eq!(b.take_object_bytes("b", 6), Err(BudgetError::Bytes));
        assert_eq!(b.bytes(), 6);
    }
}
