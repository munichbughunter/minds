//! Der aktive Intent des Witness (EA-14): Er setzt den Anker an den Anfang
//! jeder neuen Session und trägt einen Wechsel in laufende Sessions ein.
//!
//! Der Witness prüft den Anker (Textform, Id, Redaction-Policy, Form der
//! Signatur), nicht die Signatur selbst — die prüft der Leser unter
//! `minds-intent` (EA-15). Was er ins Journal schreibt, ist ein
//! synthetisches Event `minds.intent`, beobachtet und verkettet wie jedes
//! andere.
//!
//! # Wer aktiviert
//!
//! Nur die Host-Seite, über den Steuer-Socket im Zustandsverzeichnis
//! (`control/control.sock`, eigenes 0700-Verzeichnis, nur Verbindungen des
//! Witness-Nutzers). Auf dem Socket der Agent-Seite wird `IntentActivate`
//! abgewiesen: Die Signatur belegt, wer eine Anforderung freigab, nicht dass
//! sie für diese Session gilt — wählte der Agent den Anker, wäre die
//! Bindung seine Behauptung. Der Anker `-` hebt die Aktivierung auf; das
//! wirkt für neue Sessions, gebundene behalten ihren Anker.
//!
//! # Wann ein Intent-Event entsteht
//!
//! Immer **unmittelbar vor einem Hook-Event** der Session, nie zum Zeitpunkt
//! der Aktivierung:
//!
//! - Die Session ist neu und ihr erstes Event ein `SessionStart`: der aktive
//!   Intent mit `opens_session` — das erste Glied ihrer Kette (nur
//!   `source` `startup` oder `clear`; `resume`/`compact` setzen eine laufende
//!   Session fort). Beginnt eine
//!   neue Session mit einem anderen Event, wird er ohne diesen Beleg
//!   eingetragen.
//! - Die Session ist bekannt, und der zuletzt in sie eingetragene Anker ist
//!   nicht der aktive: ein Wechsel-Event (ohne `opens_session`).
//!
//! So entsteht nichts für Sessions, die längst beendet oder abgestürzt sind
//! (keine Epochen nur aus Witness-Events), ein Neustart verliert keinen
//! Wechsel, und ein Absturz zwischen Event und Buchführung wiederholt
//! höchstens denselben Anker — kein Wechsel. Scheitert das Anhängen des
//! Hook-Events selbst (Uhr, I/O), steht das Intent-Event ohne Nachfolger am
//! Ende des Bereichs — auch das wiederholt beim nächsten Event höchstens
//! denselben Anker.
//!
//! # Zustand (W1)
//!
//! Beides liegt im Zustandsverzeichnis des Witness: der aktive Intent in
//! `evidence/intent.json`, je Session der zuletzt eingetragene Anker in
//! `evidence/folders/<digest>.intent` (`b3-…` oder `-`). Neu ist eine
//! Session ohne diesen Eintrag, ohne versiegelte Epoche und ohne Glied in
//! der laufenden; ein unlesbarer Eintrag zählt als „bekannt, ungebunden" —
//! nie als neu, also nie als Beleg für einen Kettenanfang. Die Einträge
//! wachsen wie die Folds daneben mit jeder je gesehenen Session; den
//! Speicher-Cache räumt der Checkpoint mit den Folds auf.

use std::path::{Path, PathBuf};

use minds_capture::{EventKind, NewEvent, SessionKey};
use minds_core::ContentHash;
use minds_core::intent_anchor::{INTENT_EVENT_KIND, IntentEventPayload};

use super::{Config, Fallible, Writer, atomic, folder_path, log, read_private};

/// Der reservierte Präfix synthetischer Witness-Events. Ein Hook-Event der
/// Agent-Seite mit diesem Namen wird nie angenommen — sonst könnte der Agent
/// sich selbst einen Intent „bezeugen" lassen.
pub(super) const RESERVED_KIND_PREFIX: &str = "minds.";

