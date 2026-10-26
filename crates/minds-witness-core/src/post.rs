//! Post-Phase: baut aus jedem `Finding` genau eine signierte Quittung und
//! legt sie im `ReceiptStore` ab. Sie trifft keine Entscheidung und kennt
//! keine Policy; der Adapter ruft sie nur nach `Accept`. Fehler werden zu
//! Zählern und Ereignissen, nie zu einem Fehler nach außen.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::RECEIPT_SCHEMA;
use crate::events::{LogEvent, LogLevel};
use crate::finding::Finding;
use crate::receipt::{
    PublicKeyEntry, ReceiptBody, ReceiptKey, Signer, WitnessIdentity, receipt_id,
};
use crate::traits::{PutResult, ReceiptStore, StoreError};

/// Ergebnis der Post-Phase. Es gilt immer
/// `stored + skipped_existing + failed == findings.len()`.
#[derive(Debug)]
pub struct PostOutcome {
    /// Neu abgelegte Quittungen.
    pub stored: u32,
    /// Byteidentisch bereits vorhandene Quittungen.
    pub skipped_existing: u32,
    /// Findings ohne abgelegte Quittung.
    pub failed: u32,
    /// Ereignisse in Entstehungsreihenfolge.
    pub events: Vec<LogEvent>,
}

/// Zähler, die vor dem geschützten Block angelegt und durchgereicht werden,
/// damit ein Panic den bis dahin erreichten Stand nicht verliert.
#[derive(Default)]
struct Counters {
    stored: u32,
    skipped_existing: u32,
    failed: u32,
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Baut, signiert und speichert die Quittungen (Abschnitt 8).
pub fn post_receive(
    findings: &[Finding],
    identity: &WitnessIdentity,
    signer: &Signer,
    store: &mut dyn ReceiptStore,
) -> PostOutcome {
    let total = len_u32(findings.len());
    let mut counters = Counters::default();
    let mut events = Vec::new();
    let result = catch_unwind(AssertUnwindSafe(|| {
        run(
            findings,
            identity,
            signer,
            store,
            &mut counters,
            &mut events,
        )
    }));
    if let Err(payload) = result {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unbekannt".to_owned());
        // SPEC-GAP: Abschnitt 8 nennt für `panic` kein Feld; das Ereignis
        // trägt wie in der Pre-Phase `message` (best effort aus dem Payload),
        // damit beide Phasen dasselbe Ereignisformat haben.
        events.push(LogEvent::new(
            LogLevel::Error,
            "panic",
            "Die Post-Phase ist mit einem Panic abgebrochen.",
            &[("message", message)],
        ));
        counters.failed = total
            .saturating_sub(counters.stored)
            .saturating_sub(counters.skipped_existing);
    }
    PostOutcome {
        stored: counters.stored,
        skipped_existing: counters.skipped_existing,
        failed: counters.failed,
        events,
    }
}

fn run(
    findings: &[Finding],
    identity: &WitnessIdentity,
    signer: &Signer,
    store: &mut dyn ReceiptStore,
    counters: &mut Counters,
    events: &mut Vec<LogEvent>,
) {
    let total = len_u32(findings.len());

    // Schritt 1: Identität prüfen.
    if identity.key != signer.public_key() {
        events.push(LogEvent::new(
            LogLevel::Error,
            "identity_mismatch",
            "Der Schlüssel der Witness-Identität passt nicht zum Signierschlüssel.",
            &[],
        ));
        counters.failed = total;
        return;
    }

    // Schritt 2: Schlüssel registrieren, genau einmal vor dem ersten `put`.
    if findings.is_empty() {
        return;
    }
    let entry = PublicKeyEntry {
        key: identity.key.clone(),
        valid_from: signer.valid_from().clone(),
        valid_until: None,
    };
    if let Err(e) = store.register_public_key(&entry) {
        events.push(LogEvent::new(
            LogLevel::Error,
            "store_failed",
            "Der Signierschlüssel konnte nicht registriert werden.",
            &[("error", e.to_string())],
        ));
        counters.failed = total;
        return;
    }

    // Schritt 3: Quittung je Finding.
    for finding in findings {
        let commit = finding.commit.as_str().to_owned();
        let ref_name = finding.push.ref_name.clone();
        if finding.server.time < *signer.valid_from() {
            counters.failed = counters.failed.saturating_add(1);
            events.push(LogEvent::new(
                LogLevel::Error,
                "key_not_valid_at",
                "Der Befund liegt vor dem Gültigkeitsbeginn des Signierschlüssels und wird nicht signiert.",
                &[
                    ("commit", commit),
                    ("ref", ref_name),
                    ("time", finding.server.time.as_str().to_owned()),
                    ("valid_from", signer.valid_from().as_str().to_owned()),
                ],
            ));
            continue;
        }
        let body = ReceiptBody {
            commit: finding.commit.clone(),
            coverage: finding.coverage.clone(),
            policy: finding.policy.clone(),
            push: finding.push.clone(),
            pusher: finding.pusher.clone(),
            repo: finding.repo.clone(),
            schema: RECEIPT_SCHEMA.to_owned(),
            seal: finding.seal.clone(),
            server: finding.server.clone(),
            witness: identity.clone(),
        };
        let id = receipt_id(&body);
        let receipt_bytes = body.sign(signer).to_bytes();
        let key = ReceiptKey {
            commit: finding.commit.clone(),
            id,
        };
        let receipt_id_str = key.id.as_str().to_owned();
        match store.put(&key, &receipt_bytes) {
            Ok(PutResult::Stored) => {
                counters.stored = counters.stored.saturating_add(1);
                events.push(LogEvent::new(
                    LogLevel::Info,
                    "receipt_stored",
                    "Quittung abgelegt.",
                    &[
                        ("commit", commit),
                        ("ref", ref_name),
                        ("receipt_id", receipt_id_str),
                    ],
                ));
            }
            Ok(PutResult::AlreadyExists) => {
                counters.skipped_existing = counters.skipped_existing.saturating_add(1);
                events.push(LogEvent::new(
                    LogLevel::Info,
                    "receipt_exists",
                    "Die Quittung war bereits byteidentisch abgelegt.",
                    &[
                        ("commit", commit),
                        ("ref", ref_name),
                        ("receipt_id", receipt_id_str),
                    ],
                ));
            }
            Err(StoreError::Conflict(_)) => {
                counters.failed = counters.failed.saturating_add(1);
                events.push(LogEvent::new(
                    LogLevel::Error,
                    "store_conflict",
                    "Unter der Receipt-Id liegt bereits ein abweichender Eintrag.",
                    &[("commit", commit), ("receipt_id", receipt_id_str)],
                ));
            }
            Err(e @ StoreError::Internal(_)) => {
                counters.failed = counters.failed.saturating_add(1);
                events.push(LogEvent::new(
                    LogLevel::Error,
                    "store_failed",
                    "Die Quittung konnte nicht abgelegt werden.",
                    &[("commit", commit), ("error", e.to_string())],
                ));
            }
        }
    }
}
