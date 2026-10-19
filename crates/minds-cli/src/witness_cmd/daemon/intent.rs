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
//! Die Host-Seite, über den Steuer-Socket im Zustandsverzeichnis
//! (`control/control.sock`, eigenes 0700-Verzeichnis, nur Verbindungen des
//! Witness-Nutzers). Auf dem Socket der Agent-Seite wird `IntentActivate`
//! abgewiesen: Die Signatur belegt, wer eine Anforderung freigab, nicht dass
//! sie für diese Session gilt — wählte der Agent den Anker, wäre die
//! Bindung seine Behauptung. Der Anker `-` hebt die Aktivierung auf; das
//! wirkt für neue Sessions, gebundene behalten ihren Anker.
//!
//! Beim geschützten Witness teilt der Agent die Kennung des Menschen — den
//! Steuer-Socket erreicht dort keiner von beiden. Der Mensch aktiviert dann
//! entweder als Witness-Nutzer (mit Administratorrechten, also seinem
//! Passwort) oder, wenn `init` seinen Sicherheitsschlüssel festhielt
//! (`intent_key`), auf dem Socket der Agent-Seite mit einer **frischen**
//! Freigabe: `IntentChallenge` liefert eine Nonce, die nur auf dieser
//! Verbindung und nur [`CHALLENGE_TTL`] lang gilt, einmal;
//! `IntentActivateProven` trägt die Signatur des festgehaltenen Schlüssels
//! über Anker-Id und Nonce unter `minds-intent-activate`, mit gesetztem
//! User-Presence-Flag ([`check_proof`]). Eine alte Freigabe lässt sich so
//! nicht wieder einspielen, und ohne Berührung entsteht keine.
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
use std::time::Duration;

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

/// Wie lange die Nonce einer `IntentChallenge` gilt — Zeit für eine
/// Berührung des Sicherheitsschlüssels.
pub(super) const CHALLENGE_TTL: Duration = Duration::from_secs(120);

/// Mindestabstand zwischen zwei Prüfungen mit `ssh-keygen` (je Prüfung
/// zwei Aufrufe).
pub(super) const VERIFY_INTERVAL: Duration = Duration::from_millis(250);

/// Höchstens so viele Verbindungen halten zugleich eine Nonce.
pub(super) const MAX_OPEN_CHALLENGES: usize = 8;

/// Der Principal der Freigabe in der Signer-Datei, die [`check_proof`] für
/// `ssh-keygen` anlegt.
const ACTIVATE_PRINCIPAL: &str = "minds-intent-activate";

/// Eine neue Nonce: 32 Bytes aus dem Zufall des Systems, klein-hex.
pub(super) fn new_nonce() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Prüft eine frische Freigabe: `proof` signiert
/// [`crate::intent_proof::activation_message`] über die Id von `anchor` und
/// `nonce` unter `minds-intent-activate`, und `signature` ist die Signatur
/// des Ankertexts unter `minds-intent` — beide mit genau dem festgehaltenen
/// Schlüssel `pinned`, als Sicherheitsschlüssel mit User-Presence-Flag (das
/// prüft `ssh-keygen` nicht selbst). Eine einzelne erschlichene Berührung
/// reicht so nicht, um einen Anker des Agenten zu binden: Er bräuchte auch
/// dessen Freigabe. Ob eine Signatur gilt, entscheidet `verify` (Text,
/// Signatur, Signer-Datei, Namespace; `None`: nicht prüfbar) — im Dienst
/// `ssh-keygen -Y verify`. Der Fehler ist ein fester Grund.
pub(super) fn check_proof(
    pinned: Option<&str>,
    anchor: &str,
    signature: Option<&str>,
    nonce: &str,
    proof: &str,
    mut verify: impl FnMut(&str, &str, &Path, &str) -> Option<bool>,
) -> Result<(), &'static str> {
    pinned_touch(pinned, signature, proof)?;
    let (Some(pinned), Some(signature)) = (pinned, signature) else {
        return Err(crate::intent_proof::NO_INTENT_KEY);
    };
    let (_dir, signers) = signers_file(pinned).map_err(|_| "activation proof not checkable")?;
    let id = minds_core::intent_anchor::IntentAnchor::id_of_text(anchor);
    let message = crate::intent_proof::activation_message(id.as_str(), signature, nonce);
    let not_checkable = crate::intent_proof::NOT_CHECKABLE;
    match verify(&message, proof, &signers, minds_attest::NS_INTENT_ACTIVATE) {
        Some(true) => {}
        Some(false) => return Err("activation proof invalid"),
        None => return Err(not_checkable),
    }
    match verify(anchor, signature, &signers, minds_attest::NS_INTENT) {
        Some(true) => Ok(()),
        Some(false) => Err("intent signature invalid"),
        None => Err(not_checkable),
    }
}