/// Ort des aktiven Intents im Zustandsverzeichnis.
const ACTIVE_FILE: &str = "evidence/intent.json";

/// Wert eines Session-Eintrags ohne Anker.
const NONE_RECORDED: &str = "-";

/// Der Anker in `IntentActivate`, der die Aktivierung aufhebt.
pub(super) const CLEAR_INTENT: &str = "-";

/// Die `source`-Werte eines `SessionStart`, die eine Session wirklich
/// beginnen. `resume` und `compact` tragen dieselbe `session_id` wie die
/// laufende Session — dort hat die Arbeit längst begonnen.
const STARTING_SOURCES: &[&str] = &["startup", "clear"];

/// Ob dieses Hook-Event den Anfang einer Session belegt: ein `SessionStart`
/// mit einem `source` aus [`STARTING_SOURCES`]. Fehlt `source` oder ist es
/// unbekannt: kein Beleg (fail-closed).
pub(super) fn starts_session(event: &NewEvent) -> bool {
    #[derive(serde::Deserialize)]
    struct Start {
        source: Option<String>,
    }
    event.kind == EventKind::SessionStart
        && serde_json::from_str::<Start>(event.payload.get())
            .ok()
            .and_then(|start| start.source)
            .is_some_and(|source| STARTING_SOURCES.contains(&source.as_str()))
}

fn active_path(home: &Path) -> PathBuf {
    home.join(ACTIVE_FILE)
}

fn record_path(home: &Path, key: &SessionKey) -> PathBuf {
    folder_path(home, key).with_extension("intent")
}

/// Ob die Policy den Ankertext unverändert lässt. Der Anker landet im
/// Journal und in jedem Seal-Bereich: Findet die Policy darin etwas, ist er
/// nicht der, den `minds intent` gebaut hätte (EA-15 baut ihn über
/// `redact_intent`). Ohne nutzbare Policy: nein.
pub(super) fn policy_accepts(config: &Config, anchor: &str) -> bool {
    match config.policy().pipeline() {
        // Dieselbe Prüfung, mit der `redact_intent` den Anker baut — Bauen und
        // Annehmen urteilen nie verschieden.
        Ok(pipeline) => minds_core::intent_anchor::IntentAnchor::parse(anchor)
            .ok()
            .and_then(|parsed| pipeline.check_intent_anchor(&parsed).ok())
            .is_some_and(|text| text == anchor),
        Err(_) => false,
    }
}

/// Liest den aktiven Intent. Fehlt er, ist nichts aktiv. Ist er unlesbar,
/// ungültig oder nach der heutigen Policy nicht mehr zulässig, ist
/// ebenfalls nichts aktiv — das ist die schwächere Aussage („unbound"),
/// nie eine falsche Bindung — und das Log sagt es.
pub(super) fn load(home: &Path, config: &Config) -> Option<IntentEventPayload> {
    // Ohne Symlinks zu folgen: Ein hängender Symlink ist nicht „nichts
    // aktiv", sondern landet unten als unlesbar — mit Log.
    if matches!(
        std::fs::symlink_metadata(active_path(home)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    ) {
        return None;
    }
    let parsed = read_private(home, ACTIVE_FILE)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<IntentEventPayload>(&bytes).ok())
        .filter(|payload| {
            payload.check().is_ok()
                && !payload.opens_session
                && policy_accepts(config, &payload.anchor)
        });
    if parsed.is_none() {
        log(
            home,
            "active intent unreadable or refused; no intent active",
        );
    }
    parsed
}

