//! Übergabe pre-receive → post-receive (Spec S3 §3.1 bis §3.3).
//!
//! - **Datensatz** (`pending/<rp_pid>-<nonce>.json`): Findings, Ref-Updates und
//!   Trust-Snapshot genau eines Pushes, geschrieben vom pre-receive nach
//!   `Accept`. Nie überschrieben, nie verändert.
//! - **Bestätigung** (`confirmed/<name>`): der unveränderte Datensatz plus die
//!   Ref-Updates, die Git laut post-receive tatsächlich ausgeführt hat. Erst
//!   ab hier entstehen Quittungen; die Bestätigung bleibt, bis alle abgelegt
//!   sind (`reconcile`).
//! - **Altlasten** (`stale/`): nie verwendet, nur gelöscht.
//!
//! Jede Datei ist ein Umschlag `{mac, record}`; `mac` = keyed BLAKE3 über die
//! kanonischen Bytes von `record` mit einem aus dem Seed abgeleiteten
//! Schlüssel. Geschrieben wird vollständig in eine Temp-Datei, die per
//! `link` unter dem endgültigen Namen erscheint (nur anlegen, nie ersetzen),
//! jeweils mit `fsync` von Datei und Verzeichnis.

use std::io::Write;
use std::path::{Path, PathBuf};

use minds_witness_core::{Finding, HashStr, RefUpdate, canonical_bytes};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Schema eines Datensatzes.
pub const PENDING_SCHEMA: &str = "minds.witness.pending/1";
/// Schema einer Bestätigung.
pub const CONFIRMED_SCHEMA: &str = "minds.witness.confirmed/1";
/// Höchstgröße einer Datei.
pub const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAC_CONTEXT: &str = "minds witness pending v1";

/// Ein Datensatz des pre-receive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRecord {
    /// Anlagezeit (Serverzeit des pre-receive).
    pub created: String,
    /// Die Findings aus `pre_receive`, jedes mit seinem Ref-Update.
    pub findings: Vec<Finding>,
    /// Zufallswert der Anlage.
    pub nonce: String,
    /// PID des receive-pack-Prozesses (Push-Kennung).
    pub rp_pid: u32,
    /// Startzeit des receive-pack-Prozesses, sofern feststellbar.
    pub rp_start: Option<String>,
    /// Immer [`PENDING_SCHEMA`].
    pub schema: String,
    /// Hash des Trust-Snapshots (Feld `trust` der Quittungen).
    pub trust: HashStr,
    /// Hash des Binarys, das geprüft hat (Feld `witness.binary` der
    /// Quittungen), festgehalten im pre-receive.
    pub witness_binary: HashStr,
    /// Bytes des Trust-Snapshots, mit dem geprüft wurde (UTF-8, kanonisch).
    pub trust_snapshot: String,
    /// Die Ref-Updates des Pushes, wie pre-receive sie gelesen hat.
    pub updates: Vec<RefUpdate>,
}

/// Eine Bestätigung des post-receive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirmation {
    /// Bestätigungszeit.
    pub confirmed_at: String,
    /// Ref-Updates, die Git ausgeführt hat (Schnittmenge mit `pending.updates`).
    pub confirmed_updates: Vec<RefUpdate>,
    /// Der Datensatz, unverändert.
    pub pending: PendingRecord,
    /// Immer [`CONFIRMED_SCHEMA`].
    pub schema: String,
}

impl Confirmation {
    /// Genau die Findings, deren Ref-Update bestätigt ist.
    pub fn confirmed_findings(&self) -> Vec<Finding> {
        self.pending
            .findings
            .iter()
            .filter(|f| {
                self.confirmed_updates.iter().any(|u| {
                    u.ref_name == f.push.ref_name && u.old == f.push.old && u.new == f.push.new
                })
            })
            .cloned()
            .collect()
    }
}

/// Gültige Bestätigungen (Name, Inhalt) und die Namen ungültiger.
pub type Confirmations = (Vec<(String, Confirmation)>, Vec<String>);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    mac: String,
    record: T,
}

/// Fehler der Übergabe (feste Texte).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PendingError {
    /// Dateisystem.
    #[error("handoff io: {0}")]
    Io(&'static str),
    /// Eine Datei mit diesem Namen existiert schon.
    #[error("handoff record exists")]
    Exists,
}

/// Die Verzeichnisse unter `state_dir`.
#[derive(Debug, Clone)]
pub struct Handoff {
    root: PathBuf,
    key: [u8; 32],
}

