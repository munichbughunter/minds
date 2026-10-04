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
}

impl Writer {
    fn open(home: &Path, config: Config, follow: bool) -> Fallible<Self> {
        let mut writer = Self {
            home: home.into(),
            config,
            journal: Journal::at(home.join("journal")),
            epochs: EpochState::at(home.join("evidence/state")),
            folders: BTreeMap::new(),
            follow,
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
        if agent == "witness" {
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

    /// Interner Einstieg für EA-06d; absichtlich noch kein Socket-Kommando.
    #[allow(dead_code)]
    fn checkpoint_now(&mut self, commit: Option<&str>) -> Fallible<()> {
        let result = self.checkpoint_sessions(commit);
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

    fn checkpoint_sessions(&mut self, commit: Option<&str>) -> Fallible<()> {
        let root = &self.config.repo_root;
        let repo = minds_git::Repo::discover(root)?;
        let store = crate::config::load(root).open(root)?;
        let pipeline = crate::config::load_redaction(root)?.pipeline()?;
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
        let outcome = run_checkpoint_guarded(
            &env,
            &source,
            &SealSigner::Key {
                path: &key,
                namespace: minds_attest::NS_WITNESS,
            },
            Some(self),
        )?;
        if let Some(commit) = crate::checkpoint::attach_trailers(&repo, commit, &outcome.stored)? {
            crate::checkpoint::record_index(store.as_ref(), commit, &outcome.stored)?;
        }
        Ok(())
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
        if let minds_core::evidence::SealOutcome::Stored { .. } = sealed.seal.outcome {
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
