//! Post-Phase: baut aus jedem `Finding` genau eine signierte Quittung und
//! legt sie im `ReceiptStore` ab. Sie trifft keine Entscheidung und kennt
//! keine Policy; der Adapter ruft sie nur nach `Accept`. Fehler werden zu
//! Zählern und Ereignissen, nie zu einem Fehler nach außen. Signiert wird
//! nur, wenn der Schlüssel zum attestierten Serverzeitpunkt nach jedem
//! Eintrag der Schlüsselliste gültig war; ob er zum tatsächlichen
//! Signierzeitpunkt noch signieren darf, prüft der Adapter.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::events::{LogEvent, LogLevel};
use crate::finding::Finding;
use crate::ids::{HashStr, Timestamp};
use crate::receipt::{
    PublicKeyEntry, ReceiptBody, ReceiptKey, Signer, WitnessIdentity, receipt_id,
};
use crate::traits::{PutResult, ReceiptStore, StoreError};
use crate::{RECEIPT_SCHEMA, RECEIPT_SCHEMA_V2};

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
///
/// Vertrag zur Schlüsselgültigkeit (v1.4): Der Kern prüft, ob `signer` zum
/// Serverzeitpunkt jedes Befunds gültig war, und zwar gegen `signer.valid_from()`
/// und gegen jeden Eintrag desselben Schlüssels in `store.public_keys()`.
/// Ob der Schlüssel *jetzt*, zum tatsächlichen Signierzeitpunkt, noch
/// signieren darf, kann der Kern ohne Uhr nicht wissen: Der Adapter übergibt
/// nur einen `Signer`, dessen Signierberechtigung er unmittelbar vor dem
/// Aufruf geprüft hat. Sonst könnte ein abgelaufener Schlüssel nachträglich
/// neue Quittungen über alte Ereignisse ausstellen.
pub fn post_receive(
    findings: &[Finding],
    identity: &WitnessIdentity,
    signer: &Signer,
    store: &mut dyn ReceiptStore,
) -> PostOutcome {
    post_phase(findings, identity, signer, store, None)
}

/// Wie [`post_receive`], aber jede Quittung trägt `trust` und das Schema
/// `RECEIPT_SCHEMA_V2` (S1 v1.6, Spec S3 §5). `trust` ist der Hash des
/// Trust-Snapshots, mit dem die Pre-Phase geprüft hat; der Kern prüft ihn
/// nicht, er signiert ihn mit.
pub fn post_receive_with_trust(
    findings: &[Finding],
    identity: &WitnessIdentity,
    signer: &Signer,
    store: &mut dyn ReceiptStore,
    trust: &HashStr,
) -> PostOutcome {
    post_phase(findings, identity, signer, store, Some(trust))
}

