//! Ein Witness besitzt genau ein Repository und genau einen Schreiber.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use minds_capture::epoch::EpochState;
use minds_capture::{
    Journal, JournalEvent, NewEvent, SessionKey, chain, clock, hook_event, secretwall,
};
use minds_core::evidence::{
    ChainFolder, ChainItem, ChainResult, FolderState, GapRecord, SealSummary,
};
use serde::{Deserialize, Serialize};

use super::{Fallible, private_directory};
use crate::checkpoint::core::{
    CheckpointEnv, CheckpointGuard, EvidenceSource, SealSigner, run_checkpoint_guarded,
};

mod observer;
mod socket;
#[cfg(test)]
mod tests;
pub use socket::run;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WitnessProfile {
    Container,
    User,
    Managed,
}

impl WitnessProfile {
    fn name(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::User => "user",
            Self::Managed => "managed",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Config {
    schema_version: u32,
    repo_root: PathBuf,
    path_map: Vec<(PathBuf, PathBuf)>,
    profile: WitnessProfile,
    #[serde(default)]
    socket_group: Option<u32>,
}

const DIRECTORIES: &[&str] = &[
    "journal",
    "evidence",
    "evidence/state",
    "evidence/folders",
    "key",
    "log",
    "run",
];

pub fn init(home: &Path, repo: &str, mapping: Option<&str>) -> Fallible<String> {
    let home: PathBuf = home.components().collect();
    let output = Command::new("git")
        .args(["-C", repo, "rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err("--repo must identify a Git worktree".into());
    }
    let repo_root = Path::new(String::from_utf8(output.stdout)?.trim()).canonicalize()?;
    let path_map = if let Some(mapping) = mapping {
        let (agent, host) = mapping
            .split_once('=')
            .ok_or("path map must be <agent>=<host>")?;
        if !Path::new(agent).is_absolute() || !plain(Path::new(agent)) {
            return Err("agent path must be absolute without parent components".into());
        }
        let host = Path::new(host).canonicalize()?;
        if host != repo_root {
            return Err("path map must target the repository root".into());
        }
        vec![(PathBuf::from(agent), host)]
    } else {
        Vec::new()
    };
    private_directory(&home)?;
    validate_tree(&home)?;
    for dir in DIRECTORIES {
        private_directory(&home.join(dir))?;
    }
    let _lock = lock(&home)?;
    let config = Config {
        schema_version: 1,
        repo_root,
        profile: if path_map.is_empty() {
            WitnessProfile::User
        } else {
            WitnessProfile::Container
        },
        path_map,
        socket_group: None,
    };
    let mut file = private_new(&home.join("witness.json"))?;
    file.write_all(&serde_json::to_vec_pretty(&config)?)?;
    file.sync_all()?;
    let ledger = private_append(&home.join("ledger"))?;
    ledger.sync_all()?;
    sync_dir(&home)?;
    Ok("Witness initialized. Run `minds witness keygen` before `minds witness run`.".into())
}

/// Liegt das Witness-Home im beobachteten Repo, sähe der Beobachter die
/// eigenen Schreibzugriffe — und der Agent das Home. Der Witness startet
/// dann nicht (EA-08).
fn refuse_home_in_repo(home: &Path, config: &Config) -> Fallible<()> {
    if home.canonicalize()?.starts_with(&config.repo_root) {
        return Err("witness home must not be inside the repository".into());
    }
    Ok(())
}

fn plain(path: &Path) -> bool {
    !path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
}

fn load(home: &Path) -> Fallible<Config> {
    let home: PathBuf = home.components().collect();
    validate_tree(&home)?;
    let config: Config = serde_json::from_slice(&fs::read(home.join("witness.json"))?)?;
    if config.schema_version != 1
        || !config.repo_root.is_absolute()
        || config.repo_root.canonicalize()? != config.repo_root
    {
        return Err("unsupported or invalid witness configuration".into());
    }
    for (agent, host) in &config.path_map {
        if !agent.is_absolute() || !plain(agent) || host != &config.repo_root {
            return Err("invalid witness path map".into());
        }
    }
    Ok(config)
}

/// Private Zustandsbäume enthalten weder Symlinks noch fremde/hart verlinkte
/// Dateien. Der Socket ist die einzige Ausnahme von 0700/0600.
fn validate_tree(path: &Path) -> Fallible<()> {
    let meta = fs::symlink_metadata(path)?;
    // SAFETY: geteuid hat keine Vorbedingungen.
    if meta.uid() != unsafe { libc::geteuid() } || meta.file_type().is_symlink() {
        return Err("witness home must be owned by this user and contain no symlinks".into());
    }
    if meta.file_type().is_socket() {
        if meta.mode() & 0o007 != 0 {
            return Err("insecure witness socket permissions".into());
        }
        return Ok(());
    }
    if meta.mode() & 0o077 != 0 || (!meta.is_dir() && (!meta.is_file() || meta.nlink() != 1)) {
        return Err("witness home requires private directories and files (0700/0600)".into());
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            match validate_tree(&entry?.path()) {
                // Der laufende Schreiber benennt um und verwirft Sessions; ein
                // inzwischen verschwundener Eintrag ist kein unsicherer.
                Err(err)
                    if err
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
                result => result?,
            }
        }
    }
    Ok(())
}

fn private_new(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

fn private_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

fn atomic(path: &Path, bytes: &[u8]) -> Fallible<()> {
    let dir = path.parent().ok_or("missing parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    sync_dir(dir)?;
    Ok(())
}

/// flock bleibt bis zum Prozessende gültig, auch bei SIGKILL. Die Lockdatei
/// wird nie entfernt: ein zweiter Inode wäre ein zweiter Schreiber.
fn lock(home: &Path) -> Fallible<File> {
    let file = private_append(&home.join("run/lock"))?;
    // SAFETY: gültiger eigener Dateideskriptor, keine Pointer.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("witness home is already in use".into());
    }
    Ok(file)
}

fn fingerprint(home: &Path) -> Fallible<String> {
    let key = home.join("key/witness_ed25519");
    validate_tree(&key)?;
    let output = Command::new("ssh-keygen")
        .args(["-l", "-E", "sha256", "-f"])
        .arg(&key)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err("cannot read witness key fingerprint".into());
    }
    let text = String::from_utf8(output.stdout)?;
    let fingerprint = text
        .split_whitespace()
        .nth(1)
        .ok_or("missing key fingerprint")?;
    if !fingerprint.starts_with("SHA256:") {
        return Err("invalid key fingerprint".into());
    }
    Ok(fingerprint.to_owned())
}

fn log(home: &Path, message: &str) {
    // Kein fremder Payload wird als Diagnose übergeben. Auch vertrauenswürdige
    // Fehlertexte durchlaufen dieselbe Redaktion wie hook.log.
    let path = home.join("log/witness.log");
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() >= 1024 * 1024) {
        let _ = fs::rename(&path, home.join("log/witness.log.1"));
    }
    if let Ok(mut file) = private_append(&path) {
        let _ = writeln!(
            file,
            "{} witness: {}",
            clock::now().0,
            crate::hooklog::diagnostic(message)
        );
    }
}

/// Die Uhr des Witness (RFC 3339, Unix-Nanos).
pub(super) fn now() -> (String, u64) {
    clock::now()
}

fn folder_path(home: &Path, key: &SessionKey) -> PathBuf {
    let digest = blake3::hash(format!("{}\0{}", key.agent(), key.local_id()).as_bytes());
    home.join("evidence/folders")
        .join(format!("{}.json", digest.to_hex()))
}

struct Writer {
    home: PathBuf,
    config: Config,
    journal: Journal,
    epochs: EpochState,
    folders: BTreeMap<SessionKey, ChainFolder>,
    follow: bool,
    /// Ob seit dem letzten vollständigen Checkpoint ein Agent-Event kam oder
    /// eine Session vertagt blieb. Ohne das gibt es nichts zu versiegeln.
    dirty: bool,
    /// Ende des letzten Checkpoint-Laufs auf Anfrage, gleich mit welchem
    /// Ausgang.
    last_run: Option<std::time::Instant>,
    /// Der zuletzt geloggte Fehlschlag und wie oft er sich seither still
    /// wiederholt hat — ein erzwungener Fehlschlag je Sekunde darf ältere
    /// Diagnosen nicht aus dem rotierenden Log drängen.
    last_failure: Option<(String, usize)>,
    /// Der eigene Stream dieses Laufs (`agent = "witness"`): Lebenszyklus,
    /// `fs.observed` und `fs.gap`.
    stream: Option<SessionKey>,
    /// Das zweite Auge (EA-08); `None`, solange `run` es nicht startet.
    observer: Option<observer::Observer>,
    /// Ob seit dem letzten Versiegeln eine Datei-Beobachtung kam.
    fs_dirty: bool,
    /// Ob ein Scheitern der Ignore-Auswertung schon geloggt wurde.
    ignore_failure_logged: bool,
    /// Klassifizierte Beobachtungen, die noch ins Journal müssen.
    fs_ready: std::collections::VecDeque<observer::Seen>,
    /// Wann je Grund zuletzt ein `fs.gap` angehängt wurde: höchstens eines
    /// je Grund und Sekunde — eine dauerhafte Störung füllt das Journal
    /// nicht im Takt der Eventloop.
    last_gap: std::collections::HashMap<&'static str, std::time::Instant>,
    /// Ob in diesem Durchlauf der Eventloop schon für einen Checkpoint
    /// ohne Entprellung beobachtet wurde. Eine Flut von Anfragen in einem
    /// Durchlauf kostet so höchstens einmal das Budget.
    settled_this_step: bool,
}

/// Höchstzahl der `fs.observed`-Appends je Durchlauf der Eventloop.
const FS_APPENDS_PER_STEP: usize = 64;

/// Höchstzahl der `fs.observed`-Appends zu Beginn eines Checkpoints — auch
/// eine Flut von Anfragen hält die Eventloop nicht länger an. Was darüber
/// hinaus wartet, kommt in die nächste Epoche.
const FS_APPENDS_ON_CHECKPOINT: usize = 256;

/// Mindestabstand zwischen zwei versiegelnden Checkpoints auf Anfrage der
/// Agent-Seite. Ohne ihn könnte der Agent zwischen je zwei Events einen
/// Checkpoint anfordern — jedes Event ein eigener Bereich, Store, Ledger und
/// Signaturen wüchsen ohne Grenze, und die Eventloop stünde für Hooks still.
const MIN_CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

impl Writer {
    fn open(home: &Path, config: Config, follow: bool) -> Fallible<Self> {
        let mut writer = Self {
            home: home.into(),
            config,
            journal: Journal::at(home.join("journal")),
            epochs: EpochState::at(home.join("evidence/state")),
            folders: BTreeMap::new(),
            follow,
            dirty: true,
            last_run: None,
            last_failure: None,
            stream: None,
            observer: None,
            fs_dirty: false,
            ignore_failure_logged: false,
            fs_ready: std::collections::VecDeque::new(),
            settled_this_step: false,
            last_gap: std::collections::HashMap::new(),
        };
        writer.recover_all()?;
        Ok(writer)
    }