/// Je festgehaltenem Schlüssel der höchste Signaturzähler, den der Witness
/// je für eine Aktivierung sah — je Zeile `<zähler> <schlüssel-id>`; ein
/// Schlüssel, der wieder festgehalten wird, behält seinen Stand. Eine
/// Freigabe gilt einmal: Der Zähler steht in den **signierten** Daten jeder
/// FIDO-Signatur — anders als ihre Kodierung lässt er sich ohne den
/// Schlüssel nicht ändern (eine neu armierte oder umgerechnete Signatur
/// trägt denselben). Eine alte, im Store lesbare Anker-Signatur liegt
/// darunter.
const COUNTER_FILE: &str = "evidence/intent-counter";

/// Die Kennung eines festgehaltenen Schlüssels im Zählerstand.
fn key_id(pinned: &str) -> String {
    format!("b3-{}", blake3::hash(pinned.as_bytes()).to_hex())
}

/// Alle Stände, `(schlüssel-id, zähler)`. Ohne Datei: keine. Unlesbar oder
/// eine Zeile, die nicht genau so aussieht, eine Kennung doppelt: `Err`
/// (fail-closed; als Witness-Nutzer die Datei prüfen oder entfernen).
fn counter_marks(home: &Path) -> Result<Vec<(String, u32)>, ()> {
    if matches!(
        std::fs::symlink_metadata(home.join(COUNTER_FILE)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    ) {
        return Ok(Vec::new());
    }
    let text = super::read_private(home, COUNTER_FILE)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or(())?;
    let mut marks: Vec<(String, u32)> = Vec::new();
    for line in text.lines() {
        let (counter, key) = line.split_once(' ').ok_or(())?;
        let counter: u32 = counter.parse().map_err(|_| ())?;
        let wellformed = key.len() == 67
            && key.starts_with("b3-")
            && key[3..]
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !wellformed || marks.iter().any(|(known, _)| known == key) {
            return Err(());
        }
        marks.push((key.to_owned(), counter));
    }
    Ok(marks)
}

/// Der Höchststand für `pinned`; ohne Eintrag 0.
pub(super) fn counter_mark(home: &Path, pinned: &str) -> Result<u32, ()> {
    let id = key_id(pinned);
    Ok(counter_marks(home)?
        .into_iter()
        .find_map(|(key, counter)| (key == id).then_some(counter))
        .unwrap_or(0))
}

/// Hebt den Höchststand für `pinned` auf `to` (nie darunter; die Stände
/// anderer Schlüssel bleiben) — **vor** der Aktivierung und dauerhaft
/// (atomar, synchronisiert): Scheitert das, wird nicht aktiviert.
pub(super) fn raise_counter_mark(home: &Path, pinned: &str, to: u32) -> Result<(), &'static str> {
    let mut marks = counter_marks(home).map_err(|()| crate::intent_proof::COUNTER_UNREADABLE)?;
    let id = key_id(pinned);
    match marks.iter_mut().find(|(key, _)| *key == id) {
        Some((_, counter)) if *counter >= to => return Ok(()),
        Some((_, counter)) => *counter = to,
        None => marks.push((id, to)),
    }
    let text: String = marks
        .iter()
        .map(|(key, counter)| format!("{counter} {key}\n"))
        .collect();
    super::atomic(&home.join(COUNTER_FILE), text.as_bytes())
        .map_err(|_| crate::intent_proof::NOT_RECORDABLE)
}

