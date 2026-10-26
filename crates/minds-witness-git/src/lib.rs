//! Git-Adapter des minds Server-Witness (Spec S2 v0.4).
//!
//! Das Crate verbindet den I/O-freien Kern `minds-witness-core` mit echter
//! minds-Evidence: das Format `minds-commit-binding-v1` ([`binding`]), die
//! Vertrauensprüfung der Signaturen über die serverseitige Signer-Datei
//! ([`trust`]), die push-weiten Budgets ([`budget`]), die Kandidatenmenge
//! der Bindungen eines Commits ([`candidates`]) und den `SealInspector` mit
//! inhaltsgebundener Abdeckung ([`inspector`]) über einer Objektsicht
//! ([`view`]). Es kennt keine Assurance-Stufen und hängt nicht von
//! `minds-reader` ab (W2).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod binding;
pub mod budget;
pub mod candidates;
pub mod inspector;
pub mod trust;
pub mod view;
