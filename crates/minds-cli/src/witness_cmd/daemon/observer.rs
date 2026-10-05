//! Das zweite Auge des Witness (EA-08): Er beobachtet den Worktree selbst und
//! hält jede Inhaltsänderung als `fs.observed` in seinem eigenen Stream fest.
//!
//! # Was beobachtet wird — und was nie (W7)
//!
//! - Pfade werden **repo-relativ** gespeichert, nie als Host-Pfad.
//! - `.git/` und alles, was `.gitignore` / `.git/info/exclude` ausschließt,
//!   erscheint gar nicht: kein Pfad, kein Hash.
//! - Secret-Dateien erscheinen mit `reason: "secret_file"` — ohne Hash; es
//!   wird nicht einmal gelesen.
//! - Ein Hash entsteht nur über Inhalt, den die Redaction-Pipeline
//!   unverändert ließe (sonst `redacted_content`), und nur über UTF-8
//!   (sonst `unscannable`) — dieselbe Orakel-Regel wie `Effect.written`.
//! - Symlinks: Gefingerprintet wird ihr **Linktext** (das, was Git
//!   committet), nie das Ziel; zeigt der Link aus dem Repo, `outside_repo`.
//!   Eine Datei mit zweitem harten Link wird nur per `fstat` geprüft, nie
//!   gelesen (`outside_repo`).
//! - Inhalt verlässt den Prozess nie, nur sein blake3-Hash.
//!
//! # Ablauf
//!
//! `notify` meldet rohe Pfade in einen **begrenzten** Kanal. Die Eventloop
//! sammelt sie je Pfad und wartet [`DEBOUNCE`] Ruhe ab, bevor sie liest:
//! Ein Burst über dieselbe Datei wird **eine** Beobachtung des Endstands.
//! Je Durchlauf gilt ein Budget an Pfaden und Bytes — auch beim Checkpoint —,
//! damit die Hooks nie lange warten. Läuft der Kanal oder die Sammlung über,
//! wird das als `fs.gap` in die Kette geschrieben — eine sichtbare Lücke
//! statt einer stillen. Neu angelegte Verzeichnisse werden begrenzt
//! durchsucht: inotify meldet Dateien nicht, die vor ihrer Watch entstanden.
//!
//! # Ignore-Regeln
//!
//! Im Prozess ([`minds_git::ignore`]), **nie** über einen `git`-Aufruf: Die
//! Konfiguration und der Index gehören dem Agenten, und ein Git-Prozess
//! darin kann Befehle starten (Lazy-Fetch über `core.sshCommand`,
//! `core.fsmonitor` …). Maßgeblich sind genau `.gitignore` und
//! `.git/info/exclude`; getrackte Dateien (laut Index) gelten nie als
//! ignoriert. Lässt sich eine Regeldatei nicht sicher lesen, fallen die
//! Pfade darunter weg — lieber blind als einen ignorierten Pfad
//! fingerprinten — und die Lücke wird ein `fs.gap`.
//!
//! # Zugriff nur unterhalb der Wurzel
//!
//! Die Wurzel wird **einmal** als Verzeichnis-Deskriptor geöffnet; jeder
//! Zugriff geht Komponente für Komponente per `openat(O_NOFOLLOW)` bzw.
//! `fstatat(AT_SYMLINK_NOFOLLOW)` / `readlinkat` von dort aus. Ein Tausch der
//! Wurzel oder eines Verzeichnisses gegen einen Symlink nach außen lenkt den
//! Witness nicht um, und eine Dateisystemgrenze (Mount, FUSE) wird nie
//! überschritten. Gelesen werden nur gewöhnliche Dateien (kein FIFO hält die
//! Eventloop an).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use minds_core::ContentHash;
use minds_core::observation::{MAX_OBSERVED_BYTES, ObservationReason};
use minds_redact::RedactionPipeline;

use super::Fallible;

/// Ruhe je Pfad, bevor gelesen wird.
pub(super) const DEBOUNCE: Duration = Duration::from_millis(100);

/// Höchstzahl roher Meldungen zwischen zwei Durchläufen der Eventloop.
const CHANNEL_CAP: usize = 64 * 1024;

/// Höchstzahl gleichzeitig wartender Pfade. Darüber hinaus: `fs.gap`.
pub(super) const MAX_PENDING: usize = 64 * 1024;

/// Höchstzahl gemerkter Endstände und gesehener Pfade; darüber wird das
/// Gedächtnis geleert (kostet nur Wiederholungen bzw. Löschungen).
const MAX_REMEMBERED: usize = 64 * 1024;

/// Höchstsumme der gecachten Regel-Bytes; darüber wird der Cache geleert.
const MAX_RULE_CACHE_BYTES: usize = 32 * 1024 * 1024;

/// Höchstzahl der Pfade, die ein Durchlauf liest — die älteste Ruhe zuerst.
/// Der Rest wartet auf den nächsten Durchlauf: Ein `git checkout` über
/// zehntausend Dateien darf die Hooks nicht sekundenlang anhalten.
pub(super) const MAX_CLASSIFY_PER_STEP: usize = 256;

/// Höchstzahl der Bytes, die ein Durchlauf liest und hasht (mindestens eine
/// Datei je Durchlauf); der Rest wartet.
pub(super) const BYTES_PER_STEP: u64 = 8 * 1024 * 1024;

/// Höchstzahl der Einträge, die je neu angelegtem Verzeichnis eingesammelt
/// werden; darüber hinaus: `fs.gap`.
const MAX_WALK_ENTRIES: usize = 4096;

/// Größte Tiefe eines beobachteten Pfads (Komponenten); tiefer: `fs.gap`.
pub(super) const MAX_DEPTH: usize = 32;

/// Größte einzelne `.gitignore` bzw. `info/exclude`.
const MAX_RULE_BYTES: u64 = 1024 * 1024;

/// Höchstsumme der Regel-Bytes je Durchlauf; darüber gelten die übrigen
/// Regeldateien als unlesbar (fail-closed).
const RULE_BYTES_PER_STEP: u64 = 8 * 1024 * 1024;

/// Größter Index, der gelesen wird; darüber: keine getrackten Pfade.
const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;

/// Mindestabstand zwischen zwei Neu-Lesungen eines geänderten Index.
const INDEX_REREAD: Duration = Duration::from_secs(1);