    /// Der Journal-Fsync kommt vor dem Folder-Fsync. Nach einem Crash darf
    /// ausschließlich ein intakter, bereits persistierter Präfix ergänzt werden.
    fn recover(&mut self, key: &SessionKey) -> Fallible<()> {
        let read = self.journal.read(key)?;
        verify_events(&read.events)?;
        let items = base_items(&read);
        let path = folder_path(&self.home, key);
        let saved = match fs::read(&path) {
            Ok(bytes) => Some(ChainFolder::from_state(serde_json::from_slice::<
                FolderState,
            >(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let mut folder = ChainFolder::new_salted(&self.epochs.salt(key)?);
        let mut matched = saved
            .as_ref()
            .is_none_or(|saved| saved.snapshot() == folder.snapshot());
        for item in &items {
            folder.push(item);
            matched |= saved
                .as_ref()
                .is_some_and(|saved| saved.snapshot() == folder.snapshot());
        }
        if items.is_empty()
            && saved
                .as_ref()
                .is_some_and(|s| s.snapshot().coverage.events > 0)
        {
            let sealed = fs::read_to_string(path.with_extension("sealed"))?;
            if saved.as_ref().is_none_or(|s| s.head().as_str() != sealed) {
                return Err("integrity error: journal disappeared before sealing".into());
            }
        }
        if !matched && !items.is_empty() {
            return Err("integrity error: persisted chain is not a journal prefix".into());
        }
        atomic(&path, &serde_json::to_vec(&folder.to_state())?)?;
        self.folders.insert(key.clone(), folder);
        Ok(())
    }

    fn append(&mut self, key: &SessionKey, event: NewEvent) -> Fallible<JournalEvent> {
        if !self.folders.contains_key(key) {
            self.recover(key)?;
        }
        let event = self.journal.append(key, event)?;
        if self.includes(key) {
            self.dirty = true;
        }
        let hash = event.event_hash.clone().ok_or("missing event hash")?;
        let folder = self.folders.get_mut(key).ok_or("missing chain folder")?;
        let seen = folder.snapshot().coverage;
        let started = seen.events + seen.pre_chain > 0;
        // Dieselben Glieder in derselben Reihenfolge wie `chain::items`:
        // Fehlende Nummern zwischen zwei Events sind genau ein Gap-Record.
        // Das gilt für das Journal dieses Schreibers. Füllt jemand die Lücke
        // später von außen, widerspricht der Record dem Journal absichtlich,
        // und `validate`/`recover` lehnen fail-closed ab.
        if started && event.seq > seen.last_seq + 1 {
            folder.push(&ChainItem::Gap(GapRecord::Missing {
                from: seen.last_seq + 1,
                to: event.seq - 1,
            }));
        }
        if started && event.seq <= seen.last_seq {
            // Ein einzelner Schreiber vergibt nie eine Nummer unter dem Head;
            // das füllt nur ein gelöschtes Event wieder auf. Kein Neuaufbau aus
            // dem Journal: Das würde die Löschung waschen. Der Fold bleibt in
            // Empfangsreihenfolge, der Checkpoint dieser Session wird vertagt.
            log(
                &self.home,
                "integrity error: sequence number below chain head; checkpoint deferred",
            );
        }
        folder.push(&ChainItem::Event {
            seq: event.seq,
            hash,
        });
        atomic(
            &folder_path(&self.home, key),
            &serde_json::to_vec(&folder.to_state())?,
        )?;
        if self.follow {
            // Broken stdout darf den Schreiber nicht panicken lassen.
            let _ = writeln!(
                std::io::stdout().lock(),
                "{}",
                follow_line(key, &event, folder)
            );
        }
        Ok(event)
    }

    fn hook(
        &mut self,
        agent: &str,
        event_override: Option<&str>,
        stdin: Vec<u8>,
        at: (String, u64),
    ) -> Fallible<()> {
        if agent.eq_ignore_ascii_case("witness") {
            log(&self.home, "reserved agent rejected");
            return Ok(());
        }
        let mut parsed = match hook_event::parse(stdin, agent, event_override, at) {
            Ok(parsed) => parsed,
            Err(_) => {
                log(&self.home, "invalid hook dropped");
                return Ok(());
            }
        };
        secretwall::guard(&mut parsed.event);
        self.append(&parsed.key, parsed.event)?;
        Ok(())
    }

    fn lifecycle(
        &mut self,
        key: &SessionKey,
        kind: &str,
        payload: serde_json::Value,
    ) -> Fallible<()> {
        let at = clock::now();
        self.append(
            key,
            NewEvent {
                at: at.0,
                at_nanos: at.1,
                kind: minds_capture::EventKind::Other,
                raw_kind: kind.into(),
                cwd: None,
                transcript_path: None,
                payload: serde_json::value::RawValue::from_string(payload.to_string())?,
            },
        )?;
        Ok(())
    }

    /// Startet die Datei-Beobachtung in den eigenen Stream `stream`.
    fn observe_into(&mut self, stream: SessionKey, observer: observer::Observer) {
        self.stream = Some(stream);
        self.observer = Some(observer);
    }

    /// Ein Durchlauf des Beobachters: beruhigte (mit `settle` alle) Pfade
    /// lesen und als `fs.observed` an den eigenen Stream hängen. Verlorene
    /// Meldungen werden ein `fs.gap`. `Err` ist ausschließlich ein Fehler
    /// des eigenen Speichers.
    fn observe(&mut self, settle: bool) -> Fallible<()> {
        let (Some(observer), Some(stream)) = (self.observer.as_mut(), self.stream.clone()) else {
            return Ok(());
        };
        // Gebremst wird vor dem Klassifizieren: Wartet schon eine volle
        // Ladung auf das Journal, bleibt der Rest in der begrenzten Sammlung
        // des Beobachters — und läuft die über, wird das eine Lücke.
        let batch = if self.fs_ready.len() < observer::MAX_PENDING {
            // Agent-bestimmte Bytes (Index, `.gitignore`) laufen durch gix:
            // Ein Panic darin darf den einzigen Schreiber nicht beenden.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                observer.tick(std::time::Instant::now(), settle)
            })) {
                Ok(batch) => batch,
                Err(_) => {
                    observer.forget_caches();
                    self.gap(&stream, "panic", "file observer panicked; gap recorded")?;
                    observer::Batch::default()
                }
            }
        } else {
            observer::Batch::default()
        };
        self.fs_ready.extend(batch.seen);
        if batch.overflow {
            self.gap(
                &stream,
                "overflow",
                "file observer lost notifications; gap recorded",
            )?;
        }
        if batch.ignore_failed {
            // Lieber blind als einen ignorierten Pfad fingerprinten. Die
            // Lücke steht einmal je Fehlerphase in der Kette und im Log —
            // eine dauerhaft unlesbare `.gitignore` füllt beides nicht.
            if !self.ignore_failure_logged {
                log(
                    &self.home,
                    "ignore rules not evaluable; observations dropped",
                );
                self.lifecycle(
                    &stream,
                    "fs.gap",
                    serde_json::json!({"reason": "ignore_rules"}),
                )?;
                self.fs_dirty = true;
                self.ignore_failure_logged = true;
            }
        } else if batch.classified {
            // Erst eine gelungene Auswertung beendet die Fehlerphase — ein
            // ruhiger Durchlauf ohne Pfade sagt darüber nichts.
            self.ignore_failure_logged = false;
        }
        // Jeder Append fsynct Journal und Fold. Ein Burst von tausend
        // Dateien darf die Hooks nicht sekundenlang warten lassen: höchstens
        // [`FS_APPENDS_PER_STEP`] je Durchlauf der Eventloop, beim
        // Checkpoint [`FS_APPENDS_ON_CHECKPOINT`]. Erst nach dem Append
        // verlässt eine Beobachtung die Warteschlange: Scheitert er, geht
        // nichts verloren.
        let limit = if settle {
            FS_APPENDS_ON_CHECKPOINT
        } else {
            FS_APPENDS_PER_STEP
        };
        for _ in 0..limit {
            let Some(seen) = self.fs_ready.front() else {
                break;
            };
            let payload = serde_json::value::RawValue::from_string(seen.payload()?)?;
            let at = seen.at.clone();
            self.append(
                &stream,
                NewEvent {
                    at: at.0,
                    at_nanos: at.1,
                    kind: minds_capture::EventKind::Other,
                    raw_kind: crate::checkpoint::core::FS_OBSERVED.into(),
                    cwd: None,
                    transcript_path: None,
                    payload,
                },
            )?;
            self.fs_ready.pop_front();
            self.fs_dirty = true;
        }
        Ok(())
    }

    /// Hängt ein `fs.gap` mit `reason` an und loggt `note` — höchstens
    /// einmal je Grund und Sekunde.
    fn gap(&mut self, stream: &SessionKey, reason: &'static str, note: &str) -> Fallible<()> {
        let now = std::time::Instant::now();
        if self
            .last_gap
            .get(reason)
            .is_some_and(|at| now.duration_since(*at) < std::time::Duration::from_secs(1))
        {
            return Ok(());
        }
        self.last_gap.insert(reason, now);
        log(&self.home, note);
        self.lifecycle(stream, "fs.gap", serde_json::json!({ "reason": reason }))?;
        self.fs_dirty = true;
        Ok(())
    }

    /// [`Self::observe`] ohne Entprellung, höchstens einmal je Durchlauf der
    /// Eventloop ([`Self::next_step`] gibt ihn wieder frei).
    fn settle_once(&mut self) -> Fallible<()> {
        if self.settled_this_step {
            return Ok(());
        }
        self.settled_this_step = true;
        self.observe(true)
    }

    /// Ein neuer Durchlauf der Eventloop beginnt.
    pub(super) fn next_step(&mut self) {
        self.settled_this_step = false;
    }

    /// Der Einstieg für `CheckpointRequest` vom Socket (EA-06d): ein Lauf von
    /// [`Self::checkpoint_now`] mit Schutz gegen Fluten.
    ///
    /// - Ohne konkreten Commit kein Lauf: Ohne ihn gäbe es keine Vorprüfung,
    ///   und der Witness trailerte, was immer gerade an HEAD steht.
    /// - Gibt es seit dem letzten vollständigen Lauf nichts Neues, lautet die
    ///   Antwort sofort „nothing to seal".
    /// - Sonst höchstens ein Lauf je [`MIN_CHECKPOINT_INTERVAL`], gemessen
    ///   vom **Ende** des letzten Laufs — gezählt wird jeder Lauf, auch ein
    ///   gescheiterter: Sonst ließe sich die Grenze mit absichtlich
    ///   scheiternden oder langsamen Läufen umgehen, und die Eventloop käme
    ///   zwischen zwei Läufen nie zu den Hooks.
    ///
    /// `requester_alive` sagt, ob die anfragende Agent-Seite noch wartet. Ist
    /// sie gegangen (Frist abgelaufen), wird nach dem Versiegeln **nicht**
    /// mehr getrailert: Ihr `git commit` ist längst zurück, vielleicht schon
    /// gepusht — ein später Amend schriebe Historie um, die der Nutzer schon
    /// weitergegeben hat.
    ///
    /// Der Fehler ist der feste Grund für den `Nack`. Ins eigene Log kommt
    /// nur ein gescheiterter Lauf, keine abgewiesene Anfrage — eine Flut von
    /// Anfragen darf ältere Diagnosen nicht aus dem rotierenden Log drängen.
    pub(super) fn checkpoint_requested(
        &mut self,
        commit: Option<&str>,
        requester_alive: &dyn Fn() -> bool,
    ) -> Result<String, &'static str> {
        let Some(commit) = commit else {
            return Err("commit required");
        };
        // Wartete die Anfrage im Backlog, während ein langer Lauf lief, kann
        // ihr Absender längst gegangen sein. Dann gar nicht erst versiegeln:
        // Seals ohne Trailer fände `minds verify` an keinem Commit, und der
        // Absender hat „bleiben offen" gemeldet — das soll stimmen. Kein Lauf,
        // keine Frist, keine Logzeile.
        if !requester_alive() {
            return Err("requester gone");
        }
        // Was gerade noch entprellt wird, gehört in diese Epoche — auch die
        // Frage „gibt es etwas zu versiegeln?" muss es schon sehen.
        if self.settle_once().is_err() {
            self.note_failure(Some("file observations not recorded".into()));
            return Err("checkpoint failed");
        }
        if !self.dirty && !self.fs_dirty {
            return Ok(checkpoint_status(&[], 0, Ok(None)));
        }
        if self
            .last_run
            .is_some_and(|at| at.elapsed() < MIN_CHECKPOINT_INTERVAL)
        {
            return Err("rate limited");
        }
        // Ein Panic in gix über feindlichen Objekt- oder Pack-Daten darf den
        // einzigen Schreiber nicht beenden. Danach sind die Live-Folds
        // fraglich: verwerfen — `append` lädt sie über `recover` samt
        // Präfixprüfung neu von der Platte.
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.checkpoint_now_for(Some(commit), requester_alive)
        }));
        self.last_run = Some(std::time::Instant::now());
        let result = match run {
            Ok(result) => result,
            Err(_) => {
                self.folders.clear();
                self.dirty = true;
                Err("checkpoint panicked".into())
            }
        };
        match result {
            Ok(status) => {
                self.note_failure(None);
                Ok(status)
            }
            Err(err) => {
                self.note_failure(Some(format!("checkpoint failed: {err}")));
                Err("checkpoint failed")
            }
        }
    }

    /// Loggt einen Fehlschlag nur, wenn er sich vom vorigen unterscheidet;
    /// Wiederholungen werden gezählt und beim nächsten anderen Ausgang in
    /// einer Zeile nachgetragen.
    fn note_failure(&mut self, failure: Option<String>) {
        if let (Some(new), Some((old, _))) = (&failure, &self.last_failure) {
            if new == old {
                if let Some((_, repeats)) = &mut self.last_failure {
                    *repeats += 1;
                }
                return;
            }
        }
        if let Some((_, repeats)) = self.last_failure.take() {
            if repeats > 0 {
                log(
                    &self.home,
                    &format!("previous checkpoint failure repeated {repeats} more time(s)"),
                );
            }
        }
        if let Some(failure) = failure {
            log(&self.home, &failure);
            self.last_failure = Some((failure, 0));
        }
    }

    /// Versiegelt alle offenen Agent-Sessions und gibt die Statuszeile für
    /// den `Ack` zurück: die „SESSION SEALED"-Zusammenfassung, die die
    /// Agent-Seite druckt, weil sie selbst nichts vom Witness-Zustand lesen
    /// darf (W1).
    ///
    /// `Err` heißt: Es wurde **keine Agent-Session** versiegelt — alle
    /// Prüfungen, die scheitern können, laufen vor dem ersten Session-Seal,
    /// und Fehler einzelner Sessions danach vertagt der Kern nur diese
    /// Session. Beobachtungs-Epochen (`witness-fs/v1`) können dabei schon
    /// geschlossen sein. Was nach dem
    /// Versiegeln scheitert (Trailer), steht in der Statuszeile, nicht im
    /// Fehler.
    #[cfg(test)]
    fn checkpoint_now(&mut self, commit: Option<&str>) -> Fallible<String> {
        self.checkpoint_now_for(commit, &|| true)
    }

    /// [`Self::checkpoint_now`] für eine Anfrage, deren Anfragender gehen kann.
    fn checkpoint_now_for(
        &mut self,
        commit: Option<&str>,
        requester_alive: &dyn Fn() -> bool,
    ) -> Fallible<String> {
        // Was bis zum Commit geschrieben wurde, soll in diese Epoche — nicht
        // erst nach der Entprellung in die nächste.
        self.settle_once()?;
        // Die Grenze der Beobachtungs-Epoche: Jeder Checkpoint versiegelt so
        // eine, und ihr Ende liegt nicht vor dem letzten Event der Sessions,
        // die er versiegelt (die letzte Beobachtung kommt meist **vor** dem
        // Stop-Hook). Der Reader findet darüber die Epoche des Checkpoints
        // (`witness_windows`) — nicht die des nächsten, die schon die Arbeit
        // nach dem Commit enthielte.
        //
        // Gestempelt wird monoton: nie vor dem letzten Event irgendeiner
        // offenen Session — springt die Wanduhr zurück, läge die Grenze sonst
        // vor dem bezeugten Bereich.
        if let Some(stream) = self.stream.clone() {
            let latest = self
                .journal
                .sessions()?
                .keys
                .iter()
                .filter_map(|key| self.journal.read(key).ok())
                .filter_map(|read| read.events.last().map(|event| event.at_nanos))
                .max()
                .unwrap_or(0);
            let now = clock::now();
            let at = if now.1 > latest {
                now
            } else {
                (clock::rfc3339_from_nanos(latest + 1), latest + 1)
            };
            self.append(
                &stream,
                NewEvent {
                    at: at.0,
                    at_nanos: at.1,
                    kind: minds_capture::EventKind::Other,
                    raw_kind: "fs.checkpoint".into(),
                    cwd: None,
                    transcript_path: None,
                    payload: serde_json::value::RawValue::from_string("{}".into())?,
                },
            )?;
        }
        let result = self.checkpoint_sessions(commit, requester_alive);
        // Auch ein Fehler mitten im Lauf kann Sessions bereits verworfen haben.
        // Ihr alter Fold darf nie die nächste Epoche fortsetzen; `append` lädt
        // ihn über `recover` samt `.sealed`-Prüfung neu. Alle übrigen Folds
        // bleiben der Live-Stand: Neu aus dem Journal gelesen würde ein
        // vertagter Integritätsfehler still übernommen.
        match self.journal.sessions() {
            Ok(open) => self.folders.retain(|key, _| open.keys.contains(key)),
            Err(_) => self.folders.clear(),
        }
        result
    }

    fn recover_all(&mut self) -> Fallible<()> {
        for key in self.journal.sessions()?.keys {
            self.recover(&key)?;
        }
        Ok(())
    }

    fn checkpoint_sessions(
        &mut self,
        commit: Option<&str>,
        requester_alive: &dyn Fn() -> bool,
    ) -> Fallible<String> {
        let root = &self.config.repo_root;
        // Der Worktree gehört dem Agenten; seit EA-06d kann er diesen Lauf
        // jederzeit auslösen. Was er dort ändern kann, darf den Witness nicht
        // umlenken (00-conventions: „Nothing in `.git/config` may influence
        // trust decisions") — geprüft, bevor gix oder git etwas daraus lesen.
        plain_repo_layout(root)?;
        let repo = minds_git::Repo::discover(root)?;
        // Ein Ref-Namespace aus der Konfiguration des Agenten verschöbe alle
        // Schreibzugriffe unter `refs/namespaces/<ns>/…` — heraus aus
        // `refs/minds/`.
        if repo.has_ref_namespace() {
            return Err("refs namespaces are not supported; checkpoint deferred".into());
        }
        let dot_git = root.join(".git").canonicalize()?;
        if repo.git_dir().canonicalize()? != dot_git || repo.common_dir().canonicalize()? != dot_git
        {
            return Err(
                "witness repository must own its .git directory; checkpoint deferred".into(),
            );
        }
        // Erst prüfen, dann versiegeln: Steht der angefragte Commit nicht an
        // HEAD dieses Repos (anderer Checkout, inzwischen weitergewandert),
        // entstünden Seals ohne Trailer — und die Sessions wären geschlossen,
        // obwohl die Agent-Seite „bleiben offen" meldet.
        if let Some(commit) = commit {
            let expected: minds_git::CommitId = commit.parse()?;
            if !crate::checkpoint::head_carries(&repo, expected)? {
                return Err(
                    "requested commit is not at the witness HEAD; checkpoint deferred".into(),
                );
            }
        }
        // Der Store schreibt nur im eigenen Repo und nur unter `refs/minds/`.
        // TODO(EA-10): Backend und Ref in `witness.json` festhalten; bis dahin
        // nimmt der Witness nur das In-Repo-Backend — ein `minds.childPath`
        // aus `.git/config` lenkte seine Schreibzugriffe sonst an einen
        // beliebigen Ort auf dem Host.
        let store_config = crate::config::load(root);
        if !matches!(store_config.backend(), minds_store::Backend::InRepo) {
            return Err(
                "witness supports only the in-repo store backend; checkpoint deferred".into(),
            );
        }
        if !store_config
            .reference()
            .starts_with(minds_git::MINDS_REF_NAMESPACE)
        {
            return Err("context ref outside refs/minds/; checkpoint deferred".into());
        }
        let store = store_config.open(root)?;
        // Die Policy darf verschärfen, nie abschwächen — und ihre Datei ist
        // fremd: gewöhnliche Datei, begrenzt, ohne zu blockieren gelesen.
        let policy = crate::config::load_redaction_untrusted(root)?.floored_at_default();
        let pipeline = policy.pipeline()?;
        // Der Beobachter prüft Inhalte ab jetzt mit derselben, frischen
        // Policy — verschärft das Repo sie, hasht er nichts mehr, was sie
        // ersetzen würde. Aus **derselben** Lesung gebaut: Die Datei gehört
        // dem Agenten, und ein zweites Lesen könnte etwas anderes ergeben.
        // Der Beobachter wird dabei nie herausgenommen, bevor die neue
        // Pipeline feststeht — er kann nicht verloren gehen.
        if let Ok(fresh) = policy.pipeline() {
            if let Some(observer) = self.observer.take() {
                self.observer = Some(observer.with_pipeline(fresh));
            }
        }
        let tracked = crate::checkpoint::tracked_files(root);
        fingerprint(&self.home)?;
        let key = self.home.join("key/witness_ed25519");
        let env = CheckpointEnv {
            repo: &repo,
            root,
            log_dir: &self.home,
            store: store.as_ref(),
            pipeline: &pipeline,
            tracked: tracked.as_ref(),
        };
        let source = EvidenceSource {
            journal: &self.journal,
            epochs: &self.epochs,
            scope: minds_core::evidence::SCOPE_WITNESS_V1,
        };
        // Zuerst die Beobachtungs-Epoche dieses Checkpoints (`witness-fs/v1`),
        // dann die Sessions: Ein vertrauenswürdiger `witness/v1`-Seal soll
        // nur existieren, wenn die Epoche seines Checkpoints abgelegt ist.
        // Bliebe sie offen (ein Ablagefehler, den der Agent per Ref-Konflikt
        // herbeiführen kann), verschmölze sie mit der nächsten — und das
        // Fenster der Session reichte über den Commit hinaus. Dann lieber
        // vertagen: Die Sessions bleiben offen, nichts wird behauptet.
        let streams: Vec<SessionKey> = self
            .journal
            .sessions()?
            .keys
            .into_iter()
            .filter(|key| key.agent() == "witness")
            .collect();
        let fs_source = EvidenceSource {
            journal: &self.journal,
            epochs: &self.epochs,
            scope: minds_core::evidence::SCOPE_WITNESS_FS_V1,
        };
        let fs_sealed = crate::checkpoint::core::seal_observation_streams(
            &env,
            &fs_source,
            &SealSigner::Key {
                path: &key,
                namespace: minds_attest::NS_WITNESS,
            },
            &*self,
            &streams,
        );
        // Gleich nach dem Versiegeln, vor jedem frühen Ausstieg: Ist die
        // Epoche geschlossen, beginnt die nächste ohne Gedächtnis.
        self.fs_dirty = match &self.stream {
            Some(stream) => self.journal.read(stream).map_or(true, |read| {
                // Auch eine verbliebene Lücke will versiegelt werden.
                read.events.iter().any(|e| e.raw_kind.starts_with("fs."))
            }),
            None => false,
        };
        // Eine neue Epoche beginnt ohne Gedächtnis: Schreibt jemand danach
        // denselben Inhalt erneut, ist das in ihr eine eigene Beobachtung.
        if !self.fs_dirty {
            if let Some(observer) = self.observer.as_mut() {
                observer.forget_states();
            }
        }
        if let Some(stream) = &self.stream {
            if !self.journal.read(stream)?.events.is_empty() {
                return Err("observation epoch not sealed; witnessed sessions deferred".into());
            }
        }
        let mut outcome = run_checkpoint_guarded(
            &env,
            &source,
            &SealSigner::Key {
                path: &key,
                namespace: minds_attest::NS_WITNESS,
            },
            Some(self),
        )?;
        outcome.sealed.extend(fs_sealed);
        // Ab hier ist versiegelt und verworfen. Ein Fehler beim Trailern macht
        // daraus keinen `Nack` mehr — die Agent-Seite meldete sonst „bleiben
        // offen". Er steht als fester Grund in der Statuszeile.
        let home = self.home.clone();
        let report = |note: &str| log(&home, note);
        let attached = if !outcome.stored.is_empty() && !requester_alive() {
            log(&self.home, "trailer not attached: the requester is gone");
            Err("the requester is gone")
        } else {
            match crate::checkpoint::attach_trailers(&repo, commit, &outcome.stored, &report) {
                Ok(attached) => Ok(attached),
                Err(err) => {
                    log(&self.home, &format!("trailer not attached: {err}"));
                    Err(trailer_miss(err.as_ref()))
                }
            }
        };
        if let Ok(Some(update)) = attached {
            if let Err(err) = crate::checkpoint::index_trailered(
                &repo,
                store.as_ref(),
                commit,
                update,
                &outcome.stored,
            ) {
                // Der Index ist aus den Trailern rekonstruierbar.
                log(&self.home, &format!("commit index not recorded: {err}"));
            }
        }
        // Was nach dem Lauf noch offen ist, wurde vertagt (Store, Redaction,
        // Integrität — der Grund steht im eigenen Log). Die Agent-Seite soll
        // das nicht als „nichts zu tun" lesen. Lässt sich das nicht zählen,
        // bleibt `dirty` gesetzt: Lieber ein Lauf zu viel als offene Sessions,
        // die keiner mehr anfasst.
        let deferred = match self.journal.sessions() {
            Ok(open) => {
                let deferred = open.keys.iter().filter(|key| self.includes(key)).count();
                self.dirty = deferred > 0;
                deferred
            }
            Err(err) => {
                log(&self.home, &format!("open sessions not counted: {err}"));
                self.dirty = true;
                0
            }
        };
        Ok(checkpoint_status(&outcome.sealed, deferred, attached))
    }
}