impl Writer {
    /// `IntentActivate`: Anker prüfen und persistieren. In die Sessions
    /// gelangt er erst mit ihrem nächsten Hook-Event ([`Self::bind_intent`]).
    /// Außen ein fester Ablehnungsgrund für die Agent-Seite, innen (`Err`)
    /// ausschließlich Fehler des eigenen Speichers.
    pub(super) fn activate_intent(
        &mut self,
        anchor: &str,
        signature: Option<String>,
    ) -> Fallible<Result<String, &'static str>> {
        let payload = match IntentEventPayload::new(anchor, signature) {
            Ok(payload) => payload,
            Err(minds_core::intent_anchor::IntentAnchorError::Signature) => {
                return Ok(Err("invalid intent signature"));
            }
            Err(_) => return Ok(Err("invalid intent anchor")),
        };
        if !policy_accepts(&self.config, &payload.anchor) {
            return Ok(Err("intent anchor refused by the redaction policy"));
        }
        // Nur ein belegter Anker (`intent_proof`): im festgehaltenen Store,
        // Snapshot bereinigt und passend, bei Dateien die Blob-Id genau
        // dieses Snapshots. Sonst wäre seine Id in jeder gesyncten Session
        // ein mögliches Orakel über unredigiertes Material.
        let proof = (|| -> Fallible<Result<(), &'static str>> {
            let repo = self.pinned_repo()?;
            let store = self.open_pinned_store(&repo)?;
            let pipeline = self.config.policy().pipeline()?;
            Ok(crate::intent_proof::proven(
                store.as_ref(),
                &repo,
                &pipeline,
                &payload.anchor_id,
            ))
        })();
        match proof {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => return Ok(Err(reason)),
            Err(_) => return Ok(Err("pinned store unavailable")),
        }
        let id = payload.anchor_id.clone();
        if self.intent.as_ref() == Some(&payload) {
            return Ok(Ok(format!("intent unchanged {id}")));
        }
        atomic(&active_path(&self.home), &serde_json::to_vec(&payload)?)?;
        self.intent = Some(payload);
        Ok(Ok(format!("intent active {id}")))
    }

    /// Nach einem Neustart: Der geladene Intent muss denselben Beleg tragen
    /// wie bei der Aktivierung (`intent_proof`) — gegen den heutigen Store
    /// und die heutige Policy. Sonst ist nichts aktiv; das Log sagt es.
    pub(super) fn reprove_intent(&mut self) {
        let Some(payload) = self.intent.as_ref() else {
            return;
        };
        let id = payload.anchor_id.clone();
        let proof = (|| -> Fallible<Result<(), &'static str>> {
            let repo = self.pinned_repo()?;
            let store = self.open_pinned_store(&repo)?;
            let pipeline = self.config.policy().pipeline()?;
            Ok(crate::intent_proof::proven(
                store.as_ref(),
                &repo,
                &pipeline,
                &id,
            ))
        })();
        if !matches!(proof, Ok(Ok(()))) {
            log(
                &self.home,
                "active intent no longer proven; no intent active",
            );
            self.intent = None;
        }
    }

    /// Hebt die Aktivierung auf: Neue Sessions beginnen ungebunden. Schon
    /// gebundene behalten ihren Anker — ein Aufheben ist kein Wechsel zu
    /// einer anderen Anforderung.
    pub(super) fn clear_intent(&mut self) -> Fallible<String> {
        match std::fs::remove_file(active_path(&self.home)) {
            Ok(()) => super::sync_dir(&self.home.join("evidence"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.intent = None;
        Ok("intent cleared".into())
    }

    /// Vor jedem angenommenen Hook-Event: den aktiven Intent eintragen, wo
    /// er fehlt — als erstes Glied einer neuen Session oder als Wechsel in
    /// einer bekannten (siehe Modul-Doku).
    pub(super) fn bind_intent(
        &mut self,
        key: &SessionKey,
        at: &(String, u64),
        starts: bool,
    ) -> Fallible<()> {
        let recorded = match self.recorded_intent(key)? {
            Some(recorded) => recorded,
            None => {
                // Erste Sichtung durch diesen Witness. Den Anfang belegt nur
                // ein `SessionStart`: Lief die Session vorher am Witness
                // vorbei (Hooks unterdrückt), wäre ein anderes erstes Event
                // kein Anfang — dann ein gewöhnlicher Eintrag, kein Beleg.
                if starts && self.is_new_session(key)? {
                    if let Some(payload) = self.intent.clone() {
                        let payload = IntentEventPayload {
                            opens_session: true,
                            ..payload
                        };
                        self.append_intent(key, &payload, at)?;
                        return self.record_intent(key, Some(&payload.anchor_id));
                    }
                }
                self.record_intent(key, None)?;
                None
            }
        };
        let Some(payload) = self.intent.clone() else {
            return Ok(());
        };
        if recorded.as_ref() != Some(&payload.anchor_id) {
            self.append_intent(key, &payload, at)?;
            self.record_intent(key, Some(&payload.anchor_id))?;
        }
        Ok(())
    }

    /// Neu: keine versiegelte Epoche (nach dem Versiegeln beginnt der Fold
    /// der nächsten leer — er allein sagt nichts) und kein Glied in der
    /// laufenden.
    fn is_new_session(&mut self, key: &SessionKey) -> Fallible<bool> {
        if self.epochs.was_sealed(key) {
            return Ok(false);
        }
        if !self.folders.contains_key(key) {
            self.recover(key)?;
        }
        Ok(!self.folders.get(key).is_some_and(|folder| {
            let coverage = folder.snapshot().coverage;
            coverage.events + coverage.pre_chain > 0
        }))
    }

    /// Der zuletzt in die Session eingetragene Anker: `None` — keine
    /// Sichtung; `Some(None)` — bekannt, ohne Anker (auch: Eintrag
    /// unlesbar).
    fn recorded_intent(&mut self, key: &SessionKey) -> Fallible<Option<Option<ContentHash>>> {
        if let Some(recorded) = self.intent_seen.get(key) {
            return Ok(Some(recorded.clone()));
        }
        let path = record_path(&self.home, key);
        // Nur ein **fehlender** Eintrag heißt „nie gesehen"; jeder andere
        // Fehler (Rechte, Schleife, I/O) landet unten als unlesbar — nie als
        // neu.
        if matches!(
            std::fs::symlink_metadata(&path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound
        ) {
            return Ok(None);
        }
        let relative = path
            .strip_prefix(&self.home)
            .ok()
            .and_then(Path::to_str)
            .ok_or("session intent record outside the witness home")?
            .to_owned();
        // Dieselben Prüfungen wie für jede private Datei des Witness
        // (Eigentümer, 0600, kein Symlink, ein Link, Größe).
        let text = read_private(&self.home, &relative)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
        let recorded = match text.as_deref().map(str::trim) {
            Some(NONE_RECORDED) => None,
            Some(id) if id.parse::<ContentHash>().is_ok() => id.parse().ok(),
            _ => {
                log(
                    &self.home,
                    "session intent record unreadable; treated as unbound",
                );
                None
            }
        };
        self.intent_seen.insert(key.clone(), recorded.clone());
        Ok(Some(recorded))
    }

    fn record_intent(&mut self, key: &SessionKey, id: Option<&ContentHash>) -> Fallible<()> {
        let text = id.map_or(NONE_RECORDED, ContentHash::as_str);
        atomic(
            &record_path(&self.home, key),
            format!("{text}\n").as_bytes(),
        )?;
        self.intent_seen.insert(key.clone(), id.cloned());
        Ok(())
    }

    /// Hängt das synthetische Event an. Der Stempel ist der des folgenden
    /// Hook-Events; die monotone Uhr schiebt jenes dahinter.
    fn append_intent(
        &mut self,
        key: &SessionKey,
        payload: &IntentEventPayload,
        at: &(String, u64),
    ) -> Fallible<()> {
        self.append(
            key,
            NewEvent {
                at: at.0.clone(),
                at_nanos: at.1,
                kind: EventKind::Other,
                raw_kind: INTENT_EVENT_KIND.into(),
                cwd: None,
                transcript_path: None,
                payload: serde_json::value::RawValue::from_string(serde_json::to_string(payload)?)?,
            },
        )?;
        Ok(())
    }
}