/// Identität und Stand einer Datei: Gerät, Inode, Größe, mtime.
type Stamp = (u64, u64, u64, i64, i64);

/// Wie der Beobachter Inhalte fingerprintet — austauschbar, damit Tests
/// zählen können, dass für Secret- und ignorierte Dateien nie gehasht wird.
pub(super) trait ContentHasher: Send {
    fn hash(&self, bytes: &[u8]) -> ContentHash;
}

/// blake3 über die Bytes — dieselbe Form wie `Effect.written` und die
/// Reconciliation.
pub(super) struct Blake3;

impl ContentHasher for Blake3 {
    fn hash(&self, bytes: &[u8]) -> ContentHash {
        ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
    }
}

/// Eine beobachtete Änderung, bereit für das Journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Seen {
    pub(super) path: String,
    pub(super) content: Option<ContentHash>,
    pub(super) reason: Option<ObservationReason>,
    /// Wann der Witness den Stand las (RFC 3339, Unix-Nanos) — nicht, wann
    /// das Event angehängt wurde: Die Appends sind gedrosselt, und eine
    /// verspätete Zeit verschöbe die Beobachtung aus dem Fenster ihres
    /// Claims (oder in das einer Session, zu der sie nicht gehört).
    pub(super) at: (String, u64),
}

impl Seen {
    /// Der Payload des `fs.observed`-Events, kanonisch.
    pub(super) fn payload(&self) -> Fallible<String> {
        Ok(minds_core::to_canonical_string(&serde_json::json!({
            "path": self.path,
            "content": self.content,
            "reason": self.reason,
        }))?)
    }
}

/// Was ein Durchlauf ergab.
#[derive(Debug, Default)]
pub(super) struct Batch {
    pub(super) seen: Vec<Seen>,
    /// Meldungen gingen verloren (Kanal, Sammlung, zu großes Verzeichnis,
    /// Wurzel ausgetauscht).
    pub(super) overflow: bool,
    /// Ignore-Regeln ließen sich nicht sicher lesen; betroffene Pfade wurden
    /// verworfen.
    pub(super) ignore_failed: bool,
    /// Ob überhaupt Pfade klassifiziert wurden — nur dann sagt
    /// `ignore_failed == false` etwas über die Ignore-Auswertung.
    pub(super) classified: bool,
}

/// Ein wartender Pfad: zuletzt gemeldet um `at`; `fresh`, wenn er neu
/// angelegt oder hierher umbenannt wurde (dann wird ein Verzeichnis
/// durchsucht).
#[derive(Debug, Clone, Copy)]
struct Pending {
    at: Instant,
    fresh: bool,
}

pub(super) struct Observer {
    /// Kanonische Repo-Wurzel (Host-Pfad) — nur noch, um gemeldete Pfade
    /// repo-relativ zu machen.
    root: PathBuf,
    /// Die beim Start geöffnete Wurzel; aller Zugriff geht von hier aus.
    root_fd: File,
    root_dev: u64,
    root_ino: u64,
    rx: Receiver<notify::Result<notify::Event>>,
    tx: SyncSender<notify::Result<notify::Event>>,
    overflow: Arc<AtomicBool>,
    _watcher: Option<notify::RecommendedWatcher>,
    pending: HashMap<PathBuf, Pending>,
    /// Letzter festgehaltener Stand je Pfad: Eine Meldung ohne
    /// Inhaltsänderung (touch, chmod, das eigene Lesen) wird innerhalb einer
    /// Epoche keine zweite Beobachtung. Nach dem Versiegeln wird vergessen —
    /// ein `touch` danach beobachtet denselben Inhalt erneut.
    last: HashMap<String, (Option<ContentHash>, Option<ObservationReason>)>,
    /// Pfade, die dieser Lauf schon als beobachtbar (nicht ignoriert) sah.
    /// Nur für sie — und für getrackte — wird eine Löschung festgehalten:
    /// Ein gelöschtes Verzeichnis, das sich selbst ignorierte, nimmt seine
    /// `.gitignore` mit, und seine Pfade sollen trotzdem nie erscheinen.
    known: BTreeSet<String>,
    hasher: Box<dyn ContentHasher>,
    /// Prüft Inhalte vor dem Hash (Wörterbuch-Orakel, wie `Effect.written`).
    pipeline: RedactionPipeline,
    /// Die getrackten Pfade beim zuletzt gelesenen Stand des Index.
    tracked: Option<(Stamp, Instant, Arc<BTreeSet<String>>)>,
    /// Gelesene Regeldateien je repo-relativem Pfad und Stand.
    rule_cache: HashMap<String, (Stamp, Arc<Vec<u8>>)>,
}

/// Was ein Klassifizieren ergab.
#[derive(Default)]
struct Classified {
    seen: Vec<Seen>,
    ignore_failed: bool,
    /// Über das Byte-Budget hinaus: im nächsten Durchlauf.
    deferred: Vec<PathBuf>,
    /// Einträge neu angelegter Verzeichnisse.
    found: Vec<PathBuf>,
    /// Ein Verzeichnis hatte mehr Einträge, als durchsucht werden.
    truncated: bool,
}