/// `.git` muss vollständig schlicht sein, bevor gix oder git daraus lesen
/// oder hineinschreiben: Der Witness arbeitet dort mit **seinen** Rechten auf
/// dem Host, und der Baum gehört dem Agenten.
///
/// - Jeder Eintrag ist ein echtes Verzeichnis oder eine gewöhnliche Datei.
///   Ein Symlink (`logs/HEAD`, `refs/heads/main` → Host-Datei) lenkte die
///   Ref- und Reflog-Schreibzugriffe des Witness an einen fremden Ort; ein
///   FIFO oder Gerät (auch hinter dem Ref, auf den HEAD zeigt) hielte den
///   einzigen Schreiber an. Zwei Ausnahmen, die der Witness nie öffnet:
///   `hooks/` (dort sind Symlinks üblich; weder gix noch `git ls-files`
///   starten Hooks) und Sockets (etwa der des fsmonitor-Daemons — ein
///   Socket lässt sich nicht wie eine Datei öffnen, lesen oder anhängen).
/// - Außerhalb von `objects/` hat keine Datei einen zweiten harten Link —
///   sonst schriebe ein Append (Reflog) in eine Datei anderswo. In
///   `objects/` sind harte Links normal (`git clone --local`) und werden dort
///   nur gelesen oder neu angelegt.
/// - Kein `commondir` (geteiltes Verzeichnis anderswo) und keine
///   `objects/info/alternates` (fremde Objektbank).
///
/// Ehrliche Grenze: Das ist eine Prüfung vor dem Lauf. Wer zwischen Prüfung
/// und Zugriff tauscht, oder über `include.path` in der Konfiguration auf
/// eine fremde Datei verweist, wird hiervon nicht erfasst — das schließt erst
/// ein auf ein festgehaltenes Git-Verzeichnis eingeschränkter Lauf (EA-10).
fn plain_repo_layout(root: &Path) -> Fallible<()> {
    plain_repo_layout_within(root, MAX_GIT_ENTRIES)
}