impl Handoff {
    /// Übergabe unter `state_dir` mit dem MAC-Schlüssel aus `seed`.
    pub fn new(state_dir: &Path, seed: &[u8; 32]) -> Self {
        Self {
            root: state_dir.to_owned(),
            key: blake3::derive_key(MAC_CONTEXT, seed),
        }
    }

    fn dir(&self, sub: &str) -> Result<PathBuf, PendingError> {
        let d = self.root.join(sub);
        std::fs::create_dir_all(&d).map_err(|_| PendingError::Io("create dir"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
        }
        Ok(d)
    }

    fn mac<T: Serialize>(&self, record: &T) -> String {
        blake3::keyed_hash(&self.key, &canonical_bytes(record))
            .to_hex()
            .to_string()
    }

    fn write_new<T: Serialize>(
        &self,
        sub: &str,
        name: &str,
        record: &T,
    ) -> Result<(), PendingError> {
        let dir = self.dir(sub)?;
        let bytes = canonical_bytes(&Envelope {
            mac: self.mac(record),
            record,
        });
        let mut tmp =
            tempfile::NamedTempFile::new_in(&dir).map_err(|_| PendingError::Io("temp"))?;
        tmp.write_all(&bytes)
            .and_then(|()| tmp.as_file().sync_all())
            .map_err(|_| PendingError::Io("write"))?;
        match std::fs::hard_link(tmp.path(), dir.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(PendingError::Exists);
            }
            Err(_) => return Err(PendingError::Io("link")),
        }
        sync_dir(&dir);
        Ok(())
    }

    fn read<T: Serialize + DeserializeOwned>(&self, path: &Path) -> Option<T> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        if !meta.is_file() || meta.len() > MAX_RECORD_BYTES {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let env: Envelope<T> = serde_json::from_slice(&bytes).ok()?;
        (self.mac(&env.record) == env.mac).then_some(env.record)
    }

    /// Legt den Datensatz eines angenommenen Pushes an; liefert den Namen.
    pub fn write_pending(&self, record: &PendingRecord) -> Result<String, PendingError> {
        let name = format!("{}-{}.json", record.rp_pid, record.nonce);
        self.write_new("pending", &name, record)?;
        Ok(name)
    }