impl Observer {
    /// Beobachtet `root` rekursiv. `root` muss kanonisch sein.
    pub(super) fn watch(root: &Path, hasher: Box<dyn ContentHasher>) -> Fallible<Self> {
        use notify::Watcher;
        let mut observer = Self::detached(root, hasher)?;
        let tx = observer.tx.clone();
        let overflow = observer.overflow.clone();
        let handler = move |event| match tx.try_send(event) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => overflow.store(true, Ordering::Relaxed),
        };
        // Nie einem Symlink in ein Verzeichnis folgen: Ein `ln -s ~ home` im
        // Worktree ließe den Witness sonst das Home des Hosts beobachten
        // (und die inotify-Watches aufbrauchen).
        let config = notify::Config::default().with_follow_symlinks(false);
        let mut watcher = notify::RecommendedWatcher::new(handler, config)?;
        watcher.watch(root, notify::RecursiveMode::Recursive)?;
        observer._watcher = Some(watcher);
        Ok(observer)
    }

    /// Ohne Watcher — Meldungen kommen nur über [`Self::note`]. Prüft
    /// Inhalte mit der eingebauten Standard-Policy, bis
    /// [`Self::with_pipeline`] eine Repo-Policy setzt.
    pub(super) fn detached(root: &Path, hasher: Box<dyn ContentHasher>) -> Fallible<Self> {
        let root_fd = open_dir_nofollow(root)?;
        let meta = root_fd.metadata()?;
        let (tx, rx) = mpsc::sync_channel(CHANNEL_CAP);
        Ok(Self {
            root: root.to_path_buf(),
            root_dev: meta.dev(),
            root_ino: meta.ino(),
            root_fd,
            rx,
            tx,
            overflow: Arc::new(AtomicBool::new(false)),
            _watcher: None,
            pending: HashMap::new(),
            last: HashMap::new(),
            known: BTreeSet::new(),
            hasher,
            pipeline: minds_redact::RedactionConfig::default().pipeline()?,
            tracked: None,
            rule_cache: HashMap::new(),
        })
    }

    /// Die Inhalts-Prüfung mit der Policy des Repos (nie schwächer als der
    /// Standard — der Aufrufer bodet sie ab).
    pub(super) fn with_pipeline(mut self, pipeline: RedactionPipeline) -> Self {
        self.pipeline = pipeline;
        self
    }

    /// Meldet einen geänderten Host-Pfad wie der Watcher.
    #[cfg(test)]
    pub(super) fn note(&self, path: &Path) {
        let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path(path.to_path_buf());
        if self.tx.try_send(Ok(event)).is_err() {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }

    /// Meldet einen neu angelegten Host-Pfad wie der Watcher.
    #[cfg(test)]
    pub(super) fn note_created(&self, path: &Path) {
        let event = notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::Any))
            .add_path(path.to_path_buf());
        if self.tx.try_send(Ok(event)).is_err() {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    pub(super) fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Ein Durchlauf: Meldungen einsammeln, beruhigte Pfade (mit `settle`
    /// alle, ohne die Ruhe abzuwarten) lesen und klassifizieren — innerhalb
    /// der Budgets je Durchlauf.
    pub(super) fn tick(&mut self, now: Instant, settle: bool) -> Batch {
        let mut batch = Batch {
            overflow: self.drain(now),
            ..Batch::default()
        };
        // Wurde die Wurzel ausgetauscht (umbenannt, ein Symlink an ihrer
        // Stelle), beziehen sich Meldungen nicht mehr auf das, was der
        // Deskriptor sieht: verwerfen, als Lücke.
        if !self.root_unchanged() {
            batch.overflow |= !self.pending.is_empty();
            self.pending.clear();
            return batch;
        }
        let mut settled: Vec<(Instant, PathBuf, bool)> = self
            .pending
            .iter()
            .filter(|(_, p)| settle || now.duration_since(p.at) >= DEBOUNCE)
            .map(|(path, p)| (p.at, path.clone(), p.fresh))
            .collect();
        if settled.is_empty() {
            return batch;
        }
        settled.sort();
        settled.truncate(MAX_CLASSIFY_PER_STEP);
        batch.classified = true;
        let mut fresh = HashSet::new();
        let mut originals = HashMap::new();
        let mut paths = Vec::new();
        for (at, path, is_fresh) in settled {
            self.pending.remove(&path);
            if is_fresh {
                fresh.insert(path.clone());
            }
            originals.insert(path.clone(), at);
            paths.push(path);
        }
        let classified = self.classify(paths, &fresh, now, crate::witness_cmd::daemon::now());
        batch.ignore_failed = classified.ignore_failed;
        batch.overflow |= classified.truncated;
        if self.last.len() > MAX_REMEMBERED {
            self.last.clear();
        }
        for seen in classified.seen {
            let state = (seen.content.clone(), seen.reason);
            if self.last.get(&seen.path) != Some(&state) {
                self.last.insert(seen.path.clone(), state);
                batch.seen.push(seen);
            }
        }
        for path in classified.deferred {
            let at = originals.get(&path).copied().unwrap_or(now);
            self.pending.insert(path, Pending { at, fresh: false });
        }
        for path in classified.found {
            if !self.queue(path, now, true) {
                batch.overflow = true;
            }
        }
        batch
    }

    /// Vergisst die festgehaltenen Endstände — nach dem Versiegeln einer
    /// Epoche. Ohne das bliebe ein erneutes Schreiben desselben Inhalts in
    /// der nächsten Epoche unbeobachtet, und ein Claim dort unbestätigt.
    pub(super) fn forget_states(&mut self) {
        self.last.clear();
    }

    /// Verwirft gecachte Lesungen (nach einem Panic: sie könnten der
    /// Auslöser sein).
    pub(super) fn forget_caches(&mut self) {
        self.tracked = None;
        self.rule_cache.clear();
    }

    fn root_unchanged(&self) -> bool {
        std::fs::symlink_metadata(&self.root)
            .is_ok_and(|meta| meta.dev() == self.root_dev && meta.ino() == self.root_ino)
    }

    /// Reiht einen Pfad ein; `false`, wenn die Sammlung voll ist.
    fn queue(&mut self, path: PathBuf, now: Instant, fresh: bool) -> bool {
        // `.git` lexikalisch gleich hier: Ein `git gc` mit zehntausend
        // Objekten soll die Sammlung nicht füllen.
        if path
            .strip_prefix(&self.root)
            .is_ok_and(|rel| rel.components().any(|c| is_git_dir_name(c.as_os_str())))
        {
            return true;
        }
        if let Some(pending) = self.pending.get_mut(&path) {
            pending.at = now;
            pending.fresh |= fresh;
            return true;
        }
        if self.pending.len() >= MAX_PENDING {
            return false;
        }
        self.pending.insert(path, Pending { at: now, fresh });
        true
    }

    /// Gibt zurück, ob Meldungen verloren gingen.
    fn drain(&mut self, now: Instant) -> bool {
        let mut lost = self.overflow.swap(false, Ordering::Relaxed);
        while let Ok(event) = self.rx.try_recv() {
            let event = match event {
                Ok(event) => event,
                // Ein Fehler des Watchers kann Meldungen gekostet haben.
                Err(_) => {
                    lost = true;
                    continue;
                }
            };
            if event.need_rescan() {
                lost = true;
            }
            let fresh = match event.kind {
                // Lesezugriffe — auch die eigenen beim Hashen — ändern nichts.
                notify::EventKind::Access(_) => continue,
                notify::EventKind::Create(_)
                | notify::EventKind::Modify(notify::event::ModifyKind::Name(_)) => true,
                _ => false,
            };
            for path in event.paths {
                if !self.queue(path, now, fresh) {
                    lost = true;
                }
            }
        }
        lost
    }

    /// Repo-relativer Pfad eines Host-Pfads, oder `None` für alles, was nicht
    /// beobachtet wird (außerhalb, `.git`, nicht UTF-8, die Wurzel selbst).
    /// Rein lexikalisch: Gelesen wird ohnehin nur unterhalb des Deskriptors.
    fn relative(&self, path: &Path) -> Option<String> {
        relative_text(path.strip_prefix(&self.root).ok()?)
    }

    /// Die Beobachtungen der Pfade, innerhalb des Byte-Budgets.
    fn classify(
        &mut self,
        paths: Vec<PathBuf>,
        fresh: &HashSet<PathBuf>,
        now: Instant,
        at: (String, u64),
    ) -> Classified {
        enum Probe {
            Deleted,
            File,
            /// Ein neu angelegtes Verzeichnis (kein eingebettetes Repo): wird
            /// durchsucht — aber erst, wenn feststeht, dass es nicht
            /// ignoriert ist.
            Dir,
            /// Linktext und Ziel im Repo (`Some`) oder außerhalb.
            Link(Vec<u8>, Option<String>),
        }
        let mut out = Classified::default();
        let mut probes: Vec<(PathBuf, String, Probe)> = Vec::new();
        let mut unique = BTreeSet::new();
        let mut plain_dirs = HashMap::new();
        for path in paths {
            // Zu tief: Jede Prüfung kostete Aufwand mit der Tiefe — eine
            // sichtbare Lücke statt einer angehaltenen Eventloop.
            if path
                .strip_prefix(&self.root)
                .is_ok_and(|rel| rel.components().count() > MAX_DEPTH)
            {
                out.truncated = true;
                continue;
            }
            let Some(rel) = self.relative(&path) else {
                continue;
            };
            if !unique.insert(rel.clone()) || !self.plain_parents(&rel, &mut plain_dirs) {
                continue;
            }
            let probe = match stat_beneath(&self.root_fd, self.root_dev, &rel) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Probe::Deleted,
                // Nicht feststellbar: eine Lücke, keine stille Auslassung.
                Err(_) => {
                    out.truncated = true;
                    continue;
                }
                Ok(stat) => match stat.st_mode & libc::S_IFMT {
                    libc::S_IFLNK => {
                        let Ok(text) = readlink_beneath(&self.root_fd, self.root_dev, &rel) else {
                            out.truncated = true;
                            continue;
                        };
                        let target = link_target(&self.root, &rel, &text)
                            .filter(|target| self.plain_parents(target, &mut plain_dirs));
                        Probe::Link(text, target)
                    }
                    libc::S_IFREG => Probe::File,
                    // Kein eingebettetes Repo (Submodul, Klon).
                    libc::S_IFDIR if fresh.contains(&path) && !self.has_git_entry(&rel) => {
                        Probe::Dir
                    }
                    // Verzeichnisse, FIFOs, Sockets, Geräte: kein Inhalt.
                    _ => continue,
                },
            };
            probes.push((path, rel, probe));
        }
        if probes.is_empty() {
            return out;
        }
        let mut asked: Vec<String> = Vec::new();
        for (_, rel, probe) in &probes {
            asked.push(rel.clone());
            if let Probe::Link(_, Some(target)) = probe {
                asked.push(target.clone());
            }
        }
        // Regeln und Löschungen sehen **denselben** Stand des Index.
        let Some((rules, unreadable, tracked)) = self.rules(&asked, now) else {
            out.ignore_failed = true;
            return out;
        };
        let mut failed = false;
        let mut ignored = |path: &str, is_dir: Option<bool>| {
            if under_any(path, &unreadable) {
                failed = true;
                return true;
            }
            match is_dir {
                Some(is_dir) => rules.is_ignored(path, Some(is_dir)),
                // Unbekannt (gelöscht): ignoriert, wenn es als Datei **oder**
                // als Verzeichnis ignoriert wäre.
                None => rules.is_ignored(path, Some(false)) || rules.is_ignored(path, Some(true)),
            }
        };
        let mut spent = 0u64;
        let mut fanout = 0usize;
        for (path, rel, probe) in probes {
            let is_dir = match probe {
                Probe::Deleted => None,
                Probe::Dir => Some(true),
                _ => Some(false),
            };
            if ignored(&rel, is_dir) {
                continue;
            }
            if let Probe::Dir = probe {
                match walk_beneath(&self.root_fd, self.root_dev, &rel) {
                    Some((names, truncated)) => {
                        out.truncated |= truncated;
                        out.found
                            .extend(names.into_iter().map(|name| path.join(name)));
                    }
                    None => out.truncated = true,
                }
                continue;
            }
            let target = match &probe {
                Probe::Link(_, Some(target)) if ignored(target, Some(false)) => continue,
                Probe::Link(_, Some(target)) => Some(target.clone()),
                _ => None,
            };
            let secret = is_secret(&rel) || target.as_deref().is_some_and(is_secret);
            let observable = tracked.contains(&rel) || self.known.contains(&rel);
            let (content, reason) = match probe {
                // Nur was getrackt ist oder schon beobachtet wurde: Ein
                // gelöschter, unbekannter Pfad kann ignoriert gewesen sein.
                Probe::Deleted if observable => (None, Some(ObservationReason::Deleted)),
                // Ein verschwundenes Verzeichnis (umbenannt, gelöscht) meldet
                // nur sich selbst: Was darunter getrackt oder schon gesehen
                // war, ist mit ihm gelöscht.
                Probe::Deleted => {
                    let prefix = format!("{rel}/");
                    let room = MAX_WALK_ENTRIES.saturating_sub(fanout);
                    let mut gone = BTreeSet::new();
                    let under = |set: &BTreeSet<String>| {
                        set.range(prefix.clone()..)
                            .take_while(|path| path.starts_with(&prefix))
                            .take(MAX_WALK_ENTRIES + 1)
                            .cloned()
                            .collect::<Vec<_>>()
                    };
                    for path in under(&tracked).into_iter().chain(under(&self.known)) {
                        // Pfade aus dem Index gehören dem Agenten: nur, was
                        // auch ein beobachtbarer Pfad wäre.
                        if relative_text(Path::new(&path)).as_deref() != Some(path.as_str()) {
                            continue;
                        }
                        gone.insert(path);
                        if gone.len() > room {
                            break;
                        }
                    }
                    // Höchstens [`MAX_WALK_ENTRIES`] Löschungen je
                    // Durchlauf, über alle Verzeichnisse: Der Index gehört
                    // dem Agenten, und jede wird ein Journal-Eintrag.
                    if gone.len() > room {
                        out.truncated = true;
                        while gone.len() > room {
                            gone.pop_last();
                        }
                    }
                    fanout += gone.len();
                    for path in gone {
                        self.known.remove(&path);
                        out.seen.push(Seen {
                            path,
                            content: None,
                            reason: Some(ObservationReason::Deleted),
                            at: at.clone(),
                        });
                    }
                    continue;
                }
                // Schon oben durchsucht.
                Probe::Dir => continue,
                Probe::Link(_, None) => (None, Some(ObservationReason::OutsideRepo)),
                _ if secret => (None, Some(ObservationReason::SecretFile)),
                // Git committet bei einem Symlink seinen Linktext — genau
                // der wird gefingerprintet, nie das Ziel.
                Probe::Link(text, Some(_)) => self.fingerprint(&text),
                Probe::File => {
                    // Das Budget zählt, was **tatsächlich** gelesen wird —
                    // nicht die Größe vom `fstatat` vorhin: Zwischen beiden
                    // kann die Datei wachsen. Die erste Datei eines
                    // Durchlaufs darf bis zur Hash-Grenze gehen.
                    let budget = if spent == 0 {
                        MAX_OBSERVED_BYTES
                    } else {
                        BYTES_PER_STEP.saturating_sub(spent)
                    };
                    match read_beneath(&self.root_fd, self.root_dev, &rel, budget) {
                        Content::Bytes(bytes) => {
                            spent = spent.saturating_add(bytes.len() as u64);
                            self.fingerprint(&bytes)
                        }
                        Content::OverBudget => {
                            out.deferred.push(path);
                            continue;
                        }
                        Content::TooLarge => (None, Some(ObservationReason::TooLarge)),
                        Content::Linked => (None, Some(ObservationReason::OutsideRepo)),
                        Content::Gone if observable => (None, Some(ObservationReason::Deleted)),
                        Content::Gone | Content::Skip => continue,
                    }
                }
            };
            if reason == Some(ObservationReason::Deleted) {
                self.known.remove(&rel);
            } else {
                if self.known.len() >= MAX_REMEMBERED {
                    self.known.clear();
                }
                self.known.insert(rel.clone());
            }
            out.seen.push(Seen {
                path: rel,
                content,
                reason,
                at: at.clone(),
            });
        }
        out.ignore_failed |= failed;
        out
    }

    /// Hash nur über Inhalt, den die Redaction-Pipeline unverändert ließe —
    /// dieselbe Regel wie `Effect.written` (`scanned_hash` in Capture): Ein
    /// ungesalzener Hash über eine kurze Datei mit einem Passwort wäre ein
    /// Wörterbuch-Orakel. Nicht-UTF-8 lässt sich nicht scannen.
    fn fingerprint(&self, bytes: &[u8]) -> (Option<ContentHash>, Option<ObservationReason>) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return (None, Some(ObservationReason::Unscannable));
        };
        let scan = self.pipeline.redact(text);
        if scan.counts != minds_core::RedactionCounts::default() || scan.invalid_findings > 0 {
            return (None, Some(ObservationReason::RedactedContent));
        }
        (Some(self.hasher.hash(bytes)), None)
    }

    /// Die Ignore-Regeln für die Verzeichnisse der `paths` und die
    /// Verzeichnisse, deren `.gitignore` sich nicht sicher lesen ließ (oder
    /// das Regel-Budget überschritt). `None`, wenn `info/exclude` unlesbar
    /// ist — dann gilt das für alles.
    fn rules(
        &mut self,
        paths: &[String],
        now: Instant,
    ) -> Option<(
        minds_git::ignore::IgnoreRules,
        Vec<String>,
        Arc<BTreeSet<String>>,
    )> {
        let mut budget = RULE_BYTES_PER_STEP;
        let exclude = self.rule_file(".git/info/exclude", &mut budget).ok()?;
        let tracked = self.tracked_paths(now);
        let mut rules = minds_git::ignore::IgnoreRules::new(
            exclude.as_deref().map(Vec::as_slice),
            tracked.clone(),
        );
        let mut dirs: BTreeSet<(usize, String)> = BTreeSet::new();
        for path in paths {
            let parts: Vec<&str> = path.split('/').collect();
            for depth in 0..parts.len() {
                dirs.insert((depth, parts[..depth].join("/")));
            }
        }
        let mut unreadable = Vec::new();
        // Flache Verzeichnisse zuerst: tiefere `.gitignore` gehen vor.
        for (_, dir) in dirs {
            if under_any(&dir, &unreadable) {
                continue;
            }
            let file = if dir.is_empty() {
                ".gitignore".to_owned()
            } else {
                format!("{dir}/.gitignore")
            };
            match self.rule_file(&file, &mut budget) {
                Ok(Some(bytes)) => rules.add_gitignore(&dir, &bytes),
                Ok(None) => {}
                Err(()) => unreadable.push(dir),
            }
        }
        Some((rules, unreadable, tracked))
    }

    /// Eine Regeldatei, gecacht nach Identität und Stand. `Ok(None)`, wenn
    /// sie fehlt; `Err`, wenn sie da, aber nicht sicher lesbar ist oder das
    /// Budget sprengt.
    fn rule_file(&mut self, rel: &str, budget: &mut u64) -> Result<Option<Arc<Vec<u8>>>, ()> {
        let file = match open_beneath(&self.root_fd, self.root_dev, rel) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(()),
        };
        let meta = file.metadata().map_err(|_| ())?;
        if !meta.is_file() || meta.len() > MAX_RULE_BYTES || meta.len() > *budget {
            return Err(());
        }
        let stamp = stamp(&meta);
        if let Some((cached, bytes)) = self.rule_cache.get(rel) {
            if *cached == stamp {
                *budget -= bytes.len().min(*budget as usize) as u64;
                return Ok(Some(bytes.clone()));
            }
        }
        // Gelesen wird höchstens das Restbudget; berechnet wird, was
        // tatsächlich kam — die Datei kann seit dem `fstat` gewachsen sein.
        let limit = (*budget).min(MAX_RULE_BYTES);
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.len() as u64 > limit {
            return Err(());
        }
        *budget -= bytes.len() as u64;
        let bytes = Arc::new(bytes);
        let cached: usize = self.rule_cache.values().map(|(_, b)| b.len()).sum();
        if self.rule_cache.len() > 4096 || cached + bytes.len() > MAX_RULE_CACHE_BYTES {
            self.rule_cache.clear();
        }
        self.rule_cache
            .insert(rel.to_owned(), (stamp, bytes.clone()));
        Ok(Some(bytes))
    }

    /// Die getrackten Pfade laut Index. Neu gelesen nur bei geändertem Index
    /// und höchstens einmal je [`INDEX_REREAD`] (dazwischen gilt der letzte
    /// Stand). Fehlt er, ist er zu groß oder unlesbar, gilt **keiner** als
    /// getrackt: Dann entscheidet allein das Muster, und im Zweifel wird eher
    /// ignoriert als gefingerprintet.
    fn tracked_paths(&mut self, now: Instant) -> Arc<BTreeSet<String>> {
        let Ok(file) = open_beneath(&self.root_fd, self.root_dev, ".git/index") else {
            return Arc::default();
        };
        let Ok(meta) = file.metadata() else {
            return Arc::default();
        };
        if !meta.is_file() || meta.len() > MAX_INDEX_BYTES {
            return Arc::default();
        }
        let stamp = stamp(&meta);
        if let Some((cached, read_at, paths)) = &self.tracked {
            // Unverändert — oder geändert, aber zu kurz nach der letzten
            // Lesung: dann gilt der letzte Stand (höchstens eine Sekunde alt).
            if *cached == stamp || now.duration_since(*read_at) < INDEX_REREAD {
                return paths.clone();
            }
        }
        let mut bytes = Vec::new();
        if file.take(MAX_INDEX_BYTES).read_to_end(&mut bytes).is_err() {
            return Arc::default();
        }
        let paths = Arc::new(minds_git::ignore::index_paths(&bytes).unwrap_or_default());
        self.tracked = Some((stamp, now, paths.clone()));
        paths
    }

    /// Ob jedes Verzeichnis zwischen Wurzel und `rel` ein echtes Verzeichnis
    /// dieses Repos ist: kein Symlink (der Pfad läge dahinter), kein anderes
    /// Dateisystem und kein eingebettetes Repository (Submodul,
    /// verschachtelter Klon — dessen Inhalt gehört nicht zu diesem Repo). Ein
    /// fehlendes Verzeichnis (gelöscht) zählt als schlicht.
    ///
    /// Ein Abstieg je Pfad, ein Deskriptor je Ebene: linear in der Tiefe.
    fn plain_parents(&self, rel: &str, cache: &mut HashMap<String, bool>) -> bool {
        let parts: Vec<&str> = rel.split('/').collect();
        let dirs = &parts[..parts.len().saturating_sub(1)];
        let parent = dirs.join("/");
        if let Some(plain) = cache.get(&parent) {
            return *plain;
        }
        let plain = self.descend_plain(dirs);
        cache.insert(parent, plain);
        plain
    }

    fn descend_plain(&self, dirs: &[&str]) -> bool {
        let Ok(mut dir) = self.root_fd.try_clone() else {
            return false;
        };
        for part in dirs {
            let Ok(name) = CString::new(*part) else {
                return false;
            };
            // SAFETY: gültiger Verzeichnis-Deskriptor, nul-terminierter Name
            // ohne `/`; der neue Deskriptor gehört ab hier `File`.
            let fd = unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                // Fehlt der Rest (gelöscht), ist der Weg schlicht; alles
                // andere (ein Symlink, keine Berechtigung) nicht.
                return std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound;
            }
            // SAFETY: frisch geöffnet, sonst ohne Besitzer.
            dir = unsafe { File::from_raw_fd(fd) };
            if dir
                .metadata()
                .map_or(true, |meta| meta.dev() != self.root_dev)
            {
                return false;
            }
            // SAFETY: wie oben; `stat` wird vollständig gefüllt.
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            let git = c".git";
            // SAFETY: gültiger Deskriptor, Name, beschreibbare Struktur.
            if unsafe {
                libc::fstatat(
                    dir.as_raw_fd(),
                    git.as_ptr(),
                    &mut stat,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } == 0
            {
                return false;
            }
        }
        true
    }

    /// Ob im repo-relativen Verzeichnis `dir` ein `.git` liegt (gleich in
    /// welcher Schreibweise — APFS unterscheidet sie nicht).
    fn has_git_entry(&self, dir: &str) -> bool {
        stat_beneath(&self.root_fd, self.root_dev, &format!("{dir}/.git")).is_ok()
    }
}

