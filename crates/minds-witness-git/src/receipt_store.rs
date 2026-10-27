//! `GitReceiptStore` (Spec S3 §4): Quittungen, Schlüsselliste und
//! Trust-Snapshots als Git-Refs unter `refs/minds/witness/`.
//!
//! - Je Quittung und je Snapshot ein Ref auf einen Commit ohne Eltern mit
//!   genau einer Datei. Angelegt wird nur mit `git update-ref <ref> <neu>
//!   <null>`: Existiert der Ref, scheitert das atomar unter Gits Ref-Sperre,
//!   und der vorhandene Inhalt bleibt unverändert. Danach entscheidet der
//!   Bytevergleich zwischen `AlreadyExists` und `Conflict` (S1-Regel). Ein
//!   vorhandener Ref wird nie überschrieben; es gibt keinen Schreibpfad mit
//!   `--force` oder ohne erwarteten alten Wert.
//! - Die Schlüsselliste ist eine Kette von Commits unter `keys`; fortgeschrieben
//!   wird per Compare-and-Swap (`update-ref <ref> <neu> <alt>`), bei Wettlauf
//!   mit neu gelesenem Stand, höchstens [`CAS_ATTEMPTS`] Versuche.
//! - Objekte entstehen deterministisch (feste Identität und feste Zeit): Ein
//!   Wiederholungslauf mit denselben Bytes erzeugt dieselben Objekte. Ein
//!   abgebrochener Lauf hinterlässt höchstens unreferenzierte Objekte, nie
//!   einen halben Ref; er kann jederzeit wiederholt werden.
//! - Jeder Aufruf ist ein `git`-Prozess mit geleerter Umgebung, ohne
//!   Replace-Refs, ohne Grafts und mit Timeout (wie `rev_list`, Spec S2 §7.3).
//!   Fehlertexte sind feste Sätze; stderr von `git` wird verworfen.
//!
//! Geschrieben wird ausschließlich außerhalb des pre-receive (Spec S3
//! ADR-S3-002): Git verbietet Ref-Updates in der Quarantäne.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use minds_witness_core::{
    CommitId, HashStr, PublicKeyEntry, PutResult, ReceiptKey, ReceiptStore, StoreError,
    TrustSnapshot, canonical_bytes,
};

/// Präfix der Quittungs-Refs.
pub const RECEIPTS_PREFIX: &str = "refs/minds/witness/receipts/";
/// Ref der Schlüsselliste.
pub const KEYS_REF: &str = "refs/minds/witness/keys";
/// Präfix der Trust-Snapshot-Refs.
pub const TRUST_PREFIX: &str = "refs/minds/witness/trust/";
/// Dateiname im Baum einer Quittung.
pub const RECEIPT_FILE: &str = "receipt.json";
/// Dateiname im Baum der Schlüsselliste.
pub const KEYS_FILE: &str = "keys.json";
/// Dateiname im Baum eines Snapshots.
pub const TRUST_FILE: &str = "trust.json";
/// Versuche für die Fortschreibung der Schlüsselliste.
pub const CAS_ATTEMPTS: usize = 3;
/// Höchstgröße einer gelesenen Datei (Quittung, Liste, Snapshot).
pub const MAX_BLOB_BYTES: usize = 1024 * 1024;

/// Erwarteter alter Wert „Ref existiert nicht“ für `update-ref`; leer statt
/// Null-Id, damit es auch in SHA-256-Repositories gilt.
const MUST_NOT_EXIST: &str = "";
/// Ab diesem Alter gilt eine Sperrdatei unter `refs/minds/witness/` als
/// verwaist (weit über jedem Prozess-Timeout).
pub const STALE_LOCK_AGE: Duration = Duration::from_secs(300);
/// Lesungen, bevor ein roh vorhandener, aber unlesbarer Ref als kaputt gilt.
const RESOLVE_ATTEMPTS: usize = 3;
/// Sperre, die `clear_stale_locks` gegen sich selbst serialisiert.
const CLEAR_GUARD: &str = "minds-witness-clear-locks.lock";
#[cfg(not(windows))]
const NO_FILE: &str = "/dev/null";
#[cfg(windows)]
const NO_FILE: &str = "NUL";
const IDENTITY_NAME: &str = "minds-witness";
const IDENTITY_EMAIL: &str = "witness@minds.invalid";
const FIXED_DATE: &str = "1970-01-01T00:00:00Z";