/// Höchstzahl der Einträge, die [`plain_repo_layout`] in `.git` ansieht —
/// und höchste Tiefe. Ein Agent, der `.git` mit Millionen Dateien füllt, soll
/// den einzigen Schreiber nicht minutenlang beschäftigen; ein Repo dieser
/// Größe wird vertagt statt geprüft.
const MAX_GIT_ENTRIES: usize = 200_000;
const MAX_GIT_DEPTH: usize = 16;

fn plain_repo_layout_within(root: &Path, max_entries: usize) -> Fallible<()> {
    let dot_git = root.join(".git");
    let refused = |what: &str| -> Fallible<()> {
        Err(format!(
            "witness repository layout is not plain ({}); checkpoint deferred",
            crate::text::sanitize(what)
        )
        .into())
    };
    if !fs::symlink_metadata(&dot_git).is_ok_and(|meta| meta.is_dir()) {
        return refused(".git");
    }
    for redirect in ["commondir", "objects/info/alternates"] {
        if fs::symlink_metadata(dot_git.join(redirect)).is_ok() {
            return refused(redirect);
        }
    }
    let objects = dot_git.join("objects");
    // `hooks/` liest und startet der Witness nie (weder gix noch
    // `git ls-files` rufen Hooks); dort sind Symlinks üblich.
    let hooks = dot_git.join("hooks");
    let mut seen = 0usize;
    let mut stack = vec![(dot_git.clone(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        // Git räumt während des Laufs auf (gc, gelöschte Branches): Was
        // inzwischen fehlt, ist kein unsicherer Eintrag.
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err.into()),
            };
            seen += 1;
            if seen > max_entries {
                return refused("too many entries");
            }
            if path == hooks {
                continue;
            }
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err.into()),
            };
            let kind = meta.file_type();
            let relative = path.strip_prefix(&dot_git).unwrap_or(&path);
            if kind.is_dir() {
                if depth + 1 > MAX_GIT_DEPTH {
                    return refused("too deep");
                }
                stack.push((path, depth + 1));
            } else if kind.is_socket() {
                // Ein Socket lässt sich nicht öffnen wie eine Datei — weder
                // lesen noch anhängen leitet dorthin um (fsmonitor-Daemon).
            } else if !kind.is_file() {
                return refused(&relative.display().to_string());
            } else if meta.nlink() != 1 && !path.starts_with(&objects) {
                return refused(&format!("{} has another hard link", relative.display()));
            }
        }
    }
    Ok(())
}

