//! Redaction für Minds: Secrets und PII verlassen die Maschine nicht.
//!
//! Jeder Text, der in den Store geht (Turn-Texte, Tool-Argumente, Tool-Ausgaben),
//! läuft vorher durch die [`RedactionPipeline`]. Der Schutz ist in drei Schichten
//! gebaut, weil eine allein nicht reicht:
//!
//! # Schicht 1 — die Mauer: [`secretfile`]
//!
//! Ist der Pfad, den ein Tool-Call gelesen hat, eine bekannte Zugangsdaten-Datei
//! (`.env`, `id_rsa`, `.aws/credentials`, `*.pem`), wird ihr Inhalt **gar nicht
//! erst gescannt**, sondern vollständig durch [`SECRET_FILE_PLACEHOLDER`]
//! ersetzt. Kein Teilerhalt. Durchgesetzt wird das in `minds-capture` (M5);
//! hier steht nur das Prädikat.
//!
//! # Schicht 2 — das Netz: die Detektoren
//!
//! Für alles, was *nicht* aus einer erkannten Zugangsdaten-Datei stammt:
//!
//! | Detektor | fängt | erkennt woran |
//! |---|---|---|
//! | [`KnownTokenRedactor`] | `AKIA…`, `ghp_…`, `glpat-…`, PEM | Form |
//! | [`HighEntropyRedactor`] | generische base64/hex-Blobs | Form |
//! | [`EmailRedactor`] | E-Mail-Adressen | Form |
//! | [`KeyValueRedactor`] | `DB_PASSWORD=hunter2`, `--token …` | **Schlüsselname** |
//! | [`UrlCredentialRedactor`] | `postgres://user:pw@host` | **Struktur** |
//! | [`ShortFlagRedactor`] | `curl -u user:pass` | **Struktur** |
//! | [`DenyListRedactor`] | aufgezählte Begriffe | Config |
//!
//! [`KeyValueRedactor`], [`UrlCredentialRedactor`] und [`ShortFlagRedactor`]
//! sind der Grund, warum kurze, entropiearme Passwörter nicht durchrutschen:
//! `hunter2` ist als Wert nicht erkennbar — als Wert **hinter `PASSWORD=`**
//! oder **hinter `-u admin:`** schon.
//!
//! # Schicht 3 — die Garantie: [`RedactionPipeline::redact_session`]
//!
//! Ein Detektor, der etwas übersieht, ist ein Erkennungsproblem. Eine Pipeline,
//! die gar nicht lief und trotzdem „redigiert" behauptet, ist ein Leck. Die
//! dritte Schicht erkennt deshalb nichts, sie *garantiert*: Der Lauf verbraucht
//! eine [`Session`](minds_core::Session), scannt **jedes** Textfeld des
//! Envelopes und gibt im Fehlerfall nichts zurück — es gibt dann keine Session
//! zu speichern. Erfolg liefert eine [`RedactedSession`] (ein Typ ohne
//! öffentlichen Konstruktor, der den Nachweis durch das restliche System trägt)
//! plus einen [`RedactionAudit`] aus **Zählern und Ortsangaben, nie Werten**.
//!
//! # Der Normalfall
//!
//! ```no_run
//! # use minds_redact::RedactionConfig;
//! let pipeline = RedactionConfig::default().pipeline().unwrap();
//! let out = pipeline.redact("DB_PASSWORD=hunter2");
//! assert_eq!(out.text, "DB_PASSWORD=[redacted:secret]");
//! ```
//!
//! Kein Netz, keine Datei-I/O, kein Git — reine Funktionen über Strings.

mod assignment;
mod config;
mod pii;
mod pipeline;
mod redactor;
mod secret;
pub mod secretfile;
mod session;

pub use assignment::{
    KeyValueRedactor, ShortFlagRedactor, Tier, UrlCredentialRedactor, has_credential_shape,
};
pub use config::{AllowList, ConfigError, DenyListRedactor, HighEntropyConfig, RedactionConfig};
pub use pii::EmailRedactor;
pub use pipeline::{RedactedText, RedactionPipeline};
pub use redactor::{Category, Finding, Redactor};
pub use secret::{DEFAULT_ENTROPY_BITS, DEFAULT_MIN_LEN, HighEntropyRedactor, KnownTokenRedactor};
pub use secretfile::{SECRET_FILE_PLACEHOLDER, is_secret_file, secret_file_reason};
pub use session::{AuditSite, Field, RedactedSession, RedactionAudit, RedactionError};