/// Die Zähler einer Freigabe per Berührung: Die Anker-Signatur liegt über
/// dem Höchststand `mark` (sie wurde nie verwendet), und die Freigabe der
/// Aktivierung ist die **nächste** Signatur desselben Schlüssels — beide
/// Berührungen gehören zusammen; eine alte Anker-Signatur lässt sich nicht
/// mit einer neuen Berührung paaren. Liefert den neuen Höchststand.
pub(super) fn check_counters(
    mark: u32,
    signature: Option<&str>,
    proof: &str,
) -> Result<u32, &'static str> {
    let signature = signature.ok_or("an activation by touch needs the intent signature")?;
    let anchor = minds_attest::signature_counter(signature)
        .ok_or("intent signature is not from a security key")?;
    let activation = minds_attest::signature_counter(proof)
        .ok_or("activation proof is not from a security key")?;
    if anchor == 0 {
        return Err("the security key reports no signature counter — use the password route");
    }
    if anchor <= mark {
        return Err("this intent approval was already used for an activation — sign again");
    }
    if anchor.checked_add(1) != Some(activation) {
        return Err(
            "the activation touch does not directly follow the approval touch — sign again",
        );
    }
    Ok(activation)
}

/// Auf dem Steuer-Socket (Weg über das Passwort): Ist die Signatur eine
/// **gültige** des festgehaltenen Schlüssels über `anchor` (`verify`, im
/// Dienst `ssh-keygen`) mit Zähler, steigt der Höchststand mit — auch diese
/// Freigabe gilt danach nicht noch einmal per Berührung. Eine ungeprüfte
/// Zahl hebt ihn nie: Sonst sperrte eine erfundene Signatur den Weg per
/// Berührung. Ist sie ungültig oder nicht prüfbar, ein Fehler — aktiviert
/// wird dann nicht, sonst bliebe eine echte Freigabe unter dem Stand.
pub(super) fn raise_for_signature(
    home: &Path,
    pinned: Option<&str>,
    anchor: &str,
    signature: Option<&str>,
    verify: fn(&str, &str, &Path, &str) -> Option<bool>,
) -> Result<(), &'static str> {
    let (Some(pinned), Some(signature)) = (pinned, signature) else {
        return Ok(());
    };
    let from_pinned = minds_attest::public_key_blob(pinned)
        .is_some_and(|blob| minds_attest::signature_public_key(signature) == Some(blob));
    let Some(counter) = minds_attest::signature_counter(signature).filter(|_| from_pinned) else {
        return Ok(());
    };
    let (_dir, signers) = signers_file(pinned).map_err(|_| crate::intent_proof::NOT_CHECKABLE)?;
    match verify(anchor, signature, &signers, minds_attest::NS_INTENT) {
        Some(true) => raise_counter_mark(home, pinned, counter),
        // Eine Signatur, die den festgehaltenen Schlüssel nennt, aber nicht
        // gilt: Auf diesem Weg kommt sie frisch aus `intent sign` — stimmt
        // etwas nicht, wird nicht aktiviert (sonst bliebe eine echte
        // Freigabe unter dem Stand, wenn die Prüfung selbst irrte).
        Some(false) => Err("intent signature invalid"),
        None => Err(crate::intent_proof::NOT_CHECKABLE),
    }
}

/// Das `ssh-keygen` des Witness: `/usr/bin/ssh-keygen` oder das erste
/// unter `PATH`, das so heißt und samt aller Verzeichnisse darüber nur root
/// ändern kann (sein kanonischer Pfad) — `init`, `status` und `keygen`
/// können als Witness-Nutzer mit dem `PATH` des Entwicklers laufen; ein dort
/// abgelegtes Programm liefe sonst als Witness-Nutzer. Aufgerufen wird es
/// mit leerer Umgebung. `None`: keins.
pub(crate) fn witness_keygen() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = || {
        std::iter::once(PathBuf::from("/usr/bin"))
            .chain(std::env::split_paths(&path))
            .filter(|dir| dir.is_absolute())
    };
    let strict = dirs().find_map(|dir| {
        crate::verify_cmd::conformance::root_owned_program(&dir.join("ssh-keygen"), "ssh-keygen")
    });
    if strict.is_some() {
        return strict;
    }
    // Ein Witness, dessen eigenes Programm nicht nur root ändern kann, ist
    // ohnehin nicht geschützt (es teilt die Kennung mit seinem Entwickler,
    // etwa unter NixOS mit `ssh-keygen` nur in `/nix/store`): dann das erste
    // `ssh-keygen` unter `PATH`, mit absolutem Pfad. Der geschützte Witness
    // bleibt streng.
    // Fail-closed: Lässt sich das eigene Programm nicht bestimmen (etwa
    // unter Linux nach einem Austausch an Ort und Stelle), gilt es als
    // geschützt.
    let protected = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_or(true, |exe| {
            crate::verify_cmd::conformance::root_owned_chain(&exe)
        });
    if protected {
        return None;
    }
    dirs()
        .map(|dir| dir.join("ssh-keygen"))
        .find(|candidate| candidate.is_file())
}