/// Der feste Grund, aus dem ein Trailer nach dem Versiegeln fehlt — für die
/// Statuszeile, nie fremder Text.
fn trailer_miss(err: &(dyn std::error::Error + 'static)) -> &'static str {
    use minds_git::GitError;
    match err.downcast_ref::<GitError>() {
        Some(GitError::SignedCommit { .. }) => "the commit is signed",
        Some(GitError::RefRaced { .. }) => "HEAD kept moving",
        Some(GitError::NothingToAmend { .. }) => "no commit at HEAD",
        Some(GitError::MessageNotUtf8 { .. }) => "the commit message is not UTF-8",
        _ => "trailer error",
    }
}

impl CheckpointGuard for Writer {
    fn includes(&self, key: &SessionKey) -> bool {
        key.agent() != "witness"
    }
    /// Persistiert und live gefaltet ist der Präfix aus Events und Lücken.
    /// Beschädigte Dateien hängt `chain::items` immer ans Ende; sie kommen erst
    /// hier aus dem Journal dazu, damit ein späteres Event den Präfix nicht bricht.
    /// Ihre Bytes bindet damit erst der Seal, nicht schon der Live-Fold. Ein
    /// Event, das zu Schaden wird oder umgekehrt, bricht weiterhin den Präfix.
    fn validate(
        &self,
        key: &SessionKey,
        read: &minds_capture::ReadOutcome,
        result: &ChainResult,
    ) -> Fallible<()> {
        verify_events(&read.events)?;
        let bytes = fs::read(folder_path(&self.home, key))?;
        let disk = ChainFolder::from_state(serde_json::from_slice(&bytes)?).snapshot();
        let folder = self.folders.get(key).ok_or("integrity error")?;
        if disk != folder.snapshot() {
            return Err("integrity error".into());
        }
        let mut sealed = folder.clone();
        for item in chain::items(read).iter().filter(|item| is_damaged(item)) {
            sealed.push(item);
        }
        if &sealed.snapshot() != result {
            return Err("integrity error".into());
        }
        Ok(())
    }
    fn sealed(&self, key: &SessionKey, sealed: &SealSummary) -> Fallible<()> {
        let path = self.home.join("ledger");
        let existing = fs::read_to_string(&path)?;
        let line = format!(
            "{} {} {}\n",
            sealed.seal_id, sealed.seal.scope, sealed.seal.last_event_at
        );
        // Der eigene Stream wird auch nach einem Block-Seal verworfen; ohne
        // Markierung hielte `recover` das leere Journal für verschwunden.
        if matches!(
            sealed.seal.outcome,
            minds_core::evidence::SealOutcome::Stored { .. }
                | minds_core::evidence::SealOutcome::ObservationsStored { .. }
        ) || key.agent() == "witness"
        {
            atomic(
                &folder_path(&self.home, key).with_extension("sealed"),
                sealed.seal.root.as_str().as_bytes(),
            )?;
        }
        if existing.lines().any(|entry| entry == line.trim_end()) {
            return Ok(());
        }
        if !existing.is_empty() && !existing.ends_with('\n') {
            return Err("incomplete witness ledger; checkpoint deferred".into());
        }
        let mut file = private_append(&path)?;
        file.write_all(line.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }
    fn path_map(&self) -> &[(PathBuf, PathBuf)] {
        &self.config.path_map
    }
    fn report(&self, message: &str) {
        log(&self.home, message);
    }
}

/// Obergrenze der Statuszeile im `Ack` (`witness_proto`: 4 KiB).
const MAX_STATUS: usize = 4 * 1024;

/// Die Statuszeile eines Witness-Checkpoints: Kopfzeile (mit der Zahl der
/// vertagten Sessions), eine Zeile je Seal,
/// gegebenenfalls der nachgerüstete Commit — verbunden mit
/// [`LINE_SEPARATOR`](crate::checkpoint::delegate::LINE_SEPARATOR), weil das
/// Protokoll nur eine Zeile trägt. Passt nicht alles in 4 KiB, endet sie mit
/// der Zahl der ausgelassenen Seals; vollständig ist der Ledger.
///
/// Die Zahl der vertagten Sessions erfährt jeder, der den Socket erreicht —
/// über alle Agents dieses Witness. Das ist bewusst: Der Socket ist ohnehin
/// nur der Agent-Seite dieses einen Repositorys zugänglich, und eine Zahl
/// verrät keinen Inhalt.
fn checkpoint_status(
    sealed: &[SealSummary],
    deferred: usize,
    attached: Result<Option<minds_git::TrailerUpdate>, &'static str>,
) -> String {
    use crate::checkpoint::delegate::LINE_SEPARATOR;
    let head = match (sealed.len(), deferred) {
        (0, 0) => "witness: nothing to seal".to_owned(),
        (0, m) => format!(
            "witness: nothing sealed, {m} session(s) deferred — they stay open, see the witness log"
        ),
        (n, 0) => format!("witness: {n} range(s) sealed"),
        (n, m) => format!(
            "witness: {n} range(s) sealed, {m} session(s) deferred — they stay open, see the witness log"
        ),
    };
    let stored = sealed.iter().any(|s| {
        matches!(
            s.seal.outcome,
            minds_core::evidence::SealOutcome::Stored { .. }
        )
    });
    // Gespeichert, aber nicht getrailert (HEAD ist während des Laufs
    // weitergewandert): Das muss die Agent-Seite sehen, sonst hielte sie die
    // Sessions für verknüpft.
    let tail = match attached {
        Ok(Some(update)) if update.rewrote_head() => {
            Some(format!("Trailer retrofitted to {}", update.commit()))
        }
        Ok(None) if stored => {
            Some("Trailer not attached — HEAD moved; see `minds fsck`".to_owned())
        }
        Err(reason) if stored => Some(format!("Trailer not attached — {reason}; see `minds fsck`")),
        _ => None,
    };
    let omission = |n: usize| format!("… {n} more — see `minds witness status`");
    // Platz für Schwanz und Auslassungszeile, aus ihrer tatsächlichen Länge
    // (SHA-256-Repos haben längere Commit-Ids).
    let reserve = tail.as_ref().map_or(0, |t| LINE_SEPARATOR.len() + t.len())
        + LINE_SEPARATOR.len()
        + omission(sealed.len()).len();
    let budget = MAX_STATUS.saturating_sub(reserve);
    let mut lines = vec![head];
    let mut used = lines[0].len();
    let mut omitted = 0;
    for summary in sealed {
        let line = crate::checkpoint::core::session_sealed_line(summary);
        if omitted == 0 && used + LINE_SEPARATOR.len() + line.len() <= budget {
            used += LINE_SEPARATOR.len() + line.len();
            lines.push(line);
        } else {
            omitted += 1;
        }
    }
    if omitted > 0 {
        lines.push(omission(omitted));
    }
    lines.extend(tail);
    lines.join(LINE_SEPARATOR)
}

fn is_damaged(item: &ChainItem) -> bool {
    matches!(item, ChainItem::Gap(GapRecord::Damaged { .. }))
}

/// `chain::items` ohne den Damaged-Schwanz: der Teil, den ein späteres Event
/// nur verlängert, nie umordnet.
fn base_items(read: &minds_capture::ReadOutcome) -> Vec<ChainItem> {
    let mut items = chain::items(read);
    items.retain(|item| !is_damaged(item));
    items
}

fn verify_events(events: &[JournalEvent]) -> Fallible<()> {
    use minds_core::evidence::{EventFacts, event_hash, payload_hash};
    for event in events {
        let payload_hash = payload_hash(event.payload.get().as_bytes());
        let hash = event_hash(&EventFacts {
            seq: event.seq,
            at: &event.at,
            at_nanos: event.at_nanos,
            raw_kind: &event.raw_kind,
            cwd: event.cwd.as_deref(),
            transcript_path: event.transcript_path.as_deref(),
            payload_hash: &payload_hash,
        });
        if event.payload_hash.as_ref() != Some(&payload_hash)
            || event.event_hash.as_ref() != Some(&hash)
        {
            return Err("integrity error: journal event hash mismatch".into());
        }
    }
    Ok(())
}

fn follow_line(key: &SessionKey, event: &JournalEvent, folder: &ChainFolder) -> String {
    let kind = match event.kind {
        minds_capture::EventKind::SessionStart => "SessionStart",
        minds_capture::EventKind::SessionEnd => "SessionEnd",
        minds_capture::EventKind::Prompt => "UserPromptSubmit",
        minds_capture::EventKind::ToolPre => "PreToolUse",
        minds_capture::EventKind::ToolPost => "PostToolUse",
        minds_capture::EventKind::TurnEnd => "Stop",
        minds_capture::EventKind::SubagentStart => "SubagentStart",
        minds_capture::EventKind::SubagentEnd => "SubagentStop",
        minds_capture::EventKind::Other => "Other",
    };
    let tool = minds_capture::normalize::facts(key.agent(), event).tool;
    let details = tool
        .map(|tool| {
            // Unbekannte Namen sind beliebiger Agenttext, kein Diagnosevokabular.
            if !match key.agent() {
                "claude-code" => minds_capture::normalize::claude_tool_is_interpreted(&tool.name),
                "codex" => minds_capture::normalize::codex_tool_is_interpreted(&tool.name),
                _ => false,
            } {
                return String::new();
            }
            format!(
                "{} {}",
                tool.name,
                tool.effect
                    .and_then(|effect| effect.path)
                    .unwrap_or_default()
            )
        })
        .unwrap_or_default();
    let head = folder.head().to_string();
    format!(
        "seq {:06}  {kind}  {}   head {}",
        event.seq,
        crate::hooklog::diagnostic(&details),
        &head[3..11]
    )
}

pub fn status(home: &Path) -> Fallible<String> {
    let config = load(home)?;
    let journal = Journal::at(home.join("journal"));
    let keys = journal.sessions()?.keys;
    let mut last = None;
    for key in &keys {
        for event in journal.read(key)?.events {
            if last.as_ref().is_none_or(|at| at < &event.at) {
                last = Some(event.at);
            }
        }
    }
    let ledger = fs::read_to_string(home.join("ledger"))?;
    let lines: Vec<_> = ledger.lines().collect();
    Ok(format!(
        "Profile: {}\nRepository: {}\nSocket: {} ({})\nOpen sessions: {}\nLast event: {}\nKey: {}\nLedger:\n{}",
        config.profile.name(),
        crate::text::sanitize(&config.repo_root.display().to_string()),
        crate::text::sanitize(&home.join("run/witness.sock").display().to_string()),
        if socket::ping(&home.join("run/witness.sock")) {
            "running"
        } else {
            "stopped"
        },
        keys.iter().filter(|key| key.agent() != "witness").count(),
        last.as_deref().unwrap_or("none"),
        fingerprint(home)?,
        lines[lines.len().saturating_sub(10)..]
            .iter()
            .map(|line| crate::hooklog::diagnostic(line))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}