/// Stand einer geöffneten Datei für Caches.
fn stamp(meta: &std::fs::Metadata) -> Stamp {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
    )
}

/// `.git` in jeder Schreibweise.
fn is_git_dir_name(name: &std::ffi::OsStr) -> bool {
    name.as_bytes().eq_ignore_ascii_case(b".git")
}

/// Secret-Wall auf einem repo-relativen Pfad. Mit führendem `/`, damit auch
/// die Verzeichnisregeln (`/.ssh/`, `/.gnupg/`) greifen.
fn is_secret(rel: &str) -> bool {
    minds_redact::is_secret_file(&format!("/{rel}"))
}

/// `/`-getrennter Text eines relativen Pfads; `None` für `.git`-Inhalte,
/// Nicht-UTF-8, leere oder nicht schlichte Pfade.
fn relative_text(rel: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in rel.components() {
        let Component::Normal(part) = component else {
            return None;
        };
        if is_git_dir_name(part) {
            return None;
        }
        parts.push(part.to_str()?);
    }
    let text = parts.join("/");
    minds_core::observation::plain_relative(&text).then_some(text)
}

/// Das repo-relative Ziel eines Symlinks an `rel` mit Linktext `text` —
/// rein lexikalisch, ohne dem Link zu folgen. Ein absoluter Linktext zählt
/// nur unterhalb der (kanonischen) Wurzel. `None`, wenn das Ziel aus dem
/// Repo zeigt (anderswohin absolut, über die Wurzel hinaus, in `.git`).
fn link_target(root: &Path, rel: &str, text: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(text).ok()?;
    // Ein Linktext mit tausend Komponenten kostete jede weitere Prüfung
    // Aufwand mit der Tiefe — er gilt als außerhalb.
    if text.split('/').count() > MAX_DEPTH {
        return None;
    }
    let (mut parts, text): (Vec<&str>, &str) = if text.starts_with('/') {
        let inner = Path::new(text).strip_prefix(root).ok()?.to_str()?;
        (Vec::new(), inner)
    } else {
        let mut parts: Vec<&str> = rel.split('/').collect();
        parts.pop();
        (parts, text)
    };
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    if parts.len() > MAX_DEPTH {
        return None;
    }
    relative_text(Path::new(&parts.join("/")))
}

