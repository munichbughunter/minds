//! Das Hook-Binary `minds-witness-hook` (Spec S2 §11, Spec S3 §3): für
//! Tests (`"operation": "test"`) und den Pilotbetrieb auf einem eigenen
//! Git-Server (`"operation": "pilot"`). Konfiguration ([`config`]),
//! Übergabe zwischen pre- und post-receive ([`pending`]), die sichere
//! Warnungsausgabe ([`warn`]), der pre-receive ([`run`]), post-receive,
//! `reconcile` und `install` ([`post`]) sowie die Laufzeitmessung
//! ([`stats`]). Den Produktionsbetrieb regelt S4.

pub mod config;
pub mod pending;
pub mod post;
pub mod run;
pub mod stats;
pub mod warn;
