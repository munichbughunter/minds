//! Erfassung von Agent-Sessions — Hooks rein, [`Session`](minds_core::Session)
//! raus.
//!
//! # Warum Hooks und nicht Transkript-Dateien
//!
//! Der naheliegende Weg wäre, die Logdateien der Agents im Nachhinein zu
//! parsen. Drei Gründe sprechen dagegen, und der dritte ist der eigentliche:
//!
//! 1. **Vergänglichkeit.** Claude Code löscht seine Transkripte nach 30 Tagen.
//!    Was nicht rechtzeitig übernommen wurde, ist weg.
//! 2. **Formatvielfalt.** Jeder Agent hat ein eigenes Log. Ein Hook liefert
//!    dagegen ein Event, das wir selbst in Empfang nehmen.
//! 3. **Ordnung über Agents hinweg.** Ein Transkript-Parser sieht immer nur
//!    *ein* Transkript und kann deshalb prinzipiell nicht wissen, dass Codex
//!    zwischen zwei Claude-Turns ein Review geschrieben hat. Ruft dagegen
//!    *jeder* Agent-Hook dasselbe Binary auf, das in *dasselbe* Journal
//!    schreibt, dann sind beide Ereignisse von **einem Beobachter mit einer
//!    Uhr** aufgezeichnet. Die Kante zwischen ihnen ist damit
//!    [`Evidence::Observed`](minds_core::Evidence::Observed) statt
//!    `Inferred` — beobachtet, nicht geraten.
//!
//! Das Transkript wird deshalb nicht überflüssig, es wechselt nur die Rolle:
//! Jedes Hook-Event trägt einen `transcript_path`. Der Hook liefert Zeitpunkt,
//! Reihenfolge und Kausalität; das Transkript liefert den reichen Inhalt
//! (Volltext, Thinking, Token-Zähler), der im Hook-Payload nicht steht. Beide
//! Hälften werden erst beim Checkpoint zusammengeführt.
//!
//! # Die zwei Wege durch dieses Crate
//!
//! ```text
//!   Agent-Hook ──► minds hook ──► Journal          (heiß, fail-open, roh)
//!                                    │
//!   git commit ──► minds capture ────┴──► Adapter ──► Redaction ──► Store
//!                                                     (kalt, fail-closed)
//! ```
//!
//! Die Trennlinie ist Absicht und wiederholt sich im ganzen Entwurf: Der heiße
//! Pfad sammelt **Beweismittel** und darf nichts kosten und nichts riskieren.
//! Der kalte Pfad **deutet** sie und darf teuer und streng sein. Beweismittel
//! sind unwiederbringlich, Deutungen wiederholbar.
//!
//! # Zwei Fehlermodi, die nicht verwechselt werden dürfen
//!
//! - **Redaction ist fail-closed.** Kein Nachweis, keine gespeicherte Session.
//! - **Der Hook ist fail-open.** Er darf die Sitzung des Nutzers unter keinen
//!   Umständen abbrechen und endet immer mit 0.
//!
//! Das ist kein Widerspruch, sondern zwei Achsen: Verfügbarkeit beim Sammeln,
//! Strenge beim Speichern. Der Preis von fail-open sind Lücken — deshalb trägt
//! jedes Event eine lückenlose Sequenznummer, und
//! [`ReadOutcome::gaps`] macht Fehlendes sichtbar. Ehrlich lückenhaft schlägt
//! still vollständig.

mod error;
pub use error::{CaptureError, Result};
pub mod hook_event;

pub mod normalize;
pub use normalize::{EventFacts, ToolFacts};

pub mod secretwall;

pub mod transcript;
pub use transcript::Transcript;

pub mod import;
pub use import::{AgentImport, for_repo};

pub mod match_commits;
pub use match_commits::{CommitInfo, Link, SessionInfo, match_sessions};

pub mod edges;

pub mod adapter;
pub use adapter::{Checkpoint, build, build_one, checkpoint};

pub mod clock;

mod journal;
pub use journal::{EventKind, Journal, JournalEvent, NewEvent, ReadOutcome, SessionKey};
