//! Der lokale Epochen-Zustand der Evidence-Chain: welcher Seal zuletzt für
//! eine Session geschrieben wurde (ADR-0011, Entscheidung 2).
//!
//! Nach `journal.discard` beginnt dieselbe Session wieder bei `seq 0` — jede
//! Checkpoint-Epoche wird ein eigener Seal, und die `previous`-Zeile verkettet
//! sie. Diese Verkettung braucht Gedächtnis, das den `discard` überlebt und
//! **nicht** im Journal liegt. Es liegt hier:
//!
//! ```text
//! <git-dir>/minds/evidence/state/<agent>/b3-<16 hex>
//! ```
//!
//! Dieselben Härtungsregeln wie beim Journal: 0700/0600, Symlink-Refusal,
//! gehashter Verzeichnisname statt roher `local_id`. Die Datei trägt genau
//! eine Zeile — die `seal_id` der letzten Epoche.
//!
//! Der Zustand ist **lokal und best-effort**: Er wird nie gepusht, ein
//! frischer Clone hat ihn nicht. Fehlt er, schreibt der nächste Seal
//! `previous=-`, und das Verdikt sagt ehrlich „Epochenkette nicht belegt" —
//! nie wird eine Verkettung erfunden.

use std::fs;
use std::path::PathBuf;

use minds_core::ContentHash;

use crate::error::Result;
use crate::journal::{self, SessionKey};

/// Dateiendung des Session-Salts neben dem Epochen-Zustand.
const SALT_SUFFIX: &str = "salt";

/// Länge des Salts in Bytes.
const SALT_LEN: usize = 32;

/// Wurzelverzeichnis relativ zum Git-Verzeichnis.
const STATE_DIR: &str = "minds/evidence/state";

/// Zugriff auf den Epochen-Zustand eines Repos.
#[derive(Debug, Clone)]
pub struct EpochState {
    root: PathBuf,
}

impl EpochState {
    /// Öffnet den Zustand unterhalb des Git-Verzeichnisses. Legt nichts an —
    /// das passiert erst beim ersten [`record`](Self::record).
    pub fn open(git_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: git_dir.into().join(STATE_DIR),
        }
    }

    /// Die `seal_id` der letzten Epoche dieser Session, falls bekannt.
    ///
    /// Alles Unlesbare ist `None`, kein Fehler: Ein kaputter Zustand darf den
    /// Checkpoint nicht aufhalten — er kostet nur die Verkettung, und das
    /// sichtbar (`previous=-`).
    pub fn last_seal(&self, key: &SessionKey) -> Option<ContentHash> {
        let bytes = fs::read(self.file(key)).ok()?;
        std::str::from_utf8(&bytes).ok()?.trim().parse().ok()
    }

    /// Merkt sich die `seal_id` der eben geschriebenen Epoche.
    ///
    /// Atomar via tmp + rename (mit fsync — der Zustand soll einen Crash
    /// überleben, sonst wäre die Kette beim nächsten Checkpoint grundlos
    /// offen); Verzeichnisse 0700, Datei 0600.
    pub fn record(&self, key: &SessionKey, seal_id: &ContentHash) -> Result<()> {
        let file = self.file(key);
        let dir = file.parent().expect("Datei liegt unter state/");
        journal::create_dir_private(&self.root, dir)?;
        let tmp = file.with_extension("tmp");
        journal::write_private(&tmp, format!("{seal_id}\n").as_bytes(), "Epoche schreiben")?;
        fs::rename(&tmp, &file)
            .map_err(|e| crate::error::CaptureError::io("Epoche umbenennen", &file, e))?;
        Ok(())
    }

    /// Der Session-Salt für den Chain-Fold — beim ersten Zugriff erzeugt,
    /// danach stabil (Idempotenz: gleiche Events ⇒ gleicher Root ⇒ gleicher
    /// Seal, auch über Läufe und Epochen hinweg).
    ///
    /// **Warum:** Der Chain-Root reist im Seal auf die Forge; ohne Salt wäre
    /// er für Ein-Event-Epochen ein Offline-Orakel über den Payload (siehe
    /// [`minds_core::evidence::chain_salted`]). Der Salt bleibt lokal — 0600,
    /// nie gepusht — und macht den Root ohne lokalen Zugriff unnachrechenbar.
    pub fn salt(&self, key: &SessionKey) -> Result<[u8; 32]> {
        let file = self.file(key).with_extension(SALT_SUFFIX);
        if let Ok(bytes) = fs::read(&file) {
            if bytes.len() == SALT_LEN {
                let mut salt = [0u8; SALT_LEN];
                salt.copy_from_slice(&bytes);
                return Ok(salt);
            }
            // Falsche Laenge: kaputt — neu erzeugen (unten), alter Root ist
            // dann nicht mehr reproduzierbar; der Seal-Reuse-Vergleich laeuft
            // ins Leere und versiegelt schlicht neu.
        }
        let dir = file.parent().expect("Datei liegt unter state/");
        journal::create_dir_private(&self.root, dir)?;
        let salt = random_salt();
        let tmp = file.with_extension("salt.tmp");
        journal::write_private(&tmp, &salt, "Salt schreiben")?;
        fs::rename(&tmp, &file)
            .map_err(|e| crate::error::CaptureError::io("Salt umbenennen", &file, e))?;
        Ok(salt)
    }

    /// Der Pfad der Zustandsdatei — gehashter Name, nie die rohe `local_id`
    /// (dieselbe Regel wie beim Journal: Session-Kennungen können
    /// Token-förmig sein).
    fn file(&self, key: &SessionKey) -> PathBuf {
        self.root
            .join(key.agent())
            .join(journal::hashed_dir_name(key.local_id()))
    }
}

