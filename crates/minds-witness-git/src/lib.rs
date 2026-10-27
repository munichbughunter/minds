//! Git-Adapter des minds Server-Witness (Spec S2 v0.4.1).
//!
//! Das Crate verbindet den I/O-freien Kern `minds-witness-core` mit echter
//! minds-Evidence: das Format `minds-commit-binding-v1` ([`binding`]), die
//! Vertrauensprüfung der Signaturen über die serverseitige Signer-Datei
//! ([`trust`]), die push-weiten Budgets ([`budget`]), die Kandidatenmenge
//! der Bindungen eines Commits ([`candidates`]) und den `SealInspector` mit
//! inhaltsgebundener Abdeckung ([`inspector`]) über einer Objektsicht
//! ([`view`]). Es kennt keine Assurance-Stufen und hängt nicht von
//! `minds-reader` ab (W2).
//!
//! [`receipt_store`] legt Quittungen, Schlüsselliste und Trust-Snapshots
//! create-only unter `refs/minds/witness/` ab (Spec S3 §4).
//!
//! [`hook`] enthält das Test-Hook-Binary `minds-witness-hook` (§11) mit
//! der sicheren Warnungsausgabe für den Observe-Modus (ADR-PRODUCT-001).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod access;
pub mod binding;
pub mod budget;
pub mod candidates;
pub mod fsck;
pub mod guard;
pub mod hook;
pub mod inspector;
pub mod limits;
pub mod process;
pub mod receipt_store;
pub mod trust;
pub mod view;