enum Content {
    Bytes(Vec<u8>),
    TooLarge,
    /// Ein zweiter harter Link: Die Datei kann ebenso gut eine Host-Datei
    /// außerhalb des Repos sein (`ln ~/.pgpass notes.txt`) — kein Hash, der
    /// ein Orakel über sie wäre.
    Linked,
    Gone,
    /// Mehr Bytes, als das Budget dieses Durchlaufs erlaubt — im nächsten.
    OverBudget,
    /// Kein gewöhnlicher Inhalt erreichbar (Symlink im Weg, kein File,
    /// anderes Dateisystem).
    Skip,
}

/// Liest `rel` unterhalb der Wurzel, ohne einem Symlink zu folgen — weder in
/// der Datei noch in einem Verzeichnis davor — und ohne das Dateisystem zu
/// verlassen.
fn read_beneath(root: &File, dev: u64, rel: &str, budget: u64) -> Content {
    let file = match open_beneath(root, dev, rel) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Content::Gone,
        Err(_) => return Content::Skip,
    };
    let Ok(meta) = file.metadata() else {
        return Content::Skip;
    };
    if !meta.is_file() || meta.dev() != dev {
        return Content::Skip;
    }
    if meta.nlink() > 1 {
        return Content::Linked;
    }
    if meta.len() > MAX_OBSERVED_BYTES {
        return Content::TooLarge;
    }
    if meta.len() > budget {
        return Content::OverBudget;
    }
    // Höchstens ein Byte über Grenze bzw. Budget: daran erkennt das Lesen,
    // dass die Datei seit dem `fstat` gewachsen ist.
    let limit = budget.min(MAX_OBSERVED_BYTES);
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    if file.take(limit + 1).read_to_end(&mut bytes).is_err() {
        return Content::Skip;
    }
    if bytes.len() as u64 > MAX_OBSERVED_BYTES {
        return Content::TooLarge;
    }
    if bytes.len() as u64 > budget {
        return Content::OverBudget;
    }
    Content::Bytes(bytes)
}

