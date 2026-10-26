//! Push-weite Budgets eines Hook-Aufrufs (Spec S2 §9): Prozessstarts,
//! gelesene Evidence-Bytes und eine Wanduhr-Deadline. Vorprüfung,
//! `GitObjectAccess` und `GitSealInspector` teilen ein `Budget`. Ein
//! erschöpftes Budget ist immer ein technischer Fehler (im Kern
//! `Internal`), nie ein Urteil über Evidence; die Deadline ist eine harte
//! Obergrenze, kein erwarteter Normalbetrieb.

use std::cell::Cell;
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
    deadline: Option<Instant>,
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
            deadline,
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