/// Die billigen Prüfungen einer Freigabe per Berührung (ohne
/// `ssh-keygen`): Freigabe und Anker-Signatur tragen genau den
/// festgehaltenen Schlüssel, als Sicherheitsschlüssel mit
/// User-Presence-Flag. Ungeprüft, ob sie gelten — das entscheidet
/// [`check_proof`].
pub(super) fn pinned_touch(
    pinned: Option<&str>,
    signature: Option<&str>,
    proof: &str,
) -> Result<(), &'static str> {
    let pinned = pinned.ok_or(crate::intent_proof::NO_INTENT_KEY)?;
    let pinned_blob = minds_attest::public_key_blob(pinned);
    let touched_by_pinned = |armored: &str| -> Result<(), u8> {
        let signed = minds_attest::signature_key(armored).ok_or(0)?;
        if minds_attest::signature_public_key(armored) != pinned_blob || pinned_blob.is_none() {
            return Err(1);
        }
        if !minds_attest::is_security_key_type(&signed.key_type) || !signed.user_presence {
            return Err(2);
        }
        Ok(())
    };
    touched_by_pinned(proof).map_err(|kind| match kind {
        0 => "activation proof unreadable",
        1 => crate::intent_proof::NOT_PINNED_PROOF,
        _ => "activation proof without a touch of the security key",
    })?;
    let signature = signature.ok_or("an activation by touch needs the intent signature")?;
    touched_by_pinned(signature).map_err(|kind| match kind {
        0 => "intent signature unreadable",
        1 => crate::intent_proof::NOT_PINNED_SIGNATURE,
        _ => "intent signature without a touch of the security key",
    })
}

/// Eine Signer-Datei mit genau dem festgehaltenen Schlüssel, beschränkt auf
/// `minds-intent-activate` und `minds-intent`, in einem privaten
/// Temp-Verzeichnis (das mit dem zurückgegebenen Wert verschwindet).
fn signers_file(pinned: &str) -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::Builder::new()
        .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .tempdir()?;
    let signers = dir.path().join("allowed_signers");
    std::fs::write(
        &signers,
        format!(
            "{ACTIVATE_PRINCIPAL} namespaces=\"{},{}\" {pinned}\n",
            minds_attest::NS_INTENT_ACTIVATE,
            minds_attest::NS_INTENT
        ),
    )?;
    Ok((dir, signers))
}

/// Die Frist für eine Prüfung mit `ssh-keygen` in der Eventloop.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(2);