fn post_phase(
    findings: &[Finding],
    identity: &WitnessIdentity,
    signer: &Signer,
    store: &mut dyn ReceiptStore,
    trust: Option<&HashStr>,
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
            trust,
            &mut counters,
            &mut events,
        )
    }));
    if result.is_err() {
        // Spec S1 v1.4 §8: kein Payload im Ereignis (unkontrollierter Text
        // eines Adapters, kann Objektinhalte tragen); keine Felder.
        events.push(LogEvent::new(
            LogLevel::Error,
            "panic",
            "Die Post-Phase ist mit einem Panic abgebrochen.",
            &[],
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
    trust: Option<&HashStr>,
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

    // Schritt 2a: Schlüsselliste lesen, genau einmal, nach der Registrierung
    // und vor dem ersten `put`. Ohne Liste wird nichts signiert.
    let entries: Vec<PublicKeyEntry> = match store.public_keys() {
        Ok(keys) => keys.into_iter().filter(|k| k.key == identity.key).collect(),
        Err(e) => {
            events.push(LogEvent::new(
                LogLevel::Error,
                "store_failed",
                "Die Schlüsselliste konnte nicht gelesen werden; es wird nichts signiert.",
                &[("error", e.to_string())],
            ));
            counters.failed = total;
            return;
        }
    };

    // Schritt 3: Quittung je Finding.
    for finding in findings {
        let commit = finding.commit.as_str().to_owned();
        let ref_name = finding.push.ref_name.clone();
        if let Some(refusal) = key_refusal(&finding.server.time, signer, &entries) {
            counters.failed = counters.failed.saturating_add(1);
            let mut fields = vec![
                ("commit", commit),
                ("ref", ref_name),
                ("time", finding.server.time.as_str().to_owned()),
                ("valid_from", refusal.valid_from.as_str().to_owned()),
                ("reason", refusal.reason.to_owned()),
            ];
            if let Some(until) = refusal.valid_until {
                fields.push(("valid_until", until.as_str().to_owned()));
            }
            events.push(LogEvent::new(
                LogLevel::Error,
                "key_not_valid_at",
                "Der Signierschlüssel war zum Serverzeitpunkt des Befunds nicht gültig; der Befund wird nicht signiert.",
                &fields,
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
            schema: if trust.is_some() {
                RECEIPT_SCHEMA_V2
            } else {
                RECEIPT_SCHEMA
            }
            .to_owned(),
            seal: finding.seal.clone(),
            server: finding.server.clone(),
            trust: trust.cloned(),
            witness: identity.clone(),
        };
        // Spec S3 §5: die Schemaregel gilt auch beim Bau.
        if body.check().is_err() {
            counters.failed = counters.failed.saturating_add(1);
            events.push(LogEvent::new(
                LogLevel::Error,
                "receipt_schema_invalid",
                "Der Quittungsinhalt verletzt die Schemaregel; der Befund wird nicht signiert.",
                &[("commit", commit), ("ref", ref_name)],
            ));
            continue;
        }
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

/// Warum ein Befund nicht signiert wird, mit der Grenze, die gegriffen hat.
struct Refusal<'a> {
    /// `before_valid_from`, `not_listed` oder `revoked`.
    reason: &'static str,
    /// Maßgebliches `valid_from`: das des Eintrags bzw. Signers, der den
    /// Zeitpunkt ausschließt; sonst das des Signers.
    valid_from: &'a Timestamp,
    /// Maßgebliches `valid_until`, nur bei `revoked`.
    valid_until: Option<&'a Timestamp>,
}

/// Grund, aus dem ein Befund zum Serverzeitpunkt `time` nicht signiert wird,
/// oder `None`. Bewusst streng: Jeder Eintrag des Schlüssels muss `time`
/// zulassen. Widersprechen sich Einträge (etwa einer offen, einer mit
/// `valid_until`), gewinnt der einschränkendste; der Kern wählt nie den
/// günstigsten. Damit verifiziert jede ausgestellte Quittung gegen die Liste,
/// wie sie beim Signieren war (`verify_receipt` nimmt den ersten passenden
/// Eintrag, und hier passt jeder). Ein einmal zurückgezogener Schlüssel wird
/// dadurch nie wieder signierfähig, auch nicht durch einen weiteren Eintrag;
/// Reaktivierung heißt neuer Schlüssel.
fn key_refusal<'a>(
    time: &Timestamp,
    signer: &'a Signer,
    entries: &'a [PublicKeyEntry],
) -> Option<Refusal<'a>> {
    if time < signer.valid_from() {
        return Some(Refusal {
            reason: "before_valid_from",
            valid_from: signer.valid_from(),
            valid_until: None,
        });
    }
    if let Some(e) = entries.iter().find(|e| *time < e.valid_from) {
        return Some(Refusal {
            reason: "before_valid_from",
            valid_from: &e.valid_from,
            valid_until: None,
        });
    }
    if entries.is_empty() {
        return Some(Refusal {
            reason: "not_listed",
            valid_from: signer.valid_from(),
            valid_until: None,
        });
    }
    entries.iter().find_map(|e| {
        e.valid_until
            .as_ref()
            .filter(|u| time >= *u)
            .map(|u| Refusal {
                reason: "revoked",
                valid_from: &e.valid_from,
                valid_until: Some(u),
            })
    })
}
