//! Das zweite Auge des Witness (EA-08): Er beobachtet den Worktree selbst und
//! hält jede Inhaltsänderung als `fs.observed` in seinem eigenen Stream fest.
//!
//! # Was beobachtet wird — und was nie (W7)
//!
//! - Pfade werden **repo-relativ** gespeichert, nie als Host-Pfad.
//! - `.git/` (auch das Git-Verzeichnis einer `.git`-Datei im Worktree) und
//!   alles, was `.gitignore` / `.git/info/exclude` ausschließt, erscheint gar
//!   nicht: kein Pfad, kein Hash.
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
//! statt einer stillen, mit der Zahl der Meldungen, die der volle Kanal
//! abwies (`dropped`); der Sealer trägt sie ins Observation-Objekt, und
//! `minds verify` nennt sie an jedem Claim, dessen Beobachtung hineinfiel. Neu angelegte Verzeichnisse werden begrenzt
//! durchsucht: inotify meldet Dateien nicht, die vor ihrer Watch entstanden.
//!
//! # Watches: der Worktree, nicht die Git-Metadaten
//!
//! Auf Linux bekommt jedes Verzeichnis des Worktrees eine eigene, flache
//! Watch ([`watch_tree`]) — außer den Git-Metadaten (jede Komponente
//! `.git`). Private Verzeichnisse darin (`.git/minds/journal` des
//! Entwicklers, Modus `0700`, für den geschützten Witness unlesbar) legen
//! den Beobachter so nicht lahm. Eine `.git`-Datei (`gitdir: …`) nimmt
//! nichts aus: Ihr Inhalt gehört dem Agenten.
//!
//! Jedes andere Verzeichnis des Worktrees, das sich nicht beobachten, lesen
//! oder durchsuchen lässt, ist ein Fehler, nie eine stille Auslassung: Beim
//! Start scheitert der Beobachter („no file observer"), danach wird er
//! `blind`. Ebenso eine Datei, die sich nicht lesen lässt (solange, bis sie
//! gelesen oder gelöscht ist), und Ignore-Regeln, die sich nicht auswerten
//! lassen. In jedem dieser Fälle meldet `minds doctor` einen Fehler, und
//! jede Beobachtungs-Epoche trägt ein `fs.gap` (`unavailable` bzw.
//! `ignore_rules`), solange der Zustand anhält. Gehen Meldungen verloren,
//! geht der Beobachter den Worktree erneut durch: Ein neues Verzeichnis,
//! dessen Meldung fehlte, bekommt so seine Watch.
//!
//! Eine Ausnahme (Entscheidung 28.10.2026, [`Exemption`]): Ein Verzeichnis,
//! das der Witness nicht lesen oder durchsuchen darf, wird übersprungen,
//! wenn Git es nach den wirksamen Ignore-Regeln ausschließt und weder Index
//! noch HEAD darunter etwas versionieren — Docker-Volumes, als root gebaute
//! Artefakte, Caches. Nur fehlende Berechtigung zählt; bei jedem Zweifel
//! gilt die Ausnahme nicht. Übersprungene Verzeichnisse werden alle
//! [`RECHECK_INTERVAL`] neu bewertet (gilt die Ausnahme nicht mehr: blind)
//! und von `minds doctor` getrennt genannt; was darin liegt, zählt nie als
//! beobachtet.
//!
//! Auf macOS beobachtet FSEvents die Wurzel als Ganzes; die Verzeichnisse
//! werden dort beim Start und wenn sie entstehen nur geprüft (lesen und
//! durchsuchen) — mit derselben Grenze und denselben Fehlern. Ungeprüft auf
//! einem Mac: ob FSEvents Meldungen aus Verzeichnissen liefert, die der
//! Witness nicht lesen darf.
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
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Abstand, in dem übersprungene Verzeichnisse neu bewertet werden.
const RECHECK_INTERVAL: Duration = Duration::from_secs(2);

/// So lange wartet das Versiegeln höchstens auf die Neubewertung.
const RECHECK_DEADLINE: Duration = Duration::from_secs(2);

/// Mindestabstand zwischen zwei Durchgängen über den Worktree nach
/// verlorenen Meldungen.
const RESCAN_INTERVAL: Duration = Duration::from_secs(1);

/// Höchstens so viele nicht lesbare Pfade werden einzeln gemerkt.
const MAX_DENIED: usize = 1024;

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
    /// Wie viele Meldungen der volle Kanal abwies — der zählbare Teil des
    /// Verlusts (`0`: keine, oder nicht zählbar).
    pub(super) dropped: u64,
    /// Ignore-Regeln ließen sich nicht sicher lesen; betroffene Pfade wurden
    /// verworfen.
    pub(super) ignore_failed: bool,
    /// Ob überhaupt Pfade klassifiziert wurden — nur dann sagt
    /// `ignore_failed == false` etwas über die Ignore-Auswertung.
    pub(super) classified: bool,
    /// Der Beobachter wurde in diesem Durchlauf unvollständig (ein
    /// Verzeichnis des Worktrees ist nicht beobachtbar): der Grund fürs Log,
    /// genau einmal.
    pub(super) blind: Option<String>,
    /// Pfade des Worktrees, die in diesem Durchlauf nicht lesbar waren
    /// (keine Berechtigung) — fürs Log.
    pub(super) denied: Vec<String>,
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
    /// Meldungen, die der volle Kanal abwies, seit dem letzten Durchlauf.
    dropped: Arc<AtomicUsize>,
    /// Hält den Watcher und gibt neuen Verzeichnissen ihre Watches.
    walker: Option<Walker>,
    /// Übersprungene Verzeichnisse (repo-relativ): nicht lesbar, von Git
    /// ignoriert, nichts darunter versioniert ([`Exemption`]). Nie
    /// beobachtet; `minds doctor` nennt sie.
    skipped: Arc<std::sync::Mutex<BTreeSet<String>>>,
    last_recheck: Option<Instant>,
    /// Ein Verzeichnis des Worktrees ließ sich nicht beobachten: Der
    /// Beobachter ist unvollständig — sichtbar als Fehler, nie still.
    blind: Option<String>,
    /// Ob `blind` schon in einem [`Batch`] gemeldet wurde.
    blind_reported: bool,
    /// Pfade des Worktrees, die zuletzt nicht lesbar waren (keine
    /// Berechtigung) — bis sie gelesen oder gelöscht sind. Solange einer
    /// darin steht, ist der Beobachter unvollständig.
    denied: BTreeSet<String>,
    /// Mehr nicht lesbare Pfade, als [`MAX_DENIED`] fasst: unvollständig
    /// bis zum Neustart.
    denied_overflow: bool,
    /// Meldungen gingen verloren: Neue Verzeichnisse könnten ohne Watch
    /// sein — der Worktree wird erneut durchgegangen.
    rescan_due: bool,
    last_rescan: Option<Instant>,
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
    /// Pfade, die erstmals nicht lesbar waren (keine Berechtigung).
    denied: Vec<String>,
}

