//! Das Hook-Binary `minds-witness-hook` (Spec S2 §11): nur für Tests und
//! den Durchstich. Es startet nur mit `"operation": "test"`. Konfiguration
//! ([`config`]), Quittungsablage im Verzeichnis ([`store`]), die sichere
//! Warnungsausgabe auf dem Push-Kanal ([`warn`]) und der Ablauf ([`run`]).
//! Den Produktionsbetrieb regeln S3 und S4.

pub mod config;
pub mod run;
pub mod store;
pub mod warn;