/// `ssh-keygen -Y verify` für [`check_proof`].
/// `None`: nicht prüfbar (kein `ssh-keygen`, Frist, Fehler beim Starten) —
/// kein Urteil über die Signatur.
pub(super) fn ssh_verify(
    message: &str,
    signature: &str,
    signers: &Path,
    namespace: &str,
) -> Option<bool> {
    minds_attest::ssh_verify_ns_within(
        &witness_keygen()?,
        message,
        signature,
        signers,
        ACTIVATE_PRINCIPAL,
        namespace,
        VERIFY_TIMEOUT,
    )
    .ok()
}

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
    /// Der festgehaltene Freigabe-Schlüssel, frisch aus `witness.json` —
    /// ein `--replace-intent-key` gilt sofort, nicht erst nach einem
    /// Neustart. Unlesbar: `Err` (dann wird nichts per Schlüssel geprüft).
    ///
    /// Gelesen wird nur diese eine Datei (mit den Prüfungen jeder privaten
    /// Datei des Witness), nicht das ganze Home wie beim Start — der Agent
    /// kann Nonces in beliebiger Zahl erfragen. Solange sie sich nicht
    /// ändert (Gerät, Inode, Änderungs- und Statuszeit, Größe — die
    /// Statuszeit setzt kein Nutzerprogramm zurück), gilt der zuletzt
    /// gelesene Schlüssel.
    pub(super) fn pinned_intent_key(&mut self) -> Result<Option<String>, ()> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(self.home.join("witness.json")).map_err(|_| ())?;
        let stamp = (
            meta.dev(),
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
            meta.len(),
        );
        if let Some((cached, key)) = &self.pin_cache {
            if *cached == stamp {
                return Ok(key.clone());
            }
        }
        let bytes = super::read_private(&self.home, "witness.json").map_err(|_| ())?;
        let config: super::Config = serde_json::from_slice(&bytes).map_err(|_| ())?;
        let key = config.intent_key().map(str::to_owned);
        self.pin_cache = Some((stamp, key.clone()));
        Ok(key)
    }

    /// Über den Steuer-Socket (Host-Seite, auch der Weg über das Passwort):
    /// Anker prüfen, den Höchststand für eine geprüfte Signatur des
    /// festgehaltenen Schlüssels heben, dann aktivieren.
    pub(super) fn activate_by_control(
        &mut self,
        anchor: &str,
        signature: Option<String>,
    ) -> Fallible<Result<String, &'static str>> {
        let payload = match self.intent_payload(anchor, signature.clone()) {
            Ok(payload) => payload,
            Err(reason) => return Ok(Err(reason)),
        };
        // Ohne Signatur gibt es nichts zu heben — dann auch nichts zu lesen.
        let pinned = if signature.is_some() {
            let Ok(pinned) = self.pinned_intent_key() else {
                return Ok(Err(crate::intent_proof::NOT_CHECKABLE));
            };
            pinned
        } else {
            None
        };
        if let Err(reason) = raise_for_signature(
            &self.home,
            pinned.as_deref(),
            anchor,
            signature.as_deref(),
            self.verify_activation,
        ) {
            log(&self.home, "intent counter not raised; not activated");
            return Ok(Err(reason));
        }
        self.commit_intent(payload)
    }

    /// Über den Socket der Agent-Seite, nach einer frischen Nonce: erst die
    /// billigen Prüfungen (Schlüssel und Berührung, dann die Zähler),
    /// `ssh-keygen` zuletzt; dann der Anker; der neue Höchststand steht
    /// fest, **bevor** aktiviert wird.
    pub(super) fn activate_by_touch(
        &mut self,
        anchor: &str,
        signature: Option<String>,
        nonce: &str,
        proof: &str,
    ) -> Fallible<Result<String, &'static str>> {
        let pinned = match self.pinned_intent_key() {
            Ok(Some(pinned)) => pinned,
            Ok(None) => return Ok(Err(crate::intent_proof::NO_INTENT_KEY)),
            Err(()) => return Ok(Err(crate::intent_proof::NOT_CHECKABLE)),
        };
        let next = match pinned_touch(Some(&pinned), signature.as_deref(), proof)
            .and_then(|()| {
                counter_mark(&self.home, &pinned)
                    .map_err(|()| crate::intent_proof::COUNTER_UNREADABLE)
            })
            .and_then(|mark| check_counters(mark, signature.as_deref(), proof))
        {
            Ok(next) => next,
            Err(reason) => return Ok(Err(reason)),
        };
        let verify = self.verify_activation;
        let mut verified = false;
        let checked = check_proof(
            Some(&pinned),
            anchor,
            signature.as_deref(),
            nonce,
            proof,
            |text, armored, signers, namespace| {
                verified = true;
                verify(text, armored, signers, namespace)
            },
        );
        if verified {
            self.last_verify = Some(std::time::Instant::now());
        }
        if let Err(reason) = checked {
            return Ok(Err(reason));
        }
        let payload = match self.intent_payload(anchor, signature) {
            Ok(payload) => payload,
            Err(reason) => return Ok(Err(reason)),
        };
        if let Err(reason) = raise_counter_mark(&self.home, &pinned, next) {
            log(&self.home, "intent counter not raised; not activated");
            return Ok(Err(reason));
        }
        let outcome = self.commit_intent(payload)?;
        if outcome.is_ok() {
            log(&self.home, "intent activated with a security key touch");
        }
        Ok(outcome)
    }

    /// Anker und Signatur prüfen (Form, Policy, Beleg im Store) — ohne zu
    /// aktivieren.
    fn intent_payload(
        &mut self,
        anchor: &str,
        signature: Option<String>,
    ) -> Result<IntentEventPayload, &'static str> {
        let payload = match IntentEventPayload::new(anchor, signature) {
            Ok(payload) => payload,
            Err(minds_core::intent_anchor::IntentAnchorError::Signature) => {
                return Err("invalid intent signature");
            }
            Err(_) => return Err("invalid intent anchor"),
        };
        if !policy_accepts(&self.config, &payload.anchor) {
            return Err("intent anchor refused by the redaction policy");
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
            Ok(Ok(())) => Ok(payload),
            Ok(Err(reason)) => Err(reason),
            Err(_) => Err("pinned store unavailable"),
        }
    }

    /// Persistiert den geprüften Anker als aktiven Intent.
    fn commit_intent(
        &mut self,
        payload: IntentEventPayload,
    ) -> Fallible<Result<String, &'static str>> {
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
                model: None,
                payload: serde_json::value::RawValue::from_string(serde_json::to_string(payload)?)?,
            },
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let word = chunk
                .iter()
                .enumerate()
                .fold(0u32, |word, (i, b)| word | u32::from(*b) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(word >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn string(bytes: &[u8]) -> Vec<u8> {
        let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(bytes);
        out
    }

    const SK: &str = "sk-ssh-ed25519@openssh.com";

    /// Ein (ausgedachter) FIDO-Schlüssel im Wire-Format.
    fn sk_blob(seed: u8) -> Vec<u8> {
        [string(SK.as_bytes()), string(&[seed; 32]), string(b"ssh:")].concat()
    }

    fn sk_line(seed: u8) -> String {
        format!("{SK} {}", base64(&sk_blob(seed)))
    }

    /// Eine armierte `ssh-sig`-Signatur mit dem Schlüssel `key` (Wire-Format)
    /// und den FIDO-Flags `flags` — die Signaturbytes sind Unsinn; ob sie
    /// gelten, entscheidet im Test der eingesetzte `verify`.
    fn proof(key: &[u8], kind: &str, flags: Option<u8>) -> String {
        let mut signature = [string(kind.as_bytes()), string(&[7; 64])].concat();
        if let Some(flags) = flags {
            signature.push(flags);
            signature.extend_from_slice(&1u32.to_be_bytes());
        }
        let blob = [
            b"SSHSIG".to_vec(),
            1u32.to_be_bytes().to_vec(),
            string(key),
            string(minds_attest::NS_INTENT_ACTIVATE.as_bytes()),
            string(b""),
            string(b"sha512"),
            string(&signature),
        ]
        .concat();
        let body = base64(&blob);
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(70)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect();
        format!(
            "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
            lines.join("\n")
        )
    }

    const ANCHOR: &str = "minds-intent-v1\nsource=prompt\n";

    #[test]
    fn a_proof_counts_only_from_the_pinned_key_with_a_touch() {
        let nonce = "ab".repeat(32);
        let pinned = sk_line(1);
        let touched = proof(&sk_blob(1), SK, Some(0x01));
        let signed = Some(touched.as_str());
        let never = |_: &str, _: &str, _: &Path, _: &str| -> Option<bool> { panic!("not reached") };
        let check = |proof: &str, signature: Option<&str>| {
            check_proof(Some(&pinned), ANCHOR, signature, &nonce, proof, never)
        };

        // Ohne festgehaltenen Schlüssel: keine Freigabe per Berührung.
        assert_eq!(
            check_proof(None, ANCHOR, signed, &nonce, &touched, never),
            Err(crate::intent_proof::NO_INTENT_KEY)
        );
        assert_eq!(
            check("not a signature", signed),
            Err("activation proof unreadable")
        );
        // Ein anderer Sicherheitsschlüssel, ein Software-Schlüssel.
        assert_eq!(
            check(&proof(&sk_blob(2), SK, Some(1)), signed),
            Err("activation proof is not from the pinned intent key")
        );
        let software = [string(b"ssh-ed25519"), string(&[1; 32])].concat();
        assert_eq!(
            check(&proof(&software, "ssh-ed25519", None), signed),
            Err("activation proof is not from the pinned intent key")
        );
        // Der richtige Schlüssel, aber ohne User Presence (nur 0x04, „user
        // verified").
        assert_eq!(
            check(&proof(&sk_blob(1), SK, Some(0x04)), signed),
            Err("activation proof without a touch of the security key")
        );
        // Die Freigabe des Ankers selbst: Pflicht, vom selben Schlüssel, mit
        // Berührung — eine erschlichene Berührung allein bindet nichts.
        assert_eq!(
            check(&touched, None),
            Err("an activation by touch needs the intent signature")
        );
        let other = proof(&sk_blob(2), SK, Some(1));
        assert_eq!(
            check(&touched, Some(&other)),
            Err("intent signature is not from the pinned intent key")
        );
        let untouched = proof(&sk_blob(1), SK, Some(0));
        assert_eq!(
            check(&touched, Some(&untouched)),
            Err("intent signature without a touch of the security key")
        );

        // Mit Berührung entscheidet `ssh-keygen` — über genau diese Texte,
        // gegen eine Signer-Datei mit genau diesem Schlüssel.
        let mut seen = Vec::new();
        let verdict = check_proof(
            Some(&pinned),
            ANCHOR,
            signed,
            &nonce,
            &touched,
            |text, sig, signers, namespace| {
                seen.push((
                    text.to_owned(),
                    sig.to_owned(),
                    std::fs::read_to_string(signers).unwrap(),
                    namespace.to_owned(),
                ));
                Some(true)
            },
        );
        assert_eq!(verdict, Ok(()));
        let id = minds_core::intent_anchor::IntentAnchor::id_of_text(ANCHOR);
        let signers = format!(
            "minds-intent-activate namespaces=\"minds-intent-activate,minds-intent\" {pinned}\n"
        );
        assert_eq!(
            seen,
            [
                (
                    format!(
                        "minds-intent-activate-v1\nanchor {id}\nsignature {}\nnonce {nonce}\n",
                        crate::intent_proof::signature_hash(&touched)
                    ),
                    touched.clone(),
                    signers.clone(),
                    "minds-intent-activate".to_owned()
                ),
                (
                    ANCHOR.to_owned(),
                    touched.clone(),
                    signers,
                    "minds-intent".to_owned()
                ),
            ]
        );
        assert_eq!(
            check_proof(
                Some(&pinned),
                ANCHOR,
                signed,
                &nonce,
                &touched,
                |_, _, _, _| Some(false)
            ),
            Err("activation proof invalid")
        );
        assert_eq!(
            check_proof(
                Some(&pinned),
                ANCHOR,
                signed,
                &nonce,
                &touched,
                |_, _, _, ns| Some(ns == minds_attest::NS_INTENT_ACTIVATE)
            ),
            Err("intent signature invalid")
        );
    }

    /// Hinter allen Vorprüfungen urteilt `ssh-keygen`: Eine erfundene
    /// FIDO-Signatur mit gesetztem Flag lehnt es ab.
    #[test]
    fn ssh_keygen_refuses_a_made_up_security_key_signature() {
        let dir = tempfile::tempdir().unwrap();
        let signers = dir.path().join("allowed_signers");
        std::fs::write(
            &signers,
            format!(
                "minds-intent-activate namespaces=\"minds-intent-activate\" {}\n",
                sk_line(1)
            ),
        )
        .unwrap();
        assert_eq!(
            ssh_verify(
                "minds-intent-activate-v1\n",
                &proof(&sk_blob(1), SK, Some(1)),
                &signers,
                minds_attest::NS_INTENT_ACTIVATE
            ),
            Some(false)
        );
    }

    #[test]
    fn only_a_plain_security_key_is_pinned() {
        let line = sk_line(3);
        assert_eq!(
            super::super::intent_key_line(&format!("{line} patrick@host")).unwrap(),
            line
        );
        for refused in [
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl"
                .to_owned(),
            format!(
                "sk-ssh-ed25519-cert-v01@openssh.com {}",
                base64(&sk_blob(3))
            ),
            format!(
                "{SK} {}",
                base64(&[string(b"ssh-ed25519"), string(&[3; 32])].concat())
            ),
            format!("{SK} not-base64"),
            SK.to_owned(),
            String::new(),
        ] {
            assert!(
                super::super::intent_key_line(&refused).is_err(),
                "{refused}"
            );
        }
    }

    #[test]
    fn nonces_are_fresh_lowercase_hex() {
        let (a, b) = (new_nonce().unwrap(), new_nonce().unwrap());
        assert_ne!(a, b);
        for nonce in [a, b] {
            assert_eq!(nonce.len(), minds_capture::witness_proto::NONCE_HEX);
            assert!(
                nonce
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            );
        }
    }

    #[test]
    fn counters_bind_both_touches_and_rise() {
        let at = |counter: u32| {
            let mut signature = [string(SK.as_bytes()), string(&[7; 64])].concat();
            signature.push(1);
            signature.extend_from_slice(&counter.to_be_bytes());
            let blob = [
                b"SSHSIG".to_vec(),
                1u32.to_be_bytes().to_vec(),
                string(&sk_blob(1)),
                string(b"minds-intent"),
                string(b""),
                string(b"sha512"),
                string(&signature),
            ]
            .concat();
            format!(
                "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
                base64(&blob)
            )
        };
        assert_eq!(check_counters(0, Some(&at(3)), &at(4)), Ok(4));
        assert_eq!(check_counters(3, Some(&at(4)), &at(5)), Ok(5));
        assert!(check_counters(3, Some(&at(3)), &at(4)).is_err());
        assert!(check_counters(0, Some(&at(3)), &at(5)).is_err());
        assert!(check_counters(0, Some(&at(u32::MAX)), &at(0)).is_err());
        assert!(check_counters(0, Some(&at(0)), &at(1)).is_err());
        assert!(check_counters(0, None, &at(1)).is_err());
        // Ein Software-Schlüssel hat keinen Zähler.
        let software = proof(
            &[string(b"ssh-ed25519"), string(&[1; 32])].concat(),
            "ssh-ed25519",
            None,
        );
        assert!(check_counters(0, Some(&software), &at(1)).is_err());

        let dir = tempfile::Builder::new()
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir()
            .unwrap();
        std::fs::create_dir(dir.path().join("evidence")).unwrap();
        let (key, other) = (sk_line(1), sk_line(2));
        assert_eq!(counter_mark(dir.path(), &key), Ok(0));
        raise_counter_mark(dir.path(), &key, 7).unwrap();
        raise_counter_mark(dir.path(), &key, 4).unwrap();
        assert_eq!(counter_mark(dir.path(), &key), Ok(7));
        // Ein neu festgehaltener Schlüssel zählt von vorn; der alte behält
        // seinen Stand (wird er wieder festgehalten, gilt der weiter).
        assert_eq!(counter_mark(dir.path(), &other), Ok(0));
        raise_counter_mark(dir.path(), &other, 3).unwrap();
        assert_eq!(counter_mark(dir.path(), &other), Ok(3));
        assert_eq!(counter_mark(dir.path(), &key), Ok(7));
        // Eine Kennung doppelt oder eine fremde Zeile: unlesbar.
        let file = dir.path().join(COUNTER_FILE);
        let text = std::fs::read_to_string(&file).unwrap();
        let first = text.lines().next().unwrap().to_owned();
        for broken in [
            format!("{text}{first}\n"),
            format!("{text}7\n"),
            "7 b3-zz\n".into(),
        ] {
            super::super::atomic(&file, broken.as_bytes()).unwrap();
            assert_eq!(counter_mark(dir.path(), &key), Err(()), "{broken}");
        }
        super::super::atomic(&file, text.as_bytes()).unwrap();
        // Für andere lesbar: nicht vertraut (fail-closed).
        let file = dir.path().join(COUNTER_FILE);
        std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .unwrap();
        assert_eq!(counter_mark(dir.path(), &other), Err(()));
    }

    /// Der echte Weg durch `ssh-keygen`: Eine gültige Signatur des
    /// Schlüssels der Signer-Datei besteht unter ihrem Namespace — und nur
    /// dort. (Mit einem Software-Schlüssel; der Typ zählt erst davor.)
    #[test]
    fn ssh_keygen_accepts_a_real_signature_only_in_its_namespace() {
        assert!(
            witness_keygen().is_some(),
            "a root-owned ssh-keygen is required"
        );
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        assert!(
            std::process::Command::new(witness_keygen().unwrap())
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        let line = public
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
        let (_signers_dir, signers) = signers_file(&line).unwrap();
        let message = "minds-intent-activate-v1\n";
        let signature =
            minds_attest::ssh_sign_ns(message, &key, minds_attest::NS_INTENT_ACTIVATE).unwrap();
        assert_eq!(
            ssh_verify(
                message,
                &signature,
                &signers,
                minds_attest::NS_INTENT_ACTIVATE
            ),
            Some(true)
        );
        assert_eq!(
            ssh_verify(message, &signature, &signers, minds_attest::NS_INTENT),
            Some(false)
        );
        assert_eq!(
            ssh_verify(
                "other text\n",
                &signature,
                &signers,
                minds_attest::NS_INTENT_ACTIVATE
            ),
            Some(false)
        );
    }
}