impl Observer {
    /// Beobachtet den Worktree unter `root` ohne die Git-Metadaten
    /// ([`watch_tree`]). `root` muss kanonisch sein. Lässt sich ein
    /// Verzeichnis des Worktrees nicht beobachten, scheitert der Start.
    pub(super) fn watch(root: &Path, hasher: Box<dyn ContentHasher>) -> Fallible<Self> {
        use notify::Watcher;
        let mut observer = Self::detached(root, hasher)?;
        let tx = observer.tx.clone();
        let dropped = observer.dropped.clone();
        let handler = move |event| match tx.try_send(event) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
        };
        // Nie einem Symlink in ein Verzeichnis folgen: Ein `ln -s ~ home` im
        // Worktree ließe den Witness sonst das Home des Hosts beobachten
        // (und die inotify-Watches aufbrauchen).
        let config = notify::Config::default().with_follow_symlinks(false);
        let mut watcher = notify::RecommendedWatcher::new(handler, config)?;
        let mut exemption = Exemption::new(root, &observer.root_fd, observer.root_dev)?;
        watch_root(&mut watcher, root, &mut exemption, &observer.skipped)?;
        observer.walker = Some(Walker::spawn(
            watcher,
            root,
            exemption,
            observer.skipped.clone(),
        )?);
        Ok(observer)
    }

    /// Warum der Beobachter unvollständig ist: Ein Verzeichnis des
    /// Worktrees ließ sich nach dem Start nicht beobachten. Bleibt bis zum
    /// Neustart — `minds doctor` meldet es als Fehler.
    pub(super) fn blind(&self) -> Option<&str> {
        self.blind.as_deref()
    }

    /// Die übersprungenen Verzeichnisse (repo-relativ, sortiert).
    pub(super) fn skipped(&self) -> Vec<String> {
        self.skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Übersprungene Verzeichnisse werden regelmäßig neu bewertet: Ändern
    /// sich `.gitignore`, Index oder HEAD, oder wird ein Verzeichnis
    /// lesbar, gilt die Ausnahme nicht mehr — dann wird es beobachtet, oder
    /// der Beobachter wird blind.
    fn recheck(&mut self, now: Instant) {
        if self
            .last_recheck
            .is_some_and(|at| now.duration_since(at) < RECHECK_INTERVAL)
        {
            return;
        }
        let Some(walker) = self.walker.as_ref() else {
            return;
        };
        if self
            .skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            return;
        }
        self.last_recheck = Some(now);
        if let Request::Stopped = walker.request(Job::Recheck) {
            self.walker_stopped();
        }
    }

    /// Vor dem Versiegeln: die übersprungenen Verzeichnisse jetzt neu
    /// bewerten und darauf warten — ein `git add -f` mit Commit soll nicht in
    /// einer Epoche landen, die noch als lückenlos gilt. `false`, wenn das
    /// nicht rechtzeitig gelang (dann trägt die Epoche eine Lücke).
    pub(super) fn recheck_now(&mut self, now: Instant) -> bool {
        let Some(walker) = self.walker.as_ref() else {
            return true;
        };
        if self
            .skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            return true;
        }
        match walker.request(Job::Recheck) {
            Request::Taken => {}
            Request::Full => return false,
            Request::Stopped => {
                self.walker_stopped();
                return true;
            }
        }
        let deadline = Instant::now() + RECHECK_DEADLINE;
        while walker.busy.load(Ordering::SeqCst) > 0 {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        self.last_recheck = Some(now);
        self.poll_walker(now)
    }

    /// Ob Pfade des Worktrees nicht lesbar sind (keine Berechtigung) — dann
    /// ist der Beobachter unvollständig, bis sie gelesen oder gelöscht sind.
    pub(super) fn unreadable(&self) -> bool {
        self.denied_overflow || !self.denied.is_empty()
    }

    /// Ob `path` Git-Metadaten sind oder darin liegt: eine Komponente
    /// `.git` (lexikalisch, wie Git selbst, in jeder Schreibweise). Nur diese
    /// Pfade beobachtet der Witness nicht.
    fn git_metadata(&self, path: &Path) -> bool {
        path.strip_prefix(&self.root)
            .is_ok_and(|rel| rel.components().any(|c| is_git_dir_name(c.as_os_str())))
    }

    /// Ein neu angelegtes (oder hierher umbenanntes) Verzeichnis des
    /// Worktrees bekommt seine Watches ([`watch_tree`]) — auf jeder Tiefe,
    /// denn jedes Verzeichnis hat seine eigene, flache Watch. Der Gang
    /// läuft im [`Walker`]; was sich darin nicht beobachten lässt, macht den
    /// Beobachter `blind`, die Lücke dafür schreibt der Writer
    /// (`unavailable`). Nimmt der Walker nichts mehr an, geht der Beobachter
    /// später den ganzen Worktree durch ([`Self::rescan`]).
    ///
    /// Auf macOS beobachtet FSEvents die Wurzel als Ganzes; dort wird das
    /// Verzeichnis nur geprüft (lesen und durchsuchen).
    /// `false`: Der Walker nahm nichts mehr an — eine Lücke.
    fn watch_new_dir(&mut self, path: &Path) -> bool {
        if !path.starts_with(&self.root) || path == self.root || self.git_metadata(path) {
            return true;
        }
        let Some(walker) = self.walker.as_ref() else {
            return true;
        };
        // Nur Verzeichnisse; was sich nicht prüfen lässt, prüft der Walker
        // (und meldet es).
        match std::fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_dir() => return true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return true,
            _ => {}
        }
        match walker.request(Job::New(path.to_path_buf())) {
            Request::Taken => true,
            Request::Full => {
                self.rescan_due = true;
                false
            }
            Request::Stopped => {
                self.walker_stopped();
                true
            }
        }
    }

    /// Ob der Beobachter nach verlorenen Meldungen noch aufholt (ein Gang
    /// über den Worktree steht aus): Bis dahin kann ein neues Verzeichnis
    /// ohne Watch sein — jede Epoche bis dahin trägt die Lücke.
    pub(super) fn catching_up(&self) -> bool {
        self.rescan_due || self.walker.as_ref().is_some_and(Walker::rescanning)
    }

    /// Gingen Meldungen verloren, kann auch die über ein neues Verzeichnis
    /// darunter sein: Dann geht der Walker den Worktree erneut durch
    /// (vorhandene Watches bleiben), höchstens einmal je [`RESCAN_INTERVAL`].
    fn rescan(&mut self, now: Instant) {
        if !self.rescan_due
            || self
                .last_rescan
                .is_some_and(|at| now.duration_since(at) < RESCAN_INTERVAL)
        {
            return;
        }
        let Some(walker) = self.walker.as_ref() else {
            self.rescan_due = false;
            return;
        };
        self.last_rescan = Some(now);
        match walker.request(Job::Rescan) {
            Request::Taken => self.rescan_due = false,
            Request::Full => {}
            Request::Stopped => self.walker_stopped(),
        }
    }

    /// Was der Walker nicht beobachten konnte, macht den Beobachter blind;
    /// neue Verzeichnisse mit Watch werden erneut durchsucht. `false`, wenn
    /// die Sammlung dafür voll war.
    fn poll_walker(&mut self, now: Instant) -> bool {
        let Some(walker) = self.walker.as_ref() else {
            return true;
        };
        let found = walker.take_unwatchable();
        let watched = walker.take_watched();
        if let Some(unwatchable) = found {
            self.go_blind(unwatchable);
        }
        let mut kept = true;
        for dir in watched {
            kept &= self.queue(dir, now, true);
        }
        kept
    }

    fn walker_stopped(&mut self) {
        self.go_blind(Unwatchable {
            dir: self.root.clone(),
            err: "the directory walker stopped".into(),
        });
    }

    /// Wartet, bis der Walker alle Aufträge erledigt hat.
    #[cfg(test)]
    pub(super) fn walked(&self) {
        if let Some(walker) = &self.walker {
            let deadline = Instant::now() + Duration::from_secs(10);
            while walker.busy.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn go_blind(&mut self, unwatchable: Unwatchable) {
        if self.blind.is_some() {
            return;
        }
        let rel = unwatchable
            .dir
            .strip_prefix(&self.root)
            .ok()
            .and_then(relative_text)
            .unwrap_or_else(|| ".".into());
        self.blind = Some(format!(
            "file observer incomplete: directory {rel:?} of the worktree is not watchable \
             ({}); restart the witness after fixing it",
            unwatchable.err
        ));
    }

    /// Die nicht lesbaren Pfade unter `path` (Host-Pfad).
    fn denied_under(&self, path: &Path) -> Vec<String> {
        if self.denied.is_empty() {
            return Vec::new();
        }
        let prefix = match self.relative(path) {
            Some(rel) => format!("{rel}/"),
            None if path == self.root => String::new(),
            None => return Vec::new(),
        };
        self.denied
            .range(prefix.clone()..)
            .take_while(|denied| denied.starts_with(&prefix))
            .take(MAX_DENIED)
            .cloned()
            .collect()
    }

    /// Ein nicht lesbarer Pfad des Worktrees.
    fn deny(&mut self, rel: String, out: &mut Classified) {
        if self.denied.contains(&rel) {
            return;
        }
        if self.denied.len() >= MAX_DENIED {
            self.denied_overflow = true;
        } else {
            self.denied.insert(rel.clone());
        }
        out.denied.push(rel);
    }

    /// Simuliert einen vollen Kanal: alle wartenden Meldungen gehen
    /// verloren.
    #[cfg(all(test, not(target_os = "macos")))]
    pub(super) fn lose_events(&self) {
        while self.rx.try_recv().is_ok() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
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
            dropped: Arc::new(AtomicUsize::new(0)),
            walker: None,
            skipped: Arc::default(),
            last_recheck: None,
            blind: None,
            blind_reported: false,
            denied: BTreeSet::new(),
            denied_overflow: false,
            rescan_due: false,
            last_rescan: None,
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
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Meldet einen neu angelegten Host-Pfad wie der Watcher.
    #[cfg(test)]
    pub(super) fn note_created(&self, path: &Path) {
        let event = notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::Any))
            .add_path(path.to_path_buf());
        if self.tx.try_send(Ok(event)).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
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
        let (lost, dropped) = self.drain(now);
        let mut batch = Batch {
            overflow: lost,
            dropped,
            ..Batch::default()
        };
        if lost {
            self.rescan_due = true;
        }
        self.rescan(now);
        self.recheck(now);
        if !self.poll_walker(now) {
            batch.overflow = true;
        }
        if !self.blind_reported && self.blind.is_some() {
            self.blind_reported = true;
            batch.blind = self.blind.clone();
        }
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
        batch.denied = classified.denied;
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
        if self.git_metadata(&path) {
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

    /// Gibt zurück, ob Meldungen verloren gingen — und wie viele davon der
    /// volle Kanal abwies (der zählbare Teil).
    fn drain(&mut self, now: Instant) -> (bool, u64) {
        let dropped = self.dropped.swap(0, Ordering::Relaxed) as u64;
        let mut lost = dropped > 0;
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
            if fresh {
                for path in &event.paths {
                    if !self.watch_new_dir(path) {
                        lost = true;
                    }
                }
            }
            // Eine Meldung über ein Verzeichnis (etwa `chmod`): Was darin
            // nicht lesbar war, wird erneut versucht.
            for path in &event.paths {
                for denied in self.denied_under(path) {
                    if !self.queue(self.root.join(denied), now, false) {
                        lost = true;
                    }
                }
            }
            for path in event.paths {
                if !self.queue(path, now, fresh) {
                    lost = true;
                }
            }
        }
        (lost, dropped)
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
            /// Keine Berechtigung, den Pfad (oder ein Verzeichnis davor) zu
            /// prüfen: unvollständig, sofern er nicht ignoriert ist.
            Denied,
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
            if !unique.insert(rel.clone()) {
                continue;
            }
            match self.plain_parents(&rel, &mut plain_dirs) {
                Descent::Plain => {}
                Descent::NotPlain => continue,
                Descent::Denied => {
                    probes.push((path, rel, Probe::Denied));
                    continue;
                }
            }
            let probe = match stat_beneath(&self.root_fd, self.root_dev, &rel) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Probe::Deleted,
                Err(err) if denied(&err) => Probe::Denied,
                // Nicht feststellbar: eine Lücke, keine stille Auslassung.
                Err(_) => {
                    out.truncated = true;
                    continue;
                }
                Ok(stat) => match stat.st_mode & libc::S_IFMT {
                    libc::S_IFLNK => {
                        let text = match readlink_beneath(&self.root_fd, self.root_dev, &rel) {
                            Ok(text) => text,
                            Err(err) if denied(&err) => {
                                probes.push((path, rel, Probe::Denied));
                                continue;
                            }
                            Err(_) => {
                                out.truncated = true;
                                continue;
                            }
                        };
                        let target = link_target(&self.root, &rel, &text).filter(|target| {
                            self.plain_parents(target, &mut plain_dirs) == Descent::Plain
                        });
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
        let Some((rules, unreadable, denied_dirs, tracked)) = self.rules(&asked, now) else {
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
        let mut denials = Vec::new();
        for (path, rel, probe) in probes {
            let is_dir = match probe {
                Probe::Deleted | Probe::Denied => None,
                Probe::Dir => Some(true),
                _ => Some(false),
            };
            // Darunter ist keine `.gitignore` lesbar, weil der Witness das
            // Verzeichnis nicht durchsuchen darf: unvollständig, nie still.
            if under_any(&rel, &denied_dirs) {
                denials.push(rel);
                continue;
            }
            if ignored(&rel, is_dir) {
                // Ignoriert (oder nicht auswertbar, dann ist das die Lücke):
                // kein Befund „nicht lesbar" mehr.
                self.denied.remove(&rel);
                continue;
            }
            if let Probe::Denied = probe {
                denials.push(rel);
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
                Probe::Link(_, Some(target)) if under_any(target, &denied_dirs) => {
                    denials.push(rel);
                    continue;
                }
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
                    self.denied
                        .retain(|path| *path != rel && !path.starts_with(&prefix));
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
                // Schon oben durchsucht bzw. vermerkt.
                Probe::Dir | Probe::Denied => continue,
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
                        Content::Denied => {
                            denials.push(rel);
                            continue;
                        }
                    }
                }
            };
            // Gelesen (oder gelöscht): nicht mehr unlesbar.
            self.denied.remove(&rel);
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
        for rel in denials {
            self.deny(rel, &mut out);
        }
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
    fn rules(&mut self, paths: &[String], now: Instant) -> Option<RuleSet> {
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
        let mut denied_dirs = Vec::new();
        let mut ignored_dirs = Vec::new();
        // Flache Verzeichnisse zuerst: tiefere `.gitignore` gehen vor.
        for (_, dir) in dirs {
            if under_any(&dir, &unreadable)
                || under_any(&dir, &denied_dirs)
                || under_any(&dir, &ignored_dirs)
            {
                continue;
            }
            // Wie Git: In ein ausgeschlossenes Verzeichnis schaut es nicht —
            // seine `.gitignore` gilt nicht (und muss nicht lesbar sein).
            if !dir.is_empty() && rules.is_ignored(&dir, Some(true)) {
                ignored_dirs.push(dir);
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
                Err(RuleError::Denied) => denied_dirs.push(dir),
                Err(RuleError::Unsafe) => unreadable.push(dir),
            }
        }
        Some((rules, unreadable, denied_dirs, tracked))
    }

    /// Eine Regeldatei, gecacht nach Identität und Stand. `Ok(None)`, wenn
    /// sie fehlt; `Err`, wenn sie da, aber nicht sicher lesbar ist oder das
    /// Budget sprengt.
    fn rule_file(
        &mut self,
        rel: &str,
        budget: &mut u64,
    ) -> Result<Option<Arc<Vec<u8>>>, RuleError> {
        let file = match open_beneath(&self.root_fd, self.root_dev, rel) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) if denied(&err) => return Err(RuleError::Denied),
            Err(_) => return Err(RuleError::Unsafe),
        };
        let meta = file.metadata().map_err(|_| RuleError::Unsafe)?;
        if !meta.is_file() || meta.len() > MAX_RULE_BYTES || meta.len() > *budget {
            return Err(RuleError::Unsafe);
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
            .map_err(|_| RuleError::Unsafe)?;
        if bytes.len() as u64 > limit {
            return Err(RuleError::Unsafe);
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
    fn plain_parents(&self, rel: &str, cache: &mut HashMap<String, Descent>) -> Descent {
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

    fn descend_plain(&self, dirs: &[&str]) -> Descent {
        let Ok(mut dir) = self.root_fd.try_clone() else {
            return Descent::NotPlain;
        };
        for part in dirs {
            let Ok(name) = CString::new(*part) else {
                return Descent::NotPlain;
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
                // Fehlt der Rest (gelöscht), ist der Weg schlicht; keine
                // Berechtigung ist eine Lücke; alles andere (ein Symlink)
                // ist nicht schlicht.
                let err = std::io::Error::last_os_error();
                return if err.kind() == std::io::ErrorKind::NotFound {
                    Descent::Plain
                } else if denied(&err) {
                    Descent::Denied
                } else {
                    Descent::NotPlain
                };
            }
            // SAFETY: frisch geöffnet, sonst ohne Besitzer.
            dir = unsafe { File::from_raw_fd(fd) };
            if dir
                .metadata()
                .map_or(true, |meta| meta.dev() != self.root_dev)
            {
                return Descent::NotPlain;
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
                return Descent::NotPlain;
            }
        }
        Descent::Plain
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

/// Ein Verzeichnis des Worktrees, das sich nicht beobachten lässt.
#[derive(Debug)]
struct Unwatchable {
    dir: PathBuf,
    err: String,
}

impl std::fmt::Display for Unwatchable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "directory {} is not watchable: {}",
            self.dir.display(),
            self.err
        )
    }
}

impl std::error::Error for Unwatchable {}

/// Der Gang über neue Verzeichnisse — und nach verlorenen Meldungen über den
/// ganzen Worktree — in eigenem Thread: Er hält den Watcher, damit die
/// Eventloop und mit ihr die Hooks nie auf einen Gang über einen großen Baum
/// warten (vorher fügte `notify` neue Verzeichnisse ebenso in seinem
/// eigenen Thread hinzu).
struct Walker {
    tx: Option<SyncSender<Job>>,
    /// Das erste Verzeichnis, das sich nicht beobachten ließ.
    unwatchable: Arc<std::sync::Mutex<Option<Unwatchable>>>,
    /// Neue Verzeichnisse, deren Watches jetzt stehen: Ihr Inhalt wird
    /// (erneut) durchsucht — was entstand, bevor die Watch stand, meldete
    /// keine.
    watched: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    /// Angenommene, noch nicht erledigte Aufträge.
    busy: Arc<AtomicUsize>,
    /// Angenommene, noch nicht erledigte Gänge über den ganzen Worktree.
    rescans: Arc<AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Ein Auftrag an den [`Walker`].
enum Job {
    /// Ein neu angelegtes oder hereinbenanntes Verzeichnis.
    New(PathBuf),
    /// Nach verlorenen Meldungen: der ganze Worktree.
    Rescan,
    /// Die übersprungenen Verzeichnisse neu bewerten.
    Recheck,
}

/// Wie ein Auftrag an den [`Walker`] ausging.
enum Request {
    Taken,
    /// Die Warteschlange ist voll: eine Lücke, später der ganze Worktree.
    Full,
    Stopped,
}

/// Aufträge, die der [`Walker`] höchstens warten lässt.
const WALK_QUEUE: usize = 4096;

impl Walker {
    fn spawn(
        watcher: notify::RecommendedWatcher,
        root: &Path,
        exemption: Exemption,
        skipped: Arc<std::sync::Mutex<BTreeSet<String>>>,
    ) -> Fallible<Self> {
        let (tx, rx) = mpsc::sync_channel::<Job>(WALK_QUEUE);
        let mut walker = Self {
            tx: Some(tx),
            unwatchable: Arc::default(),
            watched: Arc::default(),
            busy: Arc::default(),
            rescans: Arc::default(),
            stop: Arc::default(),
            thread: None,
        };
        let (found, watched, busy, rescans, stop) = (
            walker.unwatchable.clone(),
            walker.watched.clone(),
            walker.busy.clone(),
            walker.rescans.clone(),
            walker.stop.clone(),
        );
        let root = root.to_path_buf();
        let thread = std::thread::Builder::new()
            .name("minds-witness-walker".into())
            .spawn(move || {
                let mut watcher = watcher;
                let mut exemption = exemption;
                let per_dir = !cfg!(target_os = "macos");
                let mut walk = |dir: &Path| {
                    // Beim ersten Fehler endet der Gang dieses Auftrags: Der
                    // Beobachter ist dann ohnehin blind bis zum Neustart.
                    let result = watch_tree(
                        per_dir.then_some(&mut watcher),
                        dir,
                        &mut Skip {
                            exemption: &mut exemption,
                            skipped: &skipped,
                        },
                        Some(&stop),
                    );
                    if let Err(err) = result {
                        found
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .get_or_insert(err);
                        return false;
                    }
                    true
                };
                while let Ok(first) = rx.recv() {
                    let mut batch = vec![first];
                    batch.extend(rx.try_iter());
                    let taken = batch.len();
                    let mut rescan = 0;
                    let mut news: Vec<PathBuf> = Vec::new();
                    for job in batch {
                        match job {
                            Job::Rescan => rescan += 1,
                            Job::New(dir) if dir.starts_with(&root) => news.push(dir),
                            Job::New(_) => {}
                            // Neu bewertet wird, indem der Gang sie erneut
                            // betritt: Gilt die Ausnahme noch, stehen sie
                            // danach wieder in der Liste.
                            Job::Recheck => {
                                let set = skipped.lock().unwrap_or_else(|e| e.into_inner());
                                news.extend(set.iter().map(|rel| root.join(rel)));
                            }
                        }
                    }
                    // Was unter einem anderen Auftrag liegt, geht dessen
                    // Gang mit (nach Komponenten sortiert folgt alles
                    // darunter unmittelbar).
                    news.sort();
                    news.dedup();
                    let mut tops: Vec<PathBuf> = Vec::new();
                    for dir in news {
                        if !tops.last().is_some_and(|top| dir.starts_with(top)) {
                            tops.push(dir);
                        }
                    }
                    // Ein übersprungenes Verzeichnis, das der Gang erneut
                    // betritt, steht danach nur wieder in der Liste, wenn die
                    // Ausnahme noch gilt.
                    {
                        let mut set = skipped.lock().unwrap_or_else(|e| e.into_inner());
                        for dir in &tops {
                            if let Some(rel) = dir.strip_prefix(&root).ok().and_then(relative_text)
                            {
                                set.remove(&rel);
                            }
                        }
                    }
                    let done: Vec<PathBuf> = if rescan > 0 {
                        // Der ganze Worktree deckt jedes neue Verzeichnis ab.
                        if walk(&root) { tops } else { Vec::new() }
                    } else {
                        tops.into_iter().filter(|dir| walk(dir)).collect()
                    };
                    watched
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend(done);
                    rescans.fetch_sub(rescan, Ordering::SeqCst);
                    busy.fetch_sub(taken, Ordering::SeqCst);
                }
            })?;
        walker.thread = Some(thread);
        Ok(walker)
    }

    fn request(&self, job: Job) -> Request {
        let Some(tx) = &self.tx else {
            return Request::Stopped;
        };
        let rescan = matches!(job, Job::Rescan);
        self.busy.fetch_add(1, Ordering::SeqCst);
        if rescan {
            self.rescans.fetch_add(1, Ordering::SeqCst);
        }
        match tx.try_send(job) {
            Ok(()) => Request::Taken,
            Err(err) => {
                self.busy.fetch_sub(1, Ordering::SeqCst);
                if rescan {
                    self.rescans.fetch_sub(1, Ordering::SeqCst);
                }
                match err {
                    TrySendError::Full(_) => Request::Full,
                    TrySendError::Disconnected(_) => Request::Stopped,
                }
            }
        }
    }

    /// Ob ein Gang über den ganzen Worktree aussteht: Bis er fertig ist,
    /// kann ein neues Verzeichnis ohne Watch sein.
    fn rescanning(&self) -> bool {
        self.rescans.load(Ordering::SeqCst) > 0
    }

    fn take_watched(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *self.watched.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn take_unwatchable(&self) -> Option<Unwatchable> {
        let found = self
            .unwatchable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        // Endete der Thread (Panic), fehlt jeder weitere Gang.
        found.or_else(|| {
            self.thread
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
                .then(|| Unwatchable {
                    dir: PathBuf::from("."),
                    err: "the directory walker stopped".into(),
                })
        })
    }
}

impl Drop for Walker {
    fn drop(&mut self) {
        // Ein laufender Gang bricht ab; ohne Sender endet die Schleife, mit
        // ihr der Watcher.
        self.stop.store(true, Ordering::SeqCst);
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Die Watches des Worktrees beim Start. Auf macOS (FSEvents) gibt es keine
/// Watch je Verzeichnis: dort die Wurzel rekursiv, und jedes Verzeichnis
/// wird nur geprüft — dieselbe Grenze, derselbe Fehler.
fn watch_root(
    watcher: &mut notify::RecommendedWatcher,
    root: &Path,
    exemption: &mut Exemption,
    skipped: &std::sync::Mutex<BTreeSet<String>>,
) -> Fallible<()> {
    let mut skip = Skip { exemption, skipped };
    if cfg!(target_os = "macos") {
        use notify::Watcher;
        watcher.watch(root, notify::RecursiveMode::Recursive)?;
        watch_tree(None, root, &mut skip, None)?;
    } else {
        watch_tree(Some(watcher), root, &mut skip, None)?;
    }
    Ok(())
}

/// Die Pfade eines Index: wie gelesen, und ASCII-gefaltet.
type Tracked = (Arc<BTreeSet<String>>, Arc<BTreeSet<String>>);

/// Wo ein Gang ein nicht lesbares Verzeichnis überspringen darf.
struct Skip<'a> {
    exemption: &'a mut Exemption,
    skipped: &'a std::sync::Mutex<BTreeSet<String>>,
}

impl Skip<'_> {
    /// Bei fehlender Berechtigung für `dir`: überspringen, wenn die
    /// Ausnahme gilt (dann vermerkt) — sonst der Fehler. Jeder andere
    /// Fehler bleibt ein Fehler.
    fn denied(
        &mut self,
        dir: &Path,
        permission: bool,
        err: &dyn std::fmt::Display,
    ) -> Result<(), Unwatchable> {
        let fail = || Unwatchable {
            dir: dir.to_path_buf(),
            err: err.to_string(),
        };
        if !permission {
            return Err(fail());
        }
        let Some(rel) = dir
            .strip_prefix(&self.exemption.root)
            .ok()
            .and_then(relative_text)
        else {
            return Err(fail());
        };
        if !self.exemption.allows(&rel) {
            return Err(fail());
        }
        self.skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(rel);
        Ok(())
    }
}

/// Die bedingte Ausnahme (Entscheidung des Produktverantwortlichen,
/// 28.10.2026): Ein Verzeichnis, das der Witness nicht lesen oder
/// durchsuchen darf, wird übersprungen — nur wenn Git es nach den wirksamen
/// Ignore-Regeln (`.gitignore` bis zu ihm, `.git/info/exclude`) ausschließt
/// **und** weder der Index noch der Baum von HEAD darunter etwas
/// versionieren. Ausgewertet wird im Prozess wie beim Klassifizieren, nie
/// über einen `git`-Aufruf (Konfiguration und Index gehören dem Agenten).
/// Bei jedem Zweifel — eine Regeldatei, der Index oder HEAD nicht lesbar —
/// gilt sie nicht. Eine `.gitignore` ist keine Sicherheitsgrenze: Die
/// Ausnahme entscheidet nur, welche nicht versionierten Verzeichnisse der
/// Witness nicht betreten muss; was darin liegt, zählt nie als beobachtet.
struct Exemption {
    root: PathBuf,
    root_fd: File,
    root_dev: u64,
    /// Die Pfade des zuletzt gelesenen Index (wie gelesen und ASCII-
    /// gefaltet), nach seinem Stand.
    index: Option<(Stamp, Tracked)>,
    /// Das Repository für den Blick in HEAD (geöffnet beim ersten Bedarf).
    repo: Option<minds_git::Repo>,
}

impl Exemption {
    fn new(root: &Path, root_fd: &File, root_dev: u64) -> Fallible<Self> {
        Ok(Self {
            root: root.to_path_buf(),
            root_fd: root_fd.try_clone()?,
            root_dev,
            index: None,
            repo: None,
        })
    }

    /// Eine Datei unter der Wurzel: `Ok(None)`, wenn sie fehlt; `Err` bei
    /// jedem anderen Fehler oder über `limit`.
    fn read(&self, rel: &str, limit: u64) -> Result<Option<(Stamp, Vec<u8>)>, ()> {
        let file = match open_beneath(&self.root_fd, self.root_dev, rel) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(()),
        };
        let meta = file.metadata().map_err(|_| ())?;
        if !meta.is_file() || meta.len() > limit {
            return Err(());
        }
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.len() as u64 > limit {
            return Err(());
        }
        Ok(Some((stamp(&meta), bytes)))
    }

    fn tracked(&mut self) -> Option<Tracked> {
        let file = match open_beneath(&self.root_fd, self.root_dev, ".git/index") {
            Ok(file) => file,
            // Noch kein Index: nichts getrackt.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Some((Arc::default(), Arc::default()));
            }
            Err(_) => return None,
        };
        let meta = file.metadata().ok()?;
        let stamp = stamp(&meta);
        if let Some((cached, paths)) = &self.index {
            if *cached == stamp {
                return Some(paths.clone());
            }
        }
        let (stamp, bytes) = self.read(".git/index", MAX_INDEX_BYTES).ok()??;
        // Nicht lesbar (auch SHA-256), geteilt (`sharedindex.*`) oder ein
        // Pfad, der kein UTF-8 ist: kein Beleg, dass nichts getrackt ist.
        let paths = minds_git::ignore::index_paths_complete(&bytes)?;
        let folded = paths.iter().map(|path| path.to_ascii_lowercase()).collect();
        let tracked = (Arc::new(paths), Arc::new(folded));
        self.index = Some((stamp, tracked.clone()));
        Some(tracked)
    }

    /// Ob das Verzeichnis `rel` (repo-relativ) übersprungen werden darf.
    fn allows(&mut self, rel: &str) -> bool {
        // Nur ASCII-Namen: Ein Dateisystem ohne Unterscheidung der
        // Schreibweise (APFS) setzt auch `Ä`/`ä` und NFC/NFD gleich — das
        // prüfte der Vergleich unten nicht; dann lieber keine Ausnahme.
        if rel.is_empty()
            || !rel.is_ascii()
            || relative_text(Path::new(rel)).as_deref() != Some(rel)
        {
            return false;
        }
        let Some((tracked, folded_index)) = self.tracked() else {
            return false;
        };
        // Ohne Rücksicht auf Groß- und Kleinschreibung: Auf einem
        // Dateisystem, das sie nicht unterscheidet, ist `Data/pg` im Index
        // dasselbe wie `data/pg` auf der Platte — die strenge Richtung.
        let folded = rel.to_ascii_lowercase();
        let prefix = format!("{folded}/");
        if folded_index.contains(&folded)
            || folded_index
                .range(prefix.clone()..)
                .next()
                .is_some_and(|path| path.starts_with(&prefix))
        {
            return false;
        }
        let Ok(exclude) = self.read(".git/info/exclude", MAX_RULE_BYTES) else {
            return false;
        };
        // Muster ohne Faltung, wie Git auf Linux: Die Ausnahme darf nicht
        // mehr ignorieren als Git (`cache/` ignoriert `Cache/` nicht).
        let mut rules = minds_git::ignore::IgnoreRules::new(
            exclude.as_ref().map(|(_, bytes)| bytes.as_slice()),
            tracked,
        )
        .with_case(minds_git::ignore::Case::Sensitive);
        // Die `.gitignore` der Verzeichnisse darüber, flache zuerst — wie
        // Git: in ein schon ausgeschlossenes Verzeichnis schaut es nicht.
        let parts: Vec<&str> = rel.split('/').collect();
        for depth in 0..parts.len() {
            let dir = parts[..depth].join("/");
            if !dir.is_empty() && rules.is_ignored(&dir, Some(true)) {
                break;
            }
            let file = if dir.is_empty() {
                ".gitignore".to_owned()
            } else {
                format!("{dir}/.gitignore")
            };
            match self.read(&file, MAX_RULE_BYTES) {
                Ok(Some((_, bytes))) => rules.add_gitignore(&dir, &bytes),
                Ok(None) => {}
                Err(()) => return false,
            }
        }
        if !rules.is_ignored(rel, Some(true)) {
            return false;
        }
        // Auch HEAD versioniert nichts darunter (ein Index lässt sich
        // ändern, ohne dass der Baum es tut).
        if self.repo.is_none() {
            self.repo = minds_git::Repo::open_pinned(self.root.join(".git")).ok();
        }
        let Some(repo) = &self.repo else {
            return false;
        };
        match repo.head_has_path(rel) {
            Ok(tracked) => !tracked,
            Err(_) => {
                self.repo = None;
                false
            }
        }
    }
}

/// Gibt `start` und jedem Verzeichnis darunter eine eigene, **flache**
/// Watch — außer den Git-Metadaten (jede Komponente `.git`, auch die eines
/// eingebetteten Repos), ohne einem Symlink zu folgen. Die Watch eines
/// Verzeichnisses entsteht, bevor es gelesen wird: Was danach darin
/// entsteht, meldet sie.
///
/// Jedes Verzeichnis, das sich nicht beobachten **oder** nicht lesen und
/// durchsuchen lässt, ist ein Fehler — nie wird eines still übersprungen
/// (`notify` im rekursiven Modus verwirft solche Fehler). Nur was
/// inzwischen verschwand, zählt nicht. Ohne `watcher` (macOS) wird nur
/// geprüft. Mit gesetztem `stop` endet der Gang vorzeitig (Beenden).
fn watch_tree(
    mut watcher: Option<&mut notify::RecommendedWatcher>,
    start: &Path,
    skip: &mut Skip<'_>,
    stop: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), Unwatchable> {
    use notify::Watcher;
    use std::io::ErrorKind::NotFound;
    let fail = |dir: &Path, err: &dyn std::fmt::Display| Unwatchable {
        dir: dir.to_path_buf(),
        err: err.to_string(),
    };
    let is_dir = |dir: &Path| std::fs::symlink_metadata(dir).map(|meta| meta.is_dir());
    // Fehlende Berechtigung — nur sie kann die Ausnahme decken
    // ([`Skip::denied`]); jeder andere Fehler bleibt einer.
    let denied = |err: &std::io::Error| err.kind() == std::io::ErrorKind::PermissionDenied;
    let mut stack = vec![start.to_path_buf()];
    'dirs: while let Some(dir) = stack.pop() {
        if stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            return Ok(());
        }
        let before = match std::fs::symlink_metadata(&dir) {
            // Verschwunden, oder ein Symlink (Git committet seinen Linktext;
            // gefolgt wird ihm nie).
            Err(err) if err.kind() == NotFound => continue,
            Ok(meta) if !meta.is_dir() => continue,
            Err(err) => {
                skip.denied(&dir, denied(&err), &err)?;
                continue;
            }
            Ok(meta) => (meta.dev(), meta.ino()),
        };
        match searchable(&dir) {
            Err(err) if err.kind() == NotFound => continue,
            Err(err) => {
                skip.denied(&dir, denied(&err), &err)?;
                continue;
            }
            Ok(()) => {}
        }
        if let Some(watcher) = watcher.as_deref_mut() {
            if let Err(err) = watcher.watch(&dir, notify::RecursiveMode::NonRecursive) {
                if is_dir(&dir).is_err_and(|err| err.kind() == NotFound) {
                    continue;
                }
                let permission = matches!(&err.kind, notify::ErrorKind::Io(io) if denied(io));
                skip.denied(&dir, permission, &err)?;
                continue;
            }
            // Zwischen Prüfung und Watch ausgetauscht (etwa gegen einen
            // Symlink): Die Watch kann einem anderen Verzeichnis gelten —
            // und `unwatch` dessen Watch entfernen. Also blind, nicht still.
            match std::fs::symlink_metadata(&dir) {
                Err(err) if err.kind() == NotFound => continue,
                Ok(meta) if meta.is_dir() && (meta.dev(), meta.ino()) == before => {}
                Ok(_) => return Err(fail(&dir, &"replaced while it was being watched")),
                Err(err) => return Err(fail(&dir, &err)),
            }
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == NotFound => continue,
            Err(err) => {
                skip.denied(&dir, denied(&err), &err)?;
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) if err.kind() == NotFound => continue,
                Err(err) => {
                    skip.denied(&dir, denied(&err), &err)?;
                    continue 'dirs;
                }
            };
            if is_git_dir_name(&entry.file_name()) {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                Ok(_) => {}
                Err(err) if err.kind() == NotFound => {}
                Err(err) => skip.denied(&entry.path(), denied(&err), &err)?,
            }
        }
    }
    Ok(())
}

/// Ob der Witness (seine effektive Kennung) `dir` lesen und durchsuchen
/// darf. Eine Watch allein belegt das nicht: inotify braucht nur Leserecht,
/// und ein Verzeichnis ohne Suchrecht ließe nichts darunter öffnen.
fn searchable(dir: &Path) -> std::io::Result<()> {
    let name = CString::new(dir.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: nul-terminierter Pfad; `faccessat` liest nur.
    if unsafe {
        libc::faccessat(
            libc::AT_FDCWD,
            name.as_ptr(),
            libc::R_OK | libc::X_OK,
            libc::AT_EACCESS,
        )
    } == 0
    {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    // Ältere Container-Profile (seccomp) lehnen `faccessat2` mit `EPERM` ab,
    // ohne dass die Rechte fehlen: dann selbst prüfen — öffnen (Leserecht)
    // und `.` darin nachschlagen (Suchrecht).
    if !matches!(err.raw_os_error(), Some(libc::EPERM | libc::ENOSYS)) {
        return Err(err);
    }
    let dir = open_dir_nofollow(dir)?;
    // SAFETY: wie oben; `stat` wird vollständig gefüllt.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: gültiger Deskriptor, Name, beschreibbare Struktur.
    if unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c".".as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
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
    /// Keine Berechtigung, die Datei zu öffnen.
    Denied,
}

/// Der Weg zu einem Pfad: schlicht, nicht schlicht (Symlink, anderes
/// Dateisystem, eingebettetes Repo — gehört nicht zu diesem Repo), oder
/// keine Berechtigung (unvollständig beobachtet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Descent {
    Plain,
    NotPlain,
    Denied,
}

/// Die Ignore-Regeln eines Durchlaufs: Regeln, Verzeichnisse mit nicht
/// sicher lesbarer `.gitignore`, nicht durchsuchbare Verzeichnisse, die
/// getrackten Pfade.
type RuleSet = (
    minds_git::ignore::IgnoreRules,
    Vec<String>,
    Vec<String>,
    Arc<BTreeSet<String>>,
);

/// Warum eine Regeldatei nicht gilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleError {
    /// Da, aber nicht sicher lesbar (Symlink, zu groß, Lesefehler): Lücke
    /// `ignore_rules`.
    Unsafe,
    /// Keine Berechtigung (das Verzeichnis ist nicht durchsuchbar): Die Pfade
    /// darunter sind nicht lesbar.
    Denied,
}

/// Keine Berechtigung (`EACCES`/`EPERM`).
fn denied(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied
}

/// Liest `rel` unterhalb der Wurzel, ohne einem Symlink zu folgen — weder in
/// der Datei noch in einem Verzeichnis davor — und ohne das Dateisystem zu
/// verlassen.
fn read_beneath(root: &File, dev: u64, rel: &str, budget: u64) -> Content {
    let file = match open_beneath(root, dev, rel) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Content::Gone,
        Err(err) if denied(&err) => return Content::Denied,
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
