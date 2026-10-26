//! Git-Adapter des minds Server-Witness (Spec S2 v0.4).
//!
//! Das Crate verbindet den I/O-freien Kern `minds-witness-core` mit echter
//! minds-Evidence: das Format `minds-commit-binding-v1` ([`binding`]), die
//! Vertrauensprüfung der Signaturen über die serverseitige Signer-Datei
//! ([`trust`]) und die push-weiten Budgets ([`budget`]). Es kennt keine
//! Assurance-Stufen und hängt nicht von `minds-reader` ab (W2); was es über
//! Evidence aussagt, ist ein Prüfergebnis für den Kern, keine Stufe.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod binding;
pub mod budget;
pub mod trust;
