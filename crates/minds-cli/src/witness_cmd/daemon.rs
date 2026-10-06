//! Ein Witness besitzt genau ein Repository und genau einen Schreiber.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Read, Write};
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

mod limits;
mod observer;
mod socket;
#[cfg(test)]
mod tests;
mod witness_clock;
mod worker;
pub(crate) use socket::ping;
pub use socket::run;
pub(crate) use worker::worker;

/// Das Isolationsprofil (00-conventions, ADR-0012): wie Agent und Witness
/// voneinander getrennt sind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum WitnessProfile {
    Container,
    User,
    Managed,
}

impl WitnessProfile {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::User => "user",
            Self::Managed => "managed",
        }
    }

    pub(crate) fn parse(name: &str) -> Fallible<Self> {
        match name {
            "container" => Ok(Self::Container),
            "user" => Ok(Self::User),
            "managed" => Ok(Self::Managed),
            _ => Err("profile must be container, user or managed".into()),
        }
    }
}

/// Der festgehaltene Store des Witness (EA-10).
///
/// Backend, Kontext-Ref und Child-Pfad stehen in `witness.json` — nie in der
/// `.git/config` des beobachteten Repos, die der Agent schreiben kann. Ein
/// `minds.childPath` von dort lenkte die Schreibzugriffe des Witness sonst an
/// einen beliebigen Ort auf dem Host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum PinnedStore {
    InRepo {
        context_ref: String,
    },
    ChildRepo {
        context_ref: String,
        child_path: PathBuf,
    },
}

impl PinnedStore {
    fn context_ref(&self) -> &str {
        match self {
            Self::InRepo { context_ref } | Self::ChildRepo { context_ref, .. } => context_ref,
        }
    }