/// 32 zufällige Bytes ohne neue Dependency: bevorzugt `/dev/urandom`
/// (Unix — Windows hat kein natives Binary, der Weg ist WSL), ersatzweise
/// die OS-geseedeten SipHash-Schlüssel von `RandomState`, über blake3
/// aufgefaltet. Der Salt schützt gegen Offline-Raten eines Payloads —
/// dafür genügt diese Entropie; er ist kein kryptographischer Schlüssel.
fn random_salt() -> [u8; 32] {
    // Nie `fs::read` auf /dev/urandom — das ist eine endlose Quelle. Genau
    // 32 Bytes lesen.
    let from_urandom = || -> std::io::Result<[u8; 32]> {
        use std::io::Read;
        let mut buf = [0u8; 32];
        fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
        Ok(buf)
    };
    if let Ok(salt) = from_urandom() {
        return salt;
    }
    // Fallback: zwei unabhängige RandomState-Instanzen tragen die
    // OS-geseedeten Schlüssel des Prozesses; blake3 faltet sie auf.
    use std::hash::{BuildHasher, Hasher};
    let mut material = Vec::with_capacity(32);
    for _ in 0..4 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write(b"minds-salt");
        material.extend_from_slice(&hasher.finish().to_le_bytes());
    }
    blake3::derive_key("minds/evidence/v1/salt", &material)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SessionKey {
        SessionKey::new("claude-code", "s1").unwrap()
    }

    fn id(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
    }

    #[test]
    fn records_and_reads_back_the_last_seal() {
        let tmp = tempfile::tempdir().unwrap();
        let state = EpochState::open(tmp.path());

        assert_eq!(state.last_seal(&key()), None);
        state.record(&key(), &id(1)).unwrap();
        assert_eq!(state.last_seal(&key()), Some(id(1)));

        // Die naechste Epoche ueberschreibt.
        state.record(&key(), &id(2)).unwrap();
        assert_eq!(state.last_seal(&key()), Some(id(2)));
    }

    #[test]
    fn the_state_survives_a_journal_discard() {
        // Der Zustand liegt NICHT im Journal-Verzeichnis — genau deshalb.
        let tmp = tempfile::tempdir().unwrap();
        let journal = crate::Journal::open(tmp.path());
        let state = EpochState::open(tmp.path());

        state.record(&key(), &id(7)).unwrap();
        journal.discard(&key()).unwrap();
        assert_eq!(state.last_seal(&key()), Some(id(7)));
    }

    #[test]
    fn a_corrupt_state_reads_as_none_not_as_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let state = EpochState::open(tmp.path());
        state.record(&key(), &id(1)).unwrap();

        // Datei zerstoeren: kein Fehler, nur eine offene Kette.
        let file = state.file(&key());
        fs::write(&file, b"kein hash").unwrap();
        assert_eq!(state.last_seal(&key()), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_state_file_is_owner_only_and_never_names_the_local_id() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let state = EpochState::open(tmp.path());
        let secret = SessionKey::new("claude-code", "glpat-abc123def").unwrap();
        state.record(&secret, &id(3)).unwrap();

        let file = state.file(&secret);
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Kein Pfadsegment traegt die rohe Kennung.
        assert!(!file.to_string_lossy().contains("glpat"));
    }
}