/// Öffnet ein Verzeichnis per Pfad, ohne einem Symlink an der letzten Stelle
/// zu folgen — nur für die Wurzel beim Start.
fn open_dir_nofollow(path: &Path) -> std::io::Result<File> {
    let name = CString::new(path.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: nul-terminierter Pfad; der Deskriptor gehört danach `File`.
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: frisch geöffnet, sonst ohne Besitzer.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Öffnet das Verzeichnis, in dem `rel` liegt, Komponente für Komponente
/// ohne Symlink und ohne Dateisystemwechsel; gibt es samt letztem Namen
/// zurück. Der Wurzel-Deskriptor wird dupliziert, nie verbraucht.
fn parent_beneath(root: &File, dev: u64, rel: &str) -> std::io::Result<(File, CString)> {
    let parts: Vec<&str> = rel.split('/').collect();
    let (last, dirs) = parts
        .split_last()
        .ok_or_else(|| std::io::Error::other("empty path"))?;
    let mut dir = root.try_clone()?;
    for part in dirs {
        let name = CString::new(*part).map_err(std::io::Error::other)?;
        // SAFETY: gültiger Verzeichnis-Deskriptor, nul-terminierter Name
        // ohne `/`; der neue Deskriptor gehört ab hier `File`.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: frisch geöffnet, sonst ohne Besitzer.
        dir = unsafe { File::from_raw_fd(fd) };
        if dir.metadata()?.dev() != dev {
            return Err(std::io::Error::other("crosses a file system boundary"));
        }
    }
    let name = CString::new(*last).map_err(std::io::Error::other)?;
    Ok((dir, name))
}

fn open_beneath(root: &File, dev: u64, rel: &str) -> std::io::Result<File> {
    let (dir, name) = parent_beneath(root, dev, rel)?;
    // SAFETY: wie oben; `O_NONBLOCK`: Ein FIFO darf das Öffnen nicht
    // anhalten.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: frisch geöffnet, sonst ohne Besitzer.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// `lstat` unterhalb der Wurzel.
fn stat_beneath(root: &File, dev: u64, rel: &str) -> std::io::Result<libc::stat> {
    let (dir, name) = parent_beneath(root, dev, rel)?;
    // SAFETY: `stat` ist eine C-Struktur, die `fstatat` vollständig füllt.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: gültiger Deskriptor, Name, beschreibbare Struktur.
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(stat)
}

/// Der Linktext eines Symlinks unterhalb der Wurzel.
fn readlink_beneath(root: &File, dev: u64, rel: &str) -> std::io::Result<Vec<u8>> {
    let (dir, name) = parent_beneath(root, dev, rel)?;
    let mut buf = vec![0u8; 4096];
    // SAFETY: gültiger Deskriptor, Name, Puffer mit angegebener Länge.
    let len = unsafe {
        libc::readlinkat(
            dir.as_raw_fd(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if len < 0 {
        return Err(std::io::Error::last_os_error());
    }
    buf.truncate(len as usize);
    Ok(buf)
}

/// Die Namen eines neu angelegten Verzeichnisses unterhalb der Wurzel —
/// höchstens [`MAX_WALK_ENTRIES`]; das zweite Feld sagt, ob es mehr gab.
/// Nur Namen: Jeder Eintrag durchläuft danach dieselbe Klassifikation.
fn walk_beneath(root: &File, dev: u64, rel: &str) -> Option<(Vec<std::ffi::OsString>, bool)> {
    let (parent, name) = parent_beneath(root, dev, rel).ok()?;
    // SAFETY: wie bei `open_beneath`.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fdopendir` übernimmt den frisch geöffneten Deskriptor;
    // `closedir` gibt beide frei.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        // SAFETY: der Deskriptor gehört noch uns.
        unsafe { libc::close(fd) };
        return None;
    }
    let mut names = Vec::new();
    let mut truncated = false;
    loop {
        clear_errno();
        // SAFETY: gültiger, offener Verzeichnis-Strom.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            // Ende — oder ein Lesefehler: dann ist das Verzeichnis nicht
            // vollständig durchsucht.
            truncated |= std::io::Error::last_os_error().raw_os_error().unwrap_or(0) != 0;
            break;
        }
        // SAFETY: `d_name` ist nul-terminiert und lebt bis zum nächsten
        // `readdir`; es wird sofort kopiert.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if names.len() >= MAX_WALK_ENTRIES {
            truncated = true;
            break;
        }
        names.push(std::ffi::OsStr::from_bytes(bytes).to_owned());
    }
    // SAFETY: einmal geschlossen, danach nicht mehr benutzt.
    unsafe { libc::closedir(stream) };
    Some((names, truncated))
}

/// Setzt `errno` zurück — `readdir` unterscheidet Ende und Fehler nur so.
fn clear_errno() {
    // SAFETY: die thread-lokale `errno` des Prozesses, beschreibbar.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    unsafe {
        *libc::__error() = 0;
    }
    // SAFETY: wie oben.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        *libc::__errno_location() = 0;
    }
}

/// Liegt `path` in (oder ist) einem der Verzeichnisse `dirs`?
fn under_any(path: &str, dirs: &[String]) -> bool {
    dirs.iter()
        .any(|dir| dir.is_empty() || path == dir || path.starts_with(&format!("{dir}/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_refuse_git_internals_and_odd_components() {
        assert_eq!(
            relative_text(Path::new("src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
        for refused in [".git/config", "sub/.git/HEAD", ".GIT/config", "", "../x"] {
            assert_eq!(relative_text(Path::new(refused)), None, "{refused}");
        }
    }

    #[test]
    fn link_targets_resolve_lexically_and_never_leave_the_repo() {
        let root = Path::new("/repo");
        assert_eq!(
            link_target(root, "src/a.rs", b"../shared/a.rs").as_deref(),
            Some("shared/a.rs")
        );
        assert_eq!(link_target(root, "a", b"./b").as_deref(), Some("b"));
        assert_eq!(link_target(root, "a", b"/repo/c/d").as_deref(), Some("c/d"));
        let deep = "a/".repeat(MAX_DEPTH) + "x";
        assert_eq!(link_target(root, "l", deep.as_bytes()), None);
        for outside in [
            &b"/etc/passwd"[..],
            b"../x",
            b"../../x",
            b".git/config",
            b"sub/.GIT/x",
            b"/repo/../etc/passwd",
            b"/repository/x",
        ] {
            assert_eq!(link_target(root, "a", outside), None, "{outside:?}");
        }
    }

    #[test]
    fn the_secret_wall_sees_directory_rules_on_relative_paths() {
        for secret in [
            ".env",
            "config/.env.local",
            ".ssh/id_ed25519",
            "deploy/key.pem",
        ] {
            assert!(is_secret(secret), "{secret}");
        }
        for plain in ["src/env.rs", ".env.example", "docs/ssh.md"] {
            assert!(!is_secret(plain), "{plain}");
        }
    }

    #[test]
    fn reading_beneath_never_follows_a_symlink_or_leaves_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("real/a.txt"), "in").unwrap();
        std::fs::write(outside.join("a.txt"), "host secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let root = root.canonicalize().unwrap();
        let fd = open_dir_nofollow(&root).unwrap();
        let dev = fd.metadata().unwrap().dev();
        assert!(
            matches!(read_beneath(&fd, dev, "real/a.txt", MAX_OBSERVED_BYTES), Content::Bytes(b) if b == b"in")
        );
        assert!(matches!(
            read_beneath(&fd, dev, "link/a.txt", MAX_OBSERVED_BYTES),
            Content::Skip
        ));
        assert!(matches!(
            read_beneath(&fd, dev, "real/none", MAX_OBSERVED_BYTES),
            Content::Gone
        ));
        // Über dem Restbudget — auch wenn ein früheres `fstatat` die Datei
        // kleiner sah — wird nicht gelesen, sondern zurückgestellt.
        assert!(matches!(
            read_beneath(&fd, dev, "real/a.txt", 1),
            Content::OverBudget
        ));
        // Ein FIFO hält das Lesen nicht an.
        let fifo = CString::new(root.join("fifo").as_os_str().as_bytes()).unwrap();
        // SAFETY: nul-terminierter Pfad.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            read_beneath(&fd, dev, "fifo", MAX_OBSERVED_BYTES),
            Content::Skip
        ));
        // Die Wurzel wird gegen einen Symlink nach außen getauscht: Der
        // Deskriptor liest weiter das Original, nie das Ziel.
        std::fs::rename(&root, dir.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        assert!(
            matches!(read_beneath(&fd, dev, "real/a.txt", MAX_OBSERVED_BYTES), Content::Bytes(b) if b == b"in")
        );
        assert!(matches!(
            read_beneath(&fd, dev, "a.txt", MAX_OBSERVED_BYTES),
            Content::Gone
        ));
        // Ein anderes Gerät (hier: vorgetäuscht) wird nie gelesen.
        assert!(matches!(
            read_beneath(&fd, dev ^ 1, "real/a.txt", MAX_OBSERVED_BYTES),
            Content::Skip
        ));
    }
}