    /// Das In-Repo-Backend mit dem Standard-Ref — was ein Witness von vor
    /// EA-10 (Schema 1) implizit benutzte.
    fn default_in_repo() -> Self {
        Self::InRepo {
            context_ref: minds_git::DEFAULT_CONTEXT_REF.to_owned(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Config {
    schema_version: u32,
    repo_root: PathBuf,
    path_map: Vec<(PathBuf, PathBuf)>,
    profile: WitnessProfile,
    #[serde(default)]
    socket_group: Option<u32>,
    /// Das festgehaltene Git-Verzeichnis (Schema 2): kanonisch, gleich
    /// `<repo_root>/.git`. Fehlt es (Schema 1), gilt genau dieser Pfad.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_dir: Option<PathBuf>,
    /// Der festgehaltene Store (Schema 2). Fehlt er, gilt das In-Repo-Backend
    /// mit dem Standard-Ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    store: Option<PinnedStore>,
    /// Die Witness-eigene Redaction-Policy (Schema 2). Eine
    /// `.minds/redact.json` im Worktree liest der Witness nicht mehr: Der
    /// Agent könnte bezeugte Sessions sonst mit sehr breiten Deny-Begriffen
    /// unlesbar oder jede Redaction instabil machen. Fehlt sie, gilt der
    /// strenge Default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    redaction: Option<minds_redact::RedactionConfig>,
    /// Woher die Policy stammt — Commit- und Blob-Id —, damit auch ein
    /// späteres `init` sie nennen kann.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    policy_source: Option<String>,
}

/// Die Schema-Version, die `init` schreibt. Gelesen wird auch 1 (ohne Pins).
const SCHEMA_VERSION: u32 = 2;

/// Der feste Grund, aus dem ein Witness ohne Pins nicht arbeitet.
pub(crate) const UNPINNED: &str = "witness.json has no pins (schema 1) — run `minds witness init` \
     again with the same arguments to pin git dir, store and redaction policy";

impl Config {
    pub(crate) fn profile(&self) -> WitnessProfile {
        self.profile
    }

    pub(crate) fn socket_group(&self) -> Option<u32> {
        self.socket_group
    }

    /// Ob `git_dir`, Store und Policy in der Datei stehen (Schema 2).
    pub(crate) fn pinned(&self) -> bool {
        self.git_dir.is_some() && self.store.is_some() && self.redaction.is_some()
    }

    /// Das Git-Verzeichnis, in dem der Witness arbeitet.
    fn git_dir(&self) -> PathBuf {
        self.git_dir
            .clone()
            .unwrap_or_else(|| self.repo_root.join(".git"))
    }

    fn store(&self) -> PinnedStore {
        self.store
            .clone()
            .unwrap_or_else(PinnedStore::default_in_repo)
    }

    /// Die Policy für Checkpoint und Beobachter: die festgehaltene, nach
    /// unten begrenzt durch den strengen Default — auch eine von Hand
    /// geänderte `witness.json` schwächt nicht ab.
    fn policy(&self) -> minds_redact::RedactionConfig {
        self.redaction
            .clone()
            .unwrap_or_default()
            .floored_at_default()
    }
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
/// Was `init` vorfand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Initialized {
    /// Neu angelegt; die Pins zum Anzeigen.
    Created(Pins),
    /// Dieselbe Konfiguration stand schon da, mit Pins; nichts wurde
    /// geschrieben. Die Pins zum Anzeigen — wer erneut `init` ausführt, soll
    /// sehen, was gilt.
    Unchanged(Pins),
    /// Dieselbe Konfiguration stand da, aber ohne Pins (Schema 1); sie sind
    /// jetzt ergänzt.
    Pinned(Pins),
}

/// Was `init` festgehalten hat — für den Bericht an den Menschen, der es
/// prüfen soll. Keine Policy-Werte: Deny-Begriffe können selbst Geheimnisse
/// sein; genannt wird nur, woher die Policy stammt und wie viele Begriffe sie
/// trägt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pins {
    pub(crate) git_dir: PathBuf,
    pub(crate) store: String,
    pub(crate) policy: String,
}

impl Pins {
    fn of(config: &Config, policy_source: &str) -> Self {
        let store = match config.store() {
            PinnedStore::InRepo { context_ref } => format!("in-repo, {context_ref}"),
            PinnedStore::ChildRepo {
                context_ref,
                child_path,
            } => format!("child repo {}, {context_ref}", child_path.display()),
        };
        let policy = config.policy();
        let terms = policy.deny_secrets.len() + policy.deny_pii.len() + policy.secret_keys.len();
        Self {
            git_dir: config.git_dir(),
            store,
            policy: format!("{policy_source}, {terms} custom term(s), floored at the default"),
        }
    }

    /// Die Zeilen für den Bericht.
    pub(crate) fn lines(&self) -> Vec<String> {
        vec![
            format!("pinned git dir: {}", self.git_dir.display()),
            format!("pinned store:   {}", self.store),
            format!("pinned policy:  {}", self.policy),
        ]
    }
}

/// Die Wünsche an `init`, wie sie von der Kommandozeile oder aus
/// `minds enable --witness` kommen.
pub(crate) struct InitRequest<'a> {
    pub(crate) repo: &'a str,
    pub(crate) mapping: Option<&'a str>,
    pub(crate) profile: Option<&'a str>,
    pub(crate) socket_group: Option<&'a str>,
    /// Ein Child-Repo als Store — nur ausdrücklich vom Menschen genannt,
    /// nie aus der `.git/config`, die der Agent schreiben kann.
    pub(crate) child_repo: Option<&'a Path>,
    /// Aus welchem Commit die Policy gelesen wird (Standard: HEAD) — etwa der
    /// geprüfte Stand `origin/main`.
    pub(crate) policy_rev: Option<&'a str>,
}

pub fn init(home: &Path, request: &InitRequest<'_>) -> Fallible<String> {
    let mut lines = Vec::new();
    match init_config(home, request)? {
        Initialized::Created(pins) => {
            lines.push(
                "Witness initialized. Run `minds witness keygen` before `minds witness run`."
                    .to_owned(),
            );
            lines.extend(pins.lines());
        }
        Initialized::Pinned(pins) => {
            lines.push("Witness configuration pinned (schema 2).".to_owned());
            lines.extend(pins.lines());
        }
        Initialized::Unchanged(pins) => {
            lines.push("Witness already initialized with this configuration.".to_owned());
            lines.extend(pins.lines());
        }
    }
    Ok(lines
        .iter()
        .map(|line| crate::text::sanitize(line))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Legt das Witness-Home an und hält Repository, Git-Verzeichnis, Store und
/// Policy in `witness.json` fest.
///
/// Idempotent: Steht dort schon dieselbe Konfiguration (Repository,
/// Pfadabbildung, Profil, Socket-Gruppe), bleibt alles, wie es ist —
/// insbesondere die einmal festgehaltenen Pins. Fehlen dort die Pins
/// (Schema 1), werden sie ergänzt. Eine **andere** Konfiguration wird nie
/// überschrieben; der Fehler nennt, was abweicht.
///
/// Woher die Pins kommen, entscheidet, wem sie gehören:
///
/// - **Store:** In-Repo mit dem Ref aus der Konfiguration (nur unter
///   `refs/minds/`). Ein Child-Repo nur, wenn der Mensch es nennt
///   (`--child-repo`) — wählt die `.git/config` eines, ohne dass es genannt
///   ist, bricht `init` ab, statt still einen Pfad des Agenten festzuhalten.
/// - **Policy:** `.minds/redact.json` aus dem **committeten** HEAD, nicht
///   aus dem Worktree — was gepinnt wird, steht in der Historie. Eine
///   Policy, die ihre eigenen Platzhalter träfe (jede redigierte Session
///   würde instabil und vertagt), wird abgelehnt.
pub(crate) fn init_config(home: &Path, request: &InitRequest<'_>) -> Fallible<Initialized> {
    let home: PathBuf = home.components().collect();
    // Die Wurzel über das Dateisystem, nicht über `git rev-parse`: Das
    // folgte `core.worktree` aus der `.git/config` des Agenten und hielte
    // ein anderes Verzeichnis als Repository fest.
    let repo_root = super::repo_root_of(&Path::new(request.repo).canonicalize()?)
        .ok_or("--repo must identify a Git worktree")?;
    let path_map = if let Some(mapping) = request.mapping {
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
    let profile = match request.profile {
        Some(name) => WitnessProfile::parse(name)?,
        None if path_map.is_empty() => WitnessProfile::User,
        None => WitnessProfile::Container,
    };
    match (profile, path_map.is_empty()) {
        (WitnessProfile::Container, true) => {
            return Err("the container profile requires --path-map".into());
        }
        (WitnessProfile::User | WitnessProfile::Managed, false) => {
            return Err("--path-map is only valid for the container profile".into());
        }
        _ => {}
    }
    let socket_group = request.socket_group.map(group_id).transpose()?;
    if socket_group.is_some() && profile != WitnessProfile::User {
        return Err("--socket-group is only valid for the user profile".into());
    }

    // Schon eingerichtet? Dann nur vergleichen — ohne Lock, damit ein
    // erneutes `enable --witness` auch neben einem laufenden Witness gelingt.
    if fs::symlink_metadata(home.join("witness.json")).is_ok() {
        let mut existing = load(&home)?;
        let mut differs = Vec::new();
        if existing.repo_root != repo_root {
            differs.push("repository");
        }
        if existing.path_map != path_map {
            differs.push("path map");
        }
        if existing.profile != profile {
            differs.push("profile");
        }
        if existing.socket_group != socket_group {
            differs.push("socket group");
        }
        // Ausdrücklich Genanntes, das nicht dem Festgehaltenen entspricht,
        // darf nicht still als „unverändert" durchgehen: weder ein Store noch
        // eine Policy-Revision. Verglichen wird nur, was der Mensch nennt —
        // nicht ein Context-Ref, den der Agent inzwischen umgeschrieben hat.
        if existing.pinned() {
            if let Some(child) = request.child_repo {
                let wanted = child_store_path(&repo_root, child)?;
                let pinned = match existing.store() {
                    PinnedStore::ChildRepo { child_path, .. } => Some(child_path),
                    PinnedStore::InRepo { .. } => None,
                };
                if pinned.as_ref() != Some(&wanted) {
                    differs.push("store");
                }
            }
            if request.policy_rev.is_some() {
                let (wanted, _) = pinned_policy(&existing.git_dir(), request.policy_rev)?;
                if Some(wanted) != existing.redaction.clone().map(|p| p.floored_at_default()) {
                    differs.push("policy");
                }
            }
        }
        if !differs.is_empty() {
            return Err(format!(
                "witness home already holds a different configuration ({} differ); \
                 refusing to overwrite",
                differs.join(", ")
            )
            .into());
        }
        if existing.pinned() {
            // Die gespeicherte Quelle (volle Commit- und Blob-Id), damit auch
            // ein späterer Lauf sie zum Abgleich nennt. Bereinigt: `load`
            // prüft das Feld nicht, und die Datei ist von Hand änderbar.
            let source = existing
                .policy_source
                .as_deref()
                .map(crate::text::sanitize)
                .unwrap_or_else(|| "pinned earlier (source not recorded)".to_owned());
            return Ok(Initialized::Unchanged(Pins::of(&existing, &source)));
        }
        // Schema 1 ergänzen: dieselben Quellen wie bei einer neuen Einrichtung.
        let (pins, source) = gather_pins(&repo_root, request.child_repo, request.policy_rev)?;
        let _lock = lock(&home)?;
        existing.schema_version = SCHEMA_VERSION;
        existing.git_dir = Some(pins.0);
        existing.store = Some(pins.1);
        existing.redaction = Some(pins.2);
        existing.policy_source = Some(source.clone());
        atomic(
            &home.join("witness.json"),
            &serde_json::to_vec_pretty(&existing)?,
        )?;
        return Ok(Initialized::Pinned(Pins::of(&existing, &source)));
    }

    let (pins, source) = gather_pins(&repo_root, request.child_repo, request.policy_rev)?;

    // Vor dem Anlegen prüfen: Ein abgelehntes Home soll nicht als leeres
    // Verzeichnis im Repo zurückbleiben.
    if let Some(parent) = home.parent().and_then(|parent| parent.canonicalize().ok()) {
        if parent.starts_with(&repo_root) {
            return Err("witness home must not be inside the repository".into());
        }
    }
    private_directory(&home)?;
    validate_tree(&home)?;
    if refuse_home_in(&home, &repo_root).is_err() {
        return Err("witness home must not be inside the repository".into());
    }
    for dir in DIRECTORIES {
        private_directory(&home.join(dir))?;
    }
    let _lock = lock(&home)?;
    if let Some(group) = socket_group {
        // Das `user`-Profil: Die Agent-Gruppe darf Home und `run/`
        // durchqueren (0710, ohne Lesen), um den Socket zu erreichen. Alles
        // darunter bleibt 0700/0600. Vor `witness.json`: Scheitert das (etwa
        // weil dieser Nutzer nicht in der Gruppe ist), behauptet keine
        // Konfiguration eine Gruppe, die nie gesetzt wurde.
        for dir in [home.clone(), home.join("run")] {
            share_with_group(&dir, group)?;
        }
    }
    let config = Config {
        schema_version: SCHEMA_VERSION,
        repo_root,
        profile,
        path_map,
        socket_group,
        git_dir: Some(pins.0),
        store: Some(pins.1),
        redaction: Some(pins.2),
        policy_source: Some(source.clone()),
    };
    let mut file = private_new(&home.join("witness.json"))?;
    file.write_all(&serde_json::to_vec_pretty(&config)?)?;
    file.sync_all()?;
    let ledger = private_append(&home.join("ledger"))?;
    ledger.sync_all()?;
    sync_dir(&home)?;
    Ok(Initialized::Created(Pins::of(&config, &source)))
}

/// Git-Verzeichnis, Store und Policy für `init`, samt der Quelle der Policy.
type Gathered = (PathBuf, PinnedStore, minds_redact::RedactionConfig);

fn gather_pins(
    repo_root: &Path,
    child_repo: Option<&Path>,
    policy_rev: Option<&str>,
) -> Fallible<(Gathered, String)> {
    let git_dir = pinned_git_dir(repo_root)?;
    let store = pinned_store(repo_root, child_repo)?;
    let (redaction, source) = pinned_policy(&git_dir, policy_rev)?;
    Ok(((git_dir, store, redaction), source))
}

/// Gruppen-Id aus Zahl oder Gruppenname.
fn group_id(name: &str) -> Fallible<u32> {
    if let Ok(gid) = name.parse::<u32>() {
        return Ok(gid);
    }
    let cname = std::ffi::CString::new(name).map_err(|_| "invalid socket group")?;
    // SAFETY: nul-terminierter Name; getgrnam liefert NULL oder einen Zeiger
    // auf eine statische Struktur, die hier sofort gelesen wird.
    let entry = unsafe { libc::getgrnam(cname.as_ptr()) };
    if entry.is_null() {
        return Err(format!("unknown group {}", crate::text::sanitize(name)).into());
    }
    // SAFETY: nicht NULL, siehe oben.
    Ok(unsafe { (*entry).gr_gid })
}

/// Gibt `dir` der Gruppe `group` zum Durchqueren frei (0710).
fn share_with_group(dir: &Path, group: u32) -> Fallible<()> {
    let name = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())?;
    // SAFETY: nul-terminierter Pfad; uid=-1 lässt den Eigentümer stehen.
    if unsafe { libc::chown(name.as_ptr(), !0, group) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o710))?;
    Ok(())
}

/// Das Git-Verzeichnis, das der Witness festhält: kanonisch und genau
/// `<repo_root>/.git`. Verlinkte Worktrees und ausgelagerte Git-Verzeichnisse
/// teilen ihr Verzeichnis mit Orten außerhalb dessen, was der Witness
/// festhalten kann — sie werden abgelehnt.
fn pinned_git_dir(repo_root: &Path) -> Fallible<PathBuf> {
    let dot_git = repo_root.join(".git");
    if !fs::symlink_metadata(&dot_git).is_ok_and(|meta| meta.is_dir()) {
        return Err(
            "the witness requires a plain .git directory (no linked worktree or gitdir file)"
                .into(),
        );
    }
    Ok(dot_git.canonicalize()?)
}

/// Der Store aus der Konfiguration des Repos zum Zeitpunkt von `init` — der
/// Mensch richtet den Witness ein, nicht der Agent. Ein Child-Repo muss
/// außerhalb des beobachteten Repos liegen, der Ref unter `refs/minds/`.
fn pinned_store(repo_root: &Path, child_repo: Option<&Path>) -> Fallible<PinnedStore> {
    let config = crate::config::load(repo_root);
    let context_ref = config.reference().to_owned();
    if !context_ref.starts_with(minds_git::MINDS_REF_NAMESPACE) {
        return Err("context ref outside refs/minds/ cannot be pinned for the witness".into());
    }
    match (child_repo, config.backend()) {
        (None, minds_store::Backend::InRepo) => Ok(PinnedStore::InRepo { context_ref }),
        // Die Konfiguration nennt ein Child-Repo, der Mensch nicht: Diesen
        // Pfad kann der Agent geschrieben haben — nicht still festhalten.
        (None, minds_store::Backend::ChildRepo { .. }) => Err(
            "the repository config selects a child-repo store; pass --child-repo <path> \
             to pin it for the witness explicitly"
                .into(),
        ),
        (Some(path), _) => Ok(PinnedStore::ChildRepo {
            context_ref,
            child_path: child_store_path(repo_root, path)?,
        }),
    }
}

/// Der kanonische Pfad eines ausdrücklich genannten Child-Repos — außerhalb
/// des beobachteten Repos, und das Repo nicht darin.
fn child_store_path(repo_root: &Path, path: &Path) -> Fallible<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    };
    let child_path = path
        .canonicalize()
        .map_err(|_| "the child repository for the witness store must exist")?;
    if child_path.starts_with(repo_root) || repo_root.starts_with(&child_path) {
        return Err("the child repository must lie outside the observed repository".into());
    }
    Ok(child_path)
}

/// Größte Policy-Datei, die `init` liest.
const MAX_PINNED_POLICY: u64 = 64 * 1024;

/// Die Policy für den Witness: `.minds/redact.json` aus einem Commit — HEAD
/// oder der Revision, die der Mensch mit `--policy-rev` nennt —, nach unten
/// begrenzt. Gibt die Quelle samt Commit- und Blob-Id für den Bericht mit.
///
/// Gelesen über das festgehaltene Git-Verzeichnis, ohne Config-Includes und
/// ohne `refs/replace/*`, mit einer Größengrenze am Objekt-Header — kein
/// `git`-Prozess, der der Konfiguration des Agenten folgte. Nur „die Datei
/// steht nicht im Baum" (oder es gibt noch keinen Commit) ergibt den
/// strengen Default; jeder andere Fehler bricht `init` ab, statt still den
/// Default festzuhalten.
///
/// Ehrliche Grenze: Lokale Refs gehören dem Agenten — er kann eine
/// geschwächte Policy committen, bevor ein Mensch `init` ausführt. Deshalb
/// stehen Commit- und Blob-Id im Bericht: Der Mensch vergleicht sie mit der
/// Version, die sein Team geprüft hat. Eine Policy, die ihre eigenen
/// Platzhalter trifft, wird abgelehnt — sie machte jede redigierte Session
/// instabil.
fn pinned_policy(
    git_dir: &Path,
    policy_rev: Option<&str>,
) -> Fallible<(minds_redact::RedactionConfig, String)> {
    // Dieselbe Layout-Prüfung wie vor jedem Witness-Lauf: keine
    // `alternates`, kein `commondir`, keine Symlinks in `.git`.
    plain_git_dir_within(git_dir, MAX_GIT_ENTRIES).map_err(|err| {
        err.to_string()
            .replace("; checkpoint deferred", "; witness init refused")
    })?;
    // `refs/replace/*` und `info/grafts` liest minds nicht — das `git` des
    // Menschen schon. Er sähe bei der Kontrolle der gedruckten Ids einen
    // anderen Inhalt als den festgehaltenen. Lieber abbrechen.
    let replace = git_dir.join("refs/replace");
    let loose_replacements = match fs::read_dir(&replace) {
        Ok(mut dir) => dir.next().is_some(),
        Err(err) => err.kind() != std::io::ErrorKind::NotFound,
    };
    let has_replacements = loose_replacements || packed_refs_mention(git_dir, "refs/replace/");
    if has_replacements || fs::symlink_metadata(git_dir.join("info/grafts")).is_ok() {
        return Err(
            "refs/replace or info/grafts exists — your own git would show other content than \
             minds pins; remove them (`git replace -d …`) and rerun"
                .into(),
        );
    }
    let repo = minds_git::Repo::open_pinned(git_dir)?;
    let commit = match policy_rev {
        None => repo.head()?.commit(),
        // Eine volle Commit-Id zuerst: Sie ist die einzige Angabe, die der
        // Agent nicht umlenken kann — Refs (auch einer, der so heißt wie die
        // Id) gehören ihm. Dass der Commit stimmt, prüft das Nachhashen.
        Some(rev) => match rev.parse::<minds_git::CommitId>() {
            Ok(id) => Some(id),
            _ => Some(repo.commit_at(rev)?.ok_or_else(|| {
                format!(
                    "--policy-rev {} names no commit",
                    crate::text::sanitize(rev)
                )
            })?),
        },
    };
    let Some(commit) = commit else {
        let config = minds_redact::RedactionConfig::default();
        return Ok((config, "strict default (no commit yet)".to_owned()));
    };
    let commit_hex = commit.to_string();
    let (config, source) =
        match repo.read_blob_bounded(commit, ".minds/redact.json", MAX_PINNED_POLICY)? {
            Some((blob, bytes)) => {
                let config: minds_redact::RedactionConfig =
                serde_json::from_slice(&bytes).map_err(|err| {
                    format!(
                        ".minds/redact.json at {commit_hex} is not a valid redaction policy — {} \
                             at line {}",
                        match err.classify() {
                            serde_json::error::Category::Syntax => "syntax error",
                            serde_json::error::Category::Data =>
                                "unexpected value or unknown field",
                            serde_json::error::Category::Eof => "unexpected end of file",
                            serde_json::error::Category::Io => "read error",
                        },
                        err.line()
                    )
                })?;
                (
                    config,
                    format!(".minds/redact.json at commit {commit_hex}, blob {blob}"),
                )
            }
            None => (
                minds_redact::RedactionConfig::default(),
                format!("strict default (no .minds/redact.json at commit {commit_hex})"),
            ),
        };
    let config = config.floored_at_default();
    refuse_unstable_policy(&config)?;
    Ok((config, source))
}

/// Ob `packed-refs` einen Ref unter `prefix` nennt — begrenzt gelesen.
fn packed_refs_mention(git_dir: &Path, prefix: &str) -> bool {
    packed_refs_mention_within(git_dir, prefix, MAX_PACKED_REFS)
}

/// Größte `packed-refs`, die `init` durchsucht.
const MAX_PACKED_REFS: u64 = 64 * 1024 * 1024;

fn packed_refs_mention_within(git_dir: &Path, prefix: &str, limit: u64) -> bool {
    // Nur „gibt es nicht" heißt „nichts ersetzt". Jeder andere Fehler (ein
    // inzwischen getauschter Symlink, EACCES) und jede Datei, die keine
    // gewöhnliche ist (ein FIFO liest sich leer), gelten als vorhanden.
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(git_dir.join("packed-refs"))
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        return true;
    }
    // Ein Byte mehr als die Grenze lesen: Liegt die Datei darüber, ist der
    // Rest ungelesen — er gilt als vorhanden. `take` allein schnitte still
    // ab, und ein `refs/replace/…` hinter genug Füllzeilen bliebe unsichtbar.
    let mut bytes = Vec::new();
    if file.take(limit + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > limit {
        return true;
    }
    let text = String::from_utf8_lossy(&bytes);
    text.lines().any(|line| {
        line.split_whitespace()
            .nth(1)
            .is_some_and(|name| name.starts_with(prefix))
    })
}

/// Ein Text aus Platzhaltern in den Lagen, in denen Detektoren suchen —
/// redigiert ihn die Policy um, wäre jede redigierte Session instabil.
fn refuse_unstable_policy(config: &minds_redact::RedactionConfig) -> Fallible<()> {
    const PROBE: &str = "[redacted:secret] [redacted:pii] token=[redacted:secret] \
         password: [redacted:secret] user=[redacted:pii] \
         \"api_key\": \"[redacted:secret]\" Authorization: Bearer [redacted:secret]";
    let pipeline = config.pipeline()?;
    let once = pipeline.redact(PROBE).text;
    if once != PROBE || pipeline.redact(&once).text != once {
        return Err(
            "the redaction policy matches its own placeholders (e.g. a term like \"redacted\") — \
             every redacted session would be unstable; refusing to pin it"
                .into(),
        );
    }
    Ok(())
}

/// Liegt das Witness-Home im beobachteten Repo, sähe der Beobachter die
/// eigenen Schreibzugriffe — und der Agent das Home. Der Witness startet
/// dann nicht (EA-08).
fn refuse_home_in_repo(home: &Path, config: &Config) -> Fallible<()> {
    refuse_home_in(home, &config.repo_root)
}

fn refuse_home_in(home: &Path, repo_root: &Path) -> Fallible<()> {
    if home.canonicalize()?.starts_with(repo_root) {
        return Err("witness home must not be inside the repository".into());
    }
    Ok(())
}

fn plain(path: &Path) -> bool {
    !path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
}

pub(crate) fn load(home: &Path) -> Fallible<Config> {
    let home: PathBuf = home.components().collect();
    // Erst die Konfiguration, privat und ohne Symlink gelesen — sie sagt, ob
    // Home und `run/` der Socket-Gruppe offenstehen dürfen. Dann der Baum.
    let config: Config = serde_json::from_slice(&read_private(&home, "witness.json")?)?;
    validate_tree_shared(&home, config.socket_group.map(|gid| (home.as_path(), gid)))?;
    if !matches!(config.schema_version, 1 | SCHEMA_VERSION)
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
    // Die Pins sind nur so gut wie ihre Prüfung beim Laden: Auch eine von
    // Hand geänderte Datei lenkt den Witness nicht aus dem Repo heraus.
    if let Some(git_dir) = &config.git_dir {
        if git_dir != &config.repo_root.join(".git") || !plain(git_dir) {
            return Err("invalid pinned git directory".into());
        }
    }
    let store = config.store();
    if !store
        .context_ref()
        .starts_with(minds_git::MINDS_REF_NAMESPACE)
    {
        return Err("pinned context ref outside refs/minds/".into());
    }
    if let PinnedStore::ChildRepo { child_path, .. } = &store {
        // Kanonisch wie `repo_root`: Ein Symlink im Pfad lenkte die
        // Schreibzugriffe des Workers sonst an einen anderen Ort.
        if !child_path.is_absolute()
            || !plain(child_path)
            || child_path.starts_with(&config.repo_root)
            || config.repo_root.starts_with(child_path)
            || child_path.canonicalize().ok().as_ref() != Some(child_path)
        {
            return Err("invalid pinned child repository".into());
        }
    }
    Ok(config)
}

/// Private Zustandsbäume enthalten weder Symlinks noch fremde/hart verlinkte
/// Dateien. Der Socket ist die einzige Ausnahme von 0700/0600.
fn validate_tree(path: &Path) -> Fallible<()> {
    validate_tree_shared(path, None)
}

/// Wie [`validate_tree`]; mit `shared = Some((home, gid))` (das `user`-Profil)
/// dürfen genau das Home und `run/` der Gruppe `gid` das Durchqueren erlauben
/// (0710): Ohne Lesen sieht sie keine Namen, und alles darunter bleibt
/// 0700/0600. Jedes andere Verzeichnis, eine andere Gruppe oder ein anderes
/// Profil bleibt bei 0700.
fn validate_tree_shared(path: &Path, shared: Option<(&Path, u32)>) -> Fallible<()> {
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
    let traverse = match shared {
        Some((home, gid))
            if meta.is_dir() && meta.gid() == gid && (path == home || path == home.join("run")) =>
        {
            0o010
        }
        _ => 0,
    };
    if meta.mode() & 0o077 & !traverse != 0
        || (!meta.is_dir() && (!meta.is_file() || meta.nlink() != 1))
    {
        return Err("witness home requires private directories and files (0700/0600)".into());
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            match validate_tree_shared(&entry?.path(), shared) {
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

/// Liest `home/name`: Das Home ist ein eigenes Verzeichnis, kein Symlink;
/// die Datei wird ohne Folgen und ohne Blockieren geöffnet und am offenen
/// Deskriptor geprüft (eigen, gewöhnlich, 0600, ein Link, höchstens 1 MiB).
fn read_private(home: &Path, name: &str) -> Fallible<Vec<u8>> {
    let meta = fs::symlink_metadata(home)?;
    // SAFETY: geteuid hat keine Vorbedingungen.
    let me = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != me {
        return Err("witness home must be owned by this user and contain no symlinks".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(home.join(name))?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != me
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
        || meta.len() > 1024 * 1024
    {
        return Err("witness home requires private directories and files (0700/0600)".into());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024).read_to_end(&mut bytes)?;
    Ok(bytes)
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
    // Der Worker schreibt nicht selbst: Seine Zeilen legt der Witness ab,
    // dedupliziert und bereinigt wie die eigenen (EA-10).
    if worker::is_worker() {
        println!("L {}", worker::one_line(message));
        return;
    }
    // Erst entscheiden, dann schreiben: Der Mutex ist frei, bevor Redaktion
    // und Datei-I/O laufen.
    let lines = LOG_DEDUP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .admit(home, message);
    for line in lines {
        write_log(home, &line);
    }
}

/// Wie lange eine Logzeile als Wiederholung gilt.
const DEDUP_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
/// So viele verschiedene Zeilen merkt sich die Deduplizierung je Home.
const DEDUP_SLOTS: usize = 16;

/// Die Deduplizierung aller Zeilen von `witness.log` (EA-10).
///
/// Dieselbe Zeile innerhalb von [`DEDUP_WINDOW`] wird nur gezählt, nicht
/// geschrieben; ihre nächste Ausgabe danach — oder ihr Herausfallen aus den
/// [`DEDUP_SLOTS`] — trägt die Zahl nach. Eine Flut gleicher Zeilen (ein
/// Agent, der Fehler im Sekundentakt auslöst) drängt ältere Diagnosen so
/// nicht aus dem rotierenden Log; auch abwechselnde Zeilen nicht. Der Stand
/// ist je Home getrennt: Was ein Home verdrängt, landet nie im Log eines
/// anderen.
#[derive(Default)]
struct LogDedup {
    homes: BTreeMap<PathBuf, std::collections::VecDeque<Recent>>,
}

struct Recent {
    message: String,
    since: std::time::Instant,
    suppressed: usize,
}

static LOG_DEDUP: std::sync::Mutex<LogDedup> = std::sync::Mutex::new(LogDedup {
    homes: BTreeMap::new(),
});

fn repeated(n: usize, message: &str) -> String {
    format!("previous line repeated {n} more time(s): {message}")
}

impl LogDedup {
    /// Die Zeilen, die für `message` jetzt zu schreiben sind — keine, wenn
    /// sie eine Wiederholung ist.
    fn admit(&mut self, home: &Path, message: &str) -> Vec<String> {
        let recent = self.homes.entry(home.to_path_buf()).or_default();
        if let Some(entry) = recent.iter_mut().find(|entry| entry.message == message) {
            if entry.since.elapsed() < DEDUP_WINDOW {
                entry.suppressed += 1;
                return Vec::new();
            }
            let mut lines = Vec::new();
            if entry.suppressed > 0 {
                lines.push(repeated(entry.suppressed, message));
            }
            lines.push(message.to_owned());
            entry.since = std::time::Instant::now();
            entry.suppressed = 0;
            return lines;
        }
        let mut lines = Vec::new();
        recent.push_back(Recent {
            message: message.to_owned(),
            since: std::time::Instant::now(),
            suppressed: 0,
        });
        while recent.len() > DEDUP_SLOTS {
            if let Some(evicted) = recent.pop_front() {
                if evicted.suppressed > 0 {
                    lines.push(repeated(evicted.suppressed, &evicted.message));
                }
            }
        }
        lines.push(message.to_owned());
        lines
    }

    /// Was beim Beenden noch nachzutragen ist: gezählt, aber nicht
    /// geschrieben.
    fn flush(&mut self, home: &Path) -> Vec<String> {
        self.homes
            .remove(home)
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.suppressed > 0)
            .map(|entry| repeated(entry.suppressed, &entry.message))
            .collect()
    }
}

/// Schreibt beim Beenden des Witness die noch gezählten Wiederholungen.
fn flush_log(home: &Path) {
    let lines = LOG_DEDUP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .flush(home);
    for line in lines {
        write_log(home, &line);
    }
}

fn write_log(home: &Path, message: &str) {
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

/// Die Wanduhr des Witness (RFC 3339, Unix-Nanos) — eine Ablesung. Was ins
/// Journal kommt, stempelt [`Writer::append`] darüber hinaus monoton
/// ([`witness_clock`]).
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
    /// Die monotone Uhr über alle Läufe (EA-08a).
    clock: witness_clock::WitnessClock,
    folders: BTreeMap<SessionKey, ChainFolder>,
    follow: bool,
    /// Ob seit dem letzten vollständigen Checkpoint ein Agent-Event kam oder
    /// eine Session vertagt blieb. Ohne das gibt es nichts zu versiegeln.
    dirty: bool,
    /// Ende des letzten Checkpoint-Laufs auf Anfrage je Client (Commit),
    /// gleich mit welchem Ausgang (EA-10).
    limits: limits::Limits,
    /// Das Binary, das den Checkpoint als eigenen Prozess ausführt (EA-10).
    /// `None` im Test: Dort läuft er im eigenen Prozess.
    worker: Option<PathBuf>,
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
    fn new(home: &Path, config: Config, follow: bool) -> Fallible<Self> {
        Ok(Self {
            home: home.into(),
            config,
            journal: Journal::at(home.join("journal")),
            epochs: EpochState::at(home.join("evidence/state")),
            clock: witness_clock::WitnessClock::open(home)?,
            folders: BTreeMap::new(),
            follow,
            dirty: true,
            limits: limits::Limits::default(),
            worker: None,
            last_failure: None,
            stream: None,
            observer: None,
            fs_dirty: false,
            ignore_failure_logged: false,
            fs_ready: std::collections::VecDeque::new(),
            settled_this_step: false,
            last_gap: std::collections::HashMap::new(),
        })
    }

    fn open(home: &Path, config: Config, follow: bool) -> Fallible<Self> {
        let mut writer = Self::new(home, config, follow)?;
        writer.recover_all()?;
        writer.witness_ledger()?;
        Ok(writer)
    }

    /// Der Schreiber im Worker-Prozess (EA-10): Er hängt nichts an und
    /// übernimmt die Live-Folds so, wie der Witness sie nach jedem Append
    /// persistiert hat — **ohne** sie aus dem Journal neu abzuleiten. Nur so
    /// prüft `validate` weiter, dass das Journal genau das Gefaltete enthält.
    /// Fehlt der Stand einer Session, bleibt sie ohne Fold und wird vertagt.
    fn for_worker(home: &Path, config: Config) -> Fallible<Self> {
        let mut writer = Self::new(home, config, false)?;
        for key in writer.journal.sessions()?.keys {
            match fs::read(folder_path(home, &key)) {
                Ok(bytes) => {
                    let state: FolderState = serde_json::from_slice(&bytes)?;
                    writer.folders.insert(key, ChainFolder::from_state(state));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(writer)
    }

    /// Der Journal-Fsync kommt vor dem Folder-Fsync. Nach einem Crash darf
    /// ausschließlich ein intakter, bereits persistierter Präfix ergänzt werden.
    fn recover(&mut self, key: &SessionKey) -> Fallible<()> {
        let read = self.journal.read(key)?;
        verify_events(&read.events)?;
        // Was schon im Journal steht, hat die Uhr vergeben — auch ein Witness
        // von vor EA-08a ohne Uhr-Datei stempelt danach nicht früher.
        for event in &read.events {
            self.clock.witnessed(event.at_nanos);
        }
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

    /// Jeder Stempel im Journal läuft hier durch die monotone Uhr: Lebenszyklus,
    /// `fs.observed`, `fs.gap`, `fs.checkpoint` und angenommene Hook-Events.
    /// Die Marke ist persistiert, bevor das Event angehängt wird.
    ///
    /// Der Stempel ist damit der **Journal-Zeitpunkt**: Bei `fs.observed`
    /// liegt er nie vor der Klassifikation, kann aber um die Wartezeit in der
    /// begrenzten Warteschlange (`fs_ready`) dahinter liegen.
    fn append(&mut self, key: &SessionKey, mut event: NewEvent) -> Fallible<JournalEvent> {
        if !self.folders.contains_key(key) {
            self.recover(key)?;
        }
        (event.at, event.at_nanos) = self.clock.stamp((event.at, event.at_nanos))?;
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

    /// Beginnt den eigenen Stream `key` eines Laufs: Erst schaltet `arm` den
    /// Beobachter scharf, **dann** kommt `witness.start` ins Journal. Dessen
    /// Stempel ist der Beginn der ersten Epoche (`started_at`, EA-08a), und
    /// der Leser verankert an ihm — was danach geschrieben wird, meldet der
    /// Watcher also schon (gesammelt, bis die Eventloop es abholt).
    fn start_stream<T>(
        &mut self,
        key: &SessionKey,
        payload: serde_json::Value,
        arm: impl FnOnce() -> T,
    ) -> Fallible<T> {
        let armed = arm();
        self.lifecycle(key, "witness.start", payload)?;
        // Der eigene Stream gilt auch ohne Beobachter: Jeder Checkpoint
        // schließt seine Epoche mit `fs.checkpoint` ab.
        self.stream = Some(key.clone());
        Ok(armed)
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
    /// - Sonst höchstens ein Lauf je [`MIN_CHECKPOINT_INTERVAL`] **je Client**
    ///   (Commit, siehe [`limits`]), gemessen vom **Ende** des letzten Laufs
    ///   für ihn — gezählt wird jeder Lauf, auch ein gescheiterter: Sonst
    ///   ließe sich die Grenze mit absichtlich scheiternden oder langsamen
    ///   Läufen umgehen. Die Anfrage des Menschen für seinen frischen Commit
    ///   hält eine Flut des Agenten nicht auf (EA-10).
    /// - Der Lauf selbst geschieht im Witness als eigener Prozess
    ///   ([`worker`]): mit Frist und Ressourcengrenzen, gegen das
    ///   festgehaltene Git-Verzeichnis (EA-10).
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
        if self.limits.blocked(commit, MIN_CHECKPOINT_INTERVAL) {
            return Err("rate limited");
        }
        let result = match self.worker.clone() {
            Some(exe) => self.checkpoint_in_worker(&exe, commit, requester_alive),
            None => self.checkpoint_in_process(commit, requester_alive),
        };
        match result {
            Ok(ran) => {
                self.limits.record(commit, ran.retrofitted.as_deref());
                self.note_failure(None);
                Ok(ran.status)
            }
            Err(err) => {
                self.limits.record_failure(commit);
                self.note_failure(Some(format!("checkpoint failed: {err}")));
                Err("checkpoint failed")
            }
        }
    }

    /// Der Lauf im eigenen Prozess — im Test, und überall dort, wo kein
    /// Worker-Binary feststeht.
    fn checkpoint_in_process(
        &mut self,
        commit: &str,
        requester_alive: &dyn Fn() -> bool,
    ) -> Fallible<Ran> {
        // Ein Panic in gix über feindlichen Objekt- oder Pack-Daten darf den
        // einzigen Schreiber nicht beenden. Danach sind die Live-Folds
        // fraglich: verwerfen — `append` lädt sie über `recover` samt
        // Präfixprüfung neu von der Platte.
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.checkpoint_now_for(Some(commit), requester_alive)
        }));
        match run {
            Ok(result) => result,
            Err(_) => {
                self.folders.clear();
                self.dirty = true;
                Err("checkpoint panicked".into())
            }
        }
    }

    /// Der Lauf im Worker-Prozess (EA-10). Der Witness wartet währenddessen
    /// und hängt nichts an; seine Live-Folds bleiben unberührt, auch wenn das
    /// Kind stirbt — was es schon versiegelt und verworfen hat, fällt mit
    /// [`Self::finish_checkpoint`] heraus.
    fn checkpoint_in_worker(
        &mut self,
        exe: &Path,
        commit: &str,
        requester_alive: &dyn Fn() -> bool,
    ) -> Fallible<Ran> {
        self.begin_checkpoint()?;
        let result = worker::run(
            exe,
            &self.home,
            commit,
            self.stream.as_ref(),
            requester_alive,
        );
        self.finish_checkpoint(result.is_ok());
        result.map_err(|failure| match failure {
            worker::Failure::Failed(reason) => reason.into(),
            worker::Failure::Aborted(reason) => {
                self.report_stale_locks();
                reason.into()
            }
        })
    }

    /// Nach einem abgebrochenen Lauf: Lock-Dateien, die das Kind trotz
    /// `SIGTERM`-Aufräumen hinterlassen haben könnte, beim Namen nennen — ein
    /// liegengebliebenes `HEAD.lock` blockiert sonst das nächste
    /// `git commit` des Menschen, ohne dass jemand weiß, woher es kommt.
    /// Entfernt wird nichts: Ob das Lock noch einem anderen Git-Prozess
    /// gehört, weiß der Witness nicht.
    fn report_stale_locks(&self) {
        let mut roots = vec![self.config.git_dir()];
        if let PinnedStore::ChildRepo { child_path, .. } = self.config.store() {
            roots.push(child_path.join(".git"));
            roots.push(child_path);
        }
        // Höchstens [`MAX_REPORTED_LOCKS`] Zeilen: Wie viele Lock-Dateien
        // dort liegen, bestimmt der Agent — jede eine eigene Zeile drängte
        // sonst jede andere Diagnose aus dem rotierenden Log.
        let locks: Vec<PathBuf> = roots.iter().flat_map(|root| stale_locks(root)).collect();
        for lock in locks.iter().take(MAX_REPORTED_LOCKS) {
            log(
                &self.home,
                &format!(
                    "checkpoint aborted; lock file left behind: {}",
                    lock.display()
                ),
            );
        }
        if locks.len() > MAX_REPORTED_LOCKS {
            log(
                &self.home,
                &format!(
                    "checkpoint aborted; and {} more lock file(s)",
                    locks.len() - MAX_REPORTED_LOCKS
                ),
            );
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
            .map(|ran| ran.status)
    }

    /// [`Self::checkpoint_now`] für eine Anfrage, deren Anfragender gehen kann.
    fn checkpoint_now_for(
        &mut self,
        commit: Option<&str>,
        requester_alive: &dyn Fn() -> bool,
    ) -> Fallible<Ran> {
        self.begin_checkpoint()?;
        let result = self.checkpoint_sessions(commit, requester_alive);
        self.finish_checkpoint(result.is_ok());
        result
    }

    /// Was vor jedem Lauf im Witness selbst geschieht: Beobachtungen
    /// übernehmen und die Grenze der Beobachtungs-Epoche setzen.
    fn begin_checkpoint(&mut self) -> Fallible<()> {
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
        // Gestempelt wird monoton ([`Self::append`]): nach jedem Event, das
        // der Witness je angehängt hat, also auch nach dem letzten jeder
        // offenen Session — springt die Wanduhr zurück, läge die Grenze sonst
        // vor dem bezeugten Bereich.
        if let Some(stream) = self.stream.clone() {
            let at = clock::now();
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
        Ok(())
    }

    /// Was nach jedem Lauf im Witness selbst geschieht — gleich, ob er im
    /// eigenen Prozess oder im Worker lief, und gleich mit welchem Ausgang.
    fn finish_checkpoint(&mut self, completed: bool) {
        // Auch ein Fehler mitten im Lauf kann Sessions bereits verworfen haben.
        // Ihr alter Fold darf nie die nächste Epoche fortsetzen; `append` lädt
        // ihn über `recover` samt `.sealed`-Prüfung neu. Alle übrigen Folds
        // bleiben der Live-Stand: Neu aus dem Journal gelesen würde ein
        // vertagter Integritätsfehler still übernommen.
        let open = self.journal.sessions().map(|open| open.keys);
        match &open {
            Ok(open) => self.folders.retain(|key, _| open.contains(key)),
            Err(_) => self.folders.clear(),
        }
        // Ist die Beobachtungs-Epoche geschlossen, beginnt die nächste ohne
        // Gedächtnis: Schreibt jemand danach denselben Inhalt erneut, ist das
        // in ihr eine eigene Beobachtung. Auch eine verbliebene Lücke will
        // versiegelt werden.
        self.fs_dirty = match &self.stream {
            Some(stream) => self.journal.read(stream).map_or(true, |read| {
                read.events.iter().any(|e| e.raw_kind.starts_with("fs."))
            }),
            None => false,
        };
        if !self.fs_dirty {
            if let Some(observer) = self.observer.as_mut() {
                observer.forget_states();
            }
        }
        // Was nach einem vollständigen Lauf noch offen ist, wurde vertagt
        // (Store, Redaction, Integrität — der Grund steht im eigenen Log).
        // Lässt sich das nicht zählen, oder scheiterte der Lauf, bleibt
        // `dirty` gesetzt: Lieber ein Lauf zu viel als offene Sessions, die
        // keiner mehr anfasst.
        self.dirty = match (completed, &open) {
            (true, Ok(open)) => open.iter().any(|key| self.includes(key)),
            _ => true,
        };
    }

    /// Hebt die Uhr auf jeden Seal im Ledger an: Es überdauert das Verwerfen
    /// versiegelter Journale. So stempelt auch ein Witness ohne Uhr-Datei
    /// (von vor EA-08a, oder die Datei ging verloren) nie vor einem
    /// versiegelten Event. Eine unlesbare Zeile (etwa ein abgerissenes
    /// Ende nach einem Absturz) hebt nichts an.
    fn witness_ledger(&mut self) -> Fallible<()> {
        let ledger = match fs::read_to_string(self.home.join("ledger")) {
            Ok(ledger) => ledger,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        for line in ledger.lines() {
            let nanos = line
                .split_whitespace()
                .nth(2)
                .and_then(|at| at.parse::<jiff::Timestamp>().ok())
                .and_then(|at| u64::try_from(at.as_nanosecond()).ok());
            if let Some(nanos) = nanos {
                self.clock.witnessed(nanos);
            }
        }
        Ok(())
    }

    fn recover_all(&mut self) -> Fallible<()> {
        for key in self.journal.sessions()?.keys {
            self.recover(&key)?;
        }
        Ok(())
    }

    fn checkpoint_sessions(
        &self,
        commit: Option<&str>,
        requester_alive: &dyn Fn() -> bool,
    ) -> Fallible<Ran> {
        let root = &self.config.repo_root;
        // Ohne Pins wüsste der Witness Policy und Store nur aus Dateien, die
        // der Agent schreiben kann — oder aus Standards, die die Extras des
        // Teams (`deny_secrets`, `secret_keys`) still fallen ließen.
        // Fail-closed: erst nach einem erneuten `minds witness init`.
        if !self.config.pinned() {
            return Err(UNPINNED.into());
        }
        // Der Worktree gehört dem Agenten; seit EA-06d kann er diesen Lauf
        // jederzeit auslösen. Was er dort ändern kann, darf den Witness nicht
        // umlenken (00-conventions: „Nothing in `.git/config` may influence
        // trust decisions") — geprüft, bevor gix oder git etwas daraus lesen.
        plain_repo_layout(root)?;
        // Das Git-Verzeichnis aus `witness.json`, einmal geöffnet, ohne
        // Includes: Ein `include.path` auf ein FIFO oder eine fremde Datei
        // erreicht den Witness nicht (EA-10).
        let git_dir = self.config.git_dir();
        let dot_git = root.join(".git").canonicalize()?;
        if dot_git != git_dir {
            return Err("witness git directory moved; checkpoint deferred".into());
        }
        let repo = minds_git::Repo::open_pinned(&git_dir)?;
        // Ein Ref-Namespace aus der Konfiguration des Agenten verschöbe alle
        // Schreibzugriffe unter `refs/namespaces/<ns>/…` — heraus aus
        // `refs/minds/`.
        if repo.has_ref_namespace() {
            return Err("refs namespaces are not supported; checkpoint deferred".into());
        }
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
        // Store und Policy aus `witness.json` (EA-10): Backend, Ref und
        // Child-Pfad legt fest, wer den Witness eingerichtet hat — nicht die
        // `.git/config` des Agenten. `load` hat den Ref unter `refs/minds/`
        // und den Child-Pfad außerhalb des Repos schon geprüft.
        //
        // Der Store schreibt über **dasselbe** geöffnete Repo, das oben
        // geprüft wurde — ein zweites Öffnen läse eine inzwischen geänderte
        // Konfiguration (etwa einen Ref-Namespace). Ein Child-Repo wird
        // ebenso ohne Includes geöffnet und auf denselben Namespace geprüft.
        let store: Box<dyn minds_store::ContextStore> = match self.config.store() {
            PinnedStore::InRepo { context_ref } => {
                Box::new(minds_store::InRepoStore::from_repo(repo.clone()).with_ref(context_ref))
            }
            PinnedStore::ChildRepo {
                context_ref,
                child_path,
            } => {
                // Dieselbe Layout-Prüfung wie für das beobachtete Repo: Ein
                // Symlink, `commondir` oder `alternates` im Child lenkte die
                // Schreibzugriffe des Workers sonst an einen anderen Ort.
                let child_git = if fs::symlink_metadata(child_path.join(".git"))
                    .is_ok_and(|meta| meta.is_dir())
                {
                    child_path.join(".git")
                } else {
                    child_path.clone()
                };
                plain_git_dir_within(&child_git, MAX_GIT_ENTRIES)?;
                let child = minds_git::Repo::open_pinned(&child_git)?;
                if child.has_ref_namespace() {
                    return Err(
                        "refs namespaces are not supported in the child repository; checkpoint deferred"
                            .into(),
                    );
                }
                Box::new(minds_store::ChildRepoStore::from_repo(child).with_ref(context_ref))
            }
        };
        // Die Witness-eigene Policy; `.minds/redact.json` im Worktree liest
        // der Witness nicht mehr.
        let pipeline = self.config.policy().pipeline()?;
        let tracked = crate::checkpoint::tracked_files_pinned(&repo);
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
            self,
            &streams,
        );
        // Ob die Epoche geschlossen ist, wertet `finish_checkpoint` im
        // Witness aus — auch nach einem Lauf im Worker.
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
        // das nicht als „nichts zu tun" lesen.
        let deferred = match self.journal.sessions() {
            Ok(open) => open.keys.iter().filter(|key| self.includes(key)).count(),
            Err(err) => {
                log(&self.home, &format!("open sessions not counted: {err}"));
                0
            }
        };
        // Der Commit, an dem die Trailer jetzt stehen, ist für das Rate-Limit
        // derselbe Client wie der angefragte ([`limits`]).
        let retrofitted = match &attached {
            Ok(Some(update)) if update.rewrote_head() => Some(update.commit().to_string()),
            _ => None,
        };
        Ok(Ran {
            status: checkpoint_status(&outcome.sealed, deferred, attached),
            retrofitted,
        })
    }
}

/// Das Ergebnis eines vollständigen Checkpoint-Laufs.
struct Ran {
    /// Die Statuszeile für den `Ack`.
    status: String,
    /// Der Commit, an den die Trailer nachgerüstet wurden — falls HEAD dabei
    /// umgeschrieben wurde.
    retrofitted: Option<String>,
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
/// `include.path` erreicht den Witness seit EA-10 nicht mehr: Er öffnet das
/// festgehaltene Git-Verzeichnis ohne Includes. Und was zwischen Prüfung und
/// Zugriff zum FIFO wird oder eine gemappte Datei kürzt, trifft nur den
/// Worker-Prozess mit seiner Frist.
///
/// Ehrliche Grenze: Das ist eine Prüfung vor dem Lauf. Wer zwischen Prüfung
/// und Zugriff einen Eintrag gegen einen **Symlink** tauscht, lenkt einen
/// Schreibzugriff des Workers um — er läuft unter derselben Kennung wie der
/// Witness. Das schlösse erst ein Zugriff relativ zu einem offenen
/// Verzeichnis-Deskriptor (`openat` ohne Folgen), den gix nicht anbietet.
fn plain_repo_layout(root: &Path) -> Fallible<()> {
    plain_repo_layout_within(root, MAX_GIT_ENTRIES)
}

/// So viele liegengebliebene Lock-Dateien nennt das Log je Abbruch beim Namen.
const MAX_REPORTED_LOCKS: usize = 8;

/// Die `*.lock`-Dateien eines Git-Verzeichnisses, die ein abgebrochener
/// Lauf hinterlassen haben kann: an der Wurzel (`HEAD.lock`,
/// `packed-refs.lock`, `index.lock`) und unter `refs/`. Begrenzt durchsucht,
/// ohne einem Symlink zu folgen.
fn stale_locks(git_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for name in ["HEAD.lock", "packed-refs.lock", "index.lock", "config.lock"] {
        let path = git_dir.join(name);
        if fs::symlink_metadata(&path).is_ok() {
            found.push(path);
        }
    }
    let mut seen = 0usize;
    let mut stack = vec![(git_dir.join("refs"), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > 10_000 {
                return found;
            }
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() && depth < MAX_GIT_DEPTH {
                stack.push((path, depth + 1));
            } else if path.extension().is_some_and(|ext| ext == "lock") {
                found.push(path);
            }
        }
    }
    found
}

/// Höchstzahl der Einträge, die [`plain_repo_layout`] in `.git` ansieht —
/// und höchste Tiefe. Ein Agent, der `.git` mit Millionen Dateien füllt, soll
/// den einzigen Schreiber nicht minutenlang beschäftigen; ein Repo dieser
/// Größe wird vertagt statt geprüft.
const MAX_GIT_ENTRIES: usize = 200_000;
const MAX_GIT_DEPTH: usize = 16;

fn plain_repo_layout_within(root: &Path, max_entries: usize) -> Fallible<()> {
    plain_git_dir_within(&root.join(".git"), max_entries)
}

/// [`plain_repo_layout`] für ein Git-Verzeichnis selbst — auch das eines
/// baren Child-Repos, in das der Witness-Store schreibt (EA-10).
fn plain_git_dir_within(dot_git: &Path, max_entries: usize) -> Fallible<()> {
    let dot_git = dot_git.to_path_buf();
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