fn internal(what: &'static str) -> StoreError {
    StoreError::Internal(what.to_owned())
}

/// Ablage in den Refs eines Repositorys.
#[derive(Debug, Clone)]
pub struct GitReceiptStore {
    git: PathBuf,
    git_dir: PathBuf,
    timeout: Duration,
}

struct Run {
    code: Option<i32>,
    stdout: Vec<u8>,
}

impl GitReceiptStore {
    /// Eine Ablage im Repository `git_dir`, geschrieben mit dem Programm
    /// `git` (absoluter Pfad aus der Konfiguration); jeder Prozess endet
    /// spätestens nach `timeout`.
    /// Der Timeout wird auf höchstens ein Viertel von [`STALE_LOCK_AGE`]
    /// begrenzt: So stammt eine Sperrdatei, die älter als `STALE_LOCK_AGE`
    /// ist, sicher von einem beendeten Prozess (`clear_stale_locks`).
    pub fn new(git: &Path, git_dir: &Path, timeout: Duration) -> Self {
        Self {
            git: git.to_owned(),
            git_dir: git_dir.to_owned(),
            timeout: timeout.min(STALE_LOCK_AGE / 4),
        }
    }

    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Run, StoreError> {
        let mut cmd = Command::new(&self.git);
        cmd.env_clear();
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SYSTEMROOT") {
            cmd.env("SYSTEMROOT", root);
        }
        cmd.env("GIT_DIR", &self.git_dir)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_GRAFT_FILE", NO_FILE)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_AUTHOR_NAME", IDENTITY_NAME)
            .env("GIT_AUTHOR_EMAIL", IDENTITY_EMAIL)
            .env("GIT_AUTHOR_DATE", FIXED_DATE)
            .env("GIT_COMMITTER_NAME", IDENTITY_NAME)
            .env("GIT_COMMITTER_EMAIL", IDENTITY_EMAIL)
            .env("GIT_COMMITTER_DATE", FIXED_DATE)
            // Keine Repository-Hooks (etwa `reference-transaction`) in den
            // Ablage-Prozessen: Sie liefen sonst unter dem Timeout, und ein
            // Abbruch hinterließe Sperrdateien.
            .args(["-c", &format!("core.hooksPath={NO_FILE}")])
            .args(["-c", "advice.graftFileDeprecated=false"])
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|_| internal("git not startable"))?;
        if let Some(input) = stdin {
            let mut pipe = child.stdin.take().ok_or_else(|| internal("git stdin"))?;
            let data = input.to_vec();
            std::thread::spawn(move || {
                let _ = pipe.write_all(&data);
            });
        }
        let mut stdout = child.stdout.take().ok_or_else(|| internal("git stdout"))?;
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout
                .by_ref()
                .take(MAX_BLOB_BYTES as u64 * 4)
                .read_to_end(&mut out);
            out
        });
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(internal("git timeout"));
                }
            }
        };
        let stdout = reader.join().map_err(|_| internal("git reader"))?;
        Ok(Run {
            code: status.code(),
            stdout,
        })
    }

    fn ok(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<String, StoreError> {
        let run = self.run(args, stdin)?;
        if run.code != Some(0) {
            return Err(internal("git failed"));
        }
        String::from_utf8(run.stdout)
            .map(|s| s.trim_end().to_owned())
            .map_err(|_| internal("git output"))
    }

    /// Ziel eines Refs (Commit-Id) oder `None`, streng: Ein symbolischer Ref
    /// (auch ein ins Leere zeigender), ein Ref auf etwas anderes als einen
    /// Commit (auch ein Tag) und ein kaputter Ref, den `for-each-ref`
    /// übergeht, sind technische Fehler, nie „nicht vorhanden“.
    fn resolve(&self, name: &str) -> Result<Option<String>, StoreError> {
        // Ein paralleler Prozess kann den Ref zwischen den Lesungen anlegen;
        // „kaputt“ gilt erst, wenn er mehrmals hintereinander roh vorhanden,
        // für Git aber nicht lesbar ist.
        for attempt in 0..RESOLVE_ATTEMPTS {
            if let Some(found) = self.resolve_once(name)? {
                return Ok(Some(found));
            }
            if !self.raw_exists(name)? {
                return Ok(None);
            }
            if attempt + 1 < RESOLVE_ATTEMPTS {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        Err(internal("broken witness ref"))
    }

    fn resolve_once(&self, name: &str) -> Result<Option<String>, StoreError> {
        // Exit 0 auch für einen ins Leere zeigenden Symref; ohne
        // `--no-recurse`, damit es mit Git ≥ 2.30 (S2 §12) läuft.
        let sym = self.run(&["symbolic-ref", "-q", name], None)?;
        match sym.code {
            Some(0) => return Err(internal("symbolic witness ref")),
            Some(1) => {}
            _ => return Err(internal("git symbolic-ref failed")),
        }
        // `for-each-ref <muster>` trifft auch Refs unterhalb; nur der exakte
        // Name zählt.
        let out = self.ok(
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(objecttype)",
                name,
            ],
            None,
        )?;
        for line in out.lines() {
            let mut parts = line.split('\0');
            let (Some(refname), Some(oid), Some(kind)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(internal("git for-each-ref output"));
            };
            if refname != name {
                continue;
            }
            if kind != "commit" || !is_oid(oid) {
                return Err(internal("malformed witness ref"));
            }
            return Ok(Some(oid.to_owned()));
        }
        Ok(None)
    }

    /// Ob unter `name` irgendetwas liegt (lose Datei oder Zeile in
    /// `packed-refs`), unabhängig davon, ob Git es lesen kann.
    fn raw_exists(&self, name: &str) -> Result<bool, StoreError> {
        if std::fs::symlink_metadata(self.git_dir.join(name)).is_ok() {
            return Ok(true);
        }
        match std::fs::read(self.git_dir.join("packed-refs")) {
            Ok(bytes) => Ok(bytes.split(|b| *b == b'\n').any(|line| {
                line.ends_with(name.as_bytes())
                    && line.len() > name.len()
                    && line[line.len() - name.len() - 1] == b' '
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(internal("packed-refs unreadable")),
        }
    }

    /// Entfernt verwaiste Sperrdateien unter `refs/minds/witness/` (älter als
    /// `max_age`), die ein abgebrochener Ablageprozess hinterlassen hat; sie
    /// blockierten sonst jede Wiederholung. Liefert die entfernten Ref-Namen.
    /// Aufgerufen von `reconcile` (Spec S3 §3.3), nie während einer Ablage.
    pub fn clear_stale_locks(&self, max_age: Duration) -> Result<Vec<String>, StoreError> {
        // Nie jünger als STALE_LOCK_AGE: Eine jüngere Sperre könnte ein
        // lebender Prozess halten (Timeout ≤ STALE_LOCK_AGE / 4, siehe `new`).
        let max_age = max_age.max(STALE_LOCK_AGE);
        // Gegen sich selbst serialisiert: Zwei gleichzeitige Läufe könnten
        // sonst beide ein altes Datum sehen, und der zweite entfernte eine
        // inzwischen frisch angelegte, lebende Sperre.
        let guard = self.git_dir.join(CLEAR_GUARD);
        if std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&guard)
            .is_err()
        {
            if is_older(&guard, max_age) {
                // Ein abgestürzter Lauf hinterließ die Sperre: übernehmen.
                let _ = std::fs::remove_file(&guard);
                if std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&guard)
                    .is_err()
                {
                    return Ok(Vec::new());
                }
            } else {
                return Ok(Vec::new());
            }
        }
        let result = self.clear_locked(max_age);
        let _ = std::fs::remove_file(&guard);
        result
    }

    fn clear_locked(&self, max_age: Duration) -> Result<Vec<String>, StoreError> {
        let root = self.git_dir.join("refs/minds/witness");
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if meta.is_dir() {
                    stack.push(path);
                    continue;
                }
                let is_lock = path.extension().is_some_and(|x| x == "lock");
                // Das Alter wird unmittelbar vor dem Entfernen erneut geprüft.
                if is_lock && meta.is_file() && is_older(&path, max_age) {
                    std::fs::remove_file(&path)
                        .map_err(|_| internal("stale lock not removable"))?;
                    let rel = path
                        .strip_prefix(&self.git_dir)
                        .map(|p| p.to_string_lossy().trim_end_matches(".lock").to_owned())
                        .unwrap_or_default();
                    out.push(rel);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Commit mit genau einer Datei `file` (Inhalt `bytes`) und den Eltern
    /// `parents`; liefert die Commit-Id.
    fn write_commit(
        &self,
        file: &str,
        bytes: &[u8],
        parents: &[&str],
        message: &str,
    ) -> Result<String, StoreError> {
        let blob = self.ok(&["hash-object", "-w", "--stdin"], Some(bytes))?;
        let entry = format!("100644 blob {blob}\t{file}\n");
        let tree = self.ok(&["mktree"], Some(entry.as_bytes()))?;
        let mut args = vec!["commit-tree", tree.as_str()];
        for p in parents {
            args.push("-p");
            args.push(p);
        }
        args.push("-m");
        args.push(message);
        let commit = self.ok(&args, None)?;
        if !is_oid(&commit) {
            return Err(internal("git commit-tree output"));
        }
        Ok(commit)
    }

    /// Liest die einzige Datei `file` aus `commit` streng: Der Baum enthält
    /// genau diesen Eintrag als reguläre Datei; die Elternzahl ist `parents`
    /// (`None` = beliebig).
    fn read_commit(
        &self,
        commit: &str,
        file: &str,
        parents: Option<usize>,
    ) -> Result<Vec<u8>, StoreError> {
        let header = self.ok(&["cat-file", "commit", commit], None)?;
        let count = header
            .lines()
            .take_while(|l| !l.is_empty())
            .filter(|l| l.starts_with("parent "))
            .count();
        if parents.is_some_and(|p| p != count) {
            return Err(internal("malformed witness commit"));
        }
        let tree = self.ok(&["ls-tree", "-z", "--full-tree", commit], None)?;
        let entries: Vec<&str> = tree.split('\0').filter(|e| !e.is_empty()).collect();
        let [entry] = entries.as_slice() else {
            return Err(internal("malformed witness tree"));
        };
        let (meta, name) = entry
            .split_once('\t')
            .ok_or_else(|| internal("malformed witness tree"))?;
        let mut meta = meta.split(' ');
        let (Some("100644"), Some("blob"), Some(oid)) = (meta.next(), meta.next(), meta.next())
        else {
            return Err(internal("malformed witness tree"));
        };
        if name != file || !is_oid(oid) {
            return Err(internal("malformed witness tree"));
        }
        let run = self.run(&["cat-file", "blob", oid], None)?;
        if run.code != Some(0) || run.stdout.len() > MAX_BLOB_BYTES {
            return Err(internal("witness blob unreadable"));
        }
        Ok(run.stdout)
    }

    /// Legt `name` auf `commit` an, nur wenn er nicht existiert. `Ok(true)`:
    /// angelegt; `Ok(false)`: existierte schon (unverändert).
    fn create_only(&self, name: &str, commit: &str) -> Result<bool, StoreError> {
        let run = self.run(
            &["update-ref", "--no-deref", name, commit, MUST_NOT_EXIST],
            None,
        )?;
        if run.code == Some(0) {
            return Ok(true);
        }
        // Gescheitert: entweder existiert der Ref (dann entscheidet der
        // Inhalt), oder Git konnte nicht schreiben (Sperre, Platte): dann
        // technischer Fehler, später wiederholbar.
        match self.resolve(name)? {
            Some(_) => Ok(false),
            None => Err(internal("ref not writable")),
        }
    }

    /// Create-only-Ablage einer Datei unter `name` mit S1-Konfliktregel.
    fn put_single(
        &self,
        name: &str,
        file: &str,
        bytes: &[u8],
        conflict: &str,
    ) -> Result<PutResult, StoreError> {
        if bytes.len() > MAX_BLOB_BYTES {
            return Err(internal("object too large"));
        }
        if let Some(existing) = self.resolve(name)? {
            return self.compare(&existing, file, bytes, conflict);
        }
        let commit = self.write_commit(file, bytes, &[], "minds-witness")?;
        if self.create_only(name, &commit)? {
            return Ok(PutResult::Stored);
        }
        let existing = self
            .resolve(name)?
            .ok_or_else(|| internal("ref vanished"))?;
        self.compare(&existing, file, bytes, conflict)
    }

    fn compare(
        &self,
        existing: &str,
        file: &str,
        bytes: &[u8],
        conflict: &str,
    ) -> Result<PutResult, StoreError> {
        if self.read_commit(existing, file, Some(0))? == bytes {
            Ok(PutResult::AlreadyExists)
        } else {
            Err(StoreError::Conflict(conflict.to_owned()))
        }
    }

    fn get_single(&self, name: &str, file: &str) -> Result<Option<Vec<u8>>, StoreError> {
        match self.resolve(name)? {
            Some(commit) => self.read_commit(&commit, file, Some(0)).map(Some),
            None => Ok(None),
        }
    }

    /// Ref-Name einer Quittung.
    pub fn receipt_ref(key: &ReceiptKey) -> String {
        format!(
            "{RECEIPTS_PREFIX}{}/{}",
            key.commit.as_str(),
            hex_of(&key.id)
        )
    }

    /// Ref-Name eines Snapshots.
    pub fn trust_ref(hash: &HashStr) -> String {
        format!("{TRUST_PREFIX}{}", hex_of(hash))
    }

    /// Legt einen Trust-Snapshot ab (create-only). Die Bytes müssen ein
    /// gültiger, kanonischer Snapshot sein; der Ref-Name ist ihr Hash.
    /// Existiert der Ref mit anderen Bytes: `Conflict`, nichts wird geändert.
    pub fn put_trust(&self, bytes: &[u8]) -> Result<(HashStr, PutResult), StoreError> {
        TrustSnapshot::from_bytes(bytes).map_err(|_| internal("trust snapshot invalid"))?;
        let hash = HashStr::of_bytes(bytes);
        let result = self.put_single(&Self::trust_ref(&hash), TRUST_FILE, bytes, hash.as_str())?;
        Ok((hash, result))
    }

    /// Bytes des Snapshots mit diesem Hash, sofern vorhanden. Die Bytes
    /// müssen zum Hash passen, sonst technischer Fehler.
    pub fn get_trust(&self, hash: &HashStr) -> Result<Option<Vec<u8>>, StoreError> {
        let bytes = self.get_single(&Self::trust_ref(hash), TRUST_FILE)?;
        if let Some(b) = &bytes
            && HashStr::of_bytes(b) != *hash
        {
            return Err(internal("trust snapshot does not match its name"));
        }
        Ok(bytes)
    }

    fn read_keys(&self) -> Result<(Option<String>, Vec<PublicKeyEntry>), StoreError> {
        let Some(commit) = self.resolve(KEYS_REF)? else {
            return Ok((None, Vec::new()));
        };
        let bytes = self.read_commit(&commit, KEYS_FILE, None)?;
        let keys: Vec<PublicKeyEntry> =
            serde_json::from_slice(&bytes).map_err(|_| internal("keys unreadable"))?;
        if canonical_bytes(&keys) != bytes {
            return Err(internal("keys not canonical"));
        }
        Ok((Some(commit), keys))
    }
}

impl ReceiptStore for GitReceiptStore {
    fn put(&mut self, key: &ReceiptKey, bytes: &[u8]) -> Result<PutResult, StoreError> {
        self.put_single(
            &Self::receipt_ref(key),
            RECEIPT_FILE,
            bytes,
            key.id.as_str(),
        )
    }

    fn get(&self, key: &ReceiptKey) -> Result<Option<Vec<u8>>, StoreError> {
        self.get_single(&Self::receipt_ref(key), RECEIPT_FILE)
    }

    fn list(&self, commit: &CommitId) -> Result<Vec<ReceiptKey>, StoreError> {
        let prefix = format!("{RECEIPTS_PREFIX}{}/", commit.as_str());
        // Kaputte Refs übergeht `for-each-ref` still; jeder lose Eintrag
        // unter dem Präfix muss daher auch gelistet sein. Ein paralleler
        // Prozess kann zwischen den Lesungen anlegen: erst nach mehreren
        // Versuchen gilt eine Abweichung als kaputter Ref.
        let mut names = String::new();
        let mut consistent = false;
        for attempt in 0..RESOLVE_ATTEMPTS {
            names = self.ok(&["for-each-ref", "--format=%(refname)", &prefix], None)?;
            let listed: Vec<&str> = names.lines().collect();
            let mut missing = false;
            if let Ok(entries) = std::fs::read_dir(self.git_dir.join(&prefix)) {
                for e in entries.flatten() {
                    let file = e.file_name();
                    let file = file.to_string_lossy();
                    if file.ends_with(".lock") {
                        continue;
                    }
                    if !listed
                        .iter()
                        .any(|n| n.strip_prefix(&prefix) == Some(&*file))
                    {
                        missing = true;
                    }
                }
            }
            if !missing {
                consistent = true;
                break;
            }
            if attempt + 1 < RESOLVE_ATTEMPTS {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if !consistent {
            return Err(internal("broken receipt ref"));
        }
        let listed: Vec<&str> = names.lines().collect();
        let mut out = Vec::new();
        for name in listed {
            let Some(hex) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Ok(id) = HashStr::parse(&format!("blake3:{hex}")) else {
                return Err(internal("malformed receipt ref"));
            };
            out.push(ReceiptKey {
                commit: commit.clone(),
                id,
            });
        }
        out.sort();
        Ok(out)
    }

    fn public_keys(&self) -> Result<Vec<PublicKeyEntry>, StoreError> {
        self.read_keys().map(|(_, keys)| keys)
    }

    fn register_public_key(&mut self, entry: &PublicKeyEntry) -> Result<(), StoreError> {
        for _ in 0..CAS_ATTEMPTS {
            let (old, mut keys) = self.read_keys()?;
            // S1 §11: gleicher Schlüssel mit gleichem `valid_from` genügt;
            // gleicher Schlüssel, nur andere `valid_from`: Konflikt.
            let same: Vec<&PublicKeyEntry> = keys.iter().filter(|k| k.key == entry.key).collect();
            if same.iter().any(|k| k.valid_from == entry.valid_from) {
                return Ok(());
            }
            if !same.is_empty() {
                return Err(StoreError::Conflict(entry.key.as_str().to_owned()));
            }
            // Wie `MemoryAdapter`: der Eintrag genau wie übergeben (S1 ruft
            // nur mit `valid_until = None` auf); vorhandene Einträge, auch
            // zurückgezogene, bleiben unverändert.
            keys.push(entry.clone());
            let bytes = canonical_bytes(&keys);
            if bytes.len() > MAX_BLOB_BYTES {
                return Err(internal("keys list too large"));
            }
            let parents: Vec<&str> = old.iter().map(String::as_str).collect();
            let commit = self.write_commit(KEYS_FILE, &bytes, &parents, "minds-witness keys")?;
            let expected = old.as_deref().unwrap_or(MUST_NOT_EXIST);
            let run = self.run(
                &["update-ref", "--no-deref", KEYS_REF, &commit, expected],
                None,
            )?;
            if run.code == Some(0) {
                return Ok(());
            }
            // Wettlauf (oder Sperre): neu lesen und erneut versuchen.
        }
        Err(internal("keys update contended"))
    }
}

fn is_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn hex_of(hash: &HashStr) -> &str {
    hash.as_str()
        .split_once(':')
        .map_or(hash.as_str(), |(_, h)| h)
}

fn is_older(path: &Path, age: Duration) -> bool {
    std::fs::symlink_metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
        .is_some_and(|a| a >= age)
}