    /// Verschiebt Datensätze mit dieser `rp_pid` nach `stale/` (Altlasten
    /// eines früheren Prozesses mit derselben PID). Erster Schritt jedes
    /// pre-receive.
    pub fn move_stale(&self, rp_pid: u32) -> Result<Vec<String>, PendingError> {
        let pending = self.dir("pending")?;
        let stale = self.dir("stale")?;
        let prefix = format!("{rp_pid}-");
        let mut moved = Vec::new();
        for e in std::fs::read_dir(&pending)
            .map_err(|_| PendingError::Io("read dir"))?
            .flatten()
        {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) || !name.ends_with(".json") {
                continue;
            }
            // Datensätze aus einem anderen Geltungsbereich (anderer Container
            // mit gemeinsamem Zustandsverzeichnis) gehören nicht dieser PID.
            let foreign = self
                .read::<PendingRecord>(&e.path())
                .and_then(|r| r.rp_start)
                .is_some_and(|s| !crate::process::same_scope(&s));
            if foreign {
                continue;
            }
            std::fs::rename(e.path(), stale.join(&name)).map_err(|_| PendingError::Io("rename"))?;
            moved.push(name);
        }
        if !moved.is_empty() {
            sync_dir(&pending);
            sync_dir(&stale);
        }
        moved.sort();
        Ok(moved)
    }

    /// Datensätze dieses receive-pack-Prozesses mit gültigem `mac`.
    pub fn candidates(
        &self,
        rp_pid: u32,
        rp_start: Option<&str>,
    ) -> Result<Vec<(String, PendingRecord)>, PendingError> {
        let pending = self.dir("pending")?;
        let prefix = format!("{rp_pid}-");
        let mut out = Vec::new();
        for e in std::fs::read_dir(&pending)
            .map_err(|_| PendingError::Io("read dir"))?
            .flatten()
        {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) || !name.ends_with(".json") {
                continue;
            }
            let Some(record) = self.read::<PendingRecord>(&e.path()) else {
                continue;
            };
            // Gleich heißt gleich: Eine bekannte Startzeit passt nie zu einer
            // fehlenden und umgekehrt.
            let start_ok = rp_start == record.rp_start.as_deref();
            if record.rp_pid == rp_pid && record.schema == PENDING_SCHEMA && start_ok {
                out.push((name, record));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Ein Datensatz in `pending/` mit gültigem `mac`, unabhängig von der
    /// Push-Kennung (Diagnose; nie für die Zuordnung verwenden).
    pub fn read_pending(&self, name: &str) -> Option<PendingRecord> {
        let path = self.dir("pending").ok()?.join(name);
        self.read::<PendingRecord>(&path)
    }

    /// Verschiebt einen Datensatz nach `stale/`.
    pub fn to_stale(&self, name: &str) -> Result<(), PendingError> {
        let stale = self.dir("stale")?;
        std::fs::rename(self.dir("pending")?.join(name), stale.join(name))
            .map_err(|_| PendingError::Io("rename"))
    }

    /// Legt die Bestätigung an und entfernt danach den Datensatz. `Ok(false)`:
    /// Die Bestätigung existierte schon (paralleler oder wiederholter Lauf).
    pub fn confirm(&self, name: &str, confirmation: &Confirmation) -> Result<bool, PendingError> {
        match self.write_new("confirmed", name, confirmation) {
            Ok(()) => {}
            Err(PendingError::Exists) => return Ok(false),
            Err(e) => return Err(e),
        }
        let pending = self.dir("pending")?;
        let _ = std::fs::remove_file(pending.join(name));
        sync_dir(&pending);
        Ok(true)
    }

    /// Alle Bestätigungen mit gültigem `mac`; ungültige als Namen getrennt.
    pub fn confirmations(&self) -> Result<Confirmations, PendingError> {
        let dir = self.dir("confirmed")?;
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        for e in std::fs::read_dir(&dir)
            .map_err(|_| PendingError::Io("read dir"))?
            .flatten()
        {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                continue;
            }
            match self.read::<Confirmation>(&e.path()) {
                Some(c) if c.schema == CONFIRMED_SCHEMA => {
                    let at = e
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    ok.push((at, name, c));
                }
                _ => bad.push(name),
            }
        }
        // Am längsten unbearbeitete zuerst; `defer` stellt einen Fehlschlag
        // hinten an, damit er nachfolgende nicht dauerhaft blockiert.
        ok.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        bad.sort();
        Ok((ok.into_iter().map(|(_, n, c)| (n, c)).collect(), bad))
    }

    /// Stellt eine nicht abgelegte Bestätigung in der Reihenfolge von
    /// [`Handoff::confirmations`] hinten an. Der Inhalt bleibt unverändert.
    pub fn defer(&self, name: &str) -> Result<(), PendingError> {
        let dir = self.dir("confirmed")?;
        let f = std::fs::File::options()
            .write(true)
            .open(dir.join(name))
            .map_err(|_| PendingError::Io("open"))?;
        f.set_modified(std::time::SystemTime::now())
            .map_err(|_| PendingError::Io("touch"))
    }

    /// Entfernt eine vollständig abgelegte Bestätigung.
    pub fn remove_confirmation(&self, name: &str) -> Result<(), PendingError> {
        let dir = self.dir("confirmed")?;
        std::fs::remove_file(dir.join(name)).map_err(|_| PendingError::Io("remove"))?;
        sync_dir(&dir);
        Ok(())
    }

    /// Löscht Datensätze in `pending/`, die älter als `ttl` sind, und alles in
    /// `stale/`. Für sie entsteht nie eine Quittung.
    pub fn expire(&self, ttl: std::time::Duration) -> Result<Vec<String>, PendingError> {
        let mut out = Vec::new();
        for (sub, all) in [("pending", false), ("stale", true), ("confirmed", false)] {
            let dir = self.dir(sub)?;
            for e in std::fs::read_dir(&dir)
                .map_err(|_| PendingError::Io("read dir"))?
                .flatten()
            {
                let name = e.file_name().to_string_lossy().into_owned();
                // In `confirmed/` nur Reste abgebrochener Schreibvorgänge,
                // nie Bestätigungen: Die verfallen nicht.
                if sub == "confirmed" && name.ends_with(".json") {
                    continue;
                }
                let old = all
                    || e.metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
                        .is_some_and(|age| age >= ttl);
                if old && std::fs::remove_file(e.path()).is_ok() {
                    out.push(format!("{sub}/{name}"));
                }
            }
            sync_dir(&dir);
        }
        out.sort();
        Ok(out)
    }
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}
